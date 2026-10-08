//! Each agent's desktop (docs/computers.md, "Our images": goose): an X
//! display of its own (TigerVNC's Xvnc, its RFB on a Unix socket only), a
//! light window manager, and a Chromium on it that the agent's browser tools
//! drive over CDP, so its owner watches it browse on its screen.
//!
//! - **Where.** Under the screens directory (`BRIDGE_SCREENS_DIR`, our
//!   image's `/run/desktop`: never `/data`, so nothing of a desktop is in a
//!   save), in the agent's own directory, named as the bridge's screen names
//!   it (`fragment_bridge::screens::agent_dir`): its display's socket
//!   `rfb.sock`, Take over's `lease.json`, the `activity` the screen touches
//!   while a person watches, the browser's profile, its log.
//! - **Its display number** is the agent's for the computer's life, the
//!   lowest free from `DISPLAY_FIRST`, given out under the directory's lock
//!   (`allocate`); its browser's CDP port is `CDP_PORT_BASE` plus it.
//! - **Started at its first use**, never at boot: by the screen's first
//!   viewer (the bridge's `BRIDGE_SCREEN_START`), the agent's first browser
//!   or screen tool call, or `fragment-desktop start`. A start runs a
//!   supervisor of its own (`run`), detached, which starts the display, the
//!   window manager and the browser (again, when the browser closes), and
//!   stops them all when the desktop has been unused for `idle_stop_ms`: no
//!   tool call, no one watching, no take over. A person holding the screen
//!   keeps it up.

use std::collections::BTreeSet;
use std::io::Write;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use fragment_bridge::ev;
use fragment_bridge::lease::{Holder, LeaseFile};
use fragment_bridge::screen::agent_name_ok;
use fragment_bridge::screens;

/// The screens directory unless `FRAGMENT_DESKTOPS` names another (the
/// bridge's `BRIDGE_SCREENS_DIR` is the same).
pub const ROOT_DEFAULT: &str = "/run/desktop";
/// The first display number an agent's desktop takes (`:10`), leaving the
/// low ones to whatever an agent starts itself.
pub const DISPLAY_FIRST: u32 = 10;
/// One display per agent, at most as many as a computer runs agents.
pub const DISPLAYS_MAX: u32 = fragment_bridge::limits::AGENTS_MAX as u32;
/// A desktop's browser answers CDP on loopback at this plus its display.
pub const CDP_PORT_BASE: u32 = 9200;
/// The desktop's size: what the screen's page scales to its window.
pub const WIDTH: u32 = 1280;
pub const HEIGHT: u32 = 800;
/// A start waits this long for the display and the browser to answer.
pub const START_WAIT_MS: u64 = 20_000;
/// A desktop unused this long is stopped (`FRAGMENT_DESKTOP_IDLE_MS` sets
/// another, for tests). The screen touches `activity` every 10 s while
/// someone watches, well inside it.
pub const IDLE_STOP_MS: u64 = 10 * 60 * 1000;
/// The supervisor looks at its desktop this often.
pub const LOOK_EVERY_MS: u64 = 1_000;
/// A browser that closed is started again, at most this often.
pub const BROWSER_AGAIN_MS: u64 = 3_000;
/// A process asked to stop is killed after this.
pub const STOP_GRACE_MS: u64 = 3_000;
/// A desktop's log is cut back to nothing at a start past this.
pub const LOG_MAX_BYTES: u64 = 4 * 1024 * 1024;
/// A file the desktop reads of its own (the display number, a pid) is at
/// most this.
const SMALL_FILE_MAX_BYTES: u64 = 64;

const _: () = assert!(CDP_PORT_BASE + DISPLAY_FIRST + DISPLAYS_MAX < 65_536 && DISPLAY_FIRST + DISPLAYS_MAX < 6_000 - 5_900);
const _: () = assert!(STOP_GRACE_MS < START_WAIT_MS && LOOK_EVERY_MS * 10 < IDLE_STOP_MS);

/// The screens directory.
pub fn root() -> PathBuf {
    std::env::var("FRAGMENT_DESKTOPS").ok().filter(|v| !v.trim().is_empty()).map(PathBuf::from).unwrap_or_else(|| PathBuf::from(ROOT_DEFAULT))
}

/// How long an unused desktop stays up.
pub fn idle_stop_ms() -> u64 {
    std::env::var("FRAGMENT_DESKTOP_IDLE_MS").ok().and_then(|v| v.parse().ok()).filter(|ms| *ms >= 1_000).unwrap_or(IDLE_STOP_MS)
}

/// One agent's desktop: where its files are.
#[derive(Debug, Clone)]
pub struct Desk {
    pub root: PathBuf,
    pub agent: String,
    pub dir: PathBuf,
}

impl Desk {
    /// `agent`'s desktop under `root`; refused for a name that is no agent
    /// fragment's, or a root its sockets would not fit under.
    pub fn new(root: &Path, agent: &str) -> Result<Desk, String> {
        if !agent_name_ok(agent) {
            return Err(format!("{agent:?} is no agent fragment's name (<label>.<username>)"));
        }
        if !screens::dir_ok(root) {
            return Err(format!("{} is no screens directory: absolute, at most {} bytes", root.display(), screens::DIR_PATH_MAX_BYTES));
        }
        Ok(Desk { root: root.to_path_buf(), agent: agent.to_string(), dir: screens::agent_dir(root, agent) })
    }

    pub fn rfb(&self) -> PathBuf {
        match screens::in_dir(&self.root, &self.agent).rfb {
            fragment_bridge::screen::Target::Unix(p) => p,
            fragment_bridge::screen::Target::Tcp(_) => unreachable!("a screens directory's displays are Unix sockets"),
        }
    }

    pub fn lease(&self) -> LeaseFile {
        LeaseFile::new(screens::in_dir(&self.root, &self.agent).lease.expect("a screens directory names a lease"))
    }

    pub fn activity(&self) -> PathBuf {
        screens::in_dir(&self.root, &self.agent).activity.expect("a screens directory names an activity file")
    }

    fn display_file(&self) -> PathBuf {
        self.dir.join("display")
    }

    fn pid_file(&self) -> PathBuf {
        self.dir.join("run.pid")
    }

    pub fn log_file(&self) -> PathBuf {
        self.dir.join("desktop.log")
    }

    pub fn profile(&self) -> PathBuf {
        self.dir.join("chromium")
    }

    /// Whether a person holds its screen (Take over), as its lease says: an
    /// unreadable lease is a person's.
    pub fn held(&self) -> bool {
        matches!(self.lease().read().holder, Holder::Human(_))
    }

    /// Marks it used now: its activity file's time (made when missing).
    pub fn touch(&self) {
        let now = SystemTime::now();
        let touched = std::fs::OpenOptions::new().create(true).append(true).open(self.activity()).and_then(|f| f.set_times(std::fs::FileTimes::new().set_accessed(now).set_modified(now)));
        if let Err(e) = touched {
            ev!("desktop.touch_failed", { "agent": self.agent, "error": e.to_string() });
        }
    }

    /// How long since it was last used, by its activity file; none without one.
    pub fn unused_for(&self) -> Option<Duration> {
        let at = std::fs::metadata(self.activity()).and_then(|m| m.modified()).ok()?;
        Some(SystemTime::now().duration_since(at).unwrap_or_default())
    }
}

fn read_small(path: &Path) -> Option<String> {
    use std::io::Read;
    let mut s = String::new();
    std::fs::File::open(path).ok()?.take(SMALL_FILE_MAX_BYTES).read_to_string(&mut s).ok()?;
    Some(s.trim().to_string())
}

/// Writes `text` at `path` whole (a temporary file renamed over it).
fn write_whole(path: &Path, text: &str) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

/// A display number a file holds, when it is one a desktop takes.
fn display_number(text: &str) -> Option<u32> {
    text.parse().ok().filter(|n| (DISPLAY_FIRST..DISPLAY_FIRST + DISPLAYS_MAX).contains(n))
}

/// The lowest display number none of `used` holds.
pub fn lowest_free(used: &BTreeSet<u32>) -> Option<u32> {
    (DISPLAY_FIRST..DISPLAY_FIRST + DISPLAYS_MAX).find(|n| !used.contains(n))
}

/// A desktop's browser's CDP port.
pub fn cdp_port(display: u32) -> u16 {
    u16::try_from(CDP_PORT_BASE + display).expect("a CDP port fits (asserted above)")
}

/// The directory's lock, held while numbers are given out and supervisors
/// started.
fn lock(root: &Path) -> Result<std::fs::File, String> {
    std::fs::create_dir_all(root).map_err(|e| format!("{}: {e}", root.display()))?;
    let path = root.join(".lock");
    let f = std::fs::OpenOptions::new().create(true).append(true).open(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    f.lock().map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(f)
}

/// The agent's display number: its own, or the lowest free one given to it
/// now. When every number is taken, one of a desktop not running is taken
/// back (its agent gets another at its next start). Under the lock.
fn allocate_locked(desk: &Desk) -> Result<u32, String> {
    if let Some(n) = read_small(&desk.display_file()).as_deref().and_then(display_number) {
        return Ok(n);
    }
    std::fs::create_dir_all(&desk.dir).map_err(|e| format!("{}: {e}", desk.dir.display()))?;
    let mut held: Vec<(u32, PathBuf)> = Vec::new();
    for entry in std::fs::read_dir(&desk.root).map_err(|e| format!("{}: {e}", desk.root.display()))?.flatten() {
        let file = entry.path().join("display");
        if let Some(n) = read_small(&file).as_deref().and_then(display_number) {
            held.push((n, entry.path()));
        }
    }
    let used: BTreeSet<u32> = held.iter().map(|(n, _)| *n).collect();
    let n = match lowest_free(&used) {
        Some(n) => n,
        None => {
            let idle = held.iter().filter(|(_, dir)| running_in(dir).is_none()).min_by_key(|(n, _)| *n).ok_or("every display is a running desktop's")?;
            let _ = std::fs::remove_file(idle.1.join("display"));
            ev!("desktop.display_taken_back", { "display": idle.0 });
            idle.0
        }
    };
    write_whole(&desk.dir.join("agent"), &desk.agent).map_err(|e| e.to_string())?;
    write_whole(&desk.display_file(), &n.to_string()).map_err(|e| e.to_string())?;
    Ok(n)
}

/// The agent's display number (given out at its first ask).
pub fn allocate(desk: &Desk) -> Result<u32, String> {
    let _lock = lock(&desk.root)?;
    allocate_locked(desk)
}

/// The pid of the supervisor of the desktop in `dir`, when one runs.
fn running_in(dir: &Path) -> Option<i32> {
    let pid: i32 = read_small(&dir.join("run.pid"))?.parse().ok()?;
    // SAFETY: kill with signal 0 only asks whether the process exists.
    let alive = pid > 1 && unsafe { libc::kill(pid, 0) } == 0;
    // a pid reused by another process is no supervisor
    let ours = std::fs::read(format!("/proc/{pid}/cmdline")).is_ok_and(|c| c.split(|b| *b == 0).nth(1) == Some(b"run".as_slice()) && String::from_utf8_lossy(&c).contains("fragment-desktop"));
    (alive && ours).then_some(pid)
}

/// The pid of the desktop's supervisor, when one runs.
pub fn running(desk: &Desk) -> Option<i32> {
    running_in(&desk.dir)
}

/// Whether the desktop answers: its supervisor runs, its display's socket
/// is there, and its browser's CDP answers. The display's socket is never
/// opened only to look: Xvnc counts a connection that leaves before its
/// handshake as a failed one, and refuses every viewer past a few.
pub fn up(desk: &Desk, display: u32) -> bool {
    running(desk).is_some() && desk.rfb().exists() && std::net::TcpStream::connect_timeout(&([127, 0, 0, 1], cdp_port(display)).into(), Duration::from_millis(500)).is_ok()
}

/// Starts the desktop when it is not running, and waits until it answers:
/// its display number.
pub fn start(desk: &Desk) -> Result<u32, String> {
    let t = Instant::now();
    let n = {
        let _lock = lock(&desk.root)?;
        let n = allocate_locked(desk)?;
        if running(desk).is_none() {
            spawn_supervisor(desk)?;
        }
        n
    };
    // bounded by START_WAIT_MS
    while t.elapsed() < Duration::from_millis(START_WAIT_MS) {
        if up(desk, n) {
            return Ok(n);
        }
        if running(desk).is_none() && t.elapsed() > Duration::from_millis(1_000) {
            return Err(format!("{}'s desktop stopped as it started: see {}", desk.agent, desk.log_file().display()));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err(format!("{}'s desktop did not answer within {} s: see {}", desk.agent, START_WAIT_MS / 1000, desk.log_file().display()))
}

/// Runs `fragment-desktop run <agent>` detached (its own process group, so
/// it outlives whoever started it), its output to the desktop's log.
fn spawn_supervisor(desk: &Desk) -> Result<(), String> {
    let log = desk.log_file();
    if std::fs::metadata(&log).is_ok_and(|m| m.len() > LOG_MAX_BYTES) {
        let _ = std::fs::remove_file(&log);
    }
    let out = std::fs::OpenOptions::new().create(true).append(true).open(&log).map_err(|e| format!("{}: {e}", log.display()))?;
    let err = out.try_clone().map_err(|e| e.to_string())?;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let child = std::process::Command::new(exe)
        .args(["run", &desk.agent])
        .env("FRAGMENT_DESKTOPS", &desk.root)
        .stdin(std::process::Stdio::null())
        .stdout(out)
        .stderr(err)
        .process_group(0)
        .spawn()
        .map_err(|e| format!("the desktop's supervisor: {e}"))?;
    write_whole(&desk.pid_file(), &child.id().to_string()).map_err(|e| e.to_string())?;
    ev!("desktop.starting", { "agent": desk.agent, "pid": child.id() });
    // reaped by PID 1 (tini) once it exits: it outlives this process
    std::mem::forget(child);
    Ok(())
}

/// Stops the desktop, if it runs: its supervisor stops the rest.
pub fn stop(desk: &Desk) -> bool {
    let Some(pid) = running(desk) else { return false };
    // SAFETY: a signal to a process this desktop started.
    unsafe { libc::kill(pid, libc::SIGTERM) };
    let t = Instant::now();
    // bounded by STOP_GRACE_MS and a second
    while running(desk).is_some() && t.elapsed() < Duration::from_millis(STOP_GRACE_MS + 1_000) {
        std::thread::sleep(Duration::from_millis(50));
    }
    true
}

// ---- the supervisor ----

/// Xvnc's arguments for display `n`: RFB on the desktop's Unix socket only
/// (no TCP, no password: the bridge's screen is the only way in), shared,
/// its desktop named for the agent.
pub fn xvnc_args(desk: &Desk, n: u32) -> Vec<String> {
    let mut a: Vec<String> = vec![format!(":{n}")];
    for (k, v) in [
        ("-rfbunixpath", desk.rfb().display().to_string()),
        ("-rfbunixmode", "0600".into()),
        ("-rfbport", "-1".into()),
        ("-SecurityTypes", "None".into()),
        ("-desktop", desk.agent.clone()),
        ("-geometry", format!("{WIDTH}x{HEIGHT}")),
        ("-depth", "24".into()),
        ("-MaxCutText", fragment_bridge::screen::CUT_TEXT_MAX.to_string()),
        ("-nolisten", "tcp".into()),
    ] {
        a.push(k.into());
        a.push(v);
    }
    // its viewers are the screen's and the agent's own screenshots, on a
    // socket only this computer reaches: none is ever blacklisted
    a.extend(["-AlwaysShared", "-NeverShared=0", "-AcceptSetDesktopSize=0", "-BlacklistThreshold=1000000"].map(String::from));
    a
}

/// The browser's arguments: its profile in the desktop's directory, CDP on
/// loopback for the agent's browser tools, as root in a container (no
/// sandbox, no GPU, /dev/shm spared), nothing asked at its first run.
pub fn browser_args(desk: &Desk, n: u32) -> Vec<String> {
    vec![
        format!("--user-data-dir={}", desk.profile().display()),
        format!("--remote-debugging-port={}", cdp_port(n)),
        "--remote-debugging-address=127.0.0.1".into(),
        "--no-sandbox".into(),
        "--test-type".into(),
        "--disable-dev-shm-usage".into(),
        "--disable-gpu".into(),
        "--no-first-run".into(),
        "--no-default-browser-check".into(),
        "--password-store=basic".into(),
        "--disable-features=Translate,MediaRouter,OptimizationHints,AutofillServerCommunication".into(),
        "--disable-sync".into(),
        "--window-position=0,0".into(),
        format!("--window-size={WIDTH},{HEIGHT}"),
        "--start-maximized".into(),
        "about:blank".into(),
    ]
}

/// The browser's first preferences: downloads to the work (where the agent
/// finds them, and they are saved), asked nothing.
pub fn browser_preferences() -> serde_json::Value {
    serde_json::json!({
        "download": { "default_directory": "/data/work/downloads", "prompt_for_download": false, "directory_upgrade": true },
        "browser": { "check_default_browser": false, "has_seen_welcome_page": true },
        "credentials_enable_service": false,
        "profile": { "password_manager_enabled": false, "default_content_setting_values": { "notifications": 2, "geolocation": 2 } },
        "translate": { "enabled": false },
    })
}

/// Whether an idle desktop stops now: unused past `idle_ms` and no person
/// holding it. One with no activity file is in use (it just started).
pub fn idle(unused_for: Option<Duration>, held: bool, idle_ms: u64) -> bool {
    !held && unused_for.is_some_and(|d| d >= Duration::from_millis(idle_ms))
}

struct Proc {
    what: &'static str,
    child: tokio::process::Child,
}

impl Proc {
    fn spawn(what: &'static str, cmd: &mut tokio::process::Command) -> Result<Proc, String> {
        let child = cmd.stdin(std::process::Stdio::null()).kill_on_drop(true).spawn().map_err(|e| format!("{what}: {e}"))?;
        ev!("desktop.spawned", { "what": what, "pid": child.id() });
        Ok(Proc { what, child })
    }

    fn exited(&mut self) -> bool {
        !matches!(self.child.try_wait(), Ok(None))
    }

    async fn stop(mut self) {
        if let Some(pid) = self.child.id() {
            // SAFETY: a signal to our own child.
            unsafe { libc::kill(pid as i32, libc::SIGTERM) };
        }
        if tokio::time::timeout(Duration::from_millis(STOP_GRACE_MS), self.child.wait()).await.is_err() {
            ev!("desktop.killed", { "what": self.what });
            let _ = self.child.kill().await;
        }
    }
}

fn wait_for(path: &Path, ms: u64) -> bool {
    let t = Instant::now();
    // bounded by `ms`
    while t.elapsed() < Duration::from_millis(ms) {
        if path.exists() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    false
}

fn browser(desk: &Desk, n: u32) -> Result<Proc, String> {
    let bin = std::env::var("FRAGMENT_BROWSER_BIN").unwrap_or_else(|_| "/usr/bin/chromium".into());
    Proc::spawn("browser", tokio::process::Command::new(bin).args(browser_args(desk, n)).env("DISPLAY", format!(":{n}")).env("HOME", &desk.dir))
}

/// The supervisor: the desktop up until it is idle, its display dies, or
/// it is asked to stop.
pub async fn run(desk: Desk) -> Result<(), String> {
    use tokio::signal::unix::{signal, SignalKind};
    let n = allocate(&desk)?;
    let mut term = signal(SignalKind::terminate()).map_err(|e| e.to_string())?;
    let mut int = signal(SignalKind::interrupt()).map_err(|e| e.to_string())?;
    // what an earlier desktop of this number left
    for stale in [desk.rfb(), PathBuf::from(format!("/tmp/.X11-unix/X{n}")), PathBuf::from(format!("/tmp/.X{n}-lock"))] {
        let _ = std::fs::remove_file(stale);
    }
    let profile = desk.profile();
    let prefs = profile.join("Default/Preferences");
    if !prefs.exists() {
        std::fs::create_dir_all(prefs.parent().expect("a parent")).map_err(|e| e.to_string())?;
        std::fs::write(&prefs, browser_preferences().to_string()).map_err(|e| e.to_string())?;
    }
    let _ = std::fs::create_dir_all("/data/work/downloads");
    let t = Instant::now();
    let xvnc = Proc::spawn("display", tokio::process::Command::new(std::env::var("FRAGMENT_XVNC_BIN").unwrap_or_else(|_| "Xvnc".into())).args(xvnc_args(&desk, n)))?;
    if !wait_for(&PathBuf::from(format!("/tmp/.X11-unix/X{n}")), 10_000) || !wait_for(&desk.rfb(), 10_000) {
        xvnc.stop().await;
        return Err(format!("display :{n} did not come up"));
    }
    ev!("desktop.display", { "agent": desk.agent, "display": n, "ms": t.elapsed().as_millis() as u64 });
    // matchbox: every window full screen, no title bars (the screen is the
    // browser), and a few hundred KiB where openbox pulls in ~100 MiB
    let wm_bin = std::env::var("FRAGMENT_WM_BIN").unwrap_or_else(|_| "matchbox-window-manager".into());
    let wm = Proc::spawn("window manager", tokio::process::Command::new(wm_bin).args(["-use_titlebar", "no"]).env("DISPLAY", format!(":{n}")).env("HOME", &desk.dir)).ok();
    desk.touch();
    let mut chromium = Some(browser(&desk, n)?);
    let mut browser_at = Instant::now();
    let mut xvnc = Some(xvnc);
    let idle_ms = idle_stop_ms();
    let mut every = tokio::time::interval(Duration::from_millis(LOOK_EVERY_MS));
    let why = loop {
        tokio::select! {
            _ = term.recv() => break "asked",
            _ = int.recv() => break "asked",
            _ = every.tick() => {}
        }
        if xvnc.as_mut().is_some_and(Proc::exited) {
            break "its display stopped";
        }
        if idle(desk.unused_for(), desk.held(), idle_ms) {
            break "idle";
        }
        if chromium.as_mut().is_some_and(Proc::exited) {
            chromium = None;
            ev!("desktop.browser_closed", { "agent": desk.agent });
        }
        if chromium.is_none() && browser_at.elapsed() >= Duration::from_millis(BROWSER_AGAIN_MS) {
            browser_at = Instant::now();
            chromium = browser(&desk, n).map_err(|e| ev!("desktop.browser_failed", { "error": e })).ok();
        }
    };
    ev!("desktop.stopping", { "agent": desk.agent, "why": why, "upMs": t.elapsed().as_millis() as u64 });
    if let Some(c) = chromium {
        c.stop().await;
    }
    if let Some(w) = wm {
        w.stop().await;
    }
    if let Some(x) = xvnc.take() {
        x.stop().await;
    }
    let _ = std::fs::remove_file(desk.activity());
    let _ = std::fs::remove_file(desk.rfb());
    if read_small(&desk.pid_file()).is_some_and(|p| p == std::process::id().to_string()) {
        let _ = std::fs::remove_file(desk.pid_file());
    }
    let _ = std::io::stderr().flush();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root(name: &str) -> PathBuf {
        let r = std::env::temp_dir().join(format!("dk-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&r);
        r
    }

    /// Valid: each agent its own number, the lowest free, the same at every
    /// ask. Invalid: a name that is no agent's, a root too long for its
    /// sockets. Replay: asked again, the same number.
    #[test]
    fn each_agent_has_a_display_of_its_own() {
        let r = root("alloc");
        let j = Desk::new(&r, "juniper.paul").unwrap();
        let f = Desk::new(&r, "fred.paul").unwrap();
        assert_eq!(allocate(&j).unwrap(), DISPLAY_FIRST);
        assert_eq!(allocate(&f).unwrap(), DISPLAY_FIRST + 1);
        assert_eq!(allocate(&j).unwrap(), DISPLAY_FIRST, "asked again: the same");
        assert_eq!(std::fs::read_to_string(j.dir.join("agent")).unwrap(), "juniper.paul");
        assert_eq!(cdp_port(DISPLAY_FIRST), 9210);
        assert!(Desk::new(&r, "../etc").is_err() && Desk::new(&r, "Juniper.paul").is_err());
        assert!(Desk::new(Path::new("relative"), "juniper.paul").is_err());
        assert_eq!(j.rfb(), j.dir.join("rfb.sock"));
        assert_eq!(j.lease().path(), j.dir.join("lease.json"));
        let _ = std::fs::remove_dir_all(&r);
    }

    /// Every number taken: one of a desktop not running is taken back, and
    /// its agent no longer holds it.
    #[test]
    fn a_full_computer_takes_back_a_stopped_desktops_number() {
        let r = root("full");
        let desks: Vec<Desk> = (0..DISPLAYS_MAX).map(|i| Desk::new(&r, &format!("a{i}.paul")).unwrap()).collect();
        for (i, d) in desks.iter().enumerate() {
            assert_eq!(allocate(d).unwrap(), DISPLAY_FIRST + i as u32);
        }
        let late = Desk::new(&r, "late.paul").unwrap();
        assert_eq!(allocate(&late).unwrap(), DISPLAY_FIRST, "the lowest of those not running");
        assert!(!desks[0].dir.join("display").exists(), "taken from its agent");
        assert_eq!(allocate(&desks[0]).unwrap(), DISPLAY_FIRST, "which gets one back at its next ask, the lowest not running");
        assert!(!late.dir.join("display").exists());
        let _ = std::fs::remove_dir_all(&r);
    }

    #[test]
    fn the_lowest_free_number() {
        assert_eq!(lowest_free(&BTreeSet::new()), Some(DISPLAY_FIRST));
        assert_eq!(lowest_free(&[DISPLAY_FIRST, DISPLAY_FIRST + 2].into()), Some(DISPLAY_FIRST + 1));
        assert_eq!(lowest_free(&(DISPLAY_FIRST..DISPLAY_FIRST + DISPLAYS_MAX).collect()), None);
        assert_eq!(display_number("10"), Some(10));
        assert_eq!(display_number("9"), None);
        assert_eq!(display_number("x"), None);
    }

    /// An idle desktop stops; one in use, held by a person, or just started
    /// does not.
    #[test]
    fn idle_is_unused_and_not_held() {
        let min = Duration::from_secs(60);
        assert!(idle(Some(min * 11), false, IDLE_STOP_MS));
        assert!(!idle(Some(min * 9), false, IDLE_STOP_MS), "used 9 minutes ago");
        assert!(!idle(Some(min * 60), true, IDLE_STOP_MS), "a person holds it");
        assert!(!idle(None, false, IDLE_STOP_MS), "no activity file yet");
    }

    /// The display serves RFB on its socket alone, with no password (the
    /// screen is the way in); the browser's CDP is on loopback.
    #[test]
    fn the_display_and_browser_are_the_desktops() {
        let d = Desk::new(Path::new("/run/desktop"), "juniper.paul").unwrap();
        let x = xvnc_args(&d, 10);
        let pair = |k: &str| x.iter().position(|a| a == k).map(|i| x[i + 1].clone());
        assert_eq!(x[0], ":10");
        assert_eq!(pair("-rfbunixpath"), Some(d.rfb().display().to_string()));
        assert_eq!(pair("-rfbport").as_deref(), Some("-1"));
        assert_eq!(pair("-SecurityTypes").as_deref(), Some("None"));
        assert_eq!(pair("-desktop").as_deref(), Some("juniper.paul"));
        let b = browser_args(&d, 10);
        assert!(b.contains(&"--remote-debugging-port=9210".to_string()) && b.contains(&"--remote-debugging-address=127.0.0.1".to_string()));
        assert!(b.iter().any(|a| a.starts_with("--user-data-dir=/run/desktop/")));
        assert_eq!(browser_preferences()["download"]["default_directory"], "/data/work/downloads");
    }
}
