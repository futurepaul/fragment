// Thin blocking HTTP client that signs every request with NIP-98, and the
// CLI's error codes: every answer is decoded at this door, a refusal from
// the platform's `ErrorBody` and a success into its fragment_proto type.
use crate::auth::Identity;
use anyhow::{anyhow, Context, Result};
use fragment_proto::{ErrorBody, ErrorCode};
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::time::Duration;

/// The CLI's stable error codes: `error.code` in the `--json` envelope,
/// which agents match on (listed in GUIDE.md), each with what to do next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Code {
    InvalidUsage,
    InvalidRequest,
    AuthFailed,
    Forbidden,
    NotFound,
    NameTaken,
    Conflict,
    ConflictingBody,
    TooLarge,
    AppFailed,
    RateLimited,
    BudgetUsedUp,
    StorageFull,
    Unavailable,
    OutcomeUnknown,
    ServerError,
}

impl Code {
    #[cfg(test)]
    pub const ALL: [Code; 16] = [
        Code::InvalidUsage,
        Code::InvalidRequest,
        Code::AuthFailed,
        Code::Forbidden,
        Code::NotFound,
        Code::NameTaken,
        Code::Conflict,
        Code::ConflictingBody,
        Code::TooLarge,
        Code::AppFailed,
        Code::RateLimited,
        Code::BudgetUsedUp,
        Code::StorageFull,
        Code::Unavailable,
        Code::OutcomeUnknown,
        Code::ServerError,
    ];

    /// The code for each refusal the platform names. The match is
    /// exhaustive: a new `ErrorCode` does not compile until the CLI says
    /// what it means to an agent.
    pub fn of(error: ErrorCode) -> Code {
        match error {
            ErrorCode::InvalidRequest => Code::InvalidRequest,
            ErrorCode::Unauthenticated => Code::AuthFailed,
            ErrorCode::Forbidden => Code::Forbidden,
            ErrorCode::NotFound | ErrorCode::UnknownOperation | ErrorCode::NoCode => Code::NotFound,
            ErrorCode::AlreadyExists => Code::NameTaken,
            ErrorCode::ConflictingBody => Code::ConflictingBody,
            ErrorCode::TooLarge => Code::TooLarge,
            ErrorCode::AppFailed => Code::AppFailed,
            ErrorCode::RateLimited => Code::RateLimited,
            ErrorCode::BudgetUsedUp => Code::BudgetUsedUp,
            ErrorCode::StorageFull => Code::StorageFull,
            ErrorCode::HostFailed => Code::ServerError,
            ErrorCode::UpstreamFailed | ErrorCode::RegistryUnavailable | ErrorCode::NodeFull => Code::Unavailable,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Code::InvalidUsage => "invalid_usage",
            Code::InvalidRequest => "invalid_request",
            Code::AuthFailed => "auth_failed",
            Code::Forbidden => "forbidden",
            Code::NotFound => "not_found",
            Code::NameTaken => "name_taken",
            Code::Conflict => "conflict",
            Code::ConflictingBody => "conflicting_body",
            Code::TooLarge => "too_large",
            Code::AppFailed => "app_failed",
            Code::RateLimited => "rate_limited",
            Code::BudgetUsedUp => "budget_used_up",
            Code::StorageFull => "storage_full",
            Code::Unavailable => "unavailable",
            Code::OutcomeUnknown => "outcome_unknown",
            Code::ServerError => "server_error",
        }
    }

    /// What to do next.
    pub fn hint(self) -> &'static str {
        match self {
            Code::InvalidUsage => "see `fragment --help`",
            Code::InvalidRequest => "the host refused the request for the reason in the message: fix it, then send it again",
            Code::AuthFailed => "run `fragment login`, or point at another host with --host / `fragment host <url>`",
            Code::Forbidden => "your identity lacks a role here: ask the owner for an invite (`fragment invite create`) or to add you (`fragment members add`)",
            Code::NotFound => "check the fragment's name with `fragment list`, and a call's operation with `fragment status <name>` (code.operations)",
            Code::NameTaken => "it exists already: pick another name, or remove the existing fragment with `fragment rm <name>`",
            Code::Conflict => "re-sync (`fragment sync`) and reapply your change",
            Code::ConflictingBody => "that operation id already ran with another input: use a new --id for a new action (the same id and input replay)",
            Code::TooLarge => "see the limit in the message; files of 1 MiB and up sync as blobs",
            Code::AppFailed => "the app's code refused or threw (the message says why): see `fragment events <name>` and `fragment runs <name>`",
            Code::RateLimited => "back off and retry shortly",
            Code::BudgetUsedUp => "this month's AI budget cannot cover it: `fragment budget` shows what is left; replay a held run after a top-up or next month",
            Code::StorageFull => "the app's database is at its cap and the change was rolled back: delete data before writing more",
            Code::Unavailable => "usually transient; retrying is safe",
            Code::OutcomeUnknown => "the change may have been applied: check (`fragment status`, `fragment list`, `fragment events`) before repeating it",
            Code::ServerError => "see `fragment events <name>` if it persists",
        }
    }

    /// The process's exit status for a failure with this code.
    pub fn exit_status(self) -> i32 {
        match self {
            Code::InvalidUsage => 2,
            _ => 1,
        }
    }
}

/// An error carrying a stable machine code (surfaced in the `--json`
/// envelope as `error.code`). Display is the plain human message.
#[derive(Debug)]
pub struct CodedError {
    pub code: Code,
    pub msg: String,
}
impl std::fmt::Display for CodedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.msg)
    }
}
impl std::error::Error for CodedError {}

/// Every request may take this long, plus the time its body takes at
/// [`UPLOAD_BYTES_PER_S_MIN`]: a fixed total timeout (reqwest's default is
/// 30 s, and it covers the upload) meant no upload over 30 s ever landed.
pub const REQUEST_TIMEOUT_BASE: Duration = Duration::from_secs(30);
/// The slowest link an upload is promised to finish on (256 kbit/s).
pub const UPLOAD_BYTES_PER_S_MIN: u64 = 32 * 1024;
/// How long a connection may take to open.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// Attempts per request (the first and two retries), where a retry is allowed.
pub const REQUEST_ATTEMPTS: u32 = 3;

/// The time a request carrying `bytes` (up, or down when the size is
/// known) may take.
pub fn timeout_for(bytes: u64) -> Duration {
    REQUEST_TIMEOUT_BASE + Duration::from_secs(bytes / UPLOAD_BYTES_PER_S_MIN)
}

/// Whether a request that may have reached the server can be sent again.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Replay {
    /// GET, HEAD, and a PUT to a content address: sending it twice does
    /// what sending it once does, so any transport failure is retried.
    Safe,
    /// A write that carries its own idempotency id (an operation call):
    /// the server answers a repeat of the id with the first call's answer,
    /// so it is retried like a read. When every try failed and one reached
    /// the server, the write may have been applied: the outcome is unknown,
    /// and a retry with the same id finds out.
    ById,
    /// Everything else is resent only when the connection never opened. A
    /// request that reached the server may have been applied, and sending
    /// it again turned a commit whose answer was lost into a 409 and a
    /// conflict copy of your own bytes, and a create into "name taken" for
    /// your own new fragment.
    ConnectOnly,
}

impl Replay {
    pub fn of(method: &str) -> Replay {
        match method {
            "GET" | "HEAD" => Replay::Safe,
            _ => Replay::ConnectOnly,
        }
    }

    /// Whether this failure of one attempt may be retried.
    pub fn allows_retry(self, connection_opened: bool) -> bool {
        match self {
            Replay::Safe | Replay::ById => true,
            Replay::ConnectOnly => !connection_opened,
        }
    }
}

/// What a request's NIP-98 signature binds besides its method and URL.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Signed {
    /// The body, by its hash (the `payload` tag): the host reads it whole.
    Body,
    /// The URL alone: a blob's bytes stream past the router, and its URL
    /// names their hash, which the fragment checks as they arrive. The
    /// router holds no body to check a payload tag against, so one would
    /// be refused.
    Url,
}

pub struct Client {
    pub host: String,
    pub id: Identity,
    verbose: bool,
    http: reqwest::blocking::Client,
}

pub struct Resp {
    pub status: u16,
    pub body: Vec<u8>,
}

impl Resp {
    pub fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }

    /// The host's refusal, as its `ErrorBody` names it. An answer that is
    /// not one (a proxy's page, say) says only its status: a gateway's is
    /// unavailable, anything else a server error.
    pub fn refusal(&self) -> CodedError {
        assert!(!self.ok(), "a refusal is an answer outside 2xx");
        match serde_json::from_slice::<ErrorBody>(&self.body) {
            Ok(e) => CodedError { code: Code::of(e.error), msg: format!("http {}: {}", self.status, e.message) },
            Err(_) => {
                let code = if matches!(self.status, 502..=504) { Code::Unavailable } else { Code::ServerError };
                let text: String = String::from_utf8_lossy(&self.body).chars().take(200).collect();
                CodedError { code, msg: format!("http {} (not the platform's answer): {text}", self.status) }
            }
        }
    }
}

impl Client {
    pub fn new(host: &str, id: Identity) -> Self {
        Self {
            host: host.trim_end_matches('/').to_string(),
            id,
            verbose: false,
            // every request sets its own total timeout (`timeout_for`)
            http: reqwest::blocking::Client::builder()
                .connect_timeout(CONNECT_TIMEOUT)
                .build()
                .expect("the HTTP client builds (its TLS backend is compiled in)"),
        }
    }

    /// Builder-style toggle for `-v`: one stderr line per signed request.
    pub fn with_verbose(mut self) -> Self {
        self.verbose = true;
        self
    }

    fn request(&self, method: &str, path: &str, body: Option<Vec<u8>>) -> Result<Resp> {
        let body = body.unwrap_or_default();
        let timeout = timeout_for(body.len() as u64);
        self.send(method, path, body, Replay::of(method), Signed::Body, timeout)
    }

    /// One signed request, retried within [`REQUEST_ATTEMPTS`] as `replay`
    /// allows: long-lived sync clients hold keep-alive pools that go stale
    /// when the host restarts, and without retries a watcher wedges until
    /// its process is restarted (observed live on relay-vault).
    fn send(&self, method: &str, path: &str, body: Vec<u8>, replay: Replay, signed: Signed, timeout: Duration) -> Result<Resp> {
        let url = format!("{}{}", self.host, path);
        let mut last_err = None;
        // whether any try's connection opened: its request may have landed
        let mut reached = false;
        for attempt in 0..REQUEST_ATTEMPTS {
            if attempt > 0 {
                std::thread::sleep(Duration::from_millis(300 * attempt as u64));
            }
            let auth = match signed {
                Signed::Body => self.id.nip98_header(method, &url, &body),
                Signed::Url => self.id.nip98_header(method, &url, &[]),
            };
            let t0 = std::time::Instant::now();
            let mut req = match method {
                "GET" => self.http.get(&url),
                "POST" => self.http.post(&url),
                "PUT" => self.http.put(&url),
                "DELETE" => self.http.delete(&url),
                "HEAD" => self.http.head(&url),
                _ => return Err(anyhow!("bad method")),
            };
            req = req.header("authorization", auth).timeout(timeout);
            if !body.is_empty() {
                req = req.body(body.clone());
            }
            // the body is read here too: an answer cut off mid-body is as
            // lost as one that never came
            let answer = req.send().and_then(|resp| {
                let status = resp.status().as_u16();
                resp.bytes().map(|b| Resp { status, body: b.to_vec() })
            });
            match answer {
                Ok(resp) => {
                    if self.verbose {
                        eprintln!("{method} {path} -> {} ({}ms [retries={attempt}])", resp.status, t0.elapsed().as_millis());
                    }
                    return Ok(resp);
                }
                // a request that could not be built never left
                Err(e) if e.is_builder() => return Err(e).context("building the request"),
                Err(e) if replay.allows_retry(!e.is_connect()) => {
                    if self.verbose {
                        eprintln!("{method} {path} -> retry after error ({}ms [retries={attempt}])", t0.elapsed().as_millis());
                    }
                    reached |= !e.is_connect();
                    last_err = Some(e);
                }
                Err(e) => {
                    // the request may have been applied: say so, and never
                    // send it again blind
                    return Err(anyhow::Error::new(CodedError {
                        code: Code::OutcomeUnknown,
                        msg: format!(
                            "{method} {path}: the request may have reached {host}, but its answer was lost ({e}); it may have been applied, so check before repeating it",
                            host = self.host
                        ),
                    }));
                }
            }
        }
        let last = last_err.map(|e| e.to_string()).unwrap_or_else(|| "no error recorded".into());
        if replay == Replay::ById && reached {
            return Err(anyhow::Error::new(CodedError {
                code: Code::OutcomeUnknown,
                msg: format!(
                    "{method} {path}: {REQUEST_ATTEMPTS} tries reached {host} or may have, and none got an answer ({last}); it may have been applied",
                    host = self.host
                ),
            }));
        }
        // every attempt failed: surface the last error with its cause — a
        // bare "failed after retries" turned a host dropping large bodies
        // into a silent 90s mystery (and before that, an unreachable!()
        // panicked here; found by restore agents)
        Err(anyhow::Error::new(CodedError {
            code: Code::Unavailable,
            msg: format!(
                "request failed after retries ({host} unreachable, or it dropped the connection mid-body — check the request size): {last}",
                host = self.host,
            ),
        }))
    }

    pub fn get(&self, path: &str) -> Result<Resp> {
        self.request("GET", path, None)
    }
    pub fn post_json(&self, path: &str, v: &impl serde::Serialize) -> Result<Resp> {
        self.request("POST", path, Some(serde_json::to_vec(v)?))
    }
    /// A POST whose body carries its idempotency id (an operation call):
    /// retried like a read (`Replay::ById`).
    pub fn post_json_by_id(&self, path: &str, v: &impl serde::Serialize) -> Result<Resp> {
        let body = serde_json::to_vec(v)?;
        let timeout = timeout_for(body.len() as u64);
        self.send("POST", path, body, Replay::ById, Signed::Body, timeout)
    }
    pub fn put_json(&self, path: &str, v: &impl serde::Serialize) -> Result<Resp> {
        self.request("PUT", path, Some(serde_json::to_vec(v)?))
    }
    pub fn put_bytes(&self, path: &str, bytes: Vec<u8>) -> Result<Resp> {
        self.request("PUT", path, Some(bytes))
    }
    /// A PUT to a content address (a blob named by its bytes' hash): safe
    /// to send again whatever became of the first, and signed over its URL.
    pub fn put_blob(&self, path: &str, bytes: Vec<u8>) -> Result<Resp> {
        let timeout = timeout_for(bytes.len() as u64);
        self.send("PUT", path, bytes, Replay::Safe, Signed::Url, timeout)
    }
    /// A GET whose answer is known to be about `bytes` long (a blob), given
    /// the time that takes.
    pub fn get_sized(&self, path: &str, bytes: u64) -> Result<Resp> {
        self.send("GET", path, Vec::new(), Replay::Safe, Signed::Body, timeout_for(bytes))
    }
    pub fn head(&self, path: &str) -> Result<Resp> {
        self.request("HEAD", path, None)
    }
    pub fn delete(&self, path: &str) -> Result<Resp> {
        self.request("DELETE", path, None)
    }

    /// An answer decoded at the door: a success into `T` (its
    /// fragment_proto type), a refusal into its code (`CodedError`).
    pub fn call_as<T: DeserializeOwned>(&self, resp: Resp) -> Result<T> {
        if !resp.ok() {
            return Err(anyhow::Error::new(resp.refusal()));
        }
        serde_json::from_slice(&resp.body).map_err(|e| {
            anyhow::Error::new(CodedError {
                code: Code::ServerError,
                msg: format!("the host's answer is not a {}: {e} (is this CLI older or newer than the host?)", std::any::type_name::<T>()),
            })
        })
    }

    /// `call_as` for an answer fragment_proto has no type for.
    pub fn call(&self, resp: Resp) -> Result<Value> {
        self.call_as(resp)
    }
}

pub fn encode_q(s: &str) -> String {
    // minimal percent-encoding for query values we control (paths, names)
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'/' => out.push(b as char),
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    use fragment_fakes::http::{Response as FakeResponse, Server};
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    /// A host that reads each request, counts it, and closes the
    /// connection without answering (an answer lost on its way back).
    fn silent_host() -> (Server, Arc<AtomicU32>, Client) {
        let seen = Arc::new(AtomicU32::new(0));
        let counter = Arc::clone(&seen);
        let server = Server::start(
            0,
            Arc::new(move |_: &fragment_fakes::http::Request| {
                counter.fetch_add(1, Ordering::SeqCst);
                FakeResponse::unanswered()
            }),
        )
        .expect("start the silent host");
        let client = Client::new(&server.url, crate::auth::fixed(7));
        (server, seen, client)
    }

    /// The failure's stable code (a request that got an answer fails the test).
    fn code_of(r: Result<Resp>) -> &'static str {
        match r {
            Ok(resp) => panic!("expected a failure, got http {}", resp.status),
            Err(e) => e.downcast_ref::<CodedError>().map(|c| c.code.as_str()).unwrap_or("uncoded"),
        }
    }

    /// Goal: a write that reached the host is never sent again blind.
    /// Method: the host swallows every answer; a POST and a DELETE each
    /// arrive once and fail as outcome_unknown, where the old client sent
    /// them three times.
    #[test]
    fn a_write_whose_answer_is_lost_is_sent_once() {
        let (_server, seen, c) = silent_host();
        assert_eq!(code_of(c.post_json("/api/fragments", &serde_json::json!({ "name": "x" }))), "outcome_unknown");
        assert_eq!(seen.load(Ordering::SeqCst), 1, "the POST arrived once");
        assert_eq!(code_of(c.delete("/api/f/x")), "outcome_unknown");
        assert_eq!(seen.load(Ordering::SeqCst), 2, "the DELETE arrived once");
        assert_eq!(code_of(c.put_bytes("/api/f/x/secrets/K", b"v".to_vec())), "outcome_unknown");
        assert_eq!(seen.load(Ordering::SeqCst), 3, "a PUT that is not content-addressed arrived once");
    }

    /// The flip side: reads and content-addressed uploads are safe to
    /// repeat, so they are, within REQUEST_ATTEMPTS.
    #[test]
    fn reads_and_blob_uploads_are_retried() {
        let (_server, seen, c) = silent_host();
        assert_eq!(code_of(c.get("/api/f/x/status")), "unavailable");
        assert_eq!(seen.load(Ordering::SeqCst), REQUEST_ATTEMPTS);
        assert_eq!(code_of(c.put_blob("/api/f/x/blobs/abc", vec![1, 2, 3])), "unavailable");
        assert_eq!(seen.load(Ordering::SeqCst), 2 * REQUEST_ATTEMPTS);
    }

    /// Goal: an operation call, which carries its idempotency id, is
    /// retried like a read, and a failure after it may have landed says the
    /// outcome is unknown. Method: the host swallows every answer; the call
    /// arrives REQUEST_ATTEMPTS times (a plain POST arrives once), and with
    /// no host at all it is merely unavailable.
    #[test]
    fn an_op_call_is_retried_by_its_id() {
        let (_server, seen, c) = silent_host();
        let call = serde_json::json!({ "id": "cli-1", "input": {} });
        assert_eq!(code_of(c.post_json_by_id("/api/f/x/ops/add", &call)), "outcome_unknown");
        assert_eq!(seen.load(Ordering::SeqCst), REQUEST_ATTEMPTS, "each try arrived");
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let nowhere = Client::new(&format!("http://127.0.0.1:{port}"), crate::auth::fixed(7));
        assert_eq!(code_of(nowhere.post_json_by_id("/api/f/x/ops/add", &call)), "unavailable", "no try reached a host");
    }

    /// A connection that never opened carried nothing, so even a POST is
    /// retried, and the failure is plain unavailability.
    #[test]
    fn a_write_that_never_connected_is_unavailable() {
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let c = Client::new(&format!("http://127.0.0.1:{port}"), crate::auth::fixed(7));
        assert_eq!(code_of(c.post_json("/api/fragments", &serde_json::json!({ "name": "x" }))), "unavailable");
    }

    #[test]
    fn the_timeout_grows_with_the_upload() {
        assert_eq!(timeout_for(0), REQUEST_TIMEOUT_BASE);
        assert_eq!(timeout_for(UPLOAD_BYTES_PER_S_MIN * 90), REQUEST_TIMEOUT_BASE + Duration::from_secs(90));
        // the largest blob gets hours, not 30 seconds
        assert!(timeout_for(256 * 1024 * 1024) > Duration::from_secs(2 * 3600));
    }

    /// One percent-encoder for the query values and path segments the CLI
    /// builds: unreserved bytes and `/` pass, everything else is escaped.
    #[test]
    fn query_values_are_percent_encoded() {
        assert_eq!(encode_q("site/a b.md"), "site/a%20b.md");
        assert_eq!(encode_q("x?y=1&z#é"), "x%3Fy%3D1%26z%23%C3%A9");
        assert_eq!(encode_q("preview/abc-1_2"), "preview/abc-1_2");
    }

    /// Goal: every refusal the platform names maps to the CLI code an
    /// agent should act on, read from the body's code, never its wording.
    /// Method: one body per `ErrorCode`, at that code's own status, with a
    /// message that would have fooled the old text matching; each decodes
    /// to the code this table names (written out, not computed).
    #[test]
    fn every_error_code_maps_to_its_cli_code() {
        let table = [
            (ErrorCode::InvalidRequest, "invalid_request"),
            (ErrorCode::Unauthenticated, "auth_failed"),
            (ErrorCode::Forbidden, "forbidden"),
            (ErrorCode::NotFound, "not_found"),
            (ErrorCode::UnknownOperation, "not_found"),
            (ErrorCode::NoCode, "not_found"),
            (ErrorCode::AlreadyExists, "name_taken"),
            (ErrorCode::ConflictingBody, "conflicting_body"),
            (ErrorCode::TooLarge, "too_large"),
            (ErrorCode::AppFailed, "app_failed"),
            (ErrorCode::RateLimited, "rate_limited"),
            (ErrorCode::HostFailed, "server_error"),
            (ErrorCode::UpstreamFailed, "unavailable"),
            (ErrorCode::RegistryUnavailable, "unavailable"),
            (ErrorCode::BudgetUsedUp, "budget_used_up"),
            (ErrorCode::StorageFull, "storage_full"),
            (ErrorCode::NodeFull, "unavailable"),
        ];
        for (error, cli) in table {
            let body = serde_json::to_vec(&ErrorBody { error, message: "name taken, already exists".into() }).unwrap();
            let refusal = Resp { status: error.status(), body }.refusal();
            assert_eq!(refusal.code.as_str(), cli, "{error:?}");
            assert_eq!(refusal.msg, format!("http {}: name taken, already exists", error.status()));
        }
        // an answer that is not the platform's speaks only through its status
        let page = |status: u16| Resp { status, body: b"<html>bad gateway</html>".to_vec() }.refusal().code;
        assert_eq!(page(502), Code::Unavailable);
        assert_eq!(page(504), Code::Unavailable);
        assert_eq!(page(500), Code::ServerError);
        assert_eq!(page(409), Code::ServerError, "a 409 without a code is no conflict the CLI can name");
    }

    /// Every code has its own name, and GUIDE.md lists each one.
    #[test]
    fn the_guide_lists_every_code() {
        let guide = include_str!("../GUIDE.md");
        let mut names: Vec<&str> = Code::ALL.iter().map(|c| c.as_str()).collect();
        for name in &names {
            assert!(guide.contains(&format!("`{name}`")), "GUIDE.md does not list {name}");
        }
        names.sort();
        names.dedup();
        assert_eq!(names.len(), Code::ALL.len());
    }

    /// A success decodes into its type at the door, and one in another
    /// shape is a coded failure, not a default.
    #[test]
    fn answers_decode_at_the_door() {
        let c = Client::new("http://127.0.0.1:1", crate::auth::fixed(7));
        let ok = |body: &str| Resp { status: 200, body: body.as_bytes().to_vec() };
        let listed: fragment_proto::FragmentList = c.call_as(ok(r#"{"fragments":[{"name":"a","role":"owner"}]}"#)).unwrap();
        assert_eq!(listed.fragments[0].role, fragment_proto::Role::Owner);
        let wrong = c.call_as::<fragment_proto::FragmentList>(ok(r#"{"fragments":[{"name":"a"}]}"#)).unwrap_err();
        assert_eq!(wrong.downcast_ref::<CodedError>().map(|e| e.code), Some(Code::ServerError));
        let refused = c.call_as::<Value>(Resp { status: 402, body: br#"{"error":"budget_used_up","message":"out"}"#.to_vec() }).unwrap_err();
        assert_eq!(refused.downcast_ref::<CodedError>().map(|e| e.code), Some(Code::BudgetUsedUp));
    }
}
