//! `hermes-boot`: everything the Hermes image does around Hermes itself
//! (docs/computers.md, the guest's contract; spike S3b's boot list):
//!
//! ```text
//! hermes-boot pre-init     the ENTRYPOINT: preload the gateway, wait for the
//!                          restore gate, then exec s6's /init with `main`
//! hermes-boot main         /init's main program: config, profiles from the
//!                          computer's agents and their repos, the bridge,
//!                          the gateway, Litestream; until SIGTERM
//! hermes-boot stamped <name> <input> -- <cmd…>
//!                          a setup step, skipped when this image already ran
//!                          it on this exact input
//! hermes-boot readahead    warm the page cache with the gateway's files
//! hermes-boot build-info   (at image build) the lean plugin list, Hermes'
//!                          revision, the browser's path
//! ```

mod hermes;
mod sync;

use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use fragment_bridge::api::Api;
use fragment_bridge::ev;
use fragment_bridge::runtime::relay::wire;
use fragment_bridge::runtime::Agent;

/// Where the image keeps its own files.
const OPT: &str = "/opt/fragment";
/// This boot's runtime files (the relay secret, the gateway's go): emptied
/// at every start, since a container started from a snapshot carries the
/// last boot's (S3b).
const RUN: &str = "/var/lib/fragment-run";
/// The relay's port, on loopback only: Hermes dials it, nothing else can.
const RELAY_LISTEN: &str = "127.0.0.1:8650";
/// The gateway id both sides name.
const GATEWAY_ID: &str = "fragment-computer";
/// The restore gate is given this long before the boot gives up.
const GATE_MS_MAX: u64 = 15 * 60 * 1000;
/// The CA for HTTPS interception appears shortly after boot (S3); it is
/// waited for this long, off the boot's path.
const CA_WAIT_MS: u64 = 15_000;
const CA: &str = "/etc/cloudflare/certs/cloudflare-containers-ca.crt";
const CA_BUNDLE: &str = "/etc/ssl/certs/ca-certificates.crt";
/// The agents' repos sync this often while awake.
const SYNC_EVERY_MS: u64 = 60_000;
/// SIGTERM: children are told, then the boot is gone within this.
const STOP_MS_MAX: u64 = 2_500;
/// The bridge is restarted at most this many times before the boot fails.
const BRIDGE_RESTARTS_MAX: u32 = 10;
/// Readahead reads at most this many files.
const READAHEAD_FILES_MAX: usize = 5_000;

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

fn home() -> PathBuf {
    PathBuf::from(env("HERMES_HOME").unwrap_or_else(|| "/data/hermes".into()))
}

fn fail(why: &str) -> ! {
    ev!("boot.failed", { "why": why });
    std::process::exit(1);
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("pre-init") => pre_init(),
        Some("main") => tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("a runtime").block_on(boot_main()),
        Some("stamped") => stamped(&args[2..]),
        Some("readahead") => readahead(),
        Some("build-info") => build_info(),
        _ => fail("hermes-boot pre-init | main | stamped | readahead | build-info"),
    }
}

// ---- pre-init: before s6, before the restore ----

fn pre_init() -> ! {
    let t0 = Instant::now();
    ev!("boot.pre_init");
    let _ = std::fs::remove_dir_all(RUN);
    std::fs::create_dir_all(RUN).unwrap_or_else(|e| fail(&format!("{RUN}: {e}")));
    // The browser's path, found at build: stage2 skips its own search.
    if let Ok(p) = std::fs::read_to_string(format!("{OPT}/browser-path")) {
        if !p.trim().is_empty() && env("AGENT_BROWSER_EXECUTABLE_PATH").is_none() {
            std::env::set_var("AGENT_BROWSER_EXECUTABLE_PATH", p.trim());
        }
    }
    // Warm the gateway's files on the second CPU, and start its imports, while
    // the platform restores /data (S3b: -0.2 s and -0.9 s).
    let _ = Command::new(format!("{OPT}/bin/hermes-boot")).arg("readahead").spawn();
    let home = home();
    // Hermes' imports check its home: it exists, and is Hermes', before them
    // (a restore pending replaces it; stage2 settles its ownership after).
    let _ = std::fs::create_dir_all(&home);
    chown(&home, hermes_ids());
    let preload = Command::new("/command/s6-setuidgid")
        .args(["hermes", "/opt/hermes/.venv/bin/python", &format!("{OPT}/preload.py")])
        .env("HOME", &home)
        .env("HERMES_HOME", &home)
        .env("FRAGMENT_RUN", RUN)
        .current_dir("/")
        .spawn();
    match preload {
        Ok(child) => {
            let _ = std::fs::write(format!("{RUN}/preload.pid"), child.id().to_string());
        }
        Err(e) => ev!("boot.preload_failed", { "error": e.to_string() }),
    }
    // The relay's secret: fresh each boot, shared by the bridge and Hermes only.
    let secret = random_hex(32);
    write_private(&format!("{RUN}/relay.secret"), &secret);
    if env("RESTORE_PENDING").as_deref() == Some("1") {
        let gate = Instant::now();
        // bounded by GATE_MS_MAX
        while !Path::new("/run/computer/restored").exists() {
            if gate.elapsed() > Duration::from_millis(GATE_MS_MAX) {
                fail("the restore marker never came");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        ev!("boot.gate_open", { "waitedMs": gate.elapsed().as_millis() as u64 });
    }
    ev!("boot.init", { "ms": t0.elapsed().as_millis() as u64 });
    let err = Command::new("/init").args(["/command/with-contenv", &format!("{OPT}/bin/hermes-boot"), "main"]).exec();
    fail(&format!("exec /init: {err}"));
}

fn random_hex(bytes: usize) -> String {
    use std::io::Read;
    let mut buf = vec![0u8; bytes];
    std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut buf)).unwrap_or_else(|e| fail(&format!("/dev/urandom: {e}")));
    fragment_bridge::records::hex(&buf)
}

fn write_private(path: &str, text: &str) {
    std::fs::write(path, text).unwrap_or_else(|e| fail(&format!("{path}: {e}")));
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

// ---- stamped setup steps ----

fn image_rev() -> String {
    std::fs::read_to_string(format!("{OPT}/image-rev")).map(|s| s.trim().to_string()).unwrap_or_else(|_| "unknown".into())
}

fn stamp_key(input: &Path) -> String {
    let h = std::fs::read(input).map(|b| sync::hash(&b)[..16].to_string()).unwrap_or_else(|_| "none".into());
    format!("{}:{h}", image_rev())
}

/// `stamped <name> <input> -- <cmd…>`: a setup step is a pure function of
/// (the image, its input under the Hermes home); when both match what its
/// last run recorded, it has nothing to do (S3b).
fn stamped(args: &[String]) -> ! {
    let (Some(name), Some(input), Some(sep)) = (args.first(), args.get(1), args.get(2)) else { fail("stamped <name> <input> -- <cmd…>") };
    if sep != "--" || args.len() < 4 {
        fail("stamped <name> <input> -- <cmd…>");
    }
    let stamp = home().join(".fragment-stamps").join(name);
    let input = PathBuf::from(input);
    if std::fs::read_to_string(&stamp).ok().as_deref() == Some(stamp_key(&input).as_str()) {
        ev!("setup.skipped", { "step": name });
        std::process::exit(0);
    }
    let t = Instant::now();
    let status = Command::new(&args[3]).args(&args[4..]).status().unwrap_or_else(|e| fail(&format!("{}: {e}", args[3])));
    if status.success() {
        // Re-keyed after the step: it may have rewritten its input.
        let _ = std::fs::create_dir_all(stamp.parent().expect("a parent"));
        let _ = std::fs::write(&stamp, stamp_key(&input));
    }
    ev!("setup.ran", { "step": name, "ms": t.elapsed().as_millis() as u64, "ok": status.success() });
    std::process::exit(status.code().unwrap_or(1));
}

fn readahead() -> ! {
    let Ok(list) = std::fs::read_to_string(format!("{OPT}/readahead.list")) else { std::process::exit(0) };
    let mut buf = vec![0u8; 1 << 16];
    for path in list.lines().take(READAHEAD_FILES_MAX) {
        use std::io::Read;
        if let Ok(mut f) = std::fs::File::open(path) {
            // bounded by the file
            while matches!(f.read(&mut buf), Ok(n) if n > 0) {}
        }
    }
    std::process::exit(0);
}

/// At image build: what the boot reads instead of finding it each time.
fn build_info() -> ! {
    let plugins = hermes::lean_plugins(Path::new("/opt/hermes/plugins"));
    std::fs::write(format!("{OPT}/lean-plugins.txt"), plugins.join("\n")).unwrap_or_else(|e| fail(&e.to_string()));
    let rev = std::fs::read("/etc/hermes/image-provenance.json").ok().and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok()).and_then(|v| v["revision"].as_str().map(str::to_string)).unwrap_or_else(|| "unknown".into());
    std::fs::write(format!("{OPT}/image-rev"), &rev).unwrap_or_else(|e| fail(&e.to_string()));
    let browser = find_browser(Path::new("/opt/hermes/.playwright"));
    std::fs::write(format!("{OPT}/browser-path"), browser.as_ref().map(|p| p.display().to_string()).unwrap_or_default()).unwrap_or_else(|e| fail(&e.to_string()));
    println!("{} plugins disabled; Hermes {rev}; browser {:?}", plugins.len(), browser);
    std::process::exit(0);
}

fn find_browser(root: &Path) -> Option<PathBuf> {
    let mut stack = vec![root.to_path_buf()];
    let mut seen = 0;
    // bounded: 100 000 entries of Playwright's tree
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else { continue };
        for e in entries.filter_map(Result::ok) {
            seen += 1;
            if seen > 100_000 {
                return None;
            }
            let p = e.path();
            let Ok(meta) = e.metadata() else { continue };
            if meta.is_dir() {
                stack.push(p);
            } else if meta.permissions().mode() & 0o111 != 0 && matches!(p.file_name().and_then(|n| n.to_str()), Some("chrome-headless-shell" | "headless_shell")) {
                return Some(p);
            }
        }
    }
    None
}

// ---- main: under s6, after its setup ----

/// The hermes user's uid and gid (the image's stage2 may have remapped them).
fn hermes_ids() -> Option<(u32, u32)> {
    let passwd = std::fs::read_to_string("/etc/passwd").ok()?;
    let line = passwd.lines().find(|l| l.starts_with("hermes:"))?;
    let f: Vec<&str> = line.split(':').collect();
    Some((f.get(2)?.parse().ok()?, f.get(3)?.parse().ok()?))
}

fn chown(path: &Path, ids: Option<(u32, u32)>) {
    let Some((uid, gid)) = ids else { return };
    let Ok(c) = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()) else { return };
    // SAFETY: a valid NUL-terminated path; lchown never follows a link.
    unsafe {
        libc::lchown(c.as_ptr(), uid, gid);
    }
}

fn signal(pid: u32, sig: libc::c_int) {
    // SAFETY: kill(2) on a pid this boot started (or read from its own file).
    unsafe {
        libc::kill(pid as libc::pid_t, sig);
    }
}

fn alive(pid: u32) -> bool {
    Path::new(&format!("/proc/{pid}")).exists() && !std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|s| s.split_whitespace().nth(2) == Some("Z"))
}

/// Appends the interception CA to the system bundle once it appears, never
/// rebuilding the store (S3b: `update-ca-certificates` is 0.7 s), and never
/// twice (a snapshot carries the last boot's bundle).
fn trust_ca() {
    std::thread::spawn(|| {
        let t = Instant::now();
        // bounded by CA_WAIT_MS
        while t.elapsed() < Duration::from_millis(CA_WAIT_MS) {
            if let Ok(pem) = std::fs::read_to_string(CA) {
                let bundle = std::fs::read_to_string(CA_BUNDLE).unwrap_or_default();
                if !bundle.contains(pem.trim()) {
                    use std::io::Write;
                    let appended = std::fs::OpenOptions::new().append(true).open(CA_BUNDLE).and_then(|mut f| writeln!(f, "\n{}", pem.trim()));
                    ev!("boot.ca", { "appended": appended.is_ok(), "ms": t.elapsed().as_millis() as u64 });
                } else {
                    ev!("boot.ca", { "appended": false, "already": true });
                }
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        ev!("boot.ca", { "appended": false, "why": "no CA appeared" });
    });
}

/// Each agent's profile: its directories, its config, its repo pulled.
async fn profiles(api: &Api, agents: &[Agent], home: &Path, ids: Option<(u32, u32)>, model: &str) {
    let profiles_root = home.join("profiles");
    let _ = std::fs::create_dir_all(&profiles_root);
    chown(&profiles_root, ids);
    let high_on = env("FRAGMENT_HIGH_TIER").as_deref() == Some("on");
    let own = move |p: &Path| chown(p, ids);
    for a in agents {
        let dir = hermes::profile_dir(home, &a.fragment);
        let fresh = !dir.exists();
        for sub in hermes::PROFILE_DIRS {
            let _ = std::fs::create_dir_all(dir.join(sub));
            chown(&dir.join(sub), ids);
        }
        chown(&dir, ids);
        // Which agent this profile is: what retiring it later reads.
        let _ = std::fs::write(dir.join(".fragment-agent"), &a.fragment);
        let agent_json = api.file(&a.fragment, &a.fragment, "agent.json", 64 * 1024).await.ok();
        let tier = hermes::Tier::of(agent_json.as_deref(), high_on);
        let cfg = dir.join("config.yaml");
        let _ = std::fs::write(&cfg, hermes::profile_config(a, tier, model));
        chown(&cfg, ids);
        let env_file = dir.join(".env");
        if !env_file.exists() {
            let _ = std::fs::write(&env_file, format!("# The profile of {}; the computer holds no credential.\n", a.fragment));
            chown(&env_file, ids);
        }
        let t = Instant::now();
        match sync::round(api, a, &dir, &PathBuf::from("/data/hermes-sync"), &own).await {
            Ok(d) => ev!("boot.profile", { "agent": a.fragment, "profile": wire::profile(&a.fragment), "fresh": fresh, "tier": tier.name(), "pulled": d.pulled, "pushed": d.pushed, "conflicts": d.conflicts, "ms": t.elapsed().as_millis() as u64 }),
            Err(e) => ev!("boot.profile", { "agent": a.fragment, "fresh": fresh, "syncError": e.to_string() }),
        }
    }
    // A profile whose agent left this computer is moved aside, never deleted.
    if let Ok(entries) = std::fs::read_dir(&profiles_root) {
        for e in entries.filter_map(Result::ok) {
            let marker = std::fs::read_to_string(e.path().join(".fragment-agent")).unwrap_or_default();
            if !marker.is_empty() && !agents.iter().any(|a| a.fragment == marker) {
                let to = PathBuf::from("/data/hermes-retired").join(format!("{}-{}", e.file_name().to_string_lossy(), fragment_bridge::log::now_ms()));
                let _ = std::fs::create_dir_all("/data/hermes-retired");
                let moved = std::fs::rename(e.path(), &to);
                ev!("boot.profile_retired", { "agent": marker, "to": to.display().to_string(), "ok": moved.is_ok() });
            }
        }
    }
}

fn spawn_bridge(agents: &[Agent], home: &Path) -> Option<Child> {
    let mut c = Command::new(format!("{OPT}/bin/fragment-bridge"));
    c.arg("run")
        .env("BRIDGE_RUNTIME", "relay")
        .env("BRIDGE_RELAY_LISTEN", RELAY_LISTEN)
        .env("BRIDGE_RELAY_SECRET_FILE", format!("{RUN}/relay.secret"))
        .env("GATEWAY_RELAY_ID", GATEWAY_ID)
        .env("BRIDGE_STATE_DIR", "/data/bridge")
        .env("BRIDGE_PROMPT_TTL_MS", (hermes::APPROVAL_TIMEOUT_S * 1000).to_string())
        .env("BRIDGE_SCREEN_LISTEN", "0.0.0.0:6080")
        .env("BRIDGE_SCREEN_DIR", format!("{OPT}/screen"))
        // The restore already happened: the bridge reads /data at once.
        .env_remove("RESTORE_PENDING");
    // The screen is the first agent's desktop, started at its first viewer.
    if let Some(first) = agents.first() {
        let p = wire::profile(&first.fragment);
        let profile_home = hermes::profile_dir(home, &first.fragment);
        c.env("BRIDGE_SCREEN_RFB", format!("unix:{}", profile_home.join("bot-desktop/rfb.sock").display()));
        c.env("BRIDGE_SCREEN_START", format!("/usr/bin/env HOME={h} HERMES_HOME={h} /command/s6-setuidgid hermes /opt/hermes/.venv/bin/hermes -p {p} computer-use screen start", h = home.display()));
    }
    match c.spawn() {
        Ok(child) => Some(child),
        Err(e) => {
            ev!("boot.bridge_failed", { "error": e.to_string() });
            None
        }
    }
}

/// The gateway: the preloaded one is told to go; without one, it is
/// started now.
fn start_gateway(home: &Path) -> Option<u32> {
    let secret = std::fs::read_to_string(format!("{RUN}/relay.secret")).unwrap_or_else(|e| fail(&format!("the relay secret: {e}")));
    let env_text = hermes::gateway_env(RELAY_LISTEN, GATEWAY_ID, secret.trim());
    write_private(&format!("{RUN}/gateway.env"), &env_text);
    chown(Path::new(&format!("{RUN}/gateway.env")), hermes_ids());
    let preload = std::fs::read_to_string(format!("{RUN}/preload.pid")).ok().and_then(|p| p.trim().parse::<u32>().ok()).filter(|p| alive(*p));
    if let Some(pid) = preload {
        let _ = std::fs::write(format!("{RUN}/go"), "");
        ev!("boot.gateway", { "preloaded": true, "pid": pid });
        return Some(pid);
    }
    let mut c = Command::new("/command/s6-setuidgid");
    c.args(["hermes", "/opt/hermes/.venv/bin/hermes", "gateway", "run"]).env("HOME", home).env("HERMES_HOME", home).current_dir(home);
    for line in env_text.lines() {
        if let Some((k, v)) = line.split_once('=') {
            c.env(k, v);
        }
    }
    match c.spawn() {
        Ok(child) => {
            ev!("boot.gateway", { "preloaded": false, "pid": child.id() });
            Some(child.id())
        }
        Err(e) => {
            ev!("boot.gateway_failed", { "error": e.to_string() });
            None
        }
    }
}

/// What the previous start left behind, ended before the gateway takes a
/// turn. A container start is a fresh process tree, and the bridge has
/// already ended every turn a restart cut short (docs/chat-records.md), so:
///
/// - Hermes' clean-exit receipt (`.clean_shutdown`) is written: `/data` is
///   saved before the guest is signalled, so a restored one always reads
///   as an unclean exit, and Hermes would resume its in-flight turns under
///   their old message ids, folding the person's next message into them.
///   With the receipt it discards their markers instead.
/// - The cross-process leases Hermes keeps in its databases (a session's
///   turn, a compression) are cleared: their holder's PID names a live
///   process of this start (PIDs repeat in a fresh namespace: the gateway
///   is PID 7 each time), so Hermes would wait out its five-minute TTL.
///
/// As the `hermes` user, so the files keep their owner.
fn end_previous_life(agents: &[Agent], home: &Path) {
    let receipt = home.join(".clean_shutdown");
    if let Err(e) = std::fs::write(&receipt, "") {
        ev!("boot.receipt_failed", { "error": e.to_string() });
    }
    chown(&receipt, hermes_ids());
    let mut dbs: Vec<PathBuf> = vec![home.join("state.db")];
    dbs.extend(agents.iter().map(|a| hermes::profile_dir(home, &a.fragment).join("state.db")));
    dbs.retain(|p| p.exists());
    if dbs.is_empty() {
        return;
    }
    let t = Instant::now();
    let script = r#"import sqlite3, sys
for p in sys.argv[1:]:
    c = sqlite3.connect(p, timeout=5)
    names = {r[0] for r in c.execute("SELECT name FROM sqlite_master WHERE type = 'table'")}
    for t in ('session_turn_leases', 'compression_locks'):
        if t in names:
            c.execute('DELETE FROM ' + t)
    c.commit()
    c.close()
"#;
    let out = Command::new("/command/s6-setuidgid").args(["hermes", "/opt/hermes/.venv/bin/python", "-c", script]).args(&dbs).output();
    match out {
        Ok(o) if o.status.success() => ev!("boot.leases_cleared", { "dbs": dbs.len(), "ms": t.elapsed().as_millis() as u64 }),
        Ok(o) => ev!("boot.leases_failed", { "status": o.status.code(), "error": String::from_utf8_lossy(&o.stderr).chars().take(300).collect::<String>() }),
        Err(e) => ev!("boot.leases_failed", { "error": e.to_string() }),
    }
}

/// Litestream, once the profiles' databases exist (Hermes makes them as its
/// gateway starts), off the wake path.
fn start_litestream(agents: &[Agent], home: &Path) -> Option<Child> {
    let storage = env("FRAGMENT_STORAGE")?;
    if !Path::new("/usr/local/bin/litestream").exists() {
        return None;
    }
    let mut dbs: Vec<(String, PathBuf)> = vec![("default".into(), home.join("state.db"))];
    for a in agents {
        dbs.push((wire::profile(&a.fragment), hermes::profile_dir(home, &a.fragment).join("state.db")));
    }
    dbs.retain(|(_, p)| p.exists());
    if dbs.is_empty() {
        return None;
    }
    let cfg = format!("{RUN}/litestream.yml");
    std::fs::write(&cfg, hermes::litestream_config(&dbs, &storage)).ok()?;
    ev!("boot.litestream", { "dbs": dbs.len() });
    Command::new("/usr/local/bin/litestream").args(["replicate", "-config", &cfg]).spawn().ok()
}

async fn boot_main() {
    let t0 = Instant::now();
    ev!("boot.main");
    trust_ca();
    let home = home();
    let ids = hermes_ids();
    let api = Api::new(&env("FRAGMENT_API").unwrap_or_else(|| "http://api.fragment.internal".into())).unwrap_or_else(|e| fail(&e));
    let model = env("FRAGMENT_MODEL").unwrap_or_else(|| "http://model.fragment.internal".into());

    // The agents to run (GET /api/computer), asked until the platform answers.
    let mut backoff = fragment_bridge::net::Backoff::default();
    let started = Instant::now();
    let computer = loop {
        match api.computer().await {
            Ok(c) => break c,
            Err(e) if started.elapsed() < Duration::from_secs(120) => {
                ev!("boot.computer_unread", { "error": e.to_string() });
                tokio::time::sleep(Duration::from_millis(backoff.next_ms())).await;
            }
            Err(e) => fail(&format!("GET /api/computer: {e}")),
        }
    };
    let agents = computer.agents.clone();

    let lean: Vec<String> = std::fs::read_to_string(format!("{OPT}/lean-plugins.txt")).unwrap_or_default().lines().filter(|l| !l.trim().is_empty()).map(str::to_string).collect();
    let _ = std::fs::create_dir_all("/etc/hermes");
    std::fs::write("/etc/hermes/config.yaml", hermes::managed_config(&lean)).unwrap_or_else(|e| fail(&format!("/etc/hermes/config.yaml: {e}")));
    let default_cfg = home.join("config.yaml");
    let ours = std::fs::read_to_string(&default_cfg).is_ok_and(|t| t.starts_with("# Written by hermes-boot"));
    if !ours {
        // Written once: the stamped config migration keys on this file.
        let _ = std::fs::write(&default_cfg, hermes::default_config(&model));
        chown(&default_cfg, ids);
    }
    profiles(&api, &agents, &home, ids, &model).await;
    ev!("boot.configured", { "agents": agents.len(), "ms": t0.elapsed().as_millis() as u64 });

    end_previous_life(&agents, &home);
    let mut bridge = spawn_bridge(&agents, &home);
    let Some(gateway) = start_gateway(&home) else { fail("no gateway") };
    ev!("boot.ready", { "ms": t0.elapsed().as_millis() as u64 });

    let mut litestream: Option<Child> = None;
    let mut restarts = 0u32;
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).expect("a SIGTERM handler");
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    let mut last_sync = Instant::now();
    // HERMES_BOOT_SYNC_MS: the operator's (a test's) cadence, at least 1 s.
    let sync_every = env("HERMES_BOOT_SYNC_MS").and_then(|v| v.parse::<u64>().ok()).map_or(SYNC_EVERY_MS, |v| v.max(1_000));
    let own = move |p: &Path| chown(p, ids);
    // bounded by the computer's life: one tick or one signal per pass
    loop {
        tokio::select! {
            _ = term.recv() => break,
            _ = tick.tick() => {}
        }
        if !alive(gateway) {
            ev!("boot.gateway_exited");
            stop(gateway, bridge.as_mut(), litestream.as_mut()).await;
            std::process::exit(1);
        }
        if let Some(b) = bridge.as_mut() {
            if let Ok(Some(status)) = b.try_wait() {
                restarts += 1;
                ev!("boot.bridge_exited", { "status": status.code(), "restarts": restarts });
                if restarts > BRIDGE_RESTARTS_MAX {
                    stop(gateway, None, litestream.as_mut()).await;
                    fail("the bridge keeps exiting");
                }
                bridge = spawn_bridge(&agents, &home);
            }
        }
        if litestream.is_none() && t0.elapsed() > Duration::from_secs(10) {
            litestream = start_litestream(&agents, &home);
        }
        if last_sync.elapsed() > Duration::from_millis(sync_every) {
            last_sync = Instant::now();
            for a in &agents {
                let dir = hermes::profile_dir(&home, &a.fragment);
                match sync::round(&api, a, &dir, &PathBuf::from("/data/hermes-sync"), &own).await {
                    Ok(d) if d != sync::Done::default() => ev!("sync.round", { "agent": a.fragment, "pulled": d.pulled, "pushed": d.pushed, "conflicts": d.conflicts, "deleted": d.deleted }),
                    Ok(_) => {}
                    Err(e) => ev!("sync.failed", { "agent": a.fragment, "error": e.to_string() }),
                }
            }
        }
    }
    ev!("boot.signal");
    stop(gateway, bridge.as_mut(), litestream.as_mut()).await;
    std::process::exit(0);
}

/// SIGTERM to every child, then wait for them, at most `STOP_MS_MAX`.
async fn stop(gateway: u32, bridge: Option<&mut Child>, litestream: Option<&mut Child>) {
    let t = Instant::now();
    signal(gateway, libc::SIGTERM);
    let mut children: Vec<&mut Child> = Vec::new();
    for c in [bridge, litestream].into_iter().flatten() {
        signal(c.id(), libc::SIGTERM);
        children.push(c);
    }
    // bounded by STOP_MS_MAX
    while t.elapsed() < Duration::from_millis(STOP_MS_MAX) {
        let waiting = alive(gateway) || children.iter_mut().any(|c| matches!(c.try_wait(), Ok(None)));
        if !waiting {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    ev!("boot.stopped", { "ms": t.elapsed().as_millis() as u64, "gatewayLeft": alive(gateway) });
}
