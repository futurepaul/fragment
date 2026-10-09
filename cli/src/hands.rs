//! `fragment hands`: this machine as a second set of hands for your mind
//! (docs/optchat.md, "A machine as hands"; Paul, 2026-10-09: "let's pair
//! this machine as hands"), beside your cloud computer, running the same
//! bridge and goose.
//!
//! - `pair` (signed as you) makes or reuses an agent fragment
//!   `hands-<machine>` (template `agent`, titled with the machine's name),
//!   makes a key that lives only in this machine's fragment config
//!   (`hands.json`, 0600), pairs it to the agent (docs/api.md, "A
//!   machine's keys"), and adds the agent to your mind as an editor, so
//!   the bridge follows its `chat` and `work`.
//! - `run` (in the foreground) starts the loopback proxy (proxy.rs: the
//!   Computer DO's intercepts for this machine, signing with that key) and
//!   the bridge under it (`fragment-bridge run`, runtime goose, its state,
//!   work and home under one folder, `~/fragment-hands`, never your home
//!   itself), with goose's tools: its shell and editor in the work folder,
//!   your mind's MCP server, and, when this machine has them, the web
//!   tools and a headless browser (`fragment-desktop`). No desktop tool:
//!   it never drives your screen. Ctrl-C stops the bridge and everything
//!   it started (its process group).
//! - `status` says what this machine's pairing is and whether it runs.
//! - `unpair` revokes the key (401 from its next request), takes the agent
//!   out of your mind, and forgets the pairing here.

pub mod proxy;

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use fragment_proto::{IdentityKind, IdentityView, PairedKeys};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::api::{self, Client, Code, CodedError};
use crate::auth::Identity;

/// The agent fragment a machine's hands are: `hands-<its name, as a label>`.
pub const LABEL_PREFIX: &str = "hands-";
/// The folder `run` keeps everything under, in the home directory.
pub const FOLDER: &str = "fragment-hands";
/// How long the bridge has to stop (its own bound is 3 s) before its
/// process group is killed.
pub const STOP_GRACE_MS: u64 = 5_000;
/// A 401 from the platform is checked against the key's state at most
/// this often (`GET /api/identities/me`).
pub const REVOKED_CHECK_MS: u64 = 10_000;

/// This machine's pairing, as `pair` keeps it (`hands.json` beside the
/// config, 0600: it holds the machine's key).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Pairing {
    /// The platform it is paired on.
    pub host: String,
    /// The machine's name, as the platform shows it.
    pub name: String,
    /// The agent fragment, its agent's identity, and its owner (you).
    pub agent: String,
    pub identity: String,
    pub owner: String,
    /// The mind it was added to, when there is one.
    #[serde(default)]
    pub mind: Option<String>,
    pub npub: String,
    pub secret_key: String,
    pub paired_at: i64,
}

impl Pairing {
    pub fn key(&self) -> Result<Identity> {
        Identity::from_secret_hex(&self.secret_key).ok_or_else(|| anyhow!("{} holds no key this CLI reads: `fragment hands pair` again", path().display()))
    }
}

/// Where the pairing is kept: beside the config (its directory follows
/// `XDG_CONFIG_HOME`, so a config for another host keeps its own).
pub fn path() -> PathBuf {
    crate::config_path().with_file_name("hands.json")
}

pub fn load() -> Result<Option<Pairing>> {
    let p = path();
    match std::fs::read(&p) {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes).with_context(|| format!("{} is not a pairing (remove it and pair again)", p.display()))?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}", p.display())),
    }
}

fn save(p: &Pairing) -> Result<()> {
    crate::write_secret_file(&path(), &serde_json::to_string_pretty(p)?)
}

fn forget() -> Result<()> {
    match std::fs::remove_file(path()) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("removing {}", path().display())),
    }
}

/// This machine's host name, as its system says it.
pub fn hostname() -> Option<String> {
    let set = |s: String| Some(s.trim().to_string()).filter(|s| !s.is_empty());
    for file in ["/proc/sys/kernel/hostname", "/etc/hostname"] {
        if let Some(h) = std::fs::read_to_string(file).ok().and_then(set) {
            return Some(h);
        }
    }
    let out = std::process::Command::new("hostname").stdin(std::process::Stdio::null()).output().ok()?;
    set(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// A host's name as a machine's (`fragment_core::registry::valid_machine_name`):
/// its first label (`Pauls-MacBook-Pro.local` is `Pauls-MacBook-Pro`), of
/// letters, digits, `_` and `-`, at most 63 bytes.
pub fn machine_name(host: &str) -> Option<String> {
    let first = host.trim().split('.').next().unwrap_or("");
    let kept: String = first.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').collect();
    let kept = kept.trim_start_matches(['-', '_']);
    let cut: String = kept.chars().take(fragment_proto::limits::MACHINE_NAME_MAX_BYTES).collect();
    fragment_core::registry::valid_machine_name(&cut).then_some(cut)
}

/// The agent fragment's label for a machine: `hands-<name>`, lowercase,
/// each run of anything but letters and digits one dash, within a label's
/// 63 bytes.
pub fn label_for(machine: &str) -> Option<String> {
    let mut slug = String::new();
    for c in machine.chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
        } else if !slug.ends_with('-') && !slug.is_empty() {
            slug.push('-');
        }
    }
    let room = fragment_proto::limits::NAME_MAX_BYTES - LABEL_PREFIX.len();
    let slug: String = slug.chars().take(room).collect();
    let label = format!("{LABEL_PREFIX}{}", slug.trim_end_matches('-'));
    fragment_proto::valid_label(&label).then_some(label)
}

fn coded(code: Code, msg: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(CodedError { code, msg: msg.into() })
}

/// You, as the host says: a person with a username.
fn person(c: &Client) -> Result<(IdentityView, String)> {
    if let Some(mode) = c.agent() {
        return Err(crate::usage(format!("{} is an agent: a machine is paired by the person whose hands it is, with their own `fragment login`", mode.agent)));
    }
    let me: IdentityView = c.call_as(c.get("/api/identities/me")?)?;
    if me.kind != IdentityKind::Person {
        bail!("this key is an agent's: pair a machine with your own `fragment login`");
    }
    let username = me.username.clone().ok_or_else(|| crate::usage("choose a username first: `fragment username <name>`"))?;
    Ok((me, username))
}

/// The agent fragment named `label` of yours: made from the `agent`
/// template when you have none (titled `title`), refused when it is
/// something else.
fn agent_fragment(c: &Client, label: &str, username: &str, title: &str) -> Result<String> {
    let name = fragment_proto::fragment_name(label, username);
    let list: fragment_proto::FragmentList = c.call_as(c.get("/api/fragments")?)?;
    if let Some(f) = list.fragments.iter().find(|f| f.name == name) {
        if f.kind != fragment_proto::FragmentKind::Agent || f.role != fragment_proto::Role::Owner {
            bail!("{name} is a fragment that is not an agent of yours: name this machine another way (--name)");
        }
        return Ok(name);
    }
    let made: fragment_proto::Created = c.call_as(c.post_json("/api/fragments", &json!({ "name": label, "template": "agent", "title": title }))?)?;
    Ok(made.name)
}

/// Adds the agent to your mind as an editor, so its bridge follows the
/// mind's `chat` and `work`: the mind's name, or none when you have no
/// such mind.
fn join_mind(c: &Client, mind: &str, identity: &str) -> Result<Option<String>> {
    let r = c.put_json(&format!("/api/f/{mind}/members/{}", api::encode_q(identity)), &json!({ "role": "editor" }))?;
    match r.status {
        200..=299 => {}
        404 => return Ok(None),
        _ => return Err(anyhow::Error::new(r.refusal())).context(format!("adding the hands to {mind}")),
    }
    // its full name, as the platform answers it
    let status: fragment_proto::FragmentStatus = c.call_as(c.get(&format!("/api/f/{mind}/status"))?)?;
    Ok(Some(status.name))
}

/// `fragment hands pair`.
pub fn pair(c: &Client, name: Option<String>, mind: &str, j: bool) -> Result<()> {
    let (me, username) = person(c)?;
    let machine = match name {
        Some(n) if fragment_core::registry::valid_machine_name(&n) => n,
        Some(n) => return Err(crate::usage(format!("{n:?} is no machine's name: 1 to 63 of letters, digits, `.`, `_` and `-`, starting with a letter or a digit"))),
        None => hostname().as_deref().and_then(machine_name).ok_or_else(|| crate::usage("this machine's host name makes no name: give one (--name)"))?,
    };
    let label = label_for(&machine).ok_or_else(|| crate::usage(format!("{machine:?} makes no agent's label: give another (--name)")))?;
    // paired here already: the same pairing, its mind joined again
    if let Some(p) = load()? {
        if p.host == c.host && p.owner == me.id {
            let keys: PairedKeys = c.call_as(c.get(&format!("/api/f/{}/keys", p.agent))?)?;
            if keys.keys.iter().any(|k| k.npub == p.npub && k.revoked_at.is_none()) {
                let joined = join_mind(c, mind, &p.identity)?;
                let p = Pairing { mind: joined.or(p.mind), ..p };
                save(&p)?;
                crate::json_exit(j, &paired_json(&p, false));
                println!("this machine is paired already: {} is {}'s hands ({})", p.name, p.agent, p.npub);
                print_next(&p);
                return Ok(());
            }
        }
        // unpaired since (or another host's, or another person's): forgotten here
        forget()?;
    }
    let agent = agent_fragment(c, &label, &username, &machine)?;
    let key = Identity::generate();
    let route = format!("/api/f/{agent}/keys");
    let signer = c.key()?.pubkey_hex().to_string();
    let proof = key.proof("POST", &c.url(&route), &signer);
    let paired: PairedKeys = c.call_as(c.post_json(&route, &json!({ "proof": proof, "name": machine }))?)?;
    let identity = paired.agent.clone().ok_or_else(|| anyhow!("the host paired the key but named no agent"))?;
    if !paired.keys.iter().any(|k| k.npub == key.npub() && k.revoked_at.is_none()) {
        bail!("the host did not list this machine's key as paired");
    }
    let joined = join_mind(c, mind, &identity)?;
    let p = Pairing {
        host: c.host.clone(),
        name: machine,
        agent,
        identity,
        owner: me.id.clone(),
        mind: joined,
        npub: key.npub(),
        secret_key: key.secret_hex(),
        paired_at: now_ms(),
    };
    save(&p)?;
    crate::json_exit(j, &paired_json(&p, true));
    println!("paired: {} is now {}'s hands ({}), its key in {} (0600)", p.name, p.agent, p.npub, path().display());
    match &p.mind {
        Some(m) => println!("  {} joined {m} as an editor: your mind hands it tasks with computer(task, on: \"{}\")", p.agent, p.name),
        None => println!("  you have no mind {mind}: add {} to one yourself (fragment members add <mind> {} --role editor)", p.agent, p.identity),
    }
    print_next(&p);
    Ok(())
}

fn paired_json(p: &Pairing, new: bool) -> Value {
    json!({ "paired": new, "name": p.name, "agent": p.agent, "identity": p.identity, "mind": p.mind, "npub": p.npub, "host": p.host, "file": path() })
}

fn print_next(p: &Pairing) {
    println!("next: `fragment hands run` (in the foreground; Ctrl-C stops it) runs {} here. It needs", p.name);
    println!("  fragment-bridge (cli/GUIDE.md, \"A machine as hands\": cargo build --release -p fragment-bridge -p goose-desktop in images/)");
    println!("  and goose on your PATH (or --bridge, --goose)");
}

/// `fragment hands unpair`: the key revoked, the agent out of the mind,
/// the pairing forgotten here. A key unpaired already (from the mind's
/// settings, say) is forgotten all the same.
pub fn unpair(c: &Client, j: bool) -> Result<()> {
    let p = load()?.ok_or_else(|| coded(Code::NotFound, "this machine is not paired (`fragment hands pair` pairs it)"))?;
    person(c)?;
    if p.host != c.host {
        return Err(crate::usage(format!("this machine is paired on {}, not {}: unpair it there (--host {})", p.host, c.host, p.host)));
    }
    let r = c.delete(&format!("/api/f/{}/keys/{}", p.agent, p.npub))?;
    let revoked = match r.status {
        200..=299 => true,
        404 => false,
        _ => return Err(anyhow::Error::new(r.refusal())).context("unpairing the key"),
    };
    let mut left = None;
    if let Some(mind) = &p.mind {
        let r = c.delete(&format!("/api/f/{mind}/members/{}", api::encode_q(&p.identity)))?;
        left = Some(r.ok());
    }
    forget()?;
    crate::json_exit(j, &json!({ "unpaired": revoked, "agent": p.agent, "name": p.name, "leftMind": left }));
    println!("unpaired: {}'s key signs nothing from now on; {} left {}; forgot the pairing here", p.name, p.agent, p.mind.as_deref().unwrap_or("no mind"));
    println!("  (the agent fragment {} stays yours: `fragment rm {}` removes it)", p.agent, p.agent);
    Ok(())
}

/// What `status` says.
#[derive(Debug, Serialize)]
struct Status {
    paired: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    agent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    host: Option<String>,
    /// Whether the platform takes the key now (`GET /api/identities/me`
    /// signed by it): false once unpaired.
    #[serde(skip_serializing_if = "Option::is_none")]
    key: Option<bool>,
    /// Whether `fragment hands run` runs here now (its lock held).
    running: bool,
}

/// `fragment hands status`: needs no login of yours (the key answers for
/// itself).
pub fn status(dir: Option<PathBuf>, verbose: bool, j: bool) -> Result<()> {
    let base = folder(dir)?;
    let running = running(&base);
    let Some(p) = load()? else {
        let s = Status { paired: false, name: None, agent: None, mind: None, host: None, key: None, running };
        crate::json_exit(j, &s);
        println!("this machine is not paired (`fragment hands pair` pairs it)");
        return Ok(());
    };
    let mut machine = Client::new(&p.host, p.key()?);
    machine.verbose = verbose;
    let key = match machine.get("/api/identities/me")?.status {
        200 => Some(true),
        401 => Some(false),
        _ => None,
    };
    let s = Status { paired: true, name: Some(p.name.clone()), agent: Some(p.agent.clone()), mind: p.mind.clone(), host: Some(p.host.clone()), key, running };
    crate::json_exit(j, &s);
    println!("{} is {}'s hands on {} ({})", p.name, p.agent, p.host, p.npub);
    println!("  mind:    {}", p.mind.as_deref().unwrap_or("none"));
    println!(
        "  key:     {}",
        match key {
            Some(true) => "paired",
            Some(false) => "unpaired (refused): `fragment hands unpair`, then pair again",
            None => "the host did not say",
        }
    );
    println!("  running: {}", if running { format!("yes ({})", base.display()) } else { "no (`fragment hands run`)".into() });
    Ok(())
}

/// The folder `run` keeps everything under: `--dir`, else
/// `~/fragment-hands`. Never the home directory itself.
pub fn folder(dir: Option<PathBuf>) -> Result<PathBuf> {
    let base = match dir {
        Some(d) => d,
        None => dirs::home_dir().ok_or_else(|| anyhow!("no home directory: name the folder (--dir)"))?.join(FOLDER),
    };
    let base = if base.is_absolute() { base } else { std::env::current_dir()?.join(base) };
    if dirs::home_dir().is_some_and(|h| h == base) || base.parent().is_none() {
        return Err(crate::usage(format!("{} is not a folder of its own: name one (--dir)", base.display())));
    }
    Ok(base)
}

/// Whether a `run` holds the folder's lock now.
fn running(base: &Path) -> bool {
    use fs2::FileExt;
    let Ok(f) = std::fs::OpenOptions::new().read(true).write(true).open(base.join("run.lock")) else { return false };
    match f.try_lock_exclusive() {
        Ok(()) => {
            let _ = fs2::FileExt::unlock(&f);
            false
        }
        Err(_) => true,
    }
}

fn now_ms() -> i64 {
    let since = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).expect("the clock is past 1970");
    i64::try_from(since.as_millis()).expect("ms since 1970 fit an i64")
}

/// A program on `PATH`.
pub fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(name)).find(|p| executable(p))
}

fn executable(p: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        p.is_file()
    }
}

/// A program named on the command line, else beside one of `near`, else on `PATH`.
fn found(named: Option<PathBuf>, name: &str, near: &[Option<PathBuf>]) -> Option<PathBuf> {
    if let Some(n) = named {
        return Some(n);
    }
    near.iter().flatten().filter_map(|p| p.parent().map(|d| d.join(name))).find(|p| executable(p)).or_else(|| which(name))
}

/// A Chromium for the headless browser: one on `PATH`, or a Mac's.
fn chromium() -> Option<PathBuf> {
    for name in ["chromium", "chromium-browser", "google-chrome-stable", "google-chrome"] {
        if let Some(p) = which(name) {
            return Some(p);
        }
    }
    ["/Applications/Google Chrome.app/Contents/MacOS/Google Chrome", "/Applications/Chromium.app/Contents/MacOS/Chromium"].iter().map(PathBuf::from).find(|p| executable(p))
}

/// What `run` is given.
pub struct RunOptions {
    pub dir: Option<PathBuf>,
    pub bridge: Option<PathBuf>,
    pub goose: Option<PathBuf>,
    pub desktop: Option<PathBuf>,
    pub runtime: String,
    pub no_browser: bool,
    pub verbose: bool,
}

/// The environment the bridge (and so goose and its tools) is started
/// with: none of yours but these, so no token of your shell's reaches it.
const KEPT_ENV: [&str; 10] = ["PATH", "LANG", "LC_ALL", "LC_CTYPE", "TZ", "USER", "LOGNAME", "SHELL", "TERM", "TMPDIR"];

/// `fragment hands run`.
#[cfg(unix)]
pub fn run(o: RunOptions) -> Result<()> {
    use fs2::FileExt;
    let p = load()?.ok_or_else(|| coded(Code::NotFound, "this machine is not paired: `fragment hands pair` first"))?;
    let key = p.key()?;
    // the key, asked of the platform before anything starts
    let mut machine = Client::new(&p.host, key.clone());
    machine.verbose = o.verbose;
    let r = machine.get("/api/identities/me")?;
    if r.status == 401 {
        return Err(coded(Code::AuthFailed, format!("{}'s key was unpaired: `fragment hands unpair` here, then `fragment hands pair` again", p.name)));
    }
    let me: IdentityView = machine.call_as(r)?;
    if me.id != p.identity || me.kind != IdentityKind::Agent {
        bail!("the platform says this machine's key is {} ({}), not {}: pair again", me.id, me.kind.as_str(), p.identity);
    }
    let owner = me.owner.clone().ok_or_else(|| anyhow!("the platform names no owner of {}", p.agent))?;
    let script = match o.runtime.as_str() {
        "goose" => false,
        "script" => true,
        other => return Err(crate::usage(format!("--runtime is goose or script, not {other:?}"))),
    };
    let exe = std::env::current_exe().ok();
    let bridge = found(o.bridge, "fragment-bridge", std::slice::from_ref(&exe)).ok_or_else(|| {
        crate::usage("no fragment-bridge: build it (cd images && cargo build --release -p fragment-bridge -p goose-desktop) and put images/target/release on your PATH, or name it (--bridge)")
    })?;
    let goose = match script {
        true => None,
        false => Some(found(o.goose, "goose", &[]).ok_or_else(|| crate::usage("no goose on your PATH: install it, or name it (--goose)"))?),
    };
    let desktop = found(o.desktop, "fragment-desktop", &[Some(bridge.clone()), exe.clone()]);
    let chrome = if o.no_browser { None } else { chromium() };
    let npx = which("npx");
    let mut tools: Vec<&str> = vec![];
    let mut said: Vec<String> = vec!["its shell and editor in the work folder".into(), format!("your mind's view and zoom ({})", p.mind.as_deref().unwrap_or("no mind"))];
    match (&desktop, &chrome, &npx) {
        (Some(_), Some(c), Some(_)) => {
            tools.extend(["browser", "web"]);
            said.push(format!("the web, and a headless browser ({})", c.display()));
        }
        (Some(_), _, _) => {
            tools.push("web");
            said.push(match (o.no_browser, &chrome, &npx) {
                (true, _, _) => "the web (no browser: --no-browser)".into(),
                (_, None, _) => "the web (no browser: no Chromium or Chrome on this machine)".into(),
                _ => "the web (no browser: no npx, which runs Playwright's MCP server)".into(),
            });
        }
        (None, _, _) => said.push("no web or browser tools: no fragment-desktop beside the bridge or on your PATH (--desktop)".into()),
    }

    let base = folder(o.dir)?;
    let sub = |s: &str| base.join(s);
    for d in ["bridge", "work", "home", "goose", "media", "run", "browser", "work/downloads"] {
        std::fs::create_dir_all(sub(d)).with_context(|| format!("making {}", sub(d).display()))?;
    }
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o700))?;
    }
    let lock = std::fs::OpenOptions::new().create(true).truncate(false).read(true).write(true).open(sub("run.lock"))?;
    if lock.try_lock_exclusive().is_err() {
        bail!("`fragment hands run` runs here already ({}): one at a time", base.display());
    }
    let hands = proxy::Hands { host: p.host.clone(), key, agent: p.agent.clone(), identity: p.identity.clone(), owner, machine: p.name.clone() };

    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    let result = rt.block_on(async {
        let (tx, mut heard) = tokio::sync::mpsc::channel(16);
        let proxy = proxy::Proxy::start(hands.clone(), tx).await.context("starting the loopback proxy")?;
        let api = proxy.url();
        let cli = exe.clone().ok_or_else(|| anyhow!("this CLI cannot find itself (goose's `fragment` is it)"))?;
        let mut cmd = tokio::process::Command::new(&bridge);
        cmd.arg("run").env_clear();
        for k in KEPT_ENV {
            if let Some(v) = std::env::var_os(k) {
                cmd.env(k, v);
            }
        }
        let path_of = |s: &str| sub(s).display().to_string();
        let mut env: Vec<(&str, String)> = vec![
            ("HOME", path_of("home")),
            ("FRAGMENT_API", api.clone()),
            ("FRAGMENT_MODEL", api.clone()),
            ("FRAGMENT_COMPUTER", format!("machine:{}", p.name)),
            ("FRAGMENT_IMAGE", "machine".into()),
            ("BRIDGE_RUNTIME", if script { "script".into() } else { "goose".into() }),
            ("BRIDGE_STATE_DIR", path_of("bridge")),
            ("BRIDGE_MEDIA_DIR", path_of("media")),
            // no restore gate, and no hold: no one saves a machine's folder
            ("BRIDGE_HOLD", path_of("run/hold")),
            ("BRIDGE_HELD", path_of("run/held")),
            ("BRIDGE_RESTORED", path_of("run/restored")),
            ("BRIDGE_SCRIPT_DATA", path_of("work")),
            ("BRIDGE_SCRIPT_SCRATCH", path_of("media/script")),
            ("BRIDGE_GOOSE_PLACE", "machine".into()),
            ("BRIDGE_GOOSE_WORK", path_of("work")),
            ("BRIDGE_GOOSE_HOME", path_of("home")),
            ("BRIDGE_GOOSE_ROOT", path_of("goose")),
            ("BRIDGE_GOOSE_CLI", cli.display().to_string()),
            ("BRIDGE_GOOSE_SKILLS", "1".into()),
            ("BRIDGE_GOOSE_TOOLS", if tools.is_empty() { "none".into() } else { tools.join(",") }),
            ("FRAGMENT_DOWNLOADS", path_of("work/downloads")),
            ("FRAGMENT_BROWSER_FILES", path_of("browser")),
        ];
        if let Some(g) = &goose {
            env.push(("BRIDGE_GOOSE_BIN", g.display().to_string()));
        }
        if let Some(d) = &desktop {
            env.push(("BRIDGE_GOOSE_DESKTOP", d.display().to_string()));
        }
        if let Some(c) = &chrome {
            env.push(("FRAGMENT_BROWSER_CHROME", c.display().to_string()));
        }
        for (k, v) in &env {
            cmd.env(k, v);
        }
        // its own process group: Ctrl-C reaches `run`, which stops the bridge
        // and everything it started (goose, its tools) as one
        cmd.process_group(0).stdin(std::process::Stdio::null()).kill_on_drop(true);
        let mut child = cmd.spawn().with_context(|| format!("starting {}", bridge.display()))?;
        let group = child.id().ok_or_else(|| anyhow!("the bridge exited as it started"))? as i32;
        eprintln!("hands: {} runs {} for its owner, on {}", p.name, p.agent, p.host);
        eprintln!("  folder:  {} (work: {})", base.display(), sub("work").display());
        eprintln!("  runtime: {}", match &goose { Some(g) => format!("goose ({})", g.display()), None => "the bridge's scripted agent".into() });
        eprintln!("  tools:   {}", said.join("; "));
        eprintln!("  proxy:   {api} (the bridge's FRAGMENT_API and FRAGMENT_MODEL)");
        eprintln!("Ctrl-C stops it.");
        let stopped = supervise(&mut child, &mut heard, &hands).await;
        stop_group(&mut child, group).await;
        drop(proxy);
        stopped
    });
    let _ = fs2::FileExt::unlock(&lock);
    match result? {
        Stopped::Asked(why) => {
            eprintln!("hands: stopped ({why})");
            Ok(())
        }
        Stopped::Exited(code) => Err(coded(Code::ServerError, format!("the bridge exited on its own ({code}): its log is above"))),
        Stopped::Unpaired => Err(coded(Code::AuthFailed, format!("{}'s key was unpaired: `fragment hands unpair` here, then `fragment hands pair` again", p.name))),
    }
}

#[cfg(not(unix))]
pub fn run(_: RunOptions) -> Result<()> {
    Err(crate::usage("`fragment hands run` runs on Linux and macOS"))
}

/// Why `run` stopped.
#[cfg(unix)]
enum Stopped {
    Asked(&'static str),
    Exited(String),
    Unpaired,
}

/// Waits for a signal, the bridge's exit, or the key's unpairing (a 401
/// the proxy heard, checked at most every `REVOKED_CHECK_MS`).
#[cfg(unix)]
async fn supervise(child: &mut tokio::process::Child, heard: &mut tokio::sync::mpsc::Receiver<proxy::Heard>, hands: &proxy::Hands) -> Result<Stopped> {
    use tokio::signal::unix::{signal, SignalKind};
    let mut term = signal(SignalKind::terminate())?;
    let mut hup = signal(SignalKind::hangup())?;
    let mut int = signal(SignalKind::interrupt())?;
    let mut checked: Option<std::time::Instant> = None;
    // bounded by the bridge's life: one event per pass
    loop {
        tokio::select! {
            _ = int.recv() => return Ok(Stopped::Asked("interrupted")),
            _ = term.recv() => return Ok(Stopped::Asked("terminated")),
            _ = hup.recv() => return Ok(Stopped::Asked("hung up")),
            status = child.wait() => return Ok(Stopped::Exited(status.map_or_else(|e| e.to_string(), |s| s.to_string()))),
            Some(proxy::Heard::Refused) = heard.recv() => {
                if checked.is_some_and(|t| t.elapsed() < std::time::Duration::from_millis(REVOKED_CHECK_MS)) {
                    continue;
                }
                checked = Some(std::time::Instant::now());
                if unpaired(hands).await {
                    return Ok(Stopped::Unpaired);
                }
            }
        }
    }
}

/// Whether the platform refuses the machine's key now.
#[cfg(unix)]
async fn unpaired(hands: &proxy::Hands) -> bool {
    let url = format!("{}/api/identities/me", hands.host);
    let auth = hands.key.nip98_header("GET", &url, b"");
    let http = reqwest::Client::builder().connect_timeout(proxy::CONNECT_TIMEOUT).timeout(api::REQUEST_TIMEOUT_BASE).build().expect("the HTTP client builds");
    http.get(&url).header("authorization", auth).send().await.is_ok_and(|r| r.status() == 401)
}

/// SIGTERM to the bridge's process group (the bridge, goose, its tools),
/// then SIGKILL to whatever is left after `STOP_GRACE_MS`.
#[cfg(unix)]
async fn stop_group(child: &mut tokio::process::Child, group: i32) {
    assert!(group > 1, "a process group of the bridge's own");
    // SAFETY: kill(2) with a negative pid signals that process group; the
    // group is the bridge's, made for it (`process_group(0)`)
    unsafe {
        libc::kill(-group, libc::SIGTERM);
    }
    let waited = tokio::time::timeout(std::time::Duration::from_millis(STOP_GRACE_MS), child.wait()).await;
    // what it started may outlive it (goose's tools): the group goes whole
    if waited.is_ok() {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    // SAFETY: as above
    unsafe {
        libc::kill(-group, libc::SIGKILL);
    }
    let _ = child.wait().await;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Goal: a machine is named for its host, and its agent's label is a
    /// fragment's. Method: host names a Linux box and a Mac give, ones
    /// with nothing usable, and one past the bound.
    #[test]
    fn a_machine_is_named_for_its_host() {
        assert_eq!(machine_name("paulbox").as_deref(), Some("paulbox"));
        assert_eq!(machine_name("Pauls-MacBook-Pro.local\n").as_deref(), Some("Pauls-MacBook-Pro"));
        assert_eq!(machine_name("my box!").as_deref(), Some("mybox"));
        assert_eq!(machine_name("--weird").as_deref(), Some("weird"));
        assert_eq!(machine_name("..."), None);
        assert_eq!(machine_name(&"x".repeat(80)).map(|n| n.len()), Some(63));
        assert_eq!(label_for("paulbox").as_deref(), Some("hands-paulbox"));
        assert_eq!(label_for("Pauls-MacBook-Pro").as_deref(), Some("hands-pauls-macbook-pro"));
        assert_eq!(label_for("box__2").as_deref(), Some("hands-box-2"));
        let long = label_for(&"x".repeat(63)).unwrap();
        assert!(long.len() <= 63 && fragment_proto::valid_label(&long), "{long}");
        assert_eq!(label_for("___"), None);
    }

    /// Goal: `run` keeps everything under a folder of its own, never the
    /// home directory itself. Method: the default, one named, and home.
    #[test]
    fn the_hands_folder_is_its_own() {
        let home = dirs::home_dir().unwrap();
        assert_eq!(folder(None).unwrap(), home.join(FOLDER));
        assert_eq!(folder(Some("/tmp/h".into())).unwrap(), PathBuf::from("/tmp/h"));
        assert!(folder(Some(home.clone())).is_err(), "never the home directory");
        assert!(folder(Some("/".into())).is_err());
    }

    /// Goal: a pairing kept here round-trips, its key with it. Method: one
    /// made and read back.
    #[test]
    fn a_pairing_keeps_its_key() {
        let key = Identity::generate();
        let p = Pairing {
            host: "https://h".into(),
            name: "box".into(),
            agent: "hands-box.paul".into(),
            identity: "id:a".into(),
            owner: "id:p".into(),
            mind: Some("mind.paul".into()),
            npub: key.npub(),
            secret_key: key.secret_hex(),
            paired_at: 1,
        };
        let back: Pairing = serde_json::from_str(&serde_json::to_string(&p).unwrap()).unwrap();
        assert_eq!(back, p);
        assert_eq!(back.key().unwrap().npub(), key.npub());
    }
}
