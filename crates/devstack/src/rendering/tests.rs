//! The renderer's routes and sessions, against stand-in browsers (shell
//! scripts on the same pipes); and, run by name (`-- --ignored`), the
//! pinned chrome-headless-shell itself: a card shot, and what its page
//! cannot reach.

use super::*;
use std::net::SocketAddr;
use std::os::unix::fs::PermissionsExt;
use tungstenite::Message;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("devstack-rendering-{name}-{}", crate::random_hex(6)));
    fs::create_dir_all(&dir).expect("make the test's directory");
    dir
}

/// A stand-in browser: `body` run by sh with CDP's pipes as 3 and 4. It
/// notes its pid in `<state>/pids/` first (its home goes with it).
fn stand_in(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, format!("#!/bin/sh\nmkdir -p \"$HOME/../pids\" && echo $$ > \"$HOME/../pids/$$\"\n{body}\n")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// Answers every message with itself: the probe, and each client's.
const ECHO: &str = "exec cat <&3 >&4";

fn renderer(browser: PathBuf, state: &Path) -> Rendering {
    let origins = Origins::new(&["fragment.localhost"], 8790, "127.0.0.1:9".parse().unwrap());
    Rendering::start(Options { browser, state: state.to_path_buf(), origins, ca_file: None, port: 0 }).expect("the renderer starts")
}

fn http(method: &str, url: &str) -> (u16, serde_json::Value) {
    let client = reqwest::blocking::Client::builder().timeout(Duration::from_secs(40)).build().unwrap();
    let resp = client.request(method.parse().unwrap(), url).send().expect("the renderer answers");
    let status = resp.status().as_u16();
    (status, serde_json::from_slice(&resp.bytes().unwrap_or_default()).unwrap_or(serde_json::Value::Null))
}

fn acquire_one(r: &Rendering, keep_alive_ms: u64) -> String {
    let (status, body) = http("POST", &format!("{}/v1/devtools/browser?keep_alive={keep_alive_ms}", r.url));
    assert_eq!(status, 200, "{body}");
    let id = body["sessionId"].as_str().expect("a session id").to_string();
    assert!(valid_id(&id), "{id}");
    id
}

type Socket = tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<std::net::TcpStream>>;

fn socket(r: &Rendering, id: &str) -> Result<Socket, tungstenite::Error> {
    let (ws, _) = tungstenite::connect(format!("ws://127.0.0.1:{}/v1/devtools/browser/{id}", r.port))?;
    if let tungstenite::stream::MaybeTlsStream::Plain(s) = ws.get_ref() {
        s.set_read_timeout(Some(Duration::from_secs(40))).unwrap();
    }
    Ok(ws)
}

fn status_of(e: tungstenite::Error) -> u16 {
    match e {
        tungstenite::Error::Http(resp) => resp.status().as_u16(),
        other => panic!("an HTTP refusal, not {other}"),
    }
}

/// The pids the stand-ins noted, and whether each still runs.
fn alive(state: &Path) -> Vec<bool> {
    let Ok(dir) = fs::read_dir(state.join("pids")) else { return vec![] };
    dir.flatten().map(|e| Path::new("/proc").join(e.file_name()).exists() || Command::new("kill").args(["-0", &e.file_name().to_string_lossy()]).stderr(Stdio::null()).status().is_ok_and(|s| s.success())).collect()
}

fn sessions_on_disk(state: &Path) -> usize {
    fs::read_dir(state).map(|d| d.flatten().filter(|e| e.file_name().to_string_lossy().starts_with("session-")).count()).unwrap_or(0)
}

/// Valid: a session starts once its browser answers, carries CDP both
/// ways, and closes, its browser and home gone. Replay: closing it again,
/// or reaching it after, is 404; a client that leaves lets another in.
#[test]
fn a_session_carries_cdp_and_closes() {
    let _exec = crate::TEST_EXEC.lock().unwrap_or_else(|e| e.into_inner());
    let dir = scratch("session");
    let state = dir.join("state");
    let r = renderer(stand_in(&dir, "echo", ECHO), &state);
    let id = acquire_one(&r, 10_000);
    assert_eq!((r.sessions(), sessions_on_disk(&state)), (1, 1));
    let mut ws = socket(&r, &id).expect("the session's socket");
    let big = format!("{{\"id\":1,\"method\":\"Page.captureScreenshot\",\"pad\":\"{}\"}}", "x".repeat(300_000));
    for msg in [r#"{"id":1,"method":"Target.createTarget"}"#.to_string(), big] {
        ws.send(Message::Text(msg.clone().into())).unwrap();
        match ws.read().unwrap() {
            Message::Text(t) => assert_eq!(t.as_str(), msg, "the browser's answer, whole"),
            other => panic!("text, not {other:?}"),
        }
    }
    // a second client while the first holds it
    assert_eq!(status_of(socket(&r, &id).unwrap_err()), 409);
    ws.close(None).unwrap();
    // the first's close is seen, then another may come
    let t0 = Instant::now();
    let mut again = loop {
        match socket(&r, &id) {
            Ok(ws) => break ws,
            Err(e) if t0.elapsed() < Duration::from_secs(5) => assert_eq!(status_of(e), 409),
            Err(e) => panic!("a client after the first: {e}"),
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    again.send(Message::Text(r#"{"id":2}"#.into())).unwrap();
    assert_eq!(again.read().unwrap(), Message::Text(r#"{"id":2}"#.into()));
    let (status, body) = http("DELETE", &format!("{}/v1/devtools/browser/{id}", r.url));
    assert_eq!((status, body), (200, serde_json::json!({ "status": "closed" })));
    assert!(matches!(again.read(), Ok(Message::Close(_)) | Err(_)), "the client's socket ends with the session");
    assert_eq!((r.sessions(), sessions_on_disk(&state)), (0, 0), "its home is removed");
    assert_eq!(alive(&state), vec![false], "its browser is stopped");
    assert_eq!(http("DELETE", &format!("{}/v1/devtools/browser/{id}", r.url)).0, 404, "closing it again");
    assert_eq!(status_of(socket(&r, &id).unwrap_err()), 404, "reaching it after");
    drop(r);
    fs::remove_dir_all(&dir).unwrap();
}

/// Invalid: what the routes refuse, each with its status, and the
/// sessions past `SESSIONS_MAX`; a browser that dies as it starts fails
/// the start at once; a client's message holding a NUL ends its socket.
#[test]
fn the_renderer_refuses_what_it_does_not_serve() {
    let _exec = crate::TEST_EXEC.lock().unwrap_or_else(|e| e.into_inner());
    let dir = scratch("refuses");
    let state = dir.join("state");
    let r = renderer(stand_in(&dir, "echo", ECHO), &state);
    let base = format!("{}/v1/devtools/browser", r.url);
    for (method, url, status) in [
        ("POST", format!("{base}?keep_alive=soon"), 400),
        ("POST", format!("{base}?keep_alive={}", KEEP_ALIVE_MAX.as_millis() + 1), 400),
        ("GET", format!("{}/json/version", r.url), 404),
        ("GET", base.clone(), 405),
        ("POST", format!("{base}/{}", "0".repeat(32)), 404),
        ("DELETE", format!("{base}/{}", "0".repeat(32)), 404),
        ("DELETE", format!("{base}/../../etc"), 404),
        ("GET", format!("{base}/{}", "0".repeat(32)), 426),
    ] {
        assert_eq!(http(method, &url).0, status, "{method} {url}");
    }
    let ids: Vec<String> = (0..SESSIONS_MAX).map(|_| acquire_one(&r, 60_000)).collect();
    let (status, body) = http("POST", &base);
    assert_eq!(status, 429, "{body}");
    let mut ws = socket(&r, &ids[0]).unwrap();
    ws.send(Message::Text("{\"id\":1,\"x\":\"\u{0}\"}".into())).unwrap();
    assert!(matches!(ws.read(), Ok(Message::Close(Some(f))) if u16::from(f.code) == 1007), "a NUL would end the message early");
    drop(r);
    assert_eq!(sessions_on_disk(&state), 0, "every home goes with the renderer");

    let dying = renderer(stand_in(&dir, "dies", "echo 'no usable sandbox' >&2; exit 1"), &dir.join("state2"));
    let t0 = Instant::now();
    let (status, body) = http("POST", &format!("{}/v1/devtools/browser", dying.url));
    assert_eq!(status, 500, "{body}");
    assert!(body["error"].as_str().is_some_and(|e| e.contains("no usable sandbox")), "it says what the browser said: {body}");
    assert!(t0.elapsed() < START_TIMEOUT / 2, "without waiting out the start's timeout: {:?}", t0.elapsed());
    assert_eq!(dying.sessions(), 0);
    drop(dying);
    fs::remove_dir_all(&dir).unwrap();
}

/// A session no client takes is stopped after its `keep_alive`; restart:
/// a renderer dropped stops its browsers, and one started on the same
/// state serves fresh sessions.
#[test]
fn an_idle_session_is_stopped_and_a_restart_starts_clean() {
    let _exec = crate::TEST_EXEC.lock().unwrap_or_else(|e| e.into_inner());
    let dir = scratch("idle");
    let state = dir.join("state");
    let echo = stand_in(&dir, "echo", ECHO);
    let r = renderer(echo.clone(), &state);
    let idle = acquire_one(&r, 200);
    let held = acquire_one(&r, 200);
    let mut ws = socket(&r, &held).unwrap();
    std::thread::sleep(Duration::from_millis(900));
    assert_eq!(status_of(socket(&r, &idle).unwrap_err()), 404, "the idle one is gone");
    ws.send(Message::Text(r#"{"id":3}"#.into())).unwrap();
    assert_eq!(ws.read().unwrap(), Message::Text(r#"{"id":3}"#.into()), "a held one stays past its keep_alive");
    let lasting = acquire_one(&r, 60_000);
    drop(r);
    assert!(alive(&state).iter().all(|a| !a), "every browser stopped with the renderer");
    assert_eq!(sessions_on_disk(&state), 0);

    // a renderer that crashed left a home behind: the next one clears it
    fs::create_dir_all(state.join("session-left/profile")).unwrap();
    let again = renderer(echo, &state);
    assert_eq!(sessions_on_disk(&state), 0, "a crashed renderer's homes are removed as the next starts");
    assert_eq!(status_of(socket(&again, &lasting).unwrap_err()), 404, "a session does not outlive its renderer");
    let id = acquire_one(&again, 1_000);
    let mut ws = socket(&again, &id).unwrap();
    ws.send(Message::Text(r#"{"id":4}"#.into())).unwrap();
    assert_eq!(ws.read().unwrap(), Message::Text(r#"{"id":4}"#.into()));
    drop(again);
    fs::remove_dir_all(&dir).unwrap();
}

/// The command line keeps every guard the isolation story names.
#[test]
fn the_browsers_flags_keep_it_in() {
    let f = flags("socks5://127.0.0.1:4321", Path::new("/s/profile"));
    for want in [
        "--remote-debugging-pipe",
        "--user-data-dir=/s/profile",
        "--proxy-server=socks5://127.0.0.1:4321",
        "--proxy-bypass-list=<-loopback>",
        "--host-resolver-rules=MAP * ~NOTFOUND , EXCLUDE 127.0.0.1",
        "--disable-quic",
        "--force-webrtc-ip-handling-policy=disable_non_proxied_udp",
    ] {
        assert!(f.iter().any(|a| a == want), "{want}");
    }
    assert!(!f.iter().any(|a| a.contains("no-sandbox") || a.contains("remote-debugging-port")), "the sandbox stays; no port is opened");
}

/// A CA bundle's certificates, one PEM block each; nothing else is one.
#[test]
fn a_ca_bundle_is_read_as_its_certificates() {
    let one = "-----BEGIN CERTIFICATE-----\nMIIB\nAAAA\n-----END CERTIFICATE-----\n";
    let bundle = format!("# a comment\n{one}\n{one}");
    assert_eq!(pem_certificates(&bundle).unwrap().len(), 2);
    assert_eq!(pem_certificates(one).unwrap()[0], "-----BEGIN CERTIFICATE-----\nMIIBAAAA\n-----END CERTIFICATE-----\n");
    for bad in ["", "no PEM here", "-----BEGIN CERTIFICATE-----\nMIIB\n", "-----BEGIN CERTIFICATE-----\n!!!!\n-----END CERTIFICATE-----"] {
        assert!(pem_certificates(bad).is_err(), "{bad:?}");
    }
    assert!(pem_certificates(&one.repeat(CA_CERTS_MAX + 1)).is_err(), "past the limit");
}

// ---- the pinned browser itself (`cargo test -p fragment-devstack -- --ignored`)

/// A tiny HTTP server: every request noted, each answered with `body`.
fn page_server(body: &'static str) -> (SocketAddr, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let seen = Arc::new(Mutex::new(vec![]));
    let noting = Arc::clone(&seen);
    std::thread::spawn(move || {
        for s in listener.incoming().flatten() {
            let noting = Arc::clone(&noting);
            std::thread::spawn(move || {
                let mut r = BufReader::new(s.try_clone().unwrap());
                if let Some(head) = read_head(&mut r) {
                    locked(&noting).push(format!("{} {}", head.header("host").unwrap_or(""), head.path));
                    let mut s = s;
                    let _ = write!(s, "HTTP/1.1 200 OK\r\ncontent-type: text/html\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len());
                }
            });
        }
    });
    (addr, seen)
}

/// CDP over the renderer's socket, as card.rs speaks it: flattened, the
/// page's commands carrying its session.
struct Cdp {
    ws: Socket,
    next: u64,
    session: Option<String>,
}

impl Cdp {
    fn call(&mut self, method: &str, params: serde_json::Value) -> serde_json::Value {
        self.next += 1;
        let mut msg = serde_json::json!({ "id": self.next, "method": method, "params": params });
        if let Some(s) = &self.session {
            msg["sessionId"] = serde_json::json!(s);
        }
        self.ws.send(Message::Text(msg.to_string().into())).unwrap();
        for _ in 0..10_000 {
            let Message::Text(t) = self.ws.read().unwrap() else { continue };
            let v: serde_json::Value = serde_json::from_str(&t).unwrap();
            if v["id"] == self.next {
                assert!(v.get("error").is_none(), "{method}: {v}");
                return v["result"].clone();
            }
        }
        panic!("{method}: no answer");
    }

    /// Opens `url` in a fresh page at the card's viewport: the navigation's
    /// error text, if it failed.
    fn open(&mut self, url: &str) -> Option<String> {
        self.session = None;
        let target = self.call("Target.createTarget", serde_json::json!({ "url": "about:blank" }));
        let attached = self.call("Target.attachToTarget", serde_json::json!({ "targetId": target["targetId"], "flatten": true }));
        self.session = attached["sessionId"].as_str().map(str::to_string);
        self.call("Emulation.setDeviceMetricsOverride", serde_json::json!({ "width": 1280, "height": 800, "deviceScaleFactor": 1, "mobile": false }));
        self.call("Page.enable", serde_json::json!({}));
        let opened = self.call("Page.navigate", serde_json::json!({ "url": url }));
        std::thread::sleep(Duration::from_millis(1500));
        opened["errorText"].as_str().map(str::to_string)
    }
}

fn pinned_browser() -> PathBuf {
    crate::browser::locate(&crate::repo_root().join(crate::TOOLS_DIR)).expect("the pinned browser").bin
}

/// The pinned chrome-headless-shell shoots a fragment's page as a JPEG
/// through the renderer; its page reaches its own origin and another
/// fragment's, and nothing else: not a loopback service by IP literal,
/// `localhost` or another `*.localhost` name, nor the internet, by
/// fetch, image or WebSocket; and a page there is never opened.
#[test]
#[ignore = "runs the pinned chrome-headless-shell (fetched into target/tools)"]
fn the_pinned_browser_shoots_a_card_and_reaches_nothing_else() {
    let (secret, secret_seen) = page_server("secret");
    let page: &'static str = Box::leak(
        format!(
            "<!doctype html><h1 style='font:64px sans-serif'>a card</h1>\
             <img src='http://127.0.0.1:{p}/ip'><img src='http://localhost:{p}/localhost'><img src='http://other.localhost:{p}/other'>\
             <img src='http://example.com/x'><img src='/self'>\
             <script>fetch('http://127.0.0.1:{p}/fetch').catch(()=>{{}});try{{new WebSocket('ws://127.0.0.1:{p}/ws')}}catch(e){{}}</script>",
            p = secret.port()
        )
        .into_boxed_str(),
    );
    let (site, site_seen) = page_server(page);
    let dir = scratch("chrome");
    let origins = Origins::new(&["fragment.localhost"], site.port(), site);
    let r = Rendering::start(Options { browser: pinned_browser(), state: dir.join("state"), origins, ca_file: None, port: 0 }).unwrap();
    let id = acquire_one(&r, 10_000);
    let mut cdp = Cdp { ws: socket(&r, &id).unwrap(), next: 0, session: None };
    assert_eq!(cdp.open(&format!("http://todo--paul.fragment.localhost:{}/", site.port())), None, "the fragment's page opens");
    let shot = cdp.call("Page.captureScreenshot", serde_json::json!({ "format": "jpeg", "quality": 70, "fromSurface": true }));
    use base64::Engine;
    let jpeg = base64::engine::general_purpose::STANDARD.decode(shot["data"].as_str().unwrap()).unwrap();
    assert!(jpeg.starts_with(&[0xFF, 0xD8, 0xFF]) && jpeg.len() > 1000, "a JPEG: {} bytes", jpeg.len());
    for blocked in [format!("http://127.0.0.1:{}/", secret.port()), format!("http://localhost:{}/", secret.port()), "http://example.com/".to_string()] {
        assert!(cdp.open(&blocked).is_some(), "{blocked} does not open");
    }
    let seen = locked(&site_seen).clone();
    assert!(seen.iter().any(|s| s.ends_with(" /")) && seen.iter().any(|s| s.ends_with(" /self")), "its own origin: {seen:?}");
    assert_eq!(locked(&secret_seen).clone(), Vec::<String>::new(), "the loopback service heard nothing");
    drop(cdp);
    assert_eq!(http("DELETE", &format!("{}/v1/devtools/browser/{id}", r.url)).0, 200);
    drop(r);
    fs::remove_dir_all(&dir).unwrap();
}

/// Fragments on https under a private CA: without the CA the page does
/// not open; with it (`ca_file`), it does, the certificate checked by the
/// browser. Needs `openssl` (its `s_server` is the https origin) and NSS's
/// `certutil`.
#[test]
#[ignore = "runs the pinned chrome-headless-shell, openssl and certutil"]
fn a_private_ca_is_trusted_when_named() {
    let dir = scratch("ca");
    let run = |args: &[&str]| {
        let out = Command::new("openssl").args(args).current_dir(&dir).output().expect("openssl");
        assert!(out.status.success(), "openssl {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    };
    run(&["req", "-x509", "-newkey", "ec", "-pkeyopt", "ec_paramgen_curve:P-256", "-nodes", "-keyout", "ca.key", "-out", "ca.pem", "-days", "2", "-subj", "/CN=fragment test CA", "-addext", "basicConstraints=critical,CA:TRUE", "-addext", "keyUsage=critical,keyCertSign"]);
    run(&["req", "-newkey", "ec", "-pkeyopt", "ec_paramgen_curve:P-256", "-nodes", "-keyout", "leaf.key", "-out", "leaf.csr", "-subj", "/CN=*.fragment.test"]);
    fs::write(dir.join("leaf.ext"), "subjectAltName=DNS:*.fragment.test\nextendedKeyUsage=serverAuth\n").unwrap();
    run(&["x509", "-req", "-in", "leaf.csr", "-CA", "ca.pem", "-CAkey", "ca.key", "-CAcreateserial", "-out", "leaf.pem", "-days", "1", "-extfile", "leaf.ext"]);
    let port = crate::free_port().unwrap();
    let mut server = Command::new("openssl")
        .args(["s_server", "-quiet", "-accept", &port.to_string(), "-cert", "leaf.pem", "-key", "leaf.key", "-www"])
        .current_dir(&dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(500));
    let upstream: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    let page = format!("https://todo--paul.fragment.test:{port}/");
    let shoot = |ca_file: Option<PathBuf>, state: &str| {
        let origins = Origins::new(&["fragment.test"], port, upstream);
        let r = Rendering::start(Options { browser: pinned_browser(), state: dir.join(state), origins, ca_file, port: 0 }).unwrap();
        let id = acquire_one(&r, 10_000);
        let mut cdp = Cdp { ws: socket(&r, &id).unwrap(), next: 0, session: None };
        cdp.open(&page)
    };
    let without = shoot(None, "without");
    assert!(without.as_deref().is_some_and(|e| e.contains("CERT")), "untrusted: {without:?}");
    assert_eq!(shoot(Some(dir.join("ca.pem")), "with"), None, "the CA named, the page opens");
    let _ = server.kill();
    let _ = server.wait();
    fs::remove_dir_all(&dir).unwrap();
}
