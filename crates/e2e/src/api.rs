//! Signed and unsigned HTTP against the node, the way the CLI and a
//! browser call it. Fragment hosts (`<name>.<suffix>`) are reached by
//! sending the node the right `Host` header.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use fragment_nip98::Keys;
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

    pub fn header(&self, name: &str) -> String {
        self.headers.get(name).and_then(|v| v.to_str().ok()).unwrap_or("").to_string()
    }

    /// `name=value` of each Set-Cookie.
    pub fn cookies(&self) -> Vec<String> {
        self.headers
            .get_all("set-cookie")
            .iter()
            .filter_map(|v| v.to_str().ok())
            .map(|c| c.split(';').next().unwrap_or("").to_string())
            .collect()
    }
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

pub struct Api {
    http: reqwest::blocking::Client,
    pub base: String,
    pub port: u16,
    pub suffix: Option<String>,
    /// A hosted fleet: requests go to their URLs as they are (a local node
    /// is reached at 127.0.0.1 with the site in `Host`).
    remote: bool,
}

impl Api {
    pub fn new(port: u16, suffix: Option<&str>) -> Api {
        let http = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(60))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("http client");
        Api { http, base: format!("http://127.0.0.1:{port}"), port, suffix: suffix.map(str::to_string), remote: false }
    }

    /// A hosted fleet at `base` (https), its fragments on `<name>.<suffix>`
    /// when it has a suffix.
    pub fn remote(base: &str, suffix: Option<&str>) -> Api {
        let http = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(120))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("http client");
        Api { http, base: base.trim_end_matches('/').to_string(), port: 443, suffix: suffix.map(str::to_string), remote: true }
    }

    /// The URL of `path` on a fragment's own host (or its `/f/<name>/` path
    /// when the fleet has no suffix).
    pub fn site_url(&self, name: &str, path: &str) -> String {
        match &self.suffix {
            Some(s) if self.remote => format!("https://{name}.{s}/{path}"),
            Some(s) => format!("http://{name}.{s}:{}/{path}", self.port),
            None => format!("{}/f/{name}/{path}", self.base),
        }
    }

    pub fn call(&self, c: Call<'_>) -> Result<Reply> {
        let url = reqwest::Url::parse(&c.url)?;
        let body = c.body.unwrap_or_default();
        let mut req = if self.remote {
            self.http.request(c.method.parse()?, url.clone()).body(body.clone())
        } else {
            // Everything goes to the node; the Host header names the site.
            let host = format!("{}:{}", url.host_str().unwrap_or(""), url.port().unwrap_or(80));
            let mut to = url.clone();
            to.set_host(Some("127.0.0.1")).expect("an http URL takes a host");
            to.set_port(Some(self.port)).expect("an http URL takes a port");
            self.http.request(c.method.parse()?, to).header("host", host).body(body.clone())
        };
        if let Some(ct) = c.content_type {
            req = req.header("content-type", ct);
        }
        if let Some(cookie) = c.cookie {
            req = req.header("cookie", cookie);
        }
        if let Some(keys) = c.keys {
            req = req.header("authorization", keys.header(c.method, &c.url, &body, now_s()));
        }
        for (k, v) in c.extra {
            req = req.header(k, v);
        }
        let resp = req.send().with_context(|| format!("{} {}", c.method, c.url))?;
        let status = resp.status().as_u16();
        let headers = resp.headers().clone();
        let bytes = resp.bytes()?.to_vec();
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

    /// A browser signing in as `email` through WorkOS (the fake): the
    /// platform session's cookie value.
    pub fn sign_in(&self, email: &str) -> Result<String> {
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
        self.signed(keys, "GET", "/api/identities/me", None)
    }

    /// Someone who signs: a person signed in through WorkOS (the fake),
    /// with a new CLI key they approved.
    pub fn person(&self) -> Result<Keys> {
        let keys = Keys::generate();
        let email = format!("p-{}@e2e.test", &keys.pubkey_hex()[..12]);
        let session = self.sign_in(&email)?;
        let me = self.approve(&session, &keys)?;
        anyhow::ensure!(me.status == 200 && me.body["id"].is_string(), "an approved key works: {me}");
        Ok(keys)
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
        self.create_with(keys, json!({ "name": name, "fragmentSecret": Keys::generate().secret_hex() }))
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

/// Opens the change feed; `keys` signs the upgrade.
pub fn watch(api: &Api, name: &str, query: &str, keys: Option<&Keys>) -> Result<tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<std::net::TcpStream>>> {
    use tungstenite::client::IntoClientRequest;
    let http = format!("{}/f/{name}/__watch{query}", api.base);
    let mut req = http.replacen("http", "ws", 1).into_client_request()?;
    if let Some(k) = keys {
        req.headers_mut().insert("authorization", k.header("GET", &http, &[], now_s()).parse()?);
    }
    let (socket, _) = tungstenite::connect(req)?;
    if let tungstenite::stream::MaybeTlsStream::Plain(s) = socket.get_ref() {
        s.set_read_timeout(Some(Duration::from_secs(10)))?;
    }
    Ok(socket)
}

/// A socket to a fragment's `__watch` or `__live`, read as JSON frames.
pub struct Socket(tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<std::net::TcpStream>>);

impl Socket {
    /// Opens `/f/<name>/<path>` (reachable in place on every fleet);
    /// `keys` signs the upgrade, `cookie` rides along like a browser's.
    pub fn open(api: &Api, name: &str, path: &str, keys: Option<&Keys>, cookie: Option<&str>) -> Result<Socket> {
        use tungstenite::client::IntoClientRequest;
        let http = format!("{}/f/{name}/{path}", api.base);
        let mut req = http.replacen("http", "ws", 1).into_client_request()?;
        if let Some(k) = keys {
            req.headers_mut().insert("authorization", k.header("GET", &http, &[], now_s()).parse()?);
        }
        if let Some(c) = cookie {
            req.headers_mut().insert("cookie", c.parse()?);
        }
        let (socket, _) = tungstenite::connect(req)?;
        if let tungstenite::stream::MaybeTlsStream::Plain(s) = socket.get_ref() {
            s.set_read_timeout(Some(Duration::from_secs(5)))?;
        }
        Ok(Socket(socket))
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

    /// Frames until one of `kind` arrives (at most `limit` frames).
    pub fn until(&mut self, kind: &str, limit: usize) -> Result<Value> {
        for _ in 0..limit {
            let v = self.next()?;
            if v["type"] == kind {
                return Ok(v);
            }
        }
        anyhow::bail!("no {kind} frame in {limit} frames")
    }

    pub fn close(mut self) {
        let _ = self.0.close(None);
    }
}
