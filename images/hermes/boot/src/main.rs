//! `hermes-boot`: everything the Hermes image does around Hermes itself
//! (docs/computers.md, the guest's contract; spike S3b's boot list):
//!
//! ```text
//! hermes-boot pre-init     the ENTRYPOINT: preload the gateway, wait for the
//!                          restore gate, then exec s6's /init with `main`
//! hermes-boot main         /init's main program: config, profiles from the
//!                          computer's agents and their repos, the bridge,
//!                          the gateway, its answer to the hold; then, until
//!                          SIGTERM, the agents followed as they change
//!                          (agents.rs)
//! hermes-boot stamped <name> <input> -- <cmd…>
//!                          a setup step, skipped when this image already ran
//!                          it on this exact input
//! hermes-boot readahead    warm the page cache with the gateway's files
//! hermes-boot screen-start (the bridge's, when a viewer finds the screen
//!                          down) the first agent's desktop
//! hermes-boot build-info   (at image build) the lean plugin list, Hermes'
//!                          revision, the browser's path, the platform skill
//! ```

mod agents;
mod held;
mod hermes;
mod skills;
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
/// The fragment CLI the agents' terminals run (its skill is the platform
/// skill's: skills.rs).
const FRAGMENT_CLI: &str = "/usr/local/bin/fragment";
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
/// The managed skills (skills.rs) are read again this often while awake: a
/// platform release changes them, and an install fetches only what changed.
/// While the owner has none, every `skills::ABSENT_EVERY_MS`.
const SKILLS_EVERY_MS: u64 = 10 * 60_000;
const _: () = assert!(skills::ABSENT_EVERY_MS < SKILLS_EVERY_MS && skills::ABSENT_EVERY_MS >= 10_000, "no skills fragment is looked for sooner, never in a loop");
/// SIGTERM: children are told, then the boot is gone within this.
const STOP_MS_MAX: u64 = 2_500;
/// The bridge is restarted at most this many times before the boot fails.
const BRIDGE_RESTARTS_MAX: u32 = 10;
/// Readahead reads at most this many files.
const READAHEAD_FILES_MAX: usize = 5_000;
/// The computer's agents are read again this often while awake: one
/// assigned meanwhile has its profile, and is run, within about this
/// (docs/computers.md). One `GET /api/computer`, answered by the Computer
/// DO itself.
const AGENTS_EVERY_MS: u64 = 3_000;
/// The gateway's answer to a control verb is waited for this long (its
/// rescan waits up to 5 s on its own loop before answering `pending`).
const CONTROL_WAIT_MS: u64 = 8_000;
/// Under `RUN`: the bridge's ready file (the agents whose profiles are
/// written: its `ready.rs`), the screen's socket (a link to the first
/// agent's display), and that agent's name (what `screen-start` starts).
const READY_FILE: &str = "agents.json";
const SCREEN_SOCKET: &str = "screen.sock";
const SCREEN_AGENT: &str = "screen-agent";

const _: () = assert!(AGENTS_EVERY_MS >= 1_000 && AGENTS_EVERY_MS < SYNC_EVERY_MS, "agents are read often, and at most once a tick");

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
        Some("screen-start") => screen_start(),
        Some("build-info") => build_info(),
        Some("copy") => copy(&args[2..]),
        Some("check-restore") => check_restore(),
        Some("quick-check") => quick_check(&args[2..]),
        Some("sqlite-report") => sqlite_report(&args[2..]),
        _ => fail("hermes-boot pre-init | main | stamped | readahead | screen-start | build-info | copy | check-restore | quick-check | sqlite-report"),
    }
}

// ---- the hold's copies (held.rs) ----

/// The exit that says what a start restored is unusable (the platform's
/// check, docs/computers.md: it then starts from the save before).
const UNUSABLE: i32 = 3;

/// `copy <staging> <db>…`, as the hermes user, under a hold: each database
/// copied by SQLite's online backup, then the manifest. Prints what it
/// copied (`{copies, bytes}`).
fn copy(args: &[String]) -> ! {
    let Some((staging, dbs)) = args.split_first() else { fail("copy <staging> <db>…") };
    let dbs: Vec<PathBuf> = dbs.iter().map(PathBuf::from).collect();
    match held::copy_all(&dbs, Path::new(staging)) {
        Ok(m) => {
            println!("{}", serde_json::json!({ "copies": m.copies.len(), "bytes": m.copies.iter().map(|c| c.bytes).sum::<u64>() }));
            std::process::exit(0);
        }
        Err(e) => {
            println!("{e}");
            std::process::exit(1);
        }
    }
}

/// `check-restore`, the image's `/usr/local/bin/computer-check`: run by the
/// platform as root after it restores `/data`, before the gate opens. The
/// last hold's copies go back in place, then every database is checked as
/// the hermes user. Exits 0 when all is whole, `UNUSABLE` when the save is
/// not, 1 when the check itself failed.
fn check_restore() -> ! {
    let t = Instant::now();
    let put = match held::put_back(Path::new(held::DATA), Path::new(held::STAGING)) {
        Ok(put) => put,
        Err(e @ held::HeldError::Manifest(_)) => {
            println!("{e}");
            std::process::exit(UNUSABLE);
        }
        Err(e) => {
            println!("{e}");
            std::process::exit(1);
        }
    };
    let dbs = held::databases(Path::new(held::DATA), &[Path::new(held::STAGING), Path::new(hermes::WORK)]).unwrap_or_else(|e| {
        println!("{e}");
        std::process::exit(1)
    });
    let code = if dbs.is_empty() {
        0
    } else {
        // as the hermes user: opening a database makes its -wal and -shm as the opener's
        let status = Command::new("/command/s6-setuidgid").args(["hermes", &format!("{OPT}/bin/hermes-boot"), "quick-check"]).args(&dbs).status();
        match status {
            Ok(s) => s.code().unwrap_or(1),
            Err(e) => {
                println!("quick-check: {e}");
                1
            }
        }
    };
    ev!("restore.checked", { "putBack": put.len(), "databases": dbs.len(), "code": code, "ms": t.elapsed().as_millis() as u64 });
    std::process::exit(code);
}

/// `quick-check <db>…`: `PRAGMA quick_check` on each; `UNUSABLE` and why
/// for the first that fails.
fn quick_check(args: &[String]) -> ! {
    let dbs: Vec<PathBuf> = args.iter().map(PathBuf::from).collect();
    match held::quick_check(&dbs) {
        Ok(()) => std::process::exit(0),
        Err(e) => {
            println!("{e}");
            std::process::exit(UNUSABLE);
        }
    }
}

/// `sqlite-report <root>`: every SQLite file under `root` (by its first
/// bytes), each with its `quick_check`, one JSON line each (the Docker
/// rung's measure of what a save tore).
fn sqlite_report(args: &[String]) -> ! {
    let Some(root) = args.first() else { fail("sqlite-report <root>") };
    let files = held::sqlite_files(Path::new(root)).unwrap_or_else(|e| fail(&e.to_string()));
    for f in files {
        let checked = held::quick_check(std::slice::from_ref(&f));
        println!("{}", serde_json::json!({ "path": f.display().to_string(), "ok": checked.is_ok(), "why": checked.err().map(|e| e.to_string()) }));
    }
    std::process::exit(0);
}

/// What pauses while the platform holds the computer: no new round of the
/// repo sync, the skills install or the agents' reads starts (`paused`),
/// and the hold's answer waits, a bounded while, for those under way
/// (`busy`).
#[derive(Default)]
struct Quiet {
    paused: std::sync::atomic::AtomicBool,
    busy: std::sync::atomic::AtomicUsize,
}

impl Quiet {
    fn paused(&self) -> bool {
        self.paused.load(std::sync::atomic::Ordering::SeqCst)
    }
}

/// One round under way, counted while it lives.
struct Busy(std::sync::Arc<Quiet>);

impl Busy {
    fn new(quiet: &std::sync::Arc<Quiet>) -> Busy {
        quiet.busy.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Busy(quiet.clone())
    }
}

impl Drop for Busy {
    fn drop(&mut self) {
        let before = self.0.busy.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        assert!(before > 0, "a round is counted once");
    }
}

/// The hold is looked at this often.
const HOLD_POLL_MS: u64 = 100;
/// Rounds under way are waited for this long at most: none writes a
/// database, so the copy goes on after it.
const QUIET_WAIT_MS: u64 = 8_000;
/// A copy is given this long (the platform waits 20 s for the answer).
const COPY_MS_MAX: u64 = 15_000;
/// A copy that failed is tried again no sooner (within the same hold).
const COPY_RETRY_MS: u64 = 5_000;

/// The image's answer to the platform's hold (held.rs): once its bridge
/// claims nothing (`BRIDGE_HELD`) and the rounds under way are done, each
/// database under `/data` is copied by SQLite's online backup into the
/// staging, as the hermes user, and `held` names the live files as what
/// the save leaves out. A copy that fails answers nothing: the platform
/// then saves the guest whole, not held. Once the hold goes (the save is
/// done, or the sleep called off) the copies go too. Looked at every
/// `HOLD_POLL_MS`, for the boot's life.
async fn answer_holds(quiet: std::sync::Arc<Quiet>, ids: Option<(u32, u32)>) {
    let mut since: Option<Instant> = None;
    let mut failed_at: Option<Instant> = None;
    // bounded by the boot's life: one look per HOLD_POLL_MS
    loop {
        tokio::time::sleep(Duration::from_millis(HOLD_POLL_MS)).await;
        if !Path::new(held::HOLD).exists() {
            if since.take().is_some() {
                quiet.paused.store(false, std::sync::atomic::Ordering::SeqCst);
                failed_at = None;
                if let Err(e) = held::clear_staging(Path::new(held::STAGING)) {
                    ev!("held.staging_failed", { "error": e.to_string() });
                }
            }
            continue;
        }
        let first = *since.get_or_insert_with(Instant::now);
        quiet.paused.store(true, std::sync::atomic::Ordering::SeqCst);
        // answered: the platform clears the answer before it holds again
        if Path::new(held::HELD).exists() || failed_at.is_some_and(|t| t.elapsed() < Duration::from_millis(COPY_RETRY_MS)) {
            continue;
        }
        let rounds_done = quiet.busy.load(std::sync::atomic::Ordering::SeqCst) == 0 || first.elapsed() > Duration::from_millis(QUIET_WAIT_MS);
        if !(Path::new(held::BRIDGE_HELD).exists() && rounds_done) {
            continue;
        }
        let t = Instant::now();
        match copy_databases(ids).await {
            Ok((manifest, unnamed)) => match write_answer(&manifest) {
                Ok(()) => ev!("held", {
                    "copied": manifest.copies.len(),
                    "bytes": manifest.copies.iter().map(|c| c.bytes).sum::<u64>(),
                    "unnamed": unnamed,
                    "ms": t.elapsed().as_millis() as u64,
                    "sinceHoldMs": first.elapsed().as_millis() as u64
                }),
                Err(e) => {
                    failed_at = Some(Instant::now());
                    ev!("held.failed", { "error": e.to_string() });
                }
            },
            Err(why) => {
                failed_at = Some(Instant::now());
                ev!("held.refused", { "why": why, "ms": t.elapsed().as_millis() as u64 });
            }
        }
    }
}

/// Every database under `/data` the answer can name, copied into a
/// staging made anew, as the hermes user: the copies' manifest, and the
/// databases left hot (named by no pattern the platform takes).
async fn copy_databases(ids: Option<(u32, u32)>) -> Result<(held::Manifest, Vec<String>), String> {
    let (data, staging) = (Path::new(held::DATA), Path::new(held::STAGING));
    let found = held::databases(data, &[staging, Path::new(hermes::WORK)]).map_err(|e| e.to_string())?;
    let (dbs, unnamed): (Vec<PathBuf>, Vec<PathBuf>) = found.into_iter().partition(|db| held::nameable(data, db));
    held::make_staging(staging, ids).map_err(|e| e.to_string())?;
    let run = tokio::process::Command::new("/command/s6-setuidgid").args(["hermes", &format!("{OPT}/bin/hermes-boot"), "copy"]).arg(staging).args(&dbs).kill_on_drop(true).output();
    let out = tokio::time::timeout(Duration::from_millis(COPY_MS_MAX), run).await.map_err(|_| format!("no copy within {COPY_MS_MAX} ms"))?.map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stdout).trim().chars().take(300).collect());
    }
    let manifest = held::read_manifest(data, staging).map_err(|e| e.to_string())?.ok_or("the copy wrote no manifest")?;
    assert_eq!(manifest.copies.len(), dbs.len(), "a manifest names every copy");
    Ok((manifest, unnamed.iter().map(|p| p.display().to_string()).collect()))
}

/// `held`, whole: exactly what was copied, which the save leaves out.
fn write_answer(manifest: &held::Manifest) -> std::io::Result<()> {
    let tmp = format!("{}.tmp", held::HELD);
    std::fs::write(&tmp, held::answer(Path::new(held::DATA), &manifest.copies))?;
    std::fs::rename(&tmp, held::HELD)
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
    // What the last hold copied goes back before anything opens a database
    // (the gateway waits for go): after a restore the platform's check has
    // done it already; from a snapshot it is done here, so both wakes leave
    // the same /data.
    match held::put_back(Path::new(held::DATA), Path::new(held::STAGING)) {
        Ok(put) if put.is_empty() => {}
        Ok(put) => ev!("boot.put_back", { "databases": put.len() }),
        Err(e) => ev!("boot.put_back_failed", { "error": e.to_string() }),
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

/// The first agent's desktop, as a viewer that finds the screen down asks
/// for it (the bridge runs this at most once a minute while it stays
/// down): the agent `point_screen` named last.
fn screen_start() -> ! {
    let agent = std::fs::read_to_string(format!("{RUN}/{SCREEN_AGENT}")).map(|s| s.trim().to_string()).unwrap_or_default();
    if agent.is_empty() {
        fail("no agent's desktop to start: this computer runs no agent yet");
    }
    let home = home();
    let profile = wire::profile(&agent);
    ev!("screen.start", { "agent": agent, "profile": profile });
    let err = Command::new("/command/s6-setuidgid")
        .args(["hermes", "/opt/hermes/.venv/bin/hermes", "-p", &profile, "computer-use", "screen", "start"])
        .env("HOME", &home)
        .env("HERMES_HOME", &home)
        .exec();
    fail(&format!("exec hermes computer-use: {err}"));
}

/// At image build: what the boot reads instead of finding it each time, and
/// the platform skill (skills.rs), from the image's own `fragment` CLI.
fn build_info() -> ! {
    let plugins = hermes::lean_plugins(Path::new("/opt/hermes/plugins"));
    std::fs::write(format!("{OPT}/lean-plugins.txt"), plugins.join("\n")).unwrap_or_else(|e| fail(&e.to_string()));
    let rev = std::fs::read("/etc/hermes/image-provenance.json").ok().and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok()).and_then(|v| v["revision"].as_str().map(str::to_string)).unwrap_or_else(|| "unknown".into());
    std::fs::write(format!("{OPT}/image-rev"), &rev).unwrap_or_else(|e| fail(&e.to_string()));
    let browser = find_browser(Path::new("/opt/hermes/.playwright"));
    std::fs::write(format!("{OPT}/browser-path"), browser.as_ref().map(|p| p.display().to_string()).unwrap_or_default()).unwrap_or_else(|e| fail(&e.to_string()));
    let cli = Command::new(FRAGMENT_CLI).arg("skill").output().unwrap_or_else(|e| fail(&format!("{FRAGMENT_CLI} skill: {e}")));
    if !cli.status.success() {
        fail(&format!("{FRAGMENT_CLI} skill: {}", String::from_utf8_lossy(&cli.stderr)));
    }
    let skill = skills::platform_skill(&String::from_utf8_lossy(&cli.stdout)).unwrap_or_else(|e| fail(&e));
    skills::write_platform_skill(Path::new(skills::PLATFORM_DIR), &skill).unwrap_or_else(|e| fail(&format!("{}: {e}", skills::PLATFORM_DIR)));
    println!("{} plugins disabled; Hermes {rev}; browser {:?}; the platform skill, {} bytes", plugins.len(), browser, skill.len());
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

/// Writes `text` to `path` whole, Hermes' (a temporary file renamed over
/// it): a gateway that reads it as it changes (a profile written while
/// Hermes runs) never sees half.
fn write_whole(path: &Path, text: &str, ids: Option<(u32, u32)>) {
    let tmp = path.with_extension("fragment-tmp");
    if std::fs::write(&tmp, text).is_ok() {
        chown(&tmp, ids);
        let _ = std::fs::rename(&tmp, path);
    }
}

/// An agent's credentials (docs/computers.md, "Connections and operator
/// keys"): its `.env`, which Hermes reads again at every turn, and its
/// terminal's credentials file, each written whole. Placeholders only: the
/// computer's swap fills them on the way to their providers.
fn write_credentials(a: &Agent, home: &Path, ids: Option<(u32, u32)>) {
    let dir = hermes::profile_dir(home, &a.fragment);
    write_whole(&dir.join(hermes::CREDENTIALS_FILE), &hermes::credentials_sh(a), ids);
    write_whole(&dir.join(".env"), &hermes::profile_env(a), ids);
}

/// An agent's work directory (`/data/work/<profile>`, the hermes user's:
/// its terminal's cwd) and its desktop browser's profile in it, linked from
/// where Hermes keeps one in the profile, so what its tools write is the
/// work the computer saves on its own (step 2 of docs/durable-computers.md).
/// A browser profile that is a directory already stays where it is:
/// nothing is moved.
fn work_dirs(profile: &Path, a: &Agent, ids: Option<(u32, u32)>) {
    let work = hermes::work_dir(&a.fragment);
    let browser = work.join("browser-profile");
    if let Err(e) = std::fs::create_dir_all(&browser) {
        ev!("profile.work_failed", { "agent": a.fragment, "error": e.to_string() });
        return;
    }
    chown(&work, ids);
    chown(&browser, ids);
    let link = profile.join(hermes::BROWSER_PROFILE);
    if let Some(parent) = link.parent() {
        let _ = std::fs::create_dir_all(parent);
        chown(parent, ids);
    }
    match std::fs::symlink_metadata(&link) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => match std::os::unix::fs::symlink(&browser, &link) {
            Ok(()) => chown(&link, ids),
            Err(e) => ev!("profile.work_failed", { "agent": a.fragment, "error": e.to_string() }),
        },
        Ok(m) if m.file_type().is_symlink() => {}
        Ok(_) => ev!("profile.browser_kept", { "agent": a.fragment, "why": "its browser profile is a directory of the profile's already" }),
        Err(e) => ev!("profile.work_failed", { "agent": a.fragment, "error": e.to_string() }),
    }
}

/// One agent's profile: its directories, its config, its repo pulled. At a
/// boot for each agent, and while awake for each one assigned since
/// (`follow_agents`); Hermes reads it at the agent's first turn either way.
/// The config is written before `.env`, so the directory is a profile to
/// Hermes (an identity file) only once its config is whole.
async fn write_profile(api: &Api, a: &Agent, home: &Path, ids: Option<(u32, u32)>, model: &str, credential_env: &[String]) {
    let t = Instant::now();
    let high_on = env("FRAGMENT_HIGH_TIER").as_deref() == Some("on");
    let own = move |p: &Path| chown(p, ids);
    let dir = hermes::profile_dir(home, &a.fragment);
    let fresh = !dir.exists();
    for sub in hermes::PROFILE_DIRS {
        let _ = std::fs::create_dir_all(dir.join(sub));
        chown(&dir.join(sub), ids);
    }
    chown(&dir, ids);
    work_dirs(&dir, a, ids);
    // Which agent this profile is: what retiring it later reads.
    let _ = std::fs::write(dir.join(".fragment-agent"), &a.fragment);
    let agent_json = api.file(&a.fragment, &a.fragment, "agent.json", 64 * 1024).await.ok();
    let tier = hermes::Tier::of(agent_json.as_deref(), high_on);
    write_whole(&dir.join("config.yaml"), &hermes::profile_config(a, tier, model, credential_env, &dir.join(hermes::CREDENTIALS_FILE)), ids);
    // who the agent is and its credentials, for Hermes and its terminal (the
    // fragment CLI, the skills' helpers, any SDK): written whole each time,
    // after the config
    write_credentials(a, home, ids);
    match sync::round(api, a, &dir, &PathBuf::from("/data/hermes-sync"), &own).await {
        Ok(d) => ev!("profile.written", { "agent": a.fragment, "profile": wire::profile(&a.fragment), "fresh": fresh, "tier": tier.name(), "pulled": d.pulled, "pushed": d.pushed, "conflicts": d.conflicts, "ms": t.elapsed().as_millis() as u64 }),
        Err(e) => ev!("profile.written", { "agent": a.fragment, "fresh": fresh, "syncError": e.to_string(), "ms": t.elapsed().as_millis() as u64 }),
    }
}

/// Each agent's profile at a boot, and the profiles of agents that left
/// this computer retired: moved aside, never deleted. Only a boot retires
/// one: no Hermes runs yet that could be winding down a turn in it.
async fn profiles(api: &Api, agents: &[Agent], home: &Path, ids: Option<(u32, u32)>, model: &str, credential_env: &[String]) {
    let profiles_root = home.join("profiles");
    let _ = std::fs::create_dir_all(&profiles_root);
    chown(&profiles_root, ids);
    for a in agents {
        write_profile(api, a, home, ids, model, credential_env).await;
    }
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

/// The bridge, which runs the agents the ready file names (its `ready.rs`)
/// and shows the screen's socket, whichever agents come and go: nothing
/// it is started with changes with them.
fn spawn_bridge() -> Option<Child> {
    let mut c = Command::new(format!("{OPT}/bin/fragment-bridge"));
    c.arg("run")
        .env("BRIDGE_RUNTIME", "relay")
        .env("BRIDGE_RELAY_LISTEN", RELAY_LISTEN)
        .env("BRIDGE_RELAY_SECRET_FILE", format!("{RUN}/relay.secret"))
        .env("GATEWAY_RELAY_ID", GATEWAY_ID)
        .env("BRIDGE_STATE_DIR", "/data/bridge")
        // its answer to a hold is this image's to wait for: the platform's
        // `held` comes once Hermes' databases are copied too (held.rs)
        .env("BRIDGE_HELD", held::BRIDGE_HELD)
        .env("BRIDGE_PROMPT_TTL_MS", (hermes::APPROVAL_TIMEOUT_S * 1000).to_string())
        .env("BRIDGE_AGENTS_FILE", format!("{RUN}/{READY_FILE}"))
        .env("BRIDGE_SCREEN_LISTEN", "0.0.0.0:6080")
        .env("BRIDGE_SCREEN_DIR", format!("{OPT}/screen"))
        // The screen is the first agent's desktop, started at its first viewer.
        .env("BRIDGE_SCREEN_RFB", format!("unix:{RUN}/{SCREEN_SOCKET}"))
        .env("BRIDGE_SCREEN_START", format!("{OPT}/bin/hermes-boot screen-start"))
        // The restore already happened: the bridge reads /data at once.
        .env_remove("RESTORE_PENDING");
    match c.spawn() {
        Ok(child) => Some(child),
        Err(e) => {
            ev!("boot.bridge_failed", { "error": e.to_string() });
            None
        }
    }
}

/// The bridge's ready file: every agent whose profile is written.
fn write_ready(agents: &[Agent]) {
    write_whole(Path::new(&format!("{RUN}/{READY_FILE}")), &agents::ready_file(agents), None);
}

/// Points the screen at `first`'s desktop: the screen's socket is a link to
/// its profile's display socket, and `screen-start` starts its display. A
/// computer with no agent shows no desktop.
fn point_screen(first: Option<&Agent>, home: &Path) {
    let link = PathBuf::from(format!("{RUN}/{SCREEN_SOCKET}"));
    let agent_file = PathBuf::from(format!("{RUN}/{SCREEN_AGENT}"));
    match first {
        Some(a) => {
            let target = hermes::profile_dir(home, &a.fragment).join("bot-desktop/rfb.sock");
            let tmp = link.with_extension("sock-tmp");
            let _ = std::fs::remove_file(&tmp);
            let linked = std::os::unix::fs::symlink(&target, &tmp).and_then(|()| std::fs::rename(&tmp, &link));
            write_whole(&agent_file, &a.fragment, None);
            ev!("screen.agent", { "agent": a.fragment, "ok": linked.is_ok() });
        }
        None => {
            let _ = std::fs::remove_file(&link);
            let _ = std::fs::remove_file(&agent_file);
            ev!("screen.agent", { "agent": null });
        }
    }
}

/// The control socket of the gateway serving `home`, if it has one.
fn control_socket(home: &Path) -> Option<PathBuf> {
    let direct = home.join(hermes::CONTROL_SOCKET);
    if direct.exists() {
        return Some(direct);
    }
    let pointed = std::fs::read_to_string(home.join(hermes::CONTROL_POINTER)).ok().map(|p| PathBuf::from(p.trim()))?;
    pointed.exists().then_some(pointed)
}

/// Asks the gateway one control verb (Hermes' `gateway/control_socket.py`):
/// its result, within `CONTROL_WAIT_MS`.
async fn ask_gateway(home: &Path, verb: &str) -> Result<serde_json::Value, String> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let path = control_socket(home).ok_or_else(|| "the gateway has no control socket yet".to_string())?;
    let ask = async {
        let mut s = tokio::net::UnixStream::connect(&path).await.map_err(|e| format!("{}: {e}", path.display()))?;
        s.write_all(hermes::control_request(verb).as_bytes()).await.map_err(|e| e.to_string())?;
        let mut answer = Vec::new();
        let mut chunk = [0u8; 8192];
        // bounded by CONTROL_ANSWER_MAX_BYTES (and the timeout around it)
        while !answer.contains(&b'\n') && answer.len() <= hermes::CONTROL_ANSWER_MAX_BYTES {
            let n = s.read(&mut chunk).await.map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            answer.extend_from_slice(&chunk[..n]);
        }
        hermes::control_answer(answer.split(|b| *b == b'\n').next().unwrap_or_default())
    };
    tokio::time::timeout(Duration::from_millis(CONTROL_WAIT_MS), ask).await.map_err(|_| format!("no answer to {verb} within {CONTROL_WAIT_MS} ms"))?
}

/// A change of the computer's agents while it is awake (agents.rs), carried
/// out with nothing restarted: each new agent's profile is written, the
/// gateway is asked to serve it (`rescan-profiles`, as Hermes' own `profile
/// create` asks), and then the ready file names it, so the bridge hands it
/// no turn before its profile is whole. A removed agent leaves the ready
/// file, so the bridge stops at once; its profile waits for the next boot.
async fn follow_agents(api: &Api, change: &agents::Change, now: &[Agent], home: &Path, ids: Option<(u32, u32)>, model: &str, credential_env: &[String]) {
    let t = Instant::now();
    let names = |l: &[Agent]| l.iter().map(|a| a.fragment.clone()).collect::<Vec<_>>();
    ev!("agents.changed", { "added": names(&change.added), "removed": names(&change.removed), "screen": change.screen, "credentials": names(&change.credentials), "agents": now.len() });
    for a in &change.added {
        write_profile(api, a, home, ids, model, credential_env).await;
    }
    // an agent's credentials changed: its next turn reads them (its `.env`),
    // and its terminal's next session (its credentials file)
    for a in &change.credentials {
        write_credentials(a, home, ids);
        ev!("profile.credentials", { "agent": a.fragment, "providers": a.credentials.iter().map(|c| c.provider.clone()).collect::<Vec<_>>() });
    }
    if !change.added.is_empty() {
        let asked = Instant::now();
        match ask_gateway(home, "rescan-profiles").await {
            Ok(answer) => ev!("agents.served", { "added": answer["added"], "pending": answer["pending"], "unserved": agents::unserved(&change.added, &answer), "ms": asked.elapsed().as_millis() as u64 }),
            // every relayed turn resolves its profile as it arrives, and Hermes
            // rescans every 30 s: the agent is run either way
            Err(e) => ev!("agents.serve_unasked", { "error": e, "ms": asked.elapsed().as_millis() as u64 }),
        }
    }
    if change.screen {
        point_screen(now.first(), home);
    }
    write_ready(now);
    ev!("agents.ready", { "agents": now.len(), "ms": t.elapsed().as_millis() as u64 });
}

/// The managed skills' directory (skills.rs), made if it is missing: the
/// boot's, readable by all, writable by none of the agents.
fn managed_dir() {
    use std::os::unix::fs::PermissionsExt;
    let dir = skills::managed_dir();
    if let Err(e) = std::fs::create_dir_all(&dir).and_then(|()| std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755))) {
        ev!("skills.dir_failed", { "dir": dir.display().to_string(), "error": e.to_string() });
    }
}

/// What the last install of the managed skills found: the owner's skills
/// fragment, none, or no answer (skills.rs, `next_install_ms`).
type SkillsFound = std::sync::Arc<std::sync::Mutex<Option<bool>>>;

/// One install of the managed skills, in the background (skills.rs), as the
/// computer's first agent of its owner: none while it has no such agent,
/// or while the platform holds the computer (`quiet`).
fn spawn_skills(api: &Api, agents: &[Agent], owner: &str, found: &SkillsFound, quiet: &std::sync::Arc<Quiet>) -> Option<tokio::task::JoinHandle<()>> {
    if quiet.paused() {
        return None;
    }
    let reader = skills::reader(agents, owner)?.clone();
    let (api, owner, found) = (api.clone(), owner.to_string(), found.clone());
    let busy = Busy::new(quiet);
    Some(tokio::spawn(async move {
        let _busy = busy;
        let t = Instant::now();
        let done = skills::install(&api, &reader, &owner, &skills::managed_dir(), Path::new(skills::MANIFEST)).await;
        *found.lock().expect("the skills' state is never poisoned") = done.as_ref().ok().map(|d| d.fragment.is_some());
        match done {
            Ok(d) => ev!("skills.installed", { "fragment": d.fragment, "fetched": d.fetched, "removed": d.removed, "refused": d.refused, "skills": d.skills, "ms": t.elapsed().as_millis() as u64 }),
            Err(e) => ev!("skills.failed", { "error": e.to_string(), "ms": t.elapsed().as_millis() as u64 }),
        }
    }))
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
    // What the boot runs: the agents read at its start, then as they change.
    let mut agents = computer.agents.clone();
    let mut credential_env = computer.credential_env.clone();

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
    // The managed skills' directory exists before any profile names it:
    // Hermes skips an external dir it does not find, until the profile's
    // config changes. What it holds is installed off the boot's path.
    managed_dir();
    profiles(&api, &agents, &home, ids, &model, &credential_env).await;
    ev!("boot.configured", { "agents": agents.len(), "ms": t0.elapsed().as_millis() as u64 });

    end_previous_life(&agents, &home);
    point_screen(agents.first(), &home);
    write_ready(&agents);
    let mut bridge = spawn_bridge();
    let Some(gateway) = start_gateway(&home) else { fail("no gateway") };
    // its answer to the platform's holds, for its whole life
    let quiet = std::sync::Arc::new(Quiet::default());
    tokio::spawn(answer_holds(quiet.clone(), ids));
    ev!("boot.ready", { "ms": t0.elapsed().as_millis() as u64 });

    let mut restarts = 0u32;
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).expect("a SIGTERM handler");
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut last_sync = Instant::now();
    let mut last_agents = Instant::now();
    let mut agents_unread = false;
    // HERMES_BOOT_SYNC_MS: the operator's (a test's) cadence, at least 1 s.
    let sync_every = env("HERMES_BOOT_SYNC_MS").and_then(|v| v.parse::<u64>().ok()).map_or(SYNC_EVERY_MS, |v| v.max(1_000));
    // the managed skills: installed now, off the boot's path, then again
    // every SKILLS_EVERY_MS (HERMES_BOOT_SKILLS_MS: a test's, at least 1 s),
    // or every minute while the owner has no skills fragment; one install at
    // a time, and none until the computer has an agent
    let skills_every = env("HERMES_BOOT_SKILLS_MS").and_then(|v| v.parse::<u64>().ok()).map_or(SKILLS_EVERY_MS, |v| v.max(1_000));
    let owner = computer.owner.clone();
    let skills_found = SkillsFound::default();
    let mut skills_task = spawn_skills(&api, &agents, &owner, &skills_found, &quiet);
    let mut last_skills = Instant::now();
    let own = move |p: &Path| chown(p, ids);
    // bounded by the computer's life: one tick or one signal per pass
    loop {
        // SIGTERM first: it means stop now (docs/computers.md)
        tokio::select! {
            biased;
            _ = term.recv() => break,
            _ = tick.tick() => {}
        }
        if !alive(gateway) {
            ev!("boot.gateway_exited");
            stop(gateway, bridge.as_mut()).await;
            std::process::exit(1);
        }
        if let Some(b) = bridge.as_mut() {
            if let Ok(Some(status)) = b.try_wait() {
                restarts += 1;
                ev!("boot.bridge_exited", { "status": status.code(), "restarts": restarts });
                if restarts > BRIDGE_RESTARTS_MAX {
                    stop(gateway, None).await;
                    fail("the bridge keeps exiting");
                }
                bridge = spawn_bridge();
            }
        }
        // the managed skills again: the first as soon as there is an agent to
        // read them as, then on their cadence (sooner while the owner has no
        // skills fragment: the shell makes one), never two at once
        let skills_due = match &skills_task {
            None => !agents.is_empty(),
            Some(t) => {
                let found = *skills_found.lock().expect("the skills' state is never poisoned");
                t.is_finished() && last_skills.elapsed() >= Duration::from_millis(skills::next_install_ms(found, skills_every))
            }
        };
        if skills_due {
            skills_task = spawn_skills(&api, &agents, &owner, &skills_found, &quiet);
            last_skills = Instant::now();
        }
        // What waits on the platform (each call up to its 15 s), cut short by
        // SIGTERM: a slow platform never holds the stop. Cut, it is done
        // again at the next boot (a profile written whole again, a sync
        // whose commits are keyed by their content).
        let slow = async {
            // held, nothing new starts: the next tick after the hold goes does it
            if quiet.paused() {
                return;
            }
            let _busy = Busy::new(&quiet);
            // The computer's agents may change while it runs (docs/computers.md).
            if last_agents.elapsed() >= Duration::from_millis(AGENTS_EVERY_MS) {
                last_agents = Instant::now();
                match api.computer().await {
                    Ok(c) => {
                        agents_unread = false;
                        // the deployment's credential names changed (a release):
                        // every profile's config names them, which Hermes reads
                        // at its next start
                        if c.credential_env != credential_env {
                            credential_env = c.credential_env.clone();
                            for a in agents.iter().filter(|a| c.agents.iter().any(|n| n.fragment == a.fragment)) {
                                write_profile(&api, a, &home, ids, &model, &credential_env).await;
                            }
                        }
                        let change = agents::diff(&agents, &c.agents);
                        if !change.is_empty() {
                            follow_agents(&api, &change, &c.agents, &home, ids, &model, &credential_env).await;
                        }
                        agents = c.agents;
                    }
                    // once per outage: the next read that answers ends it
                    Err(e) if !agents_unread => {
                        agents_unread = true;
                        ev!("agents.unread", { "error": e.to_string() });
                    }
                    Err(_) => {}
                }
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
        };
        let stopped = tokio::select! {
            biased;
            _ = term.recv() => true,
            () = slow => false,
        };
        if stopped {
            break;
        }
    }
    ev!("boot.signal");
    stop(gateway, bridge.as_mut()).await;
    std::process::exit(0);
}

/// SIGTERM to the gateway and the bridge, then wait for them, at most
/// `STOP_MS_MAX`.
async fn stop(gateway: u32, mut bridge: Option<&mut Child>) {
    let t = Instant::now();
    signal(gateway, libc::SIGTERM);
    if let Some(b) = bridge.as_deref_mut() {
        signal(b.id(), libc::SIGTERM);
    }
    // bounded by STOP_MS_MAX
    while t.elapsed() < Duration::from_millis(STOP_MS_MAX) {
        let waiting = alive(gateway) || bridge.as_deref_mut().is_some_and(|b| matches!(b.try_wait(), Ok(None)));
        if !waiting {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    ev!("boot.stopped", { "ms": t.elapsed().as_millis() as u64, "gatewayLeft": alive(gateway) });
}
