//! Signed and unsigned HTTP against the node, the way the CLI and a
//! browser call it. Fragment hosts (`<name>.<suffix>`) are reached by
//! sending the node the right `Host` header.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use fragment_nip98::Keys;
use serde_json::{json, Value};

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
}

impl Api {
    pub fn new(port: u16, suffix: Option<&str>) -> Api {
        let http = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(60))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("http client");
        Api { http, base: format!("http://127.0.0.1:{port}"), port, suffix: suffix.map(str::to_string) }
    }

    /// The URL of `path` on a fragment's own host (or its `/f/<name>/` path
    /// when the fleet has no suffix).
    pub fn site_url(&self, name: &str, path: &str) -> String {
        match &self.suffix {
            Some(s) => format!("http://{name}.{s}:{}/{path}", self.port),
            None => format!("{}/f/{name}/{path}", self.base),
        }
    }

    pub fn call(&self, c: Call<'_>) -> Result<Reply> {
        let url = reqwest::Url::parse(&c.url)?;
        let host = format!("{}:{}", url.host_str().unwrap_or(""), url.port().unwrap_or(80));
        // Everything goes to the node; the Host header names the site.
        let mut to = url.clone();
        to.set_host(Some("127.0.0.1")).expect("an http URL takes a host");
        to.set_port(Some(self.port)).expect("an http URL takes a port");
        let body = c.body.unwrap_or_default();
        let mut req = self.http.request(c.method.parse()?, to).header("host", host).body(body.clone());
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
