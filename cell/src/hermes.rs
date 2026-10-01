//! The `Hermes` cell: one per fragment whose live `fragment.json` declares
//! `"computer": {"preset": "hermes"}` (docs/hermes-chat.md), named by the
//! fragment, holding its owner's own Hermes on the fleet's sandcastle node.
//! It is Finite Core's hosted-Hermes binding in the platform's shape:
//!
//! - A key `KEYS` made for this cell owns the computer on the node. It is
//!   registered as a computer its fragment's owner owns, so the model
//!   route bills them and revoking it cuts the Hermes off. The platform's
//!   grantor key (in `KEYS`) grants it one computer.
//! - The computer is reached by its key over iroh (docs/runtime-seam.md):
//!   Hermes runs in its loopback mode, behind a bridge in the guest, with no
//!   login and no password. A viewer who owns or edits the fragment gets an
//!   admission for their page's iroh key, signed by the computer's owner
//!   key in `KEYS` (`access`), and Hermes' session token, its second check;
//!   the page then talks to Hermes directly.
//! - The node wakes and sleeps it; the cell only makes it and removes it.
//!
//! Every step is the alarm's, from the row below, and lands once however
//! often it is retried: a failed one is tried again with backoff, and its
//! fragment's `events` says why.

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
use fragment_core::hermes::{self as pure, Phase};

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
  token_sealed TEXT, made INTEGER NOT NULL, endpoint TEXT, relay TEXT, node_key TEXT, tries INTEGER NOT NULL,
  relay_sealed TEXT, inbox TEXT, memory_mib INTEGER);";
/// Columns a row from before its Relay lacks.
const RELAY_COLUMNS: [(&str, &str); 3] = [("relay_sealed", "TEXT"), ("inbox", "TEXT"), ("memory_mib", "INTEGER")];

/// What is asked of a Hermes.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum Ask {
    /// A deploy of `fragment` (its owner's) declares a Hermes, or no longer
    /// does.
    Declare { fragment: String, owner: String, declared: bool },
    /// An admission for a page's iroh key, for a viewer the fragment let in.
    Access { peer: String },
    /// A chat of `owner`'s that names it as who answers asks who that is:
    /// its computer identity, once made (relay.rs).
    Answerer { owner: String },
    /// That chat, with its Hermes a member, asks it to follow `channel`.
    Listen { chat: String, channel: String, owner: String },
    /// A chat no longer names it.
    Unlisten { chat: String },
    /// Its fragment was deleted, or its owner removed its key's computer
    /// (`fragment computers rm`): its computer goes, and the key's computer.
    /// A later deploy that declares it makes a new one.
    Destroy,
}

#[derive(Serialize, Deserialize, Clone)]
pub(crate) struct Row {
    pub fragment: String,
    pub owner: String,
    #[serde(deserialize_with = "flag")]
    pub declared: bool,
    pub phase: Phase,
    /// Its name on the node: derived from the platform and the fragment,
    /// so a retried create names the same one.
    pub computer: String,
    pub pubkey: Option<String>,
    pub key_sealed: Option<String>,
    pub identity: Option<String>,
    #[serde(deserialize_with = "flag")]
    pub granted: bool,
    /// Hermes' session token, pinned in its spec: its second check behind
    /// the admission.
    pub token_sealed: Option<String>,
    /// The node took the computer as last specified.
    #[serde(deserialize_with = "flag")]
    pub made: bool,
    /// Its iroh key (64 hex) and its node's relay, as the node's view says.
    pub endpoint: Option<String>,
    pub relay: Option<String>,
    /// The node's own key (64 hex), which an admission names.
    pub node_key: Option<String>,
    pub tries: i64,
    /// Its Relay's per-gateway secret (sealed), and the token of the inbox
    /// its chats deliver to (relay.rs).
    #[serde(default)]
    pub relay_sealed: Option<String>,
    #[serde(default)]
    pub inbox: Option<String>,
    /// Its size, as made: none for one made before sizes were kept
    /// (`pure::LEGACY_MEMORY_MIB`).
    #[serde(default)]
    pub memory_mib: Option<i64>,
}

impl Row {
    /// Its size: as made, or a legacy one's.
    pub(crate) fn memory(&self) -> u64 {
        self.memory_mib.and_then(|m| u64::try_from(m).ok()).unwrap_or(pure::LEGACY_MEMORY_MIB)
    }
}

fn flag<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<bool, D::Error> {
    Ok(i64::deserialize(d)? != 0)
}

/// What a page talks to its Hermes with: where it is (its key, its
/// relay), an admission for the page's key, the Host its requests carry,
/// and Hermes' session token.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Access {
    pub endpoint: String,
    pub relay: Option<String>,
    pub node: String,
    /// The admission, as the event's JSON.
    pub admission: String,
    pub host: String,
    pub token: String,
    /// The admission's end, seconds since the epoch: ask again before.
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

/// A request to the Hermes of `fragment` at `path`, as it came (its method,
/// its headers: a WebSocket's upgrade and its token; its body): its Relay's
/// own routes, which check what they are given (relay.rs).
pub(crate) async fn forward(env: &Env, fragment: &str, path: &str, mut req: Request) -> CellResult<Response> {
    if !fragment_proto::valid_fragment_name(fragment) {
        return Err(CellError::new(ErrorCode::NotFound, "no Hermes there"));
    }
    let mut init = RequestInit::new();
    init.with_method(req.method()).with_headers(req.headers().clone());
    if req.method() != Method::Get {
        init.with_body(Some(req.bytes().await?.into()));
    }
    let inner = Request::new_with_init(&format!("https://hermes.internal{path}"), &init)?;
    Ok(env.durable_object("HERMES")?.get_by_name(fragment)?.fetch_with_request(inner).await?)
}

fn upstream(m: impl Into<String>) -> CellError {
    CellError::new(ErrorCode::UpstreamFailed, m)
}

#[durable_object]
pub struct HermesCell {
    pub(crate) state: State,
    pub(crate) env: Env,
    pub(crate) cfg: &'static Config,
    /// One alarm step at a time (celld fires an alarm again when a handler
    /// outlives the node's operation deadline).
    stepping: futures_util::lock::Mutex<()>,
    /// Its Relay's work, one piece at a time: frames, deliveries, its look.
    pub(crate) relaying: futures_util::lock::Mutex<()>,
}

impl DurableObject for HermesCell {
    fn new(state: State, env: Env) -> Self {
        let sql = state.storage().sql();
        sql.exec(SCHEMA, None).expect("the Hermes schema applies");
        sql.exec(crate::relay::SCHEMA, None).expect("the Relay schema applies");
        // a row from before its Relay
        let cols: Vec<Value> = sql.exec("PRAGMA table_info(hermes)", None).and_then(|c| c.to_array()).unwrap_or_default();
        for (col, ty) in RELAY_COLUMNS.iter().filter(|(c, _)| !cols.iter().any(|x| x["name"] == **c)) {
            sql.exec(&format!("ALTER TABLE hermes ADD COLUMN {col} {ty}"), None).expect("the hermes table migrates");
        }
        let cfg = Config::from_env(&env);
        HermesCell { state, env, cfg, stepping: futures_util::lock::Mutex::new(()), relaying: futures_util::lock::Mutex::new(()) }
    }

    async fn fetch(&self, mut req: Request) -> Result<Response> {
        // its Hermes' socket, and its chats' deliveries (the router's, relay.rs)
        let path = req.path();
        if path == "/relay" {
            return self.relay_upgrade(req).await.or_else(|e| e.response());
        }
        if let Some(token) = path.strip_prefix("/inbox/") {
            let _one = self.relaying.lock().await;
            return self.inbox(token, req).await.or_else(|e| e.response());
        }
        let asked = match serde_json::from_slice::<Ask>(&req.bytes().await?) {
            Ok(Ask::Access { peer }) => self.access(&peer).await.and_then(|g| serde_json::to_value(g).map_err(|e| CellError::host(e.to_string()))),
            Ok(Ask::Answerer { owner }) => self.answerer(&owner),
            Ok(Ask::Listen { chat, channel, owner }) => self.listen(&chat, &channel, &owner).await,
            Ok(Ask::Unlisten { chat }) => self.unlisten(&chat).await,
            Ok(a) => self.asked(a).await.map(|()| Value::Null),
            Err(e) => Err(CellError::invalid(format!("body: {e}"))),
        };
        match asked {
            Ok(v) => Response::from_json(&v),
            Err(e) => e.response(),
        }
    }

    async fn websocket_message(&self, ws: WebSocket, message: WebSocketIncomingMessage) -> Result<()> {
        let text = match message {
            WebSocketIncomingMessage::String(s) => s,
            WebSocketIncomingMessage::Binary(b) => String::from_utf8_lossy(&b).into_owned(),
        };
        self.relay_message(&ws, &text).await;
        Ok(())
    }

    async fn websocket_close(&self, _ws: WebSocket, _code: usize, _reason: String, _clean: bool) -> Result<()> {
        Ok(())
    }

    async fn websocket_error(&self, _ws: WebSocket, _error: Error) -> Result<()> {
        Ok(())
    }

    async fn alarm(&self) -> Result<Response> {
        let _one = self.stepping.lock().await;
        let mut next = match self.step().await {
            Ok(again) => again,
            Err(e) => {
                let Some(mut h) = self.row().ok().flatten() else { return Response::ok("") };
                h.tries += 1;
                let wait = (self.cfg.hermes_tick_ms << h.tries.min(12)).min(RETRY_MAX_MS);
                let _ = self.save_step(&mut h);
                self.tell(&h, "hermes.failed", &format!("{} (again in {} s)", e.message, wait / 1000)).await;
                Some(wait)
            }
        };
        match self.relay_tick().await {
            Ok(Some(ms)) => next = Some(next.map_or(ms, |n| n.min(ms))),
            Ok(None) => {}
            Err(e) => console_error!("its Relay's look: {}", e.message),
        }
        if let Some(ms) = next {
            let _ = self.arm(ms).await;
        }
        Response::ok("")
    }
}

impl HermesCell {
    pub(crate) fn sql(&self) -> SqlStorage {
        self.state.storage().sql()
    }

    pub(crate) fn row(&self) -> CellResult<Option<Row>> {
        Ok(self.sql().exec("SELECT * FROM hermes", None)?.to_array::<Row>()?.pop())
    }

    pub(crate) fn save(&self, h: &Row) -> CellResult<()> {
        let v = serde_json::to_value(h).map_err(|e| CellError::host(e.to_string()))?;
        let cols = [
            "fragment", "owner", "declared", "phase", "computer", "pubkey", "key_sealed", "identity", "granted", "token_sealed", "made", "endpoint", "relay", "node_key", "tries",
            "relay_sealed", "inbox", "memory_mib",
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
    pub(crate) fn save_step(&self, h: &mut Row) -> CellResult<()> {
        if let Some(asked) = self.row()? {
            (h.declared, h.phase) = pure::merged(asked.declared, asked.phase, h.phase);
        }
        self.save(h)
    }

    pub(crate) async fn arm(&self, in_ms: i64) -> CellResult<()> {
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
                    token_sealed: None,
                    made: false,
                    endpoint: None,
                    relay: None,
                    node_key: None,
                    tries: 0,
                    relay_sealed: None,
                    inbox: None,
                    memory_mib: Some(pure::MEMORY_MIB as i64),
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
            (_, Ask::Access { .. } | Ask::Answerer { .. } | Ask::Listen { .. } | Ask::Unlisten { .. }) => unreachable!("asked of their own methods"),
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
            let (status, answer) = keys::sandcastle_grant(&self.env, &pubkey, &pure::grant(h.memory())).await?;
            if status != 200 {
                return Err(upstream(format!("the sandcastle node refused the grant ({status}): {answer}")));
            }
            h.granted = true;
            self.save_step(h)?;
        }
        if h.token_sealed.is_none() {
            h.token_sealed = Some(keys::seal(&self.env, js::random_hex::<32>().as_bytes()).await?);
            self.save_step(h)?;
        }
        if h.node_key.is_none() {
            h.node_key = Some(self.node_key(api).await?);
            self.save_step(h)?;
        }
        // its Relay: every Hermes may answer chats (relay.rs)
        if h.relay_sealed.is_none() || h.inbox.is_none() {
            self.give_relay(h).await?;
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
        let Some(endpoint) = view["iroh"]["endpoint"].as_str().filter(|e| pure::valid_peer(e)) else {
            return Err(upstream("the node does not serve computers by their keys (its --iroh-relay)"));
        };
        h.endpoint = Some(endpoint.to_string());
        h.relay = view["iroh"]["relay"].as_str().map(str::to_string);
        h.phase = Phase::Ready;
        self.tell(h, "hermes.ready", "its Hermes serves").await;
        Ok(None)
    }

    /// Makes (or converges) its computer on the node, as specified now.
    async fn put_computer(&self, h: &mut Row, api: &str, platform: &str) -> CellResult<()> {
        let _ = api;
        let token = self.token(h).await?;
        let secret = self.relay_secret(h).await?;
        let relay = secret.as_deref().map(|secret| pure::Relay { id: &h.fragment, secret });
        let body = pure::Spec { image: &self.cfg.hermes_image, model: &self.cfg.hermes_model, memory_mib: h.memory(), platform }.json(&token, relay.as_ref());
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
        // A new declaration makes a new one: a new key, a new computer, a new Relay.
        (h.pubkey, h.key_sealed, h.identity, h.granted, h.token_sealed, h.made, h.endpoint, h.relay) = (None, None, None, false, None, false, None, None);
        (h.relay_sealed, h.inbox, h.memory_mib) = (None, None, Some(pure::MEMORY_MIB as i64));
        self.relay_revoked();
        // the chats it answered join again: a new Hermes is a new identity
        let chats: Vec<Value> = self.sql().exec("SELECT chat FROM relay_chats", None)?.to_array()?;
        for chat in chats.iter().filter_map(|c| c["chat"].as_str()) {
            let told = async {
                let req = crate::routed::internal_request("hermes/rejoin", "{}")?;
                self.env.durable_object("FRAGMENT")?.get_by_name(chat)?.fetch_with_request(req).await?;
                Ok::<(), CellError>(())
            };
            if let Err(e) = told.await {
                console_error!("{}: {chat} was not told to join anew: {}", h.fragment, e.message);
            }
        }
        for t in ["relay_chats", "relay_inbox", "relay_out", "relay_state"] {
            self.sql().exec(&format!("DELETE FROM {t}"), None)?;
        }
        h.phase = Phase::Gone;
        self.tell(h, "hermes.removed", "its Hermes was removed").await;
        Ok(())
    }

    /// Hermes' session token, opened; stored again when `KEYS` resealed it.
    async fn token(&self, h: &mut Row) -> CellResult<String> {
        let sealed = h.token_sealed.clone().ok_or_else(|| CellError::host("its Hermes has no session token yet"))?;
        let opened = keys::open(&self.env, &sealed, "").await?;
        if let Some(fresh) = opened.resealed {
            h.token_sealed = Some(fresh);
            self.save_step(h)?;
        }
        String::from_utf8(opened.plaintext).map_err(|_| CellError::host("its session token is not text"))
    }

    /// The node's own key, from its health (unsigned): what an admission names.
    async fn node_key(&self, api: &str) -> CellResult<String> {
        let req = Request::new(&format!("{api}/v1/health"), Method::Get)?;
        let mut resp = crate::cs::fetch(req, CALL_DEADLINE).await.map_err(|e| match e {
            crate::cs::FetchError::Failed(m) => upstream(format!("the sandcastle node: {m}")),
            refused => CellError::from(refused),
        })?;
        let health: Value = resp.json().await.map_err(|_| upstream("the node's health is not JSON"))?;
        health["node_key"].as_str().filter(|k| pure::valid_peer(k)).map(str::to_string).ok_or_else(|| upstream("the node names no key of its own (its --node-key-file)"))
    }

    /// One call to the node's API, signed by its computer's key: (status,
    /// the answer as JSON, or null).
    pub(crate) async fn node(&self, h: &mut Row, method: &str, path: &str, body: Option<&Value>) -> CellResult<(u16, Value)> {
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

    /// An admission for the page whose iroh key is `peer` (the fragment
    /// checked who asks), signed by its computer's owner key in `KEYS`, and
    /// Hermes' session token: everything the page needs to talk to Hermes
    /// directly, by its key.
    async fn access(&self, peer: &str) -> CellResult<Access> {
        if !pure::valid_peer(peer) {
            return Err(CellError::invalid("peer is the page's iroh key, 64 lowercase hex"));
        }
        let Some(mut h) = self.row()? else { return Err(CellError::new(ErrorCode::NotFound, "this fragment declares no Hermes")) };
        let (endpoint, node) = match (h.phase, h.endpoint.clone(), h.node_key.clone()) {
            (Phase::Ready, Some(endpoint), Some(node)) => (endpoint, node),
            (Phase::Make, _, _) => return Err(CellError::new(ErrorCode::NotReady, "its Hermes is starting; ask again shortly")),
            _ => return Err(CellError::new(ErrorCode::NotFound, "this fragment has no Hermes")),
        };
        let now_s = js::now_ms() / 1000;
        let expires_at = now_s + pure::ADMISSION_S;
        let sealed = h.key_sealed.clone().expect("a ready Hermes has its key");
        let (admission, resealed) = keys::nostr_admission(&self.env, &sealed, peer, &h.computer, &node, now_s, expires_at).await?;
        if let Some(fresh) = resealed {
            h.key_sealed = Some(fresh);
            self.save_step(&mut h)?;
        }
        let token = self.token(&mut h).await?;
        Ok(Access { endpoint, relay: h.relay.clone(), node, admission, host: pure::HOST.to_string(), token, expires_at })
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

    /// `POST /__hermes/access` `{peer}`: an admission to its Hermes for the
    /// page's iroh key, for its owner alone (docs/runtime-seam.md; Hermes'
    /// session token is the whole computer, its screen and logins with it:
    /// docs/one-home.md, decision 5).
    pub(crate) async fn hermes_access(&self, caller: &crate::fragment::Caller, peer: &str) -> CellResult<Response> {
        self.require(caller, false, fragment_proto::Role::Owner)?;
        if self.meta(MetaKey::HermesDeclared)?.is_none() {
            return Err(CellError::new(ErrorCode::NotFound, "this fragment declares no Hermes"));
        }
        let access = ask(&self.env, &self.name()?, &Ask::Access { peer: peer.to_string() }).await?;
        json_response(&access)
    }

    /// `hermes/rejoin`: a Hermes this chat names was removed (a new one, a
    /// new identity, may come): the chat joins whoever answers anew, at its
    /// alarm, until it can, and what is said meanwhile reaches it as it
    /// joins (`hold_floor`).
    pub(crate) async fn hermes_rejoin(&self) -> CellResult<Response> {
        self.hold_floor()?;
        self.set_meta(MetaKey::AgentPending, &js::random_hex::<8>())?;
        self.schedule().await?;
        json_response(&json!({ "ok": true }))
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
