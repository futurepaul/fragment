//! Signed and unsigned HTTP against the node, the way the CLI and a
//! browser call it. Fragment hosts (`<label>--<username>.<suffix>`) are reached by
//! sending the node the right `Host` header; a hosted run's, at their own
//! names over https (`Target::Hosted`).

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use fragment_nip98::Keys;
use fragment_proto::ErrorCode;
use serde_json::{json, Value};

pub fn url_enc(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

pub fn now_s() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).expect("clock after 1970").as_secs() as i64
}

/// The wall clock as the node's logs stamp their lines (UTC, to the
/// millisecond), so a call that failed can be found in them.
pub fn clock() -> String {
    let ms = SystemTime::now().duration_since(UNIX_EPOCH).expect("clock after 1970").as_millis();
    let s = (ms / 1000) % 86_400;
    format!("{:02}:{:02}:{:02}.{:03}Z", s / 3600, (s / 60) % 60, s % 60, ms % 1000)
}

/// How long workerd keeps an idle keep-alive connection: it leaves kj's
/// `HttpServerSettings::pipelineTimeout` at its default, and closes a
/// connection 5 s after its last answer (measured under `wrangler dev`).
const SERVER_KEEP_ALIVE: Duration = Duration::from_secs(5);
/// How long the run's client keeps an idle connection to use again: under
/// the server's. A request written onto a connection as the server closes
/// it gets no answer ("connection closed before message completed", or a
/// reset), and the client does not send a POST again: the hermes lane's
/// skills polls, 5 s apart, lost one now and then (2026-10-05). Under the
/// server's, the pool never hands out a connection the server may be
/// closing.
const POOL_IDLE: Duration = Duration::from_secs(4);
const _: () = assert!(POOL_IDLE.as_millis() < SERVER_KEEP_ALIVE.as_millis(), "the client drops an idle connection before the server does");

fn client() -> reqwest::blocking::Client {
    client_with(POOL_IDLE)
}

fn client_with(pool_idle: Duration) -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(60))
        .pool_idle_timeout(pool_idle)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("http client")
}

/// The current second, waited for until it has just begun: a request sent
/// now reaches the node within the same second (it shares this clock), so
/// a timestamp at the exact edge of a window is judged at that edge.
pub fn second_start() -> i64 {
    let into = SystemTime::now().duration_since(UNIX_EPOCH).expect("clock after 1970").subsec_millis();
    std::thread::sleep(Duration::from_millis(u64::from(1000 - into) + 5));
    now_s()
}

pub struct Reply {
    pub status: u16,
    pub body: Value,
    pub text: String,
    pub bytes: Vec<u8>,
    pub headers: reqwest::header::HeaderMap,
}

impl std::fmt::Display for Reply {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let text: String = self.text.chars().take(400).collect();
        write!(f, "{} {text}", self.status)
    }
}

impl Reply {
    pub fn error(&self) -> &str {
        self.body["error"].as_str().unwrap_or("")
    }

    pub fn message(&self) -> &str {
        self.body["message"].as_str().unwrap_or("")
    }

    /// The refusal's typed code (`{"error": code}`), when the body names one:
    /// a check matches it, never the message.
    pub fn code(&self) -> Option<ErrorCode> {
        serde_json::from_value(self.body["error"].clone()).ok()
    }

    pub fn header(&self, name: &str) -> String {
        self.headers.get(name).and_then(|v| v.to_str().ok()).unwrap_or("").to_string()
    }

    /// `name=value` of each Set-Cookie, a `__Host-` name as its plain one
    /// (over https the platform's and a site's sessions are `__Host-`
    /// cookies: the hosted run's checks read them as a local run's).
    pub fn cookies(&self) -> Vec<String> {
        self.headers
            .get_all("set-cookie")
            .iter()
            .filter_map(|v| v.to_str().ok())
            .map(|c| c.split(';').next().unwrap_or(""))
            .map(|c| c.strip_prefix(HOST_PREFIX).unwrap_or(c).to_string())
            .collect()
    }
}

/// The prefix browsers hold an https host's own cookies under (auth.rs `cookie_name`).
pub(crate) const HOST_PREFIX: &str = "__Host-";
/// The cookies the cell names `__Host-` over https, at a host's root.
pub(crate) const HOST_COOKIES: [&str; 5] = ["fragment_session", "fragment_site", "fragment_frame", "fragment_login", "fragment_computer"];

/// A `Cookie` header as a browser on https sends it: each of the cell's
/// host cookies under its `__Host-` name, the rest as they are.
pub fn https_cookies(header: &str) -> String {
    header
        .split(';')
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .map(|c| match c.split_once('=') {
            Some((name, value)) if HOST_COOKIES.contains(&name) => format!("{HOST_PREFIX}{name}={value}"),
            _ => c.to_string(),
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// One request's shape.
#[derive(Default)]
pub struct Call<'a> {
    pub method: &'a str,
    /// Absolute URL as the server will see it (the Host header derives from it).
    pub url: String,
    pub body: Option<Vec<u8>>,
    pub content_type: Option<&'a str>,
    pub cookie: Option<String>,
    pub keys: Option<&'a Keys>,
    /// Sent verbatim (tests of forged headers).
    pub extra: Vec<(&'a str, String)>,
}

/// Where a run's requests go.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// The local node: every request to 127.0.0.1 on its port, the site
    /// named in `Host` (`*.localhost`, as a browser resolves it).
    Local,
    /// A branch deployment (a preview), over https at its own hosts: the
    /// platform at `<branch>.<zone>`, a fragment at
    /// `<label>--<username>--<branch>.<zone>`.
    Hosted(Preview),
}

/// A branch deployment, as the hosted run reaches it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Preview {
    pub zone: String,
    pub branch: String,
    /// `https`, but in the client's own tests, which serve http on a port.
    scheme: &'static str,
    port: Option<u16>,
}

impl Preview {
    pub fn new(zone: &str, branch: &str) -> Preview {
        Preview { zone: zone.to_string(), branch: branch.to_string(), scheme: "https", port: None }
    }

    /// The platform's origin.
    pub fn platform(&self) -> String {
        format!("{}://{}.{}{}", self.scheme, self.branch, self.zone, self.port_part())
    }

    /// A fragment's origin (`name` is `<label>.<username>`).
    pub fn fragment(&self, name: &str) -> Option<String> {
        Some(format!("{}://{}--{}.{}{}", self.scheme, fragment_proto::flat_name(name)?, self.branch, self.zone, self.port_part()))
    }

    fn port_part(&self) -> String {
        self.port.map(|p| format!(":{p}")).unwrap_or_default()
    }

    /// A preview served over http on `port`: the client's own tests'.
    #[cfg(test)]
    pub fn local_http(zone: &str, branch: &str, port: u16) -> Preview {
        Preview { zone: zone.to_string(), branch: branch.to_string(), scheme: "http", port: Some(port) }
    }
}

/// What every API of one run shares: the levers' secret (sent to the
/// platform's `/api/test/*` and nowhere else), and, hosted, the people it
/// signed in and the paid calls it may still lend them.
pub struct Run {
    secret: String,
    /// People sign in through the levers (the hosted lane's rules: a
    /// preview, or its rehearsal on the local node), not through WorkOS.
    levers_sign_in: bool,
    /// Lanes call from threads of their own (site.rs): shared under locks.
    people: Mutex<Vec<String>>,
    budget: Mutex<Budget>,
}

/// The run's paid calls: those it may still lend, and those it lent.
#[derive(Clone, Copy)]
struct Budget {
    left: u64,
    lent: u64,
}

impl Run {
    /// A local run's: people sign in through the WorkOS fake.
    pub fn new(secret: String, paid_calls: u64) -> Arc<Run> {
        Run::make(secret, false, paid_calls)
    }

    /// The hosted lane's: people sign in through the levers, each lent
    /// some of `paid_calls`.
    pub fn signing_in_by_levers(secret: String, paid_calls: u64) -> Arc<Run> {
        Run::make(secret, true, paid_calls)
    }

    fn make(secret: String, levers_sign_in: bool, paid_calls: u64) -> Arc<Run> {
        assert!(secret.len() >= fragment_core::levers::SECRET_BYTES_MIN, "a test secret is long");
        Arc::new(Run { secret, levers_sign_in, people: Mutex::new(vec![]), budget: Mutex::new(Budget { left: paid_calls, lent: 0 }) })
    }

    /// The people this run signed in (hosted), oldest first.
    pub fn people(&self) -> Vec<String> {
        self.people.lock().expect("the people's lock").clone()
    }

    /// The paid calls the run may still lend, and those it lent.
    pub fn paid_calls_left(&self) -> u64 {
        self.budget.lock().expect("the budget's lock").left
    }

    pub fn paid_calls_lent(&self) -> u64 {
        self.budget.lock().expect("the budget's lock").lent
    }

    /// Lends `n` of the run's paid calls to a person about to sign in;
    /// refused when the run has fewer left.
    fn lend(&self, n: u64) -> Result<()> {
        let mut budget = self.budget.lock().expect("the budget's lock");
        let before = *budget;
        anyhow::ensure!(n <= before.left, "the run's paid calls are spent: {n} asked, {} left (--max-paid-calls)", before.left);
        *budget = Budget { left: before.left - n, lent: before.lent + n };
        assert_eq!(budget.left + budget.lent, before.left + before.lent, "lending moves calls, and makes none");
        Ok(())
    }

    /// Notes a person this run signed in, once.
    fn signed_in(&self, identity: &str) {
        let mut people = self.people.lock().expect("the people's lock");
        if !people.iter().any(|p| p == identity) {
            people.push(identity.to_string());
        }
    }
}

pub struct Api {
    http: reqwest::blocking::Client,
    pub base: String,
    pub port: u16,
    pub suffix: Option<String>,
    /// A branch's mark on its fragments' hosts (`--<branch>`), on a local
    /// node shaped as a branch deployment (the hosted lane's rehearsal).
    label_suffix: String,
    pub target: Target,
    run: Arc<Run>,
}

impl Api {
    /// The local node's API (`suffix`: fragments on their own hosts).
    pub fn new(port: u16, suffix: Option<&str>, run: &Arc<Run>) -> Api {
        let base = format!("http://127.0.0.1:{port}");
        Api { http: client(), base, port, suffix: suffix.map(str::to_string), label_suffix: String::new(), target: Target::Local, run: Arc::clone(run) }
    }

    /// The local node's API, the node shaped as the branch `branch`.
    pub fn branch(mut self, branch: &str) -> Api {
        self.label_suffix = format!("--{branch}");
        self
    }

    /// A preview's API, at its own hosts over https.
    pub fn hosted(preview: &Preview, run: &Arc<Run>) -> Api {
        let (base, label_suffix) = (preview.platform(), String::new());
        Api { http: client(), base, port: preview.port.unwrap_or(443), suffix: Some(preview.zone.clone()), label_suffix, target: Target::Hosted(preview.clone()), run: Arc::clone(run) }
    }

    /// `hosted`, each of `hosts` resolved to `at`: the client's own tests,
    /// against a server of theirs.
    #[cfg(test)]
    pub fn hosted_at(preview: &Preview, run: &Arc<Run>, hosts: &[String], at: std::net::SocketAddr) -> Api {
        let mut api = Api::hosted(preview, run);
        let mut builder = reqwest::blocking::Client::builder().timeout(Duration::from_secs(10)).redirect(reqwest::redirect::Policy::none());
        for host in hosts {
            builder = builder.resolve(host, at);
        }
        api.http = builder.build().expect("http client");
        api
    }

    /// Whether people sign in through the levers (the hosted lane's rules).
    pub fn signs_in_by_levers(&self) -> bool {
        self.run.levers_sign_in
    }

    /// The URL of `path` on a fragment's own host (or its `/f/<name>/` path
    /// when the fleet has no suffix).
    /// A fragment's page: on its own host (`<label>--<username>.<suffix>`)
    /// when the fleet has a suffix, else by path.
    pub fn site_url(&self, name: &str, path: &str) -> String {
        let host = fragment_proto::flat_name(name).unwrap_or_else(|| name.to_string());
        match (&self.target, &self.suffix) {
            (Target::Hosted(preview), _) => match preview.fragment(name) {
                Some(origin) => format!("{origin}/{path}"),
                None => format!("{}://{host}--{}.{}{}/{path}", preview.scheme, preview.branch, preview.zone, preview.port_part()),
            },
            (Target::Local, Some(s)) => format!("http://{host}{}.{s}:{}/{path}", self.label_suffix, self.port),
            (Target::Local, None) => format!("{}/f/{name}/{path}", self.base),
        }
    }

    /// A fragment's own origin, as a browser on its page names it (`Origin`).
    pub fn site_origin(&self, name: &str) -> String {
        reqwest::Url::parse(&self.site_url(name, "")).expect("a fragment's URL parses").origin().ascii_serialization()
    }

    /// Whether a request to `url` is a lever's: the platform's `/api/test/*`.
    fn is_lever(&self, url: &str) -> bool {
        url.strip_prefix(self.base.as_str()).is_some_and(|path| path.starts_with("/api/test/"))
    }

    pub fn call(&self, c: Call<'_>) -> Result<Reply> {
        self.send(c, true)
    }

    /// `call`, without the run's secret even on a lever's route: the gate's
    /// own checks send none, or another.
    pub fn call_without_secret(&self, c: Call<'_>) -> Result<Reply> {
        self.send(c, false)
    }

    fn send(&self, c: Call<'_>, secret: bool) -> Result<Reply> {
        let url = reqwest::Url::parse(&c.url)?;
        let body = c.body.unwrap_or_default();
        let mut req = match &self.target {
            // Everything goes to the node; the Host header names the site.
            Target::Local => {
                let host = format!("{}:{}", url.host_str().unwrap_or(""), url.port().unwrap_or(80));
                let mut to = url.clone();
                to.set_host(Some("127.0.0.1")).expect("an http URL takes a host");
                to.set_port(Some(self.port)).expect("an http URL takes a port");
                self.http.request(c.method.parse()?, to).header("host", host)
            }
            // A deployment's hosts are its own, over https.
            Target::Hosted(_) => self.http.request(c.method.parse()?, url.clone()),
        }
        .body(body.clone());
        if let Some(ct) = c.content_type {
            req = req.header("content-type", ct);
        }
        if let Some(cookie) = c.cookie {
            let cookie = match url.scheme() {
                "https" => https_cookies(&cookie),
                _ => cookie,
            };
            req = req.header("cookie", cookie);
        }
        if let Some(keys) = c.keys {
            req = req.header("authorization", keys.header(c.method, &c.url, &body, now_s()));
        }
        // the secret goes to the platform's levers, never to a fragment's
        // host, where an app's code reads the request's headers
        if secret && self.is_lever(&c.url) {
            req = req.header(fragment_core::levers::SECRET_HEADER, self.run.secret.as_str());
        }
        for (k, v) in c.extra {
            req = req.header(k, v);
        }
        // when it went and how long it waited, so a failure tells a refused
        // or dropped connection (at once) from a request that hung
        let (sent, t0) = (clock(), Instant::now());
        let failed = || format!("{} {} (sent {sent}, failed after {:.1?})", c.method, c.url, t0.elapsed());
        let resp = req.send().with_context(failed)?;
        let status = resp.status().as_u16();
        let headers = resp.headers().clone();
        let bytes = resp.bytes().with_context(failed)?.to_vec();
        let text = String::from_utf8_lossy(&bytes).into_owned();
        let body = serde_json::from_str(&text).unwrap_or(Value::Null);
        Ok(Reply { status, body, text, bytes, headers })
    }

    /// The control API, signed by `keys`.
    pub fn signed(&self, keys: &Keys, method: &str, path: &str, body: Option<&Value>) -> Result<Reply> {
        self.call(Call {
            method,
            url: format!("{}{path}", self.base),
            body: body.map(|b| b.to_string().into_bytes()),
            content_type: body.map(|_| "application/json"),
            keys: Some(keys),
            ..Call::default()
        })
    }

    pub fn unsigned(&self, method: &str, path: &str, body: Option<&Value>) -> Result<Reply> {
        self.call(Call {
            method,
            url: format!("{}{path}", self.base),
            body: body.map(|b| b.to_string().into_bytes()),
            content_type: body.map(|_| "application/json"),
            ..Call::default()
        })
    }

    /// A GET to another service (the WorkOS fake), as it is.
    pub fn external(&self, url: &str) -> Result<Reply> {
        let resp = self.http.get(url).send().with_context(|| format!("GET {url}"))?;
        let status = resp.status().as_u16();
        let headers = resp.headers().clone();
        let bytes = resp.bytes()?.to_vec();
        let text = String::from_utf8_lossy(&bytes).into_owned();
        Ok(Reply { status, body: serde_json::from_str(&text).unwrap_or(Value::Null), text, bytes, headers })
    }

    /// A browser signing in as `email`: the platform session's cookie
    /// value. Locally through WorkOS (the fake); hosted, through the
    /// levers' e2e sign-in, with no paid calls.
    pub fn sign_in(&self, email: &str) -> Result<String> {
        match self.run.levers_sign_in {
            false => self.sign_in_through_workos(email),
            true => self.e2e_sign_in(email, 0).map(|(session, _)| session),
        }
    }

    /// An e2e person's platform session and identity, from the levers'
    /// sign-in (`POST /api/test/signin`): a seat whose paid calls (model
    /// calls and AI steps) are capped at `paid_calls`, lent from the run's
    /// budget. Their identity joins the run's people (its spend is theirs).
    pub fn e2e_sign_in(&self, email: &str, paid_calls: u64) -> Result<(String, String)> {
        self.run.lend(paid_calls)?;
        let r = self.unsigned("POST", "/api/test/signin", Some(&json!({ "email": email, "paidCalls": paid_calls })))?;
        anyhow::ensure!(r.status == 200, "the e2e sign-in of {email}: {r}");
        let session = r.body["session"].as_str().context("an e2e sign-in answers a session")?.to_string();
        let identity = r.body["identity"].as_str().context("an e2e sign-in answers an identity")?.to_string();
        anyhow::ensure!(r.body["paidCalls"] == paid_calls, "the e2e sign-in capped {email} at {}, not {paid_calls}", r.body["paidCalls"]);
        self.run.signed_in(&identity);
        Ok((session, identity))
    }

    /// Someone who signs, with `paid_calls` of the run's budget to spend
    /// (hosted; locally `person`, whose model is the fake's).
    pub fn person_paying(&self, paid_calls: u64) -> Result<Keys> {
        if !self.run.levers_sign_in {
            return self.person();
        }
        let keys = Keys::generate();
        let (session, _) = self.e2e_sign_in(&Api::email_of(&keys), paid_calls)?;
        let me = self.approve(&session, &keys)?;
        anyhow::ensure!(me.status == 200 && me.body["id"].is_string(), "an approved key works: {me}");
        Ok(keys)
    }

    /// A browser signing in as `email` through WorkOS (the fake): the
    /// platform session's cookie value.
    fn sign_in_through_workos(&self, email: &str) -> Result<String> {
        let path = format!("/auth/login?return=/&login_hint={}", url_enc(email));
        let start = self.unsigned("GET", &path, None)?;
        anyhow::ensure!(start.status == 302, "GET {path}: {start}");
        let bound = start.cookies().into_iter().find(|c| c.starts_with("fragment_login=")).context("a login cookie")?;
        let back = self.external(&start.header("location"))?;
        anyhow::ensure!(back.status == 302, "WorkOS (fake) authorize: {back}");
        let done = self.call(Call { method: "GET", url: back.header("location"), cookie: Some(bound), ..Call::default() })?;
        anyhow::ensure!(done.status == 302, "the callback: {done}");
        let session = done.cookies().into_iter().find_map(|c| c.strip_prefix("fragment_session=").map(str::to_string)).context("a session cookie")?;
        Ok(session)
    }

    /// The link `fragment login` prints for `keys`: its npub and its own
    /// proof for `POST /cli/approve` (made `age_s` ago).
    pub fn approval_link(&self, keys: &Keys, age_s: i64) -> String {
        let proof = keys.header("POST", &format!("{}/cli/approve", self.base), b"", now_s() - age_s);
        let proof = proof.strip_prefix("Nostr ").unwrap_or(&proof).to_string();
        format!("{}/cli?key={}&proof={}", self.base, fragment_core::npub::encode(keys.pubkey_hex()), url_enc(&proof))
    }

    /// The signed-in browser approves the key an approval link names
    /// (`/cli`'s form, with the link's key and proof).
    pub fn approve_link(&self, session: &str, link: &str) -> Result<Reply> {
        let url = reqwest::Url::parse(link)?;
        let field = |k: &str| url.query_pairs().find(|(q, _)| q == k).map(|(_, v)| v.into_owned()).unwrap_or_default();
        let body = format!("key={}&proof={}", url_enc(&field("key")), url_enc(&field("proof")));
        self.call(Call {
            method: "POST",
            url: format!("{}/cli/approve", self.base),
            body: Some(body.into_bytes()),
            content_type: Some("application/x-www-form-urlencoded"),
            cookie: Some(format!("fragment_session={session}")),
            extra: vec![("origin", self.base.clone())],
            ..Call::default()
        })
    }

    /// The signed-in browser approves `keys` (its own link), and it works:
    /// the answer is the key's `GET /api/identities/me`.
    pub fn approve(&self, session: &str, keys: &Keys) -> Result<Reply> {
        let r = self.approve_link(session, &self.approval_link(keys, 0))?;
        anyhow::ensure!(r.status == 200, "approving a key: {r}");
        let me = self.signed(keys, "GET", "/api/identities/me", None)?;
        // every person the e2e makes takes a username at once (decision 16),
        // named after their identity
        if me.status == 200 && me.body["kind"] == "person" && me.body["username"].is_null() {
            let id = me.body["id"].as_str().unwrap_or("id:0000000000");
            let username = format!("p{}", &id.trim_start_matches("id:")[..10]);
            let r = self.signed(keys, "PUT", "/api/identities/me/username", Some(&json!({ "username": username })))?;
            anyhow::ensure!(r.status == 200, "taking a username: {r}");
            return self.signed(keys, "GET", "/api/identities/me", None);
        }
        Ok(me)
    }

    /// A person with an approved key and no username yet.
    pub fn person_without_username(&self) -> Result<Keys> {
        let keys = Keys::generate();
        let session = self.sign_in(&format!("n-{}@e2e.test", &keys.pubkey_hex()[..12]))?;
        let r = self.approve_link(&session, &self.approval_link(&keys, 0))?;
        anyhow::ensure!(r.status == 200, "approving a key: {r}");
        Ok(keys)
    }

    /// The username of the person `keys` belongs to (an agent's owner's).
    pub fn username(&self, keys: &Keys) -> Result<String> {
        let r = self.signed(keys, "GET", "/api/identities/me", None)?;
        anyhow::ensure!(r.status == 200, "GET /api/identities/me: {r}");
        if let Some(u) = r.body["username"].as_str() {
            return Ok(u.to_string());
        }
        let owner = r.body["owner"].as_str().context("no username, and no owner")?;
        let v = self.signed(keys, "GET", &format!("/api/identities/{owner}"), None)?;
        Ok(v.body["username"].as_str().context("the owner has no username")?.to_string())
    }

    /// `label`'s full name under the username of whoever `keys` is.
    pub fn qualified(&self, keys: &Keys, label: &str) -> Result<String> {
        Ok(fragment_proto::fragment_name(label, &self.username(keys)?))
    }

    /// Someone who signs: a person signed in through WorkOS (the fake),
    /// with a new CLI key they approved.
    pub fn person(&self) -> Result<Keys> {
        let keys = Keys::generate();
        let session = self.sign_in(&Api::email_of(&keys))?;
        let me = self.approve(&session, &keys)?;
        anyhow::ensure!(me.status == 200 && me.body["id"].is_string(), "an approved key works: {me}");
        Ok(keys)
    }

    /// The email `person` signs in with: a lane connects that WorkOS
    /// user's accounts (Pipes) by it.
    pub fn email_of(keys: &Keys) -> String {
        format!("p-{}@e2e.test", &keys.pubkey_hex()[..12])
    }

    /// The identity `keys` belongs to.
    pub fn identity(&self, keys: &Keys) -> Result<String> {
        let r = self.signed(keys, "GET", "/api/identities/me", None)?;
        anyhow::ensure!(r.status == 200, "GET /api/identities/me: {r}");
        Ok(r.body["id"].as_str().context("an identity has an id")?.to_string())
    }

    /// A key proof by `new` for a request `signer` signs.
    pub fn proof(&self, new: &Keys, method: &str, path: &str, signer: &Keys) -> String {
        new.proof(method, &format!("{}{path}", self.base), signer.pubkey_hex(), now_s())
    }

    pub fn create(&self, keys: &Keys, name: &str) -> Result<Reply> {
        self.create_with(keys, json!({ "name": name }))
    }

    pub fn create_with(&self, keys: &Keys, body: Value) -> Result<Reply> {
        self.signed(keys, "POST", "/api/fragments", Some(&body))
    }

    pub fn status(&self, keys: &Keys, name: &str) -> Result<Reply> {
        self.signed(keys, "GET", &format!("/api/f/{name}/status"), None)
    }

    pub fn op(&self, keys: &Keys, name: &str, op: &str, id: &str, input: Value) -> Result<Reply> {
        self.signed(keys, "POST", &format!("/api/f/{name}/ops/{op}"), Some(&json!({ "id": id, "input": input })))
    }

    /// A page of the fragment's site, as a browser with `cookie` asks.
    pub fn page(&self, name: &str, path: &str, cookie: Option<&str>) -> Result<Reply> {
        self.call(Call { method: "GET", url: self.site_url(name, path), cookie: cookie.map(str::to_string), ..Call::default() })
    }

    /// A browser's operation call.
    pub fn browser_op(&self, name: &str, op: &str, id: &str, input: Value, cookie: Option<&str>) -> Result<Reply> {
        self.call(Call {
            method: "POST",
            url: self.site_url(name, &format!("__op/{op}")),
            body: Some(json!({ "id": id, "input": input }).to_string().into_bytes()),
            content_type: Some("application/json"),
            cookie: cookie.map(str::to_string),
            ..Call::default()
        })
    }
}

/// The TCP stream under a socket, plain (local) or TLS (hosted), for its timeouts.
fn tcp(stream: &tungstenite::stream::MaybeTlsStream<std::net::TcpStream>) -> Option<&std::net::TcpStream> {
    match stream {
        tungstenite::stream::MaybeTlsStream::Plain(s) => Some(s),
        tungstenite::stream::MaybeTlsStream::Rustls(s) => Some(s.get_ref()),
        _ => None,
    }
}

/// Opens the change feed; `keys` signs the upgrade.
pub fn watch(api: &Api, name: &str, query: &str, keys: Option<&Keys>) -> Result<tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<std::net::TcpStream>>> {
    use tungstenite::client::IntoClientRequest;
    let http = format!("{}/f/{name}/__watch{query}", api.base);
    let mut req = http.replacen("http", "ws", 1).into_client_request()?;
    if let Some(k) = keys {
        req.headers_mut().insert("authorization", k.header("GET", &http, &[], now_s()).parse()?);
    }
    let (socket, _) = tungstenite::connect(req)?;
    if let Some(s) = tcp(socket.get_ref()) {
        s.set_read_timeout(Some(Duration::from_secs(10)))?;
    }
    Ok(socket)
}

/// A socket to a fragment's `__watch` or `__live`, read as JSON frames.
pub struct Socket(tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<std::net::TcpStream>>);

impl Socket {
    /// Opens `/f/<name>/<path>` (reachable in place on every fleet);
    /// `keys` signs the upgrade, `cookie` rides along like a browser's. A
    /// `__live` socket speaks the current protocol (`?v=2`), as the browser
    /// library's does; `__live?v=1` opens one as a page from before it.
    pub fn open(api: &Api, name: &str, path: &str, keys: Option<&Keys>, cookie: Option<&str>) -> Result<Socket> {
        Socket::open_answered(api, name, path, keys, cookie).map(|(socket, _)| socket)
    }

    /// `open`, with the cookies the upgrade's answer set. A cookie rides
    /// along as a browser's does: from a page on the fragment's own origin,
    /// which the upgrade names (one that names none is no browser's, and
    /// its cookies count for nothing).
    pub fn open_answered(api: &Api, name: &str, path: &str, keys: Option<&Keys>, cookie: Option<&str>) -> Result<(Socket, Vec<String>)> {
        let path = if path == "__live" { "__live?v=2" } else { path };
        let origin = cookie.map(|_| api.site_origin(name));
        Socket::connect(api, &format!("{}/f/{name}/{path}", api.base), keys, cookie, origin.as_deref())
    }

    /// A socket to `path` on the fragment's own host, opened as a page on
    /// `origin` opens one (`None`: a client that names no page, as the CLI).
    pub fn on_host(api: &Api, name: &str, path: &str, keys: Option<&Keys>, cookie: Option<&str>, origin: Option<&str>) -> Result<Socket> {
        let path = if path == "__live" { "__live?v=2" } else { path };
        Socket::connect(api, &api.site_url(name, path), keys, cookie, origin).map(|(socket, _)| socket)
    }

    /// A socket to `http`, whichever host it names, opened as a page on
    /// `origin` opens one.
    pub fn connect(api: &Api, http: &str, keys: Option<&Keys>, cookie: Option<&str>, origin: Option<&str>) -> Result<(Socket, Vec<String>)> {
        use tungstenite::client::IntoClientRequest;
        let url = reqwest::Url::parse(http)?;
        let mut to = url.clone();
        to.set_scheme(if url.scheme() == "https" { "wss" } else { "ws" }).map_err(|()| anyhow::anyhow!("{http} is not http(s)"))?;
        let mut req = match api.target {
            // the node is reached at 127.0.0.1, the site in `Host` (as `call` does)
            Target::Local => {
                to.set_host(Some("127.0.0.1"))?;
                to.set_port(Some(api.port)).map_err(|()| anyhow::anyhow!("{http} takes no port"))?;
                let mut req = to.as_str().into_client_request()?;
                req.headers_mut().insert("host", format!("{}:{}", url.host_str().unwrap_or(""), url.port().unwrap_or(80)).parse()?);
                req
            }
            // a deployment's hosts are its own, over TLS
            Target::Hosted(_) => to.as_str().into_client_request()?,
        };
        if let Some(k) = keys {
            req.headers_mut().insert("authorization", k.header("GET", http, &[], now_s()).parse()?);
        }
        if let Some(c) = cookie {
            let c = match url.scheme() {
                "https" => https_cookies(c),
                _ => c.to_string(),
            };
            req.headers_mut().insert("cookie", c.parse()?);
        }
        if let Some(o) = origin {
            req.headers_mut().insert("origin", o.parse()?);
        }
        let (socket, answer) = tungstenite::connect(req)?;
        if let Some(s) = tcp(socket.get_ref()) {
            s.set_read_timeout(Some(Duration::from_secs(5)))?;
        }
        let cookies = answer.headers().get_all("set-cookie").iter().filter_map(|v| v.to_str().ok()).map(str::to_string).collect();
        Ok((Socket(socket), cookies))
    }

    pub fn send(&mut self, v: &Value) -> Result<()> {
        self.0.send(tungstenite::Message::Text(v.to_string().into()))?;
        Ok(())
    }

    /// The next frame; `Err` on a timeout or a close (its code in the message).
    pub fn next(&mut self) -> Result<Value> {
        loop {
            match self.0.read()? {
                tungstenite::Message::Text(t) => return Ok(serde_json::from_str(&t)?),
                tungstenite::Message::Close(f) => anyhow::bail!("closed {}", f.map(|f| u16::from(f.code)).unwrap_or(0)),
                _ => {}
            }
        }
    }

    /// The next `n` bytes of binary frames (an RFB stream's); `Err` on a
    /// timeout or a close.
    pub fn bytes(&mut self, n: usize) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        // bounded by the socket's read timeout
        while out.len() < n {
            match self.0.read()? {
                tungstenite::Message::Binary(b) => out.extend_from_slice(&b),
                tungstenite::Message::Close(f) => anyhow::bail!("closed {}", f.map(|f| u16::from(f.code)).unwrap_or(0)),
                _ => {}
            }
        }
        out.truncate(n);
        Ok(out)
    }

    /// The very next frame, which must be of `kind`: a check that nothing
    /// else came first (`until` would skip it).
    pub fn expect(&mut self, kind: &str) -> Result<Value> {
        let v = self.next()?;
        anyhow::ensure!(v["type"] == kind, "the next frame is not {kind}: {v}");
        Ok(v)
    }

    /// Frames until one of `kind` arrives (at most `limit` frames).
    /// How long one frame may take to come (5 s unless set): a real
    /// runtime's first reply after a wake may take longer.
    pub fn patience(&mut self, wait: Duration) -> Result<()> {
        if let Some(s) = tcp(self.0.get_ref()) {
            s.set_read_timeout(Some(wait))?;
        }
        Ok(())
    }

    pub fn until(&mut self, kind: &str, limit: usize) -> Result<Value> {
        self.until_where(kind, limit, |_| true)
    }

    /// The first frame of `kind` that `wanted` holds for, within `limit`
    /// frames: one of another turn's (its draft cleared after its reply)
    /// may still arrive first.
    pub fn until_where(&mut self, kind: &str, limit: usize, wanted: impl Fn(&Value) -> bool) -> Result<Value> {
        for _ in 0..limit {
            let v = self.next()?;
            if v["type"] == kind && wanted(&v) {
                return Ok(v);
            }
        }
        anyhow::bail!("no {kind} frame that was wanted in {limit} frames")
    }

    pub fn close(mut self) {
        let _ = self.0.close(None);
    }
}

#[cfg(test)]
mod tests {
    //! The run's client against a keep-alive server of the test's own that
    //! counts its connections: a connection idle past the client's limit is
    //! never used again, so the client never writes onto one the server may
    //! be closing (workerd's, 5 s after its last answer).
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    /// A server that keeps every connection open, answering each request on
    /// it, and counts the connections it accepts.
    fn keep_alive_server() -> (String, Arc<AtomicUsize>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let url = format!("http://{}/", listener.local_addr().expect("its address"));
        let accepted = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&accepted);
        std::thread::spawn(move || {
            // bounded by the test's process: it ends with it
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                counted.fetch_add(1, Ordering::SeqCst);
                std::thread::spawn(move || {
                    let mut out = stream.try_clone().expect("clone the stream");
                    let mut lines = BufReader::new(stream);
                    let mut line = String::new();
                    // one answer per request head, until the client hangs up
                    loop {
                        line.clear();
                        match lines.read_line(&mut line) {
                            Ok(0) | Err(_) => return,
                            Ok(_) if line == "\r\n" => {
                                if out.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nok").is_err() {
                                    return;
                                }
                            }
                            Ok(_) => {}
                        }
                    }
                });
            }
        });
        (url, accepted)
    }

    /// Valid: a request soon after another goes on the same connection.
    /// The property: one after the client's idle limit goes on a new one,
    /// though the server kept the old one open.
    #[test]
    fn a_connection_idle_past_the_limit_is_never_used_again() {
        let (url, accepted) = keep_alive_server();
        let limit = Duration::from_millis(200);
        let http = super::client_with(limit);
        let get = || http.get(&url).send().and_then(|r| r.bytes()).expect("the server answers");
        get();
        std::thread::sleep(Duration::from_millis(20));
        get();
        assert_eq!(accepted.load(Ordering::SeqCst), 1, "used again within the limit");
        std::thread::sleep(limit + Duration::from_millis(150));
        get();
        assert_eq!(accepted.load(Ordering::SeqCst), 2, "a new connection past the limit");
    }
}
