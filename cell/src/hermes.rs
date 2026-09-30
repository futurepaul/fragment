//! The `Hermes` cell: one per fragment whose live `fragment.json` declares
//! `"hermes": {}` (docs/hermes-chat.md), named by the fragment, holding its
//! owner's own Hermes on the fleet's sandcastle node. It is Finite Core's
//! hosted-Hermes binding in the platform's shape:
//!
//! - A key `KEYS` made for this cell owns the computer on the node. It is
//!   registered as a computer its fragment's owner owns, so the model
//!   route bills them and revoking it cuts the Hermes off. The platform's
//!   grantor key (in `KEYS`) grants it one computer.
//! - The computer is public at the node's router; Hermes' own login is the
//!   gate. Its password is generated here, sealed, and never leaves the
//!   platform: a viewer who owns or edits the fragment gets a native
//!   session for it (`grant`), as Finite's dashboard does, and talks to
//!   Hermes directly (`/api/ws`, `/api/sessions`).
//! - The node wakes and sleeps it; the cell only makes it, names the page
//!   that may read it (`cors_origins`), and removes it.
//!
//! Every step is the alarm's, from the row below, and lands once however
//! often it is retried: a failed one is tried again with backoff, and its
//! fragment's `events` says why.

use std::cell::RefCell;
use std::time::Duration;

use fragment_proto::ErrorCode;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use worker::*;

use crate::config::Config;
use crate::error::{CellError, CellResult};
use crate::fragment::{json_response, FragmentCell, MetaKey};
use crate::registry::calls;
use crate::{js, keys};
use fragment_core::hermes::{self as pure, Phase, USERNAME};

/// The marker of a Hermes cell's calls into its fragment (`hermes/…`).
pub(crate) const HEADER: &str = "x-fragment-hermes";
/// The longest wait before a failed step is tried again.
const RETRY_MAX_MS: i64 = 600_000;
/// A call to the node or to Hermes.
const CALL_DEADLINE: Duration = Duration::from_secs(30);
const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS hermes (
  one INTEGER PRIMARY KEY CHECK (one = 1), fragment TEXT NOT NULL, owner TEXT NOT NULL, declared INTEGER NOT NULL,
  phase TEXT NOT NULL, computer TEXT NOT NULL, pubkey TEXT, key_sealed TEXT, identity TEXT, granted INTEGER NOT NULL,
  password_sealed TEXT, secret_sealed TEXT, made INTEGER NOT NULL, url TEXT, origins TEXT NOT NULL, tries INTEGER NOT NULL);";

/// What is asked of a Hermes.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum Ask {
    /// A deploy of `fragment` (its owner's) declares a Hermes, or no longer
    /// does.
    Declare { fragment: String, owner: String, declared: bool },
    /// A native session for a viewer the fragment let in, on a page served
    /// from `origin`.
    Grant { origin: String },
    /// Its fragment was deleted, or its owner removed its key's computer
    /// (`fragment computers rm`): its computer goes, and the key's computer.
    /// A later deploy that declares it makes a new one.
    Destroy,
}

#[derive(Serialize, Deserialize, Clone)]
struct Row {
    fragment: String,
    owner: String,
    #[serde(deserialize_with = "flag")]
    declared: bool,
    phase: Phase,
    /// Its name on the node: derived from the platform and the fragment,
    /// so a retried create names the same one.
    computer: String,
    pubkey: Option<String>,
    key_sealed: Option<String>,
    identity: Option<String>,
    #[serde(deserialize_with = "flag")]
    granted: bool,
    password_sealed: Option<String>,
    secret_sealed: Option<String>,
    /// The node took the computer as last specified.
    #[serde(deserialize_with = "flag")]
    made: bool,
    /// Its URL, as the node answers it (`https://<name>.<domain>/`).
    url: Option<String>,
    /// JSON: the page origins it names (`cors_origins`).
    origins: String,
    tries: i64,
}

fn flag<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<bool, D::Error> {
    Ok(i64::deserialize(d)? != 0)
}

impl Row {
    fn origins(&self) -> Vec<String> {
        serde_json::from_str(&self.origins).expect("origins is the JSON the cell wrote")
    }
}

/// A granted native session: what a page talks to Hermes with.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Grant {
    pub base_url: String,
    pub access_token: String,
    pub expires_at: i64,
}

/// Asks the Hermes of `fragment`; its refusal keeps its code.
pub(crate) async fn ask(env: &Env, fragment: &str, what: &Ask) -> CellResult<Value> {
    let body = serde_json::to_string(what).map_err(|e| CellError::host(e.to_string()))?;
    let mut init = RequestInit::new();
    init.with_method(Method::Post).with_body(Some(body.into()));
    let req = Request::new_with_init("https://hermes.internal/", &init)?;
    let mut resp = env.durable_object("HERMES")?.get_by_name(fragment)?.fetch_with_request(req).await?;
    let (status, bytes) = (resp.status_code(), resp.bytes().await?);
    match (status, serde_json::from_slice::<fragment_proto::ErrorBody>(&bytes)) {
        (200, _) => serde_json::from_slice(&bytes).map_err(|e| CellError::host(format!("{fragment}'s Hermes answered: {e}"))),
        (_, Ok(e)) => Err(CellError::new(e.error, e.message)),
        (s, Err(_)) => Err(CellError::host(format!("{fragment}'s Hermes answered {s}"))),
    }
}

fn upstream(m: impl Into<String>) -> CellError {
    CellError::new(ErrorCode::UpstreamFailed, m)
}

#[durable_object]
pub struct HermesCell {
    state: State,
    env: Env,
    cfg: &'static Config,
    /// One alarm step at a time (celld fires an alarm again when a handler
    /// outlives the node's operation deadline).
    stepping: futures_util::lock::Mutex<()>,
    /// The native session last taken: renewed a minute before it ends.
    session: RefCell<Option<Grant>>,
}

impl DurableObject for HermesCell {
    fn new(state: State, env: Env) -> Self {
        state.storage().sql().exec(SCHEMA, None).expect("the Hermes schema applies");
        let cfg = Config::from_env(&env);
        HermesCell { state, env, cfg, stepping: futures_util::lock::Mutex::new(()), session: RefCell::new(None) }
    }

    async fn fetch(&self, mut req: Request) -> Result<Response> {
        let asked = match serde_json::from_slice::<Ask>(&req.bytes().await?) {
            Ok(Ask::Grant { origin }) => self.grant(&origin).await.and_then(|g| serde_json::to_value(g).map_err(|e| CellError::host(e.to_string()))),
            Ok(a) => self.asked(a).await.map(|()| Value::Null),
            Err(e) => Err(CellError::invalid(format!("body: {e}"))),
        };
        match asked {
            Ok(v) => Response::from_json(&v),
            Err(e) => e.response(),
        }
    }

    async fn alarm(&self) -> Result<Response> {
        let _one = self.stepping.lock().await;
        match self.step().await {
            Ok(Some(again_ms)) => {
                let _ = self.arm(again_ms).await;
            }
            Ok(None) => {}
            Err(e) => {
                let Some(mut h) = self.row().ok().flatten() else { return Response::ok("") };
                h.tries += 1;
                let wait = (self.cfg.hermes_tick_ms << h.tries.min(12)).min(RETRY_MAX_MS);
                let _ = self.save_step(&mut h);
                self.tell(&h, "hermes.failed", &format!("{} (again in {} s)", e.message, wait / 1000)).await;
                let _ = self.arm(wait).await;
            }
        }
        Response::ok("")
    }
}

impl HermesCell {
    fn sql(&self) -> SqlStorage {
        self.state.storage().sql()
    }

    fn row(&self) -> CellResult<Option<Row>> {
        Ok(self.sql().exec("SELECT * FROM hermes", None)?.to_array::<Row>()?.pop())
    }

    fn save(&self, h: &Row) -> CellResult<()> {
        let v = serde_json::to_value(h).map_err(|e| CellError::host(e.to_string()))?;
        let cols = [
            "fragment", "owner", "declared", "phase", "computer", "pubkey", "key_sealed", "identity", "granted", "password_sealed", "secret_sealed", "made", "url", "origins", "tries",
        ];
        let binds = cols.iter().map(|k| match &v[*k] {
            Value::Bool(b) => SqlStorageValue::Integer(i64::from(*b)),
            Value::Number(n) => SqlStorageValue::Integer(n.as_i64().expect("the row's numbers are integers")),
            Value::String(s) => s.as_str().into(),
            _ => SqlStorageValue::Null,
        });
        let q = format!("INSERT OR REPLACE INTO hermes (one, {}) VALUES (1, {})", cols.join(", "), vec!["?"; cols.len()].join(", "));
        self.sql().exec(&q, binds.collect::<Vec<_>>())?;
        Ok(())
    }

    /// Saves a step's row over what asks wrote while it ran (every write
    /// but an ask's own: a step's awaits let a deploy's ask in): a deploy's
    /// `declared` and a removal stand.
    fn save_step(&self, h: &mut Row) -> CellResult<()> {
        if let Some(asked) = self.row()? {
            (h.declared, h.phase) = pure::merged(asked.declared, asked.phase, h.phase);
        }
        self.save(h)
    }

    async fn arm(&self, in_ms: i64) -> CellResult<()> {
        let at = js::now_ms() + in_ms;
        Ok(self.state.storage().set_alarm(ScheduledTime::new(js_sys::Date::new(&worker::wasm_bindgen::JsValue::from_f64(at as f64)))).await?)
    }

    async fn asked(&self, what: Ask) -> CellResult<()> {
        let (_, platform) = self.cfg.sandcastle()?;
        let mut h = match (self.row()?, what) {
            (None, Ask::Declare { fragment, owner, declared }) => {
                let computer = pure::computer_name(platform, &fragment);
                let phase = if declared { Phase::Make } else { Phase::Gone };
                Row {
                    fragment,
                    owner,
                    declared,
                    phase,
                    computer,
                    pubkey: None,
                    key_sealed: None,
                    identity: None,
                    granted: false,
                    password_sealed: None,
                    secret_sealed: None,
                    made: false,
                    url: None,
                    origins: "[]".into(),
                    tries: 0,
                }
            }
            // nothing was ever declared here: nothing to make or remove
            (None, _) => return Ok(()),
            (Some(mut h), Ask::Declare { declared, .. }) => {
                h.declared = declared;
                h.phase = pure::declared(declared, h.phase);
                h
            }
            (Some(mut h), Ask::Destroy) => {
                if h.phase != Phase::Gone {
                    h.phase = Phase::Remove;
                }
                h
            }
            (_, Ask::Grant { .. }) => unreachable!("a grant is asked of `grant`"),
        };
        let due = matches!(h.phase, Phase::Make | Phase::Remove);
        h.tries = if due { 0 } else { h.tries };
        self.save(&h)?;
        if due {
            self.arm(0).await?;
        }
        Ok(())
    }

    /// One step toward what the row asks: `Some(ms)` to look again then.
    async fn step(&self) -> CellResult<Option<i64>> {
        let Some(mut h) = self.row()? else { return Ok(None) };
        let again = match h.phase {
            Phase::Make => self.make(&mut h).await?,
            Phase::Remove => {
                self.remove(&mut h).await?;
                None
            }
            Phase::Ready | Phase::Gone => None,
        };
        h.tries = 0;
        self.save_step(&mut h)?;
        // an ask that came while it ran is due now
        Ok(again.or(matches!(h.phase, Phase::Make | Phase::Remove).then_some(0)))
    }

    /// The next of its making's steps; each lands once, however often it
    /// is retried.
    async fn make(&self, h: &mut Row) -> CellResult<Option<i64>> {
        let (api, platform) = self.cfg.sandcastle()?;
        if h.key_sealed.is_none() {
            let (pubkey, sealed) = keys::nostr_keypair(&self.env).await?;
            (h.pubkey, h.key_sealed) = (Some(pubkey), Some(sealed));
            self.save_step(h)?;
        }
        let pubkey = h.pubkey.clone().expect("a key was made");
        if h.identity.is_none() {
            // A computer its fragment's owner owns: the model route bills
            // them for it, and they may remove it. Pairing again with the
            // same key names the same computer.
            let token = crate::ask_registry(&self.env, &calls::MintPairing { owner: h.owner.clone(), name: pure::identity_name(&h.fragment) }).await?.token;
            let paired = crate::ask_registry(&self.env, &calls::PairWithToken { token, key: pubkey.clone() }).await?;
            h.identity = Some(paired.id);
            self.save_step(h)?;
        }
        if !h.granted {
            let (status, answer) = keys::sandcastle_grant(&self.env, &pubkey, &pure::grant()).await?;
            if status != 200 {
                return Err(upstream(format!("the sandcastle node refused the grant ({status}): {answer}")));
            }
            h.granted = true;
            self.save_step(h)?;
        }
        if h.password_sealed.is_none() {
            h.password_sealed = Some(keys::seal(&self.env, js::random_hex::<32>().as_bytes()).await?);
            h.secret_sealed = Some(keys::seal(&self.env, js::random_hex::<32>().as_bytes()).await?);
            self.save_step(h)?;
        }
        if !h.made {
            self.put_computer(h, api, platform).await?;
            h.made = true;
            self.save_step(h)?;
        }
        // It serves (or sleeps: the node's to choose) once the node has
        // acted on its spec.
        let (status, view) = self.node(h, "GET", &format!("/v1/computers/{}", h.computer), None).await?;
        if status != 200 {
            return Err(upstream(format!("the node answered {status} for its computer: {view}")));
        }
        let state = view["observed"]["state"].as_str().unwrap_or("");
        if let Some(reason) = view["observed"]["reason"].as_str().filter(|_| state == "failed") {
            return Err(upstream(format!("its computer failed: {reason}")));
        }
        let settled = view["pending"] == json!(false) && matches!(state, "serving" | "warm" | "cold");
        if !settled {
            return Ok(Some(self.cfg.hermes_tick_ms));
        }
        h.url = view["url"].as_str().map(str::to_string);
        h.phase = Phase::Ready;
        self.tell(h, "hermes.ready", "its Hermes serves").await;
        Ok(None)
    }

    /// Makes (or converges) its computer on the node, as specified now.
    async fn put_computer(&self, h: &mut Row, api: &str, platform: &str) -> CellResult<()> {
        let _ = api;
        let password = self.opened(h, "password").await?;
        let secret = self.opened(h, "secret").await?;
        let origins = h.origins();
        let body = pure::Spec { image: &self.cfg.hermes_image, model: &self.cfg.hermes_model, platform, origins: &origins }.json(&password, &secret);
        let (status, answer) = self.node(h, "PUT", &format!("/v1/computers/{}", h.computer), Some(&body)).await?;
        match status {
            200 | 201 => Ok(()),
            _ => Err(upstream(format!("the node refused its computer ({status}): {}", answer["message"].as_str().unwrap_or("")))),
        }
    }

    /// Its computer removed, then its key's computer identity (every key it
    /// holds revoked): gone. Its backups stay, as the node keeps them.
    async fn remove(&self, h: &mut Row) -> CellResult<()> {
        if h.key_sealed.is_some() && h.granted {
            let (status, answer) = self.node(h, "DELETE", &format!("/v1/computers/{}", h.computer), None).await?;
            if !matches!(status, 202 | 404) {
                return Err(upstream(format!("the node refused the removal ({status}): {answer}")));
            }
        }
        if let Some(identity) = &h.identity {
            let removed = crate::ask_registry(&self.env, &calls::RemoveComputer { by: calls::By::Identity(h.owner.clone()), computer: identity.clone() }).await;
            match removed {
                Ok(_) => {}
                Err(e) if e.code == ErrorCode::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        // A new declaration makes a new one: a new key, a new computer.
        (h.pubkey, h.key_sealed, h.identity, h.granted, h.made, h.url) = (None, None, None, false, false, None);
        h.phase = Phase::Gone;
        *self.session.borrow_mut() = None;
        self.tell(h, "hermes.removed", "its Hermes was removed").await;
        Ok(())
    }

    /// A sealed value of the row, opened; stored again when `KEYS` resealed it.
    async fn opened(&self, h: &mut Row, which: &str) -> CellResult<String> {
        let sealed = match which {
            "password" => h.password_sealed.clone(),
            _ => h.secret_sealed.clone(),
        }
        .ok_or_else(|| CellError::host(format!("its Hermes has no {which} yet")))?;
        let opened = keys::open(&self.env, &sealed, "").await?;
        if let Some(fresh) = opened.resealed {
            match which {
                "password" => h.password_sealed = Some(fresh),
                _ => h.secret_sealed = Some(fresh),
            }
            self.save_step(h)?;
        }
        String::from_utf8(opened.plaintext).map_err(|_| CellError::host(format!("its {which} is not text")))
    }

    /// One call to the node's API, signed by its computer's key: (status,
    /// the answer as JSON, or null).
    async fn node(&self, h: &mut Row, method: &str, path: &str, body: Option<&Value>) -> CellResult<(u16, Value)> {
        let (api, _) = self.cfg.sandcastle()?;
        let url = format!("{api}{path}");
        let bytes = body.map(|b| b.to_string()).unwrap_or_default();
        let sealed = h.key_sealed.clone().expect("a signed call comes after its key");
        let (header, resealed) = keys::nostr_sign(&self.env, &sealed, method, &url, bytes.as_bytes()).await?;
        if let Some(fresh) = resealed {
            h.key_sealed = Some(fresh);
            self.save_step(h)?;
        }
        let headers = Headers::new();
        headers.set("authorization", &header)?;
        if body.is_some() {
            headers.set("content-type", "application/json")?;
        }
        let mut init = RequestInit::new();
        init.with_method(Method::from(method.to_string())).with_headers(headers);
        if body.is_some() {
            init.with_body(Some(bytes.into()));
        }
        let req = Request::new_with_init(&url, &init)?;
        let mut resp = crate::cs::fetch(req, CALL_DEADLINE).await.map_err(|e| match e {
            crate::cs::FetchError::Failed(m) => upstream(format!("the sandcastle node: {m}")),
            refused => CellError::from(refused),
        })?;
        let status = resp.status_code();
        let answer = resp.json::<Value>().await.unwrap_or(Value::Null);
        Ok((status, answer))
    }

    /// A native session for a page served from `origin` (the fragment
    /// checked who asks): Hermes' own, from its login, which this cell
    /// alone holds. A page origin it does not name yet is named first.
    async fn grant(&self, origin: &str) -> CellResult<Grant> {
        let Some(mut h) = self.row()? else { return Err(CellError::new(ErrorCode::NotFound, "this fragment declares no Hermes")) };
        let url = match (h.phase, h.url.clone()) {
            (Phase::Ready, Some(url)) => url,
            (Phase::Make, _) => return Err(CellError::new(ErrorCode::NotReady, "its Hermes is starting; ask again shortly")),
            _ => return Err(CellError::new(ErrorCode::NotFound, "this fragment has no Hermes")),
        };
        if !h.origins().iter().any(|o| o == origin) {
            // Named as a step is taken, never beside one: a removal's
            // delete must not race this put.
            let _one = self.stepping.lock().await;
            h = self.row()?.ok_or_else(|| CellError::new(ErrorCode::NotFound, "this fragment has no Hermes"))?;
            if h.phase != Phase::Ready {
                return Err(CellError::new(ErrorCode::NotFound, "this fragment has no Hermes"));
            }
            if let Some(origins) = pure::with_origin(&h.origins(), origin) {
                h.origins = serde_json::to_string(&origins).expect("a list serializes");
                let (api, platform) = self.cfg.sandcastle()?;
                let (api, platform) = (api.to_string(), platform.to_string());
                self.put_computer(&mut h, &api, &platform).await?;
                self.save_step(&mut h)?;
            }
        }
        let now_s = js::now_ms() / 1000;
        if let Some(g) = self.session.borrow().as_ref().filter(|g| pure::session_fresh(g.expires_at, now_s) && g.base_url == url) {
            return Ok(g.clone());
        }
        let password = self.opened(&mut h, "password").await?;
        let login = json!({ "provider": "basic", "username": USERNAME, "password": password }).to_string();
        let headers = Headers::new();
        headers.set("content-type", "application/json")?;
        let mut init = RequestInit::new();
        init.with_method(Method::Post).with_headers(headers).with_body(Some(login.into()));
        let req = Request::new_with_init(&format!("{url}auth/password-login"), &init)?;
        let resp = crate::cs::fetch(req, CALL_DEADLINE).await.map_err(|e| match e {
            crate::cs::FetchError::Failed(m) => upstream(format!("its Hermes: {m}")),
            refused => CellError::from(refused),
        })?;
        if resp.status_code() != 200 {
            return Err(upstream(format!("its Hermes refused the platform's login ({})", resp.status_code())));
        }
        let token = resp.headers().get("set-cookie")?.as_deref().and_then(pure::session_cookie).ok_or_else(|| upstream("its Hermes' login gave no session"))?;
        // Its session's end, as Hermes says (never the token's own words).
        let headers = Headers::new();
        headers.set("authorization", &format!("Bearer {token}"))?;
        let mut init = RequestInit::new();
        init.with_method(Method::Get).with_headers(headers);
        let req = Request::new_with_init(&format!("{url}api/auth/me"), &init)?;
        let mut me = crate::cs::fetch(req, CALL_DEADLINE).await.map_err(|e| match e {
            crate::cs::FetchError::Failed(m) => upstream(format!("its Hermes: {m}")),
            refused => CellError::from(refused),
        })?;
        if me.status_code() != 200 {
            return Err(upstream(format!("its Hermes did not know the session it gave ({})", me.status_code())));
        }
        let me: Value = me.json().await.map_err(|_| upstream("its Hermes' answer about its session is not JSON"))?;
        let expires_at = pure::session_end(me["expires_at"].as_i64(), now_s);
        let g = Grant { base_url: url, access_token: token, expires_at };
        *self.session.borrow_mut() = Some(g.clone());
        Ok(g)
    }

    /// An event in its fragment's `events`.
    async fn tell(&self, h: &Row, kind: &str, summary: &str) {
        let body = json!({ "kind": kind, "summary": summary, "computer": h.computer }).to_string();
        let sent = async {
            let req = crate::routed::internal_request("hermes/event", &body)?;
            self.env.durable_object("FRAGMENT")?.get_by_name(&h.fragment)?.fetch_with_request(req).await?;
            Ok::<(), CellError>(())
        };
        if let Err(e) = sent.await {
            console_error!("{}: {kind} ({summary}) did not reach its events: {}", h.fragment, e.message);
        }
    }
}

/// What a fragment keeps of its Hermes' declaration until the Hermes heard it.
#[derive(Serialize, Deserialize)]
struct Declared {
    declared: bool,
}

/// A fragment's side: its Hermes is told what live declares, and asked for
/// sessions.
impl FragmentCell {
    /// Live's `hermes` block, installed: its Hermes is to be told (a
    /// fragment that never declared one tells nothing).
    pub(crate) fn want_hermes(&self, declared: bool) -> CellResult<()> {
        if declared || self.meta(MetaKey::HermesDeclared)?.is_some() {
            self.set_meta(MetaKey::HermesPending, &serde_json::to_string(&Declared { declared }).expect("a declaration serializes"))?;
        }
        Ok(())
    }

    /// Tells its Hermes what live declares; one that did not hear is told
    /// again by the alarm.
    pub(crate) async fn tell_hermes(&self) {
        let told = async {
            let Some(pending) = self.meta(MetaKey::HermesPending)? else { return Ok(()) };
            let d: Declared = serde_json::from_str(&pending).expect("hermes_pending is the JSON the cell wrote");
            let name = self.name()?;
            ask(&self.env, &name, &Ask::Declare { fragment: name.clone(), owner: self.must(MetaKey::Owner)?, declared: d.declared }).await?;
            match d.declared {
                true => self.set_meta(MetaKey::HermesDeclared, "1")?,
                false => self.del_meta(MetaKey::HermesDeclared)?,
            }
            // a deploy meanwhile has something newer to tell
            if self.meta(MetaKey::HermesPending)?.as_deref() == Some(pending.as_str()) {
                self.del_meta(MetaKey::HermesPending)?;
            }
            Ok::<(), CellError>(())
        };
        if let Err(e) = told.await {
            self.event("hermes.failed", &format!("its Hermes did not hear the deploy: {}", e.message), json!({ "code": e.code }));
        }
    }

    /// `POST /__hermes/access`: a native session for its Hermes, for a
    /// signed-in viewer who owns or edits the fragment (docs/hermes-chat.md),
    /// on the page's own origin.
    pub(crate) async fn hermes_access(&self, caller: &crate::fragment::Caller, origin: &str) -> CellResult<Response> {
        self.require(caller, false, fragment_proto::Role::Editor)?;
        if self.meta(MetaKey::HermesDeclared)?.is_none() {
            return Err(CellError::new(ErrorCode::NotFound, "this fragment declares no Hermes"));
        }
        let grant = ask(&self.env, &self.name()?, &Ask::Grant { origin: origin.to_string() }).await?;
        json_response(&grant)
    }

    /// An event its Hermes tells (`hermes/event`).
    pub(crate) fn hermes_asks(&self, what: &str, body: &Value) -> CellResult<Response> {
        match (what, body["kind"].as_str(), body["summary"].as_str()) {
            ("event", Some(kind), Some(summary)) if kind.starts_with("hermes.") => {
                self.event(kind, summary, json!({ "computer": body["computer"] }));
                json_response(&json!({ "ok": true }))
            }
            _ => Err(CellError::invalid(format!("hermes/{what} is event {{kind, summary}}"))),
        }
    }
}
