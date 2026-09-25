//! A headless Chrome driven over the DevTools protocol: enough to open
//! pages, evaluate JavaScript in them, and wait for what they show. Chrome
//! resolves `*.localhost` to the loopback address, so pages open on each
//! fragment's own origin exactly as a person's browser would.

use std::cell::Cell;
use std::net::TcpStream;
use std::ops::{Deref, DerefMut};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::rc::Rc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};

pub struct Browser {
    child: Child,
    ws: WebSocket<MaybeTlsStream<TcpStream>>,
    next: u64,
    /// The browser context pages open in (`None`: Chrome's default one).
    context: Option<String>,
    /// More contexts the lease made (`another_context`), ended with it.
    others: Vec<String>,
    /// Chrome's own state for this launch, removed when it stops: never
    /// evidence, and tens of megabytes once a page has loaded.
    profile: PathBuf,
}

/// One Chrome for a whole run, started when a lane first asks for it.
/// Each lane leases it with a browser context of its own: its own cookies,
/// storage, cache, and service workers, as a fresh profile has, so no lane
/// sees another's session. The lease's pages close when it ends.
pub struct Shared {
    scratch: PathBuf,
    /// The browser between leases (`None`: not started, or out on a lease).
    idle: Rc<Cell<Option<Browser>>>,
}

impl Shared {
    pub fn new(scratch: &Path) -> Shared {
        Shared { scratch: scratch.to_path_buf(), idle: Rc::new(Cell::new(None)) }
    }

    /// The browser in a fresh context (`None` when no Chrome is installed).
    pub fn lease(&self) -> Result<Option<Lease>> {
        let mut browser = match self.idle.take() {
            Some(browser) => browser,
            None => match Browser::launch(&self.scratch)? {
                Some(browser) => browser,
                None => return Ok(None),
            },
        };
        // on an error the browser is dropped (Chrome stops); the next lease starts another
        browser.begin_context()?;
        Ok(Some(Lease { browser: Some(browser), idle: Rc::clone(&self.idle) }))
    }

    /// Stops Chrome (a lease still out stops it when it ends).
    pub fn close(&self) {
        drop(self.idle.take());
    }
}

/// A lane's hold on the shared browser; it ends when dropped, a lane that
/// stopped early included.
pub struct Lease {
    /// `Some` until the lease ends.
    browser: Option<Browser>,
    idle: Rc<Cell<Option<Browser>>>,
}

impl Deref for Lease {
    type Target = Browser;
    fn deref(&self) -> &Browser {
        self.browser.as_ref().expect("a lease holds its browser until it ends")
    }
}

impl DerefMut for Lease {
    fn deref_mut(&mut self) -> &mut Browser {
        self.browser.as_mut().expect("a lease holds its browser until it ends")
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        let Some(mut browser) = self.browser.take() else { return };
        // a browser whose context does not close is not lent again: dropped
        // here, Chrome stops, and the next lease starts another
        if browser.end_context().is_ok() {
            self.idle.set(Some(browser));
        }
    }
}

/// One tab, addressed by its DevTools session.
pub struct Page {
    session: String,
    target: String,
}

/// A second person's browser beside the lease's own: a browser context of
/// its own in the same Chrome (its own cookies, storage, and cache), which
/// ends with the lease. Its pages open with `Browser::open_in`.
pub struct BrowserContext(String);

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
    pub fn launch(scratch: &Path) -> Result<Option<Browser>> {
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
                // two people's windows at once (the phase7 section): one
                // that covers another must not hide it, or the hidden page
                // stops drawing (no requestAnimationFrame) and pausing its timers
                "--disable-backgrounding-occluded-windows",
                "--disable-renderer-backgrounding",
                "--disable-background-timer-throttling",
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
        Ok(Some(Browser { child, ws, next: 0, context: None, others: vec![], profile }))
    }

    /// Pages open in a new browser context from here on.
    fn begin_context(&mut self) -> Result<()> {
        assert!(self.context.is_none(), "one browser context at a time");
        let made = self.send("Target.createBrowserContext", json!({}), None)?;
        let id = made["browserContextId"].as_str().context("Target.createBrowserContext answers its id")?;
        self.context = Some(id.to_string());
        Ok(())
    }

    /// Disposes of the context, and any more the lease made, closing their pages.
    fn end_context(&mut self) -> Result<()> {
        let id = self.context.take().expect("a browser context to end");
        for other in std::mem::take(&mut self.others) {
            self.send("Target.disposeBrowserContext", json!({ "browserContextId": other }), None)?;
        }
        self.send("Target.disposeBrowserContext", json!({ "browserContextId": id }), None)?;
        Ok(())
    }

    /// Another browser context for this lease: a second person's browser,
    /// signed in to nothing, ended with the lease.
    pub fn another_context(&mut self) -> Result<BrowserContext> {
        assert!(self.context.is_some(), "a leased browser makes more contexts");
        let made = self.send("Target.createBrowserContext", json!({}), None)?;
        let id = made["browserContextId"].as_str().context("Target.createBrowserContext answers its id")?.to_string();
        self.others.push(id.clone());
        Ok(BrowserContext(id))
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
        let context = self.context.clone();
        self.open_at(context.as_deref(), url)
    }

    /// Opens a page in another of the lease's contexts (`another_context`).
    pub fn open_in(&mut self, context: &BrowserContext, url: &str) -> Result<Page> {
        self.open_at(Some(&context.0), url)
    }

    fn open_at(&mut self, context: Option<&str>, url: &str) -> Result<Page> {
        let mut params = json!({ "url": url });
        if let Some(context) = context {
            params["browserContextId"] = json!(context);
        }
        let target = self.send("Target.createTarget", params, None)?["targetId"].as_str().unwrap_or("").to_string();
        self.attach(&target)
    }

    /// A page `pages` listed (a popup a page opened), to drive as one `open` answered.
    pub fn attach(&mut self, target: &str) -> Result<Page> {
        let session = self.send("Target.attachToTarget", json!({ "targetId": target, "flatten": true }), None)?["sessionId"]
            .as_str()
            .unwrap_or("")
            .to_string();
        Ok(Page { session, target: target.to_string() })
    }

    pub fn close(&mut self, page: Page) -> Result<()> {
        self.send("Target.closeTarget", json!({ "targetId": page.target }), None)?;
        Ok(())
    }

    /// The pages open in this lease's contexts, popups a page opened
    /// included: (target id, URL).
    pub fn pages(&mut self) -> Result<Vec<(String, String)>> {
        let v = self.send("Target.getTargets", json!({}), None)?;
        let context = self.context.clone();
        let ours = |id: &Value| context.as_deref().is_none_or(|c| id == c || self.others.iter().any(|o| id == o.as_str()));
        Ok(v["targetInfos"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|t| t["type"] == "page" && ours(&t["browserContextId"]))
            .map(|t| (t["targetId"].as_str().unwrap_or("").to_string(), t["url"].as_str().unwrap_or("").to_string()))
            .collect())
    }

    /// Closes a page `pages` listed.
    pub fn close_target(&mut self, target: &str) -> Result<()> {
        self.send("Target.closeTarget", json!({ "targetId": target }), None)?;
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

    /// Brings the page's window to the front, as a person looks at the
    /// window they are using: shown, it draws.
    pub fn front(&mut self, page: &Page) -> Result<()> {
        self.send("Page.bringToFront", json!({}), Some(&page.session))?;
        Ok(())
    }

    pub fn reload(&mut self, page: &Page) -> Result<()> {
        self.send("Page.reload", json!({}), Some(&page.session))?;
        Ok(())
    }

    /// The page as it looks now, a PNG at `path` (evidence for a person).
    pub fn screenshot(&mut self, page: &Page, path: &Path) -> Result<()> {
        use base64::Engine;
        let shot = self.send("Page.captureScreenshot", json!({ "format": "png" }), Some(&page.session))?;
        let bytes = base64::engine::general_purpose::STANDARD.decode(shot["data"].as_str().unwrap_or_default())?;
        std::fs::write(path, bytes)?;
        Ok(())
    }

    /// A cookie the browser holds for `url`, as if a response had set it.
    pub fn set_cookie(&mut self, url: &str, name: &str, value: &str) -> Result<()> {
        let mut params = json!({ "cookies": [{ "name": name, "value": value, "url": url, "httpOnly": true, "sameSite": "Lax" }] });
        if let Some(context) = &self.context {
            params["browserContextId"] = json!(context);
        }
        self.send("Storage.setCookies", params, None)?;
        Ok(())
    }

    /// The page's viewport, as a phone's (`mobile`) or a desktop's.
    pub fn viewport(&mut self, page: &Page, width: u32, height: u32, mobile: bool) -> Result<()> {
        let params = json!({ "width": width, "height": height, "deviceScaleFactor": 1, "mobile": mobile });
        self.send("Emulation.setDeviceMetricsOverride", params, Some(&page.session))?;
        Ok(())
    }

    /// The page's `prefers-color-scheme`: `dark` or `light`.
    pub fn color_scheme(&mut self, page: &Page, scheme: &str) -> Result<()> {
        let params = json!({ "features": [{ "name": "prefers-color-scheme", "value": scheme }] });
        self.send("Emulation.setEmulatedMedia", params, Some(&page.session))?;
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

    /// A click at a point, as a hand makes it: a user gesture, so a popup
    /// it opens is allowed (a script's `click()` is not one).
    pub fn click_at(&mut self, page: &Page, (x, y): (f64, f64)) -> Result<()> {
        let mouse = |kind: &str, buttons: u8| json!({ "type": kind, "x": x, "y": y, "button": "left", "buttons": buttons, "clickCount": 1 });
        self.send("Input.dispatchMouseEvent", mouse("mouseMoved", 0), Some(&page.session))?;
        self.send("Input.dispatchMouseEvent", mouse("mousePressed", 1), Some(&page.session))?;
        self.send("Input.dispatchMouseEvent", mouse("mouseReleased", 0), Some(&page.session))?;
        Ok(())
    }

    /// `click_at` the middle of the first element `selector` matches.
    pub fn click(&mut self, page: &Page, selector: &str) -> Result<()> {
        let at = self.eval(
            page,
            &format!("(() => {{ const e = document.querySelector({selector:?}); if (!e) return null; e.scrollIntoView({{ block: 'nearest' }}); const r = e.getBoundingClientRect(); return [r.x + r.width / 2, r.y + r.height / 2]; }})()"),
        )?;
        let (Some(x), Some(y)) = (at[0].as_f64(), at[1].as_f64()) else { bail!("nothing on the page matches {selector}") };
        self.click_at(page, (x, y))
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
        let _ = std::fs::remove_dir_all(&self.profile);
    }
}
