//! A headless Chrome driven over the DevTools protocol: enough to open
//! pages, evaluate JavaScript in them, and wait for what they show. Chrome
//! resolves `*.localhost` to the loopback address, so pages open on each
//! fragment's own origin exactly as a person's browser would.

use std::net::TcpStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};

pub struct Browser {
    child: Child,
    ws: WebSocket<MaybeTlsStream<TcpStream>>,
    next: u64,
    _profile: PathBuf,
}

/// One tab, addressed by its DevTools session.
pub struct Page {
    session: String,
    target: String,
}

fn chrome() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("CHROME_BIN") {
        return Some(PathBuf::from(p));
    }
    ["/Applications/Google Chrome.app/Contents/MacOS/Google Chrome", "/Applications/Chromium.app/Contents/MacOS/Chromium", "/usr/bin/google-chrome", "/usr/bin/chromium"]
        .iter()
        .map(PathBuf::from)
        .find(|p| p.is_file())
}

impl Browser {
    /// `None` when no Chrome is installed (the section says so and skips).
    pub fn launch(scratch: &std::path::Path) -> Result<Option<Browser>> {
        let Some(bin) = chrome() else { return Ok(None) };
        let port = fragment_devstack::free_port()?;
        let profile = scratch.join(format!("chrome-{port}"));
        std::fs::create_dir_all(&profile)?;
        let child = Command::new(bin)
            .args([
                "--headless=new",
                &format!("--remote-debugging-port={port}"),
                &format!("--user-data-dir={}", profile.display()),
                "--no-first-run",
                "--no-default-browser-check",
                "--disable-gpu",
                "--disable-extensions",
                "about:blank",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .context("start Chrome")?;
        let t0 = Instant::now();
        let url = loop {
            let found = reqwest::blocking::get(format!("http://127.0.0.1:{port}/json/version"))
                .ok()
                .and_then(|r| r.json::<Value>().ok())
                .and_then(|v| v["webSocketDebuggerUrl"].as_str().map(str::to_string));
            if let Some(u) = found {
                break u;
            }
            if t0.elapsed() > Duration::from_secs(20) {
                bail!("Chrome's DevTools endpoint did not come up on :{port}");
            }
            std::thread::sleep(Duration::from_millis(100));
        };
        let (ws, _) = tungstenite::connect(url.as_str())?;
        if let MaybeTlsStream::Plain(s) = ws.get_ref() {
            s.set_read_timeout(Some(Duration::from_secs(30)))?;
        }
        Ok(Some(Browser { child, ws, next: 0, _profile: profile }))
    }

    /// Sends one command and waits for its answer (events are skipped).
    fn send(&mut self, method: &str, params: Value, session: Option<&str>) -> Result<Value> {
        self.next += 1;
        let id = self.next;
        let mut msg = json!({ "id": id, "method": method, "params": params });
        if let Some(s) = session {
            msg["sessionId"] = json!(s);
        }
        self.ws.send(Message::Text(msg.to_string().into()))?;
        loop {
            let Message::Text(t) = self.ws.read()? else { continue };
            let v: Value = serde_json::from_str(&t)?;
            if v["id"].as_u64() == Some(id) {
                if !v["error"].is_null() {
                    bail!("{method}: {}", v["error"]);
                }
                return Ok(v["result"].clone());
            }
        }
    }

    pub fn open(&mut self, url: &str) -> Result<Page> {
        let target = self.send("Target.createTarget", json!({ "url": url }), None)?["targetId"].as_str().unwrap_or("").to_string();
        let session = self.send("Target.attachToTarget", json!({ "targetId": target, "flatten": true }), None)?["sessionId"]
            .as_str()
            .unwrap_or("")
            .to_string();
        Ok(Page { session, target })
    }

    pub fn close(&mut self, page: Page) -> Result<()> {
        self.send("Target.closeTarget", json!({ "targetId": page.target }), None)?;
        Ok(())
    }

    /// Evaluates `expr` in the page (awaiting a promise) and returns its value.
    pub fn eval(&mut self, page: &Page, expr: &str) -> Result<Value> {
        let r = self.send("Runtime.evaluate", json!({ "expression": expr, "awaitPromise": true, "returnByValue": true }), Some(&page.session))?;
        if let Some(e) = r.get("exceptionDetails") {
            bail!("the page threw: {}", e["exception"]["description"].as_str().unwrap_or(&e.to_string()));
        }
        Ok(r["result"]["value"].clone())
    }

    /// Waits until `expr` is truthy in the page.
    pub fn until(&mut self, page: &Page, expr: &str, timeout: Duration) -> bool {
        let t0 = Instant::now();
        while t0.elapsed() < timeout {
            if self.eval(page, &format!("!!({expr})")).ok() == Some(Value::Bool(true)) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        false
    }

    pub fn reload(&mut self, page: &Page) -> Result<()> {
        self.send("Page.reload", json!({}), Some(&page.session))?;
        Ok(())
    }
}

impl Drop for Browser {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
