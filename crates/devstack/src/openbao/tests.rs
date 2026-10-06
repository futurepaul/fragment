//! OpenBao's state and config, and the cell's shim (cell/secrets.mjs) read
//! on the pinned Node: against a scripted server here, and, in the tests
//! marked `ignore`, against the pinned OpenBao itself (fetched into
//! target/tools): `cargo test -p fragment-devstack openbao -- --include-ignored`.

use std::io::{BufRead, BufReader};
use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::*;

fn scratch(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("fragment-openbao-{name}-{}", crate::random_hex(6)))
}

fn mode(p: &Path) -> u32 {
    fs::metadata(p).unwrap().permissions().mode() & 0o777
}

/// The secrets one test seeds: a store name, and a value.
const SEEDED: [(&str, &str); 2] = [("fragment-host-secret", "hs-e2e-0123456789abcdef0123456789abcdef"), ("fragment-oidc-client-secret", "oidc-e2e-secret")];

// Goal: a state directory is made once, its secrets owner-only and of the
// shape OpenBao needs, and kept as they are by every later start; one made
// loose, cut short, or gone with its data still there is refused, saying
// which. Method: prepare twice, then spoil each in turn.
#[test]
fn prepare_makes_its_secrets_once_and_refuses_them_spoiled() {
    let dir = scratch("prepare");
    let layout = prepare(&dir).expect("a fresh state");
    assert_eq!(mode(&dir), 0o700);
    assert_eq!(mode(&layout.data()), 0o700);
    let key = fs::read(layout.seal_key()).unwrap();
    assert_eq!(key.len(), SEAL_KEY_BYTES, "the static seal's key is 32 raw bytes");
    for f in [layout.seal_key(), layout.admin_role_id(), layout.admin_secret_id()] {
        assert_eq!(mode(&f), 0o600, "{}", f.display());
    }
    let ids: Vec<String> = [layout.admin_role_id(), layout.admin_secret_id()].iter().map(|f| fs::read_to_string(f).unwrap()).collect();
    assert!(ids.iter().all(|id| id.len() == 64 && id.bytes().all(|b| b.is_ascii_hexdigit())), "ids are 32 random bytes as hex");
    assert_ne!(ids[0], ids[1]);

    let again = prepare(&dir).expect("the same state again");
    assert_eq!(again, layout);
    assert_eq!(fs::read(layout.seal_key()).unwrap(), key, "the key is made once");
    assert_eq!(fs::read_to_string(layout.admin_secret_id()).unwrap(), ids[1]);

    fs::set_permissions(layout.seal_key(), fs::Permissions::from_mode(0o640)).unwrap();
    let e = prepare(&dir).unwrap_err().to_string();
    assert!(e.contains("seal.key") && e.contains("mode 640"), "{e}");
    fs::set_permissions(layout.seal_key(), fs::Permissions::from_mode(0o600)).unwrap();
    fs::write(layout.seal_key(), &key[..16]).unwrap();
    let e = prepare(&dir).unwrap_err().to_string();
    assert!(e.contains("16 bytes"), "{e}");
    fs::remove_file(layout.seal_key()).unwrap();
    let e = prepare(&dir).unwrap_err().to_string();
    assert!(e.contains("is gone") && e.contains("remove"), "a seal key gone with its data still there: {e}");
    assert!(!layout.seal_key().exists(), "no new key is made over data it would not open");
    fs::remove_dir_all(&dir).unwrap();

    let quoted = std::env::temp_dir().join(format!("fragment-openbao-\"q\"-{}", crate::random_hex(4)));
    assert!(matches!(prepare(&quoted), Err(OpenBaoError::BadPath(_))));
    assert!(!quoted.exists());
}

// Goal: the config listens on loopback alone, keeps its data in the state,
// unseals with the key file by an id named from the key, and initialises
// the mount, the two policies, the cell's role and the stack's AppRole, in
// that order, reading the AppRole's ids from their files: no secret is in
// the config itself. The cell's policy reads the mount and nothing else.
#[test]
fn the_config_unseals_by_its_key_file_and_holds_no_secret() {
    let dir = scratch("config");
    let layout = prepare(&dir).unwrap();
    let config = render_config(&layout, 18_402, false).unwrap();
    let key_id = format!("fragment-{}", &crate::node::sha256_hex(&fs::read(layout.seal_key()).unwrap())[..16]);
    for want in [
        "address = \"127.0.0.1:18402\"".to_string(),
        "tls_disable = true".into(),
        "disable_clustering = true".into(),
        format!("storage \"pebbledb\" {{\n  path = \"{}\"", layout.data().display()),
        format!("current_key_id = \"{key_id}\""),
        format!("current_key = \"file://{}\"", layout.seal_key().display()),
        format!("path = \"{}\"", layout.admin_secret_id().display()),
    ] {
        assert!(config.contains(&want), "{want} in:\n{config}");
    }
    let order: Vec<usize> = ["\"mount\"", "\"cell-policy\"", "\"admin-policy\"", "\"cell-role\"", "\"approle\"", "\"admin-role\"", "\"admin-role-id\"", "\"admin-secret-id\""]
        .iter()
        .map(|r| config.find(&format!("request {r}")).unwrap_or_else(|| panic!("request {r}")))
        .collect();
    assert!(order.windows(2).all(|w| w[0] < w[1]), "the requests in order: {order:?}");
    for secret in [layout.admin_role_id(), layout.admin_secret_id()] {
        assert!(!config.contains(fs::read_to_string(secret).unwrap().trim()), "no id is in the config");
    }
    assert!(!config.contains("audit"), "no audit log unless asked");
    assert!(render_config(&layout, 18_402, true).unwrap().contains(&format!("file_path = \"{}\"", layout.audit().display())));
    assert_eq!(cell_policy(), "path \"fragment/data/*\" { capabilities = [\"read\"] }\n");
    assert!(!admin_policy().contains("delete") && !admin_policy().contains("sys/"), "the stack's policy deletes nothing and reaches no sys path");
    fs::write(layout.seal_key(), [7u8; SEAL_KEY_BYTES]).unwrap();
    assert!(!render_config(&layout, 18_402, false).unwrap().contains(&key_id), "a new key is a new id");
    fs::remove_dir_all(&dir).unwrap();
}

// Goal: only the pinned tarball is installed, an intranet's copy
// (`FRAGMENT_OPENBAO_TARBALL`) checked as a fetch is, and only its `bao`,
// a regular file, comes out of one. Method: a tarball that is not the pin,
// by path; tarballs whose `bao` is a link, or missing.
#[test]
fn only_the_pinned_tarball_and_its_bao_are_taken() {
    let pack = |entries: &[(&str, tar::EntryType, &[u8])]| {
        let mut tar = tar::Builder::new(Vec::new());
        for (path, kind, content) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(*kind);
            header.set_size(content.len() as u64);
            header.set_mode(0o755);
            if *kind == tar::EntryType::Symlink {
                header.set_link_name("/bin/sh").unwrap();
            }
            header.set_cksum();
            tar.append_data(&mut header, path, *content).unwrap();
        }
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        gz.write_all(&tar.into_inner().unwrap()).unwrap();
        gz.finish().unwrap()
    };
    let good = pack(&[("LICENSE", tar::EntryType::Regular, b"MPL-2.0"), ("bao", tar::EntryType::Regular, b"#!/bin/sh\necho not bao\n")]);
    assert_eq!(binary_from(&good, "t.tar.gz").unwrap(), b"#!/bin/sh\necho not bao\n");
    assert!(binary_from(&pack(&[("bao", tar::EntryType::Symlink, b"")]), "t.tar.gz").unwrap_err().to_string().contains("not a regular file"));
    assert!(binary_from(&pack(&[("LICENSE", tar::EntryType::Regular, b"x")]), "t.tar.gz").unwrap_err().to_string().contains("holds no bao"));

    let tools = scratch("tools");
    fs::create_dir_all(&tools).unwrap();
    let copy = tools.join("intranet-copy.tar.gz");
    fs::write(&copy, &good).unwrap();
    let e = locate_from(&tools, Some(copy)).unwrap_err();
    assert!(matches!(e, OpenBaoError::HashMismatch { .. }), "{e}");
    let tarball = tarball_for(std::env::consts::OS, std::env::consts::ARCH).unwrap();
    assert!(!tools.join(tarball.dir_name()).exists() && !tools.join(".partial-openbao").exists(), "nothing of it is installed");
    fs::remove_dir_all(&tools).unwrap();
}

/// What one request to the scripted server carried.
#[derive(Debug, Clone)]
struct Hit {
    path: String,
    token: Option<String>,
    namespace: Option<String>,
}

/// What a scripted server answers a path: its status, body and extra
/// headers, or, for `None`, nothing: it holds the connection.
type Answer = fn(&str) -> Option<(u16, String, Vec<(String, String)>)>;

/// A scripted server, and what each request to it carried.
struct Scripted {
    addr: String,
    hits: Arc<Mutex<Vec<Hit>>>,
}

fn scripted(answer: Answer) -> Scripted {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = format!("http://{}", listener.local_addr().unwrap());
    let hits: Arc<Mutex<Vec<Hit>>> = Arc::default();
    let log = Arc::clone(&hits);
    std::thread::spawn(move || {
        // bounded: the test's requests, then the test process ends
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let log = Arc::clone(&log);
            std::thread::spawn(move || serve(stream, answer, log));
        }
    });
    Scripted { addr, hits }
}

fn serve(mut stream: TcpStream, answer: Answer, log: Arc<Mutex<Vec<Hit>>>) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut first = String::new();
    // a connection opened and closed with no request in it
    if reader.read_line(&mut first).unwrap_or(0) == 0 {
        return;
    }
    let path = first.split_whitespace().nth(1).unwrap_or("").to_string();
    let (mut token, mut namespace) = (None, None);
    // bounded: a request's head, at most 100 lines
    for _ in 0..100 {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 || line.trim().is_empty() {
            break;
        }
        let (k, v) = line.split_once(':').unwrap_or(("", ""));
        match k.trim().to_ascii_lowercase().as_str() {
            "x-vault-token" => token = Some(v.trim().to_string()),
            "x-vault-namespace" => namespace = Some(v.trim().to_string()),
            _ => {}
        }
    }
    log.lock().unwrap().push(Hit { path: path.clone(), token, namespace });
    match answer(&path) {
        Some((status, body, extra)) => {
            let extra: String = extra.iter().map(|(k, v)| format!("{k}: {v}\r\n")).collect();
            let head = format!("HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n{extra}\r\n", body.len());
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(body.as_bytes());
        }
        // a service that hangs: the connection held, no answer
        None => std::thread::sleep(Duration::from_secs(30)),
    }
}

/// Each case (`env`, and the binding to `get()`, or null for the env
/// alone) through `withSecrets` on the pinned Node, at once: each one's
/// value, or its error's name, code and message, and how long it took.
/// The cases, tokens among them, go on Node's standard input.
fn shim(cases: &Value) -> Vec<Value> {
    let root = crate::repo_root();
    let node = crate::node::locate(&root.join(crate::TOOLS_DIR)).expect("the pinned Node");
    let dir = scratch("shim");
    fs::create_dir_all(&dir).unwrap();
    let script = dir.join("harness.mjs");
    let shim = root.join("cell/secrets.mjs");
    fs::write(
        &script,
        format!(
            r#"import {{ withSecrets }} from {shim};
let input = "";
for await (const chunk of process.stdin) input += chunk;
const results = await Promise.all(JSON.parse(input).map(async (c) => {{
  const t0 = Date.now();
  try {{
    const env = withSecrets(c.env);
    const hidden = env.FRAGMENT_SECRETS_TOKEN === undefined;
    if (c.binding === null) return {{ hidden, ms: Date.now() - t0 }};
    const value = await env[c.binding].get();
    return {{ value, hidden, ms: Date.now() - t0 }};
  }} catch (e) {{
    return {{ error: {{ name: e.name, code: e.code ?? null, message: String(e.message) }}, ms: Date.now() - t0 }};
  }}
}}));
console.log(JSON.stringify(results));
"#,
            shim = serde_json::to_string(&format!("file://{}", shim.display())).unwrap()
        ),
    )
    .unwrap();
    let mut child = node.script(&script, &root.join(crate::CACHE_DIR)).unwrap().stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    child.stdin.take().unwrap().write_all(cases.to_string().as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    fs::remove_dir_all(&dir).unwrap();
    assert!(out.status.success(), "the harness: {}", String::from_utf8_lossy(&out.stderr));
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&out.stdout)))
}

/// An env whose `HOST_SECRET` is `name` under OpenBao at `addr`, with
/// `extra` keys in its config.
fn env(addr: &str, name: &str, token: Option<&str>, extra: Value) -> Value {
    let mut cfg = json!({ "backend": "openbao", "addr": addr, "mount": "fragment", "secrets": [{ "binding": "HOST_SECRET", "secret_name": name }] });
    for (k, v) in extra.as_object().into_iter().flatten() {
        cfg[k] = v.clone();
    }
    let mut env = json!({ "FRAGMENT_SECRETS": cfg.to_string() });
    if let Some(t) = token {
        env[TOKEN_VAR] = json!(t);
    }
    env
}

fn case(env: Value, binding: Option<&str>) -> Value {
    json!({ "env": env, "binding": binding })
}

/// A port nothing listens on.
fn dead_addr() -> String {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    format!("http://{}", l.local_addr().unwrap())
}

/// Asserts a case failed with `code`, its message holding each of `says`.
fn failed(r: &Value, code: &str, says: &[&str]) {
    assert_eq!(r["error"]["name"], "SecretsError", "{r}");
    assert_eq!(r["error"]["code"], code, "{r}");
    let message = r["error"]["message"].as_str().unwrap();
    for s in says {
        assert!(message.contains(s), "{s:?} in {message:?}");
    }
}

/// Where the scripted OpenBao's redirect points: another server, which must
/// see nothing.
static ELSEWHERE: OnceLock<String> = OnceLock::new();

// Goal: the shim's openbao backend reads `<addr>/v1/<mount>/data/[<prefix>/]<name>`
// with the token (and the namespace when set), answers the value, and says
// why when it cannot, each kind its own: missing (404), refused (403),
// sealed (503, Vault's body), malformed (no `value`), failed (500, and a
// redirect, which it never follows, so the token goes nowhere else), and
// down: nothing listening at once, a server that hangs within its bound.
// The token is hidden from the env it hands on. Method: the pinned Node
// against a scripted server, every case at once.
#[test]
fn the_shim_reads_through_the_kv_api_and_says_why_it_cannot() {
    let elsewhere = scripted(|_| Some((200, "{}".into(), vec![])));
    ELSEWHERE.set(elsewhere.addr.clone()).expect("set once");
    let bao = scripted(|path| {
        let ok = |v: &str| Some((200, json!({ "data": { "data": { "value": v }, "metadata": { "version": 3 } } }).to_string(), vec![]));
        match path.rsplit('/').next().unwrap_or("") {
            "kept" => ok("the-value"),
            "missing" => Some((404, "{\"errors\":[]}".into(), vec![])),
            "refused" => Some((403, "{\"errors\":[\"1 error occurred:\\n\\t* permission denied\\n\\n\"]}".into(), vec![])),
            "sealed" => Some((503, "{\"errors\":[\"Vault is sealed\"]}".into(), vec![])),
            "odd" => Some((200, json!({ "data": { "data": { "other": "x" } } }).to_string(), vec![])),
            "broken" => Some((500, "{\"errors\":[\"internal error\"]}".into(), vec![])),
            "moved" => Some((307, "{}".into(), vec![("location".into(), format!("{}/v1/fragment/data/moved", ELSEWHERE.get().expect("set")))])),
            "hang" => None,
            _ => Some((404, "{\"errors\":[]}".into(), vec![])),
        }
    });
    let a = bao.addr.as_str();
    let t = Some("s.test-token");
    let none = json!({});
    let cases = json!([
        case(env(a, "kept", t, none.clone()), Some("HOST_SECRET")),
        case(env(a, "kept", t, json!({ "prefix": "team/fragment", "namespace": "corp/eng" })), Some("HOST_SECRET")),
        case(env(a, "missing", t, none.clone()), Some("HOST_SECRET")),
        case(env(a, "refused", t, none.clone()), Some("HOST_SECRET")),
        case(env(a, "sealed", t, none.clone()), Some("HOST_SECRET")),
        case(env(a, "odd", t, none.clone()), Some("HOST_SECRET")),
        case(env(a, "broken", t, none.clone()), Some("HOST_SECRET")),
        case(env(a, "moved", t, none.clone()), Some("HOST_SECRET")),
        case(env(&dead_addr(), "kept", t, none.clone()), Some("HOST_SECRET")),
        case(env(a, "hang", t, none.clone()), Some("HOST_SECRET")),
        case(env(a, "kept", t, none.clone()), None),
    ]);
    let r = shim(&cases);
    assert_eq!(r[0]["value"], "the-value", "{}", r[0]);
    assert_eq!(r[1]["value"], "the-value", "{}", r[1]);
    failed(&r[2], "missing", &["fragment/missing is not in OpenBao at", "bao kv put -mount=fragment missing value="]);
    failed(&r[3], "refused", &["refused the cell's token for fragment/refused", "permission denied"]);
    failed(&r[4], "sealed", &["is sealed", "Vault is sealed"]);
    failed(&r[5], "malformed", &["has no \"value\" field"]);
    failed(&r[6], "failed", &["answered 500", "internal error"]);
    failed(&r[7], "failed", &["answered 307"]);
    failed(&r[8], "down", &["is not answering"]);
    assert!(r[8]["ms"].as_u64().unwrap() < 2000, "nothing listening is an error at once: {}", r[8]);
    failed(&r[9], "down", &["did not answer for fragment/hang within 5 s"]);
    let hung = r[9]["ms"].as_u64().unwrap();
    assert!((4_900..8_000).contains(&hung), "a hang is cut at its bound: {hung} ms");
    assert_eq!(r[10]["hidden"], true, "the token is the shim's, not the env's");
    for (i, res) in r.iter().enumerate() {
        assert!(!res.to_string().contains("s.test-token"), "no answer or message holds the token: case {i}: {res}");
    }

    let hits = bao.hits.lock().unwrap().clone();
    let kept: Vec<&Hit> = hits.iter().filter(|h| h.path.ends_with("/kept")).collect();
    let paths: Vec<&str> = kept.iter().map(|h| h.path.as_str()).collect();
    assert!(paths.contains(&"/v1/fragment/data/kept") && paths.contains(&"/v1/fragment/data/team/fragment/kept"), "{paths:?}");
    assert!(hits.iter().all(|h| h.token.as_deref() == Some("s.test-token")), "every read carries the token: {hits:?}");
    assert!(kept.iter().any(|h| h.namespace.as_deref() == Some("corp/eng")) && kept.iter().any(|h| h.namespace.is_none()), "the namespace when set, alone");
    assert!(elsewhere.hits.lock().unwrap().is_empty(), "a redirect is never followed: the token goes nowhere else");
}

// Goal: a config that is wrong, or a token that is missing, is refused at
// the first request, saying what; the vars backend takes no OpenBao keys.
#[test]
fn a_wrong_config_is_refused_saying_what() {
    let a = "http://127.0.0.1:9";
    let t = Some("s.tok");
    let vars = json!({ "FRAGMENT_SECRETS": json!({ "backend": "vars", "addr": a, "secrets": [] }).to_string() });
    let bad = json!([
        case(env("http://127.0.0.1:9/vault", "x", t, json!({})), None),
        case(env("ftp://bao", "x", t, json!({})), None),
        case(env(a, "x", t, json!({ "mount": "../sys" })), None),
        case(env(a, "x", t, json!({ "prefix": "a/../b" })), None),
        case(env(a, "x", t, json!({ "adress": a })), None),
        case(env(a, "x", None, json!({})), None),
        case(env(a, "x", Some("has space"), json!({})), None),
        case(vars, None),
    ]);
    let r = shim(&bad);
    let says = [
        "addr is the service's origin",
        "addr is the service's origin",
        "mount is a KV v2 mount's path",
        "prefix, when set, is a path",
        "not adress",
        "FRAGMENT_SECRETS_TOKEN, which is not set",
        "which is not a token",
        "backend vars takes backend, secrets, not addr",
    ];
    for (i, says) in says.iter().enumerate() {
        let message = r[i]["error"]["message"].as_str().unwrap_or_else(|| panic!("case {i} refused: {}", r[i]));
        assert!(message.contains(says), "case {i}: {says:?} in {message:?}");
    }
}

/// The pinned `bao`, installed (written, then run) while no other test
/// forks.
fn pinned() -> PathBuf {
    let _held = crate::TEST_EXEC.lock().unwrap_or_else(|e| e.into_inner());
    locate(&crate::repo_root().join(crate::TOOLS_DIR)).expect("the pinned OpenBao")
}

/// The pinned OpenBao, started on a fresh state in `dir`.
fn started(dir: &Path, audit: bool) -> OpenBao {
    let bin = pinned();
    let port = crate::free_port().unwrap();
    OpenBao::start(&bin, Options { dir: dir.to_path_buf(), port, audit }).unwrap_or_else(|e| panic!("{e}")).0
}

// Goal: OpenBao initialises itself once, on its first start, and unseals
// itself from its key file at every start after, unattended; a seeding
// again writes nothing new; a key that does not open its data stops the
// start, saying so. Method: start, seed, stop, start again, seed again;
// then start with another key.
#[test]
#[ignore = "runs the pinned OpenBao (fetched into target/tools)"]
fn openbao_initialises_once_and_unseals_itself_at_every_start() {
    let dir = scratch("restarts");
    let mut bao = started(&dir, false);
    let target = bao.target();
    assert_eq!(target.seed(&SEEDED).unwrap(), Seeded { written: 2, kept: 0 });
    let key = fs::read(target.layout.seal_key()).unwrap();
    bao.down().unwrap();
    bao.up().expect("started again: unsealed by its key file");
    assert_eq!(target.seed(&SEEDED).unwrap(), Seeded { written: 0, kept: 2 }, "a restart writes no new version");
    assert_eq!(target.seed(&[("fragment-host-secret", "rotated")]).unwrap(), Seeded { written: 1, kept: 0 });
    bao.down().unwrap();
    bao.up().unwrap();
    assert_eq!(initialisations(&target.layout).unwrap(), 1, "initialised on the first start alone");
    assert_eq!(fs::read(target.layout.seal_key()).unwrap(), key);
    bao.down().unwrap();

    fs::write(target.layout.seal_key(), [9u8; SEAL_KEY_BYTES]).unwrap();
    let e = bao.up().unwrap_err().to_string();
    assert!(e.contains("stays sealed") && e.contains("does not open"), "{e}");
    assert!(!bao.running());
    fs::write(target.layout.seal_key(), &key).unwrap();
    bao.up().expect("its own key opens it again");
    drop(bao);
    fs::remove_dir_all(&dir).unwrap();
}

// Goal: the cell's token reads the mount and nothing else: no write, no
// other mount, no sys path; a second start keeps (renews) it; one that is
// not OpenBao's any more is replaced, owner-only. The stack's own token
// reaches no sys path either.
#[test]
#[ignore = "runs the pinned OpenBao (fetched into target/tools)"]
fn the_cells_token_reads_its_mount_and_nothing_else() {
    let dir = scratch("policy");
    let bao = started(&dir, false);
    let target = bao.target();
    target.seed(&SEEDED).unwrap();
    let token = target.cell_token().unwrap();
    assert_eq!(mode(&target.layout.cell_token()), 0o600);
    assert_eq!(target.cell_token().unwrap(), token, "kept, renewed");
    assert!(target.renew_cell_token().unwrap());
    let get = |path: &str, t: &str| target.call(reqwest::Method::GET, path, Some(t), None, path).unwrap().0;
    let put = |path: &str, t: &str| target.call(reqwest::Method::POST, path, Some(t), Some(&json!({ "data": { "value": "x" } })), path).unwrap().0;
    assert_eq!(get("fragment/data/fragment-host-secret", &token), 200);
    assert_eq!(get("fragment/data/nothing-here", &token), 404, "a missing secret is a 404 to it");
    assert_eq!(put("fragment/data/fragment-host-secret", &token), 403, "it writes nothing");
    assert_eq!(put("fragment/data/new-one", &token), 403);
    for refused in ["sys/mounts", "sys/policies/acl/fragment-admin", "cubbyhole/x", "fragment/metadata/fragment-host-secret", "auth/token/lookup-self"] {
        assert_eq!(get(refused, &token), 403, "{refused}");
    }
    let admin = target.admin_token().unwrap();
    assert_eq!(get("sys/mounts", &admin), 403, "the stack's token reaches no sys path");
    assert_eq!(get("fragment/data/nothing-here", "s.not-a-token"), 403, "a wrong token is refused");

    fs::write(target.layout.cell_token(), "s.lapsed-or-forged").unwrap();
    let new = target.cell_token().unwrap();
    assert_ne!(new, token);
    assert_eq!(get("fragment/data/fragment-host-secret", &new), 200);
    assert_eq!(mode(&target.layout.cell_token()), 0o600);
    drop(bao);
    fs::remove_dir_all(&dir).unwrap();
}

// Goal: the shim reads a seeded secret through the real OpenBao, and says
// why it cannot: missing, a wrong token (refused), OpenBao down, and
// OpenBao sealed (started with a key that does not open its data).
#[test]
#[ignore = "runs the pinned OpenBao (fetched into target/tools)"]
fn the_shim_reads_through_openbao_and_says_why_it_cannot() {
    let dir = scratch("shim-real");
    let mut bao = started(&dir, true);
    let target = bao.target();
    target.seed(&SEEDED).unwrap();
    let token = target.cell_token().unwrap();
    let a = target.addr.clone();
    let r = shim(&json!([
        case(env(&a, "fragment-host-secret", Some(&token), json!({})), Some("HOST_SECRET")),
        case(env(&a, "fragment-not-seeded", Some(&token), json!({})), Some("HOST_SECRET")),
        case(env(&a, "fragment-host-secret", Some("s.wrong-token"), json!({})), Some("HOST_SECRET")),
    ]));
    assert_eq!(r[0]["value"], SEEDED[0].1, "{}", r[0]);
    failed(&r[1], "missing", &["fragment/fragment-not-seeded is not in OpenBao"]);
    failed(&r[2], "refused", &["refused the cell's token", "permission denied"]);
    let audit = audited(bao.layout()).unwrap();
    assert_eq!(audit.cell_reads, 2, "the audit log holds the cell's two reads, the wrong token's none: {audit:?}");
    let log = fs::read_to_string(bao.layout().audit()).unwrap();
    assert!(!log.contains(&token) && !log.contains(SEEDED[0].1), "the audit log keeps tokens and values HMAC'd");

    bao.down().unwrap();
    let t0 = Instant::now();
    let r = shim(&json!([case(env(&a, "fragment-host-secret", Some(&token), json!({})), Some("HOST_SECRET"))]));
    failed(&r[0], "down", &["is not answering"]);
    assert!(t0.elapsed() < Duration::from_secs(5), "down is said at once");

    // sealed: the server up, with a key that does not open its data
    let key = fs::read(target.layout.seal_key()).unwrap();
    fs::write(target.layout.seal_key(), [3u8; SEAL_KEY_BYTES]).unwrap();
    let port: u16 = target.addr.rsplit(':').next().unwrap().parse().unwrap();
    fs::write(target.layout.config(), render_config(&target.layout, port, false).unwrap()).unwrap();
    let bin = pinned();
    let mut sealed = Command::new(&bin).arg("server").arg(format!("-config={}", target.layout.config().display())).env_clear().stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
    let up = Instant::now();
    while target.call(reqwest::Method::GET, "sys/health", None, None, "health").ok().map(|(s, _)| s) != Some(503) {
        assert!(up.elapsed() < READY_TIMEOUT, "OpenBao answers, sealed");
        std::thread::sleep(Duration::from_millis(50));
    }
    let r = shim(&json!([case(env(&a, "fragment-host-secret", Some(&token), json!({})), Some("HOST_SECRET"))]));
    let _ = sealed.kill();
    let _ = sealed.wait();
    failed(&r[0], "sealed", &["is sealed", "Vault is sealed"]);
    fs::write(target.layout.seal_key(), &key).unwrap();
    drop(bao);
    fs::remove_dir_all(&dir).unwrap();
}
