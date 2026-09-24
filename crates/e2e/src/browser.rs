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

    /// The page as it looks now, a PNG at `path` (evidence for a person).
    pub fn screenshot(&mut self, page: &Page, path: &std::path::Path) -> Result<()> {
        use base64::Engine;
        let shot = self.send("Page.captureScreenshot", json!({ "format": "png" }), Some(&page.session))?;
        let bytes = base64::engine::general_purpose::STANDARD.decode(shot["data"].as_str().unwrap_or_default())?;
        std::fs::write(path, bytes)?;
        Ok(())
    }

    /// A cookie the browser holds for `url`, as if a response had set it.
    pub fn set_cookie(&mut self, url: &str, name: &str, value: &str) -> Result<()> {
        self.send("Storage.setCookies", json!({ "cookies": [{ "name": name, "value": value, "url": url, "httpOnly": true, "sameSite": "Lax" }] }), None)?;
        Ok(())
    }

    /// The page's viewport, as a phone's (`mobile`) or a desktop's.
    pub fn viewport(&mut self, page: &Page, width: u32, height: u32, mobile: bool) -> Result<()> {
        let params = json!({ "width": width, "height": height, "deviceScaleFactor": 1, "mobile": mobile });
        self.send("Emulation.setDeviceMetricsOverride", params, Some(&page.session))?;
        Ok(())
    }

    /// Evaluates `expr` inside the page's first frame whose URL contains
    /// `url_part` (a same-site frame shares the page's process), in a world
    /// of its own that shares the frame's DOM.
    pub fn eval_in_frame(&mut self, page: &Page, url_part: &str, expr: &str) -> Result<Value> {
        fn find(tree: &Value, part: &str) -> Option<String> {
            if tree["frame"]["url"].as_str().is_some_and(|u| u.contains(part)) {
                return tree["frame"]["id"].as_str().map(str::to_string);
            }
            tree["childFrames"].as_array()?.iter().find_map(|c| find(c, part))
        }
        let tree = self.send("Page.getFrameTree", json!({}), Some(&page.session))?;
        let frame = find(&tree["frameTree"], url_part).with_context(|| format!("no frame at {url_part}"))?;
        let world = self.send("Page.createIsolatedWorld", json!({ "frameId": frame }), Some(&page.session))?;
        let r = self.send(
            "Runtime.evaluate",
            json!({ "expression": expr, "contextId": world["executionContextId"], "awaitPromise": true, "returnByValue": true }),
            Some(&page.session),
        )?;
        if let Some(e) = r.get("exceptionDetails") {
            bail!("the frame threw: {}", e["exception"]["description"].as_str().unwrap_or(&e.to_string()));
        }
        Ok(r["result"]["value"].clone())
    }

    /// A mouse drag from one point to another, in steps, as a hand makes it.
    pub fn drag(&mut self, page: &Page, from: (f64, f64), to: (f64, f64)) -> Result<()> {
        let mouse = |kind: &str, (x, y): (f64, f64), buttons: u8| json!({ "type": kind, "x": x, "y": y, "button": "left", "buttons": buttons, "clickCount": 1 });
        self.send("Input.dispatchMouseEvent", mouse("mouseMoved", from, 0), Some(&page.session))?;
        self.send("Input.dispatchMouseEvent", mouse("mousePressed", from, 1), Some(&page.session))?;
        for i in 1..=10 {
            let t = i as f64 / 10.0;
            let at = (from.0 + (to.0 - from.0) * t, from.1 + (to.1 - from.1) * t);
            self.send("Input.dispatchMouseEvent", mouse("mouseMoved", at, 1), Some(&page.session))?;
        }
        self.send("Input.dispatchMouseEvent", mouse("mouseReleased", to, 0), Some(&page.session))?;
        Ok(())
    }
}

impl Drop for Browser {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
