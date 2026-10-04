mod api;
mod auth;
mod blobs;
mod codestorage;
mod sync;
mod watch;

#[cfg(test)]
mod mockcs;

use fragment_templates::ALL as TEMPLATES;

use crate::api::{encode_q, Code, CodedError};
use crate::codestorage::{Author, CodeStorage, CsError, LIVE, MAIN};
use crate::sync::{Mode, SyncOptions};
use anyhow::{anyhow, Context, Result};
use clap::{Parser, Subcommand};
use fragment_core::price::{dollars, USD};
use fragment_proto::ledger::{LedgerStatus, Standing};
use fragment_proto::limits::AGENT_STATE_WAIT_MS_MAX;
use fragment_proto::{
    AgentState, ChannelPage, Created, FragmentList, FragmentStatus, IdentityView, Invite, InviteList, Member, MemberList, OpResult, Posted, Rotated,
    Run, RunList, TurnOutcome, Visibility,
};
use serde::Serialize;
use serde_json::{json, Value};
use std::io::Read;
use std::path::{Path, PathBuf};

const GUIDE: &str = include_str!("../GUIDE.md");
const SKILL: &str = include_str!("../SKILL.md");

#[derive(Parser)]
#[command(name = "fragment", version, about = "make and run fragments: a folder in git, an app of operations and jobs, channels, members; on Cloudflare")]
struct Cli {
    /// Host base URL (else FRAGMENT_HOST, else config, else https://fragment.club)
    #[arg(long, global = true)]
    host: Option<String>,
    /// Machine-readable output: one-line {"ok":true/false} envelope on stdout
    /// (also via FRAGMENT_OUTPUT=json)
    #[arg(long, global = true)]
    json: bool,
    /// Log every signed request to stderr (stdout stays clean)
    #[arg(short = 'v', long, global = true)]
    verbose: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

/// A file `fragment write` sends: the files API's own cap (docs/api.md).
const WRITE_MAX_BYTES: usize = 256 * 1024;

#[derive(Subcommand)]
enum Cmd {
    /// Make a nostr key (or use the one you have) and add it to you: sign
    /// in on the host in a browser and approve it there (--force makes a
    /// new key)
    Login {
        #[arg(long)]
        force: bool,
        /// Print the approval link and return (run `fragment login` again after approving)
        #[arg(long)]
        no_wait: bool,
        /// Print the link instead of opening a browser
        #[arg(long)]
        no_browser: bool,
    },
    /// Who the host says you are: your identity, username, this key, your other keys
    Whoami,
    /// Your username, chosen once: your fragments live at
    /// <label>--<username>.<host>
    Username {
        username: Option<String>,
        /// Release this username instead (the fleet's operators: undoes one
        /// taken by mistake, so its person chooses again)
        #[arg(long, requires = "username")]
        release: bool,
    },
    /// Your keys: list them, rotate this one (a new key replaces it and
    /// keeps every grant), or revoke one
    Keys {
        #[command(subcommand)]
        sub: Option<KeysCmd>,
    },
    /// Set the default host (or show it, with no argument)
    Host {
        /// Base URL, e.g. http://127.0.0.1:8790
        url: Option<String>,
    },
    /// Create a fragment (empty, or from one of the platform's templates)
    Create {
        name: String,
        /// public | link (default) | members
        #[arg(long)]
        visibility: Option<String>,
        /// Start from one of the platform's templates: a blessed one (chat,
        /// agent, skills, brain, …) runs the platform's current release, any
        /// other is copied in as its first commit
        #[arg(long)]
        template: Option<String>,
        /// Its title (a blessed template's fragment only)
        #[arg(long, requires = "template")]
        title: Option<String>,
        /// Show the share link, the webhook URL and the webhook secret (they
        /// are credentials: `fragment open` shows the links later)
        #[arg(long)]
        show_tokens: bool,
    },
    /// List fragments you have a role on
    List,
    /// Status of a fragment (pins, urls, tokens, counts)
    Status { name: String },
    /// Read the event log (--since is an event ID cursor; --tail shows the last N)
    Events {
        name: String,
        /// Show events after this event ID
        #[arg(long, default_value = "0", conflicts_with = "tail")]
        since: u64,
        /// Show only the newest N events (at most 500)
        #[arg(long)]
        tail: Option<u64>,
    },
    /// Print the manifest
    Manifest { name: String },
    /// Replace the manifest from a local JSON file
    ManifestSet { name: String, file: PathBuf },
    /// Sync a local folder with the fragment's code.storage repo
    /// (default: bidirectional mirror)
    Sync {
        name: String,
        #[arg(long, default_value = ".")]
        dir: PathBuf,
        /// Keep syncing continuously (OS events + live channel + sweeps)
        #[arg(long)]
        watch: bool,
        /// push: local→repo only; pull: repo→local only (never deletes
        /// without --prune); mirror: bidirectional (default)
        #[arg(long)]
        mode: Option<String>,
        /// In pull mode, delete local files that were deleted remotely
        #[arg(long)]
        prune: bool,
        /// Pull what is live (the files served), not main: into the folder
        /// only, deletions included
        #[arg(long, conflicts_with_all = ["mode", "watch", "install", "uninstall", "mirror_from"])]
        live: bool,
        /// Overlay this read-only source folder into --dir before each
        /// pass (new/changed files copy in; source never written)
        #[arg(long)]
        mirror_from: Option<PathBuf>,
        /// Allow a mass deletion to propagate (the guard refuses otherwise)
        #[arg(long)]
        apply_mass_delete: bool,
        /// Delete local state and start fresh (folder moved/replaced)
        #[arg(long)]
        rebuild_state: bool,
        /// Disable the live change channel in continuous mode (sweeps only)
        #[arg(long)]
        no_live: bool,
        /// Install (or, with --uninstall, remove) a LaunchAgent/systemd
        /// unit that keeps this folder syncing after logout/reboot
        #[arg(long, conflicts_with = "uninstall")]
        install: bool,
        #[arg(long)]
        uninstall: bool,
    },
    /// Full-hash audit of the folder against the fragment (no shortcuts)
    Verify { name: String, #[arg(long, default_value = ".")] dir: PathBuf },
    /// Delete a fragment you own (its cell data and blobs; the repo stays; the name is reusable)
    Rm { name: String },
    /// Deploy: sync (if --dir), then move the `live` ref to main's tip.
    /// Prints the canonical URL. History is git history (`fragment drafts`).
    Deploy {
        name: String,
        #[arg(long)]
        dir: Option<PathBuf>,
        #[arg(long)]
        note: Option<String>,
        /// Preview only — point an ephemeral ref at main's tip, don't go live
        #[arg(long)]
        preview: bool,
    },
    /// List deploy history (commits of the `live` ref; newest is live)
    Drafts { name: String },
    /// Roll `live` back to an earlier deploy (default: the one before live)
    Rollback {
        name: String,
        /// Deploy commit SHA to roll back to (see `fragment drafts`)
        #[arg(long)]
        to: Option<String>,
    },
    /// Scaffold + create + deploy in one command; prints share link + webhook URL
    Init {
        /// Fragment name (also the folder name, created in the current dir)
        name: String,
        /// Template name: todo (default) | inbox | notes
        #[arg(long)]
        template: Option<String>,
    },
    /// List runs (jobs and triggered operations; --status held shows parked failures), or show one
    Runs {
        name: String,
        /// Show this run in full (input, output, error)
        run: Option<u64>,
        /// Filter by status: queued | running | succeeded | held | blocked
        #[arg(long)]
        status: Option<String>,
        #[arg(long, default_value = "30")]
        limit: u64,
    },
    /// List a fragment's triggers (cron, channel, files) and what is paused
    Triggers { name: String },
    /// Your usage ledger: your credit, your plan, what you may still
    /// spend, and this month's spend by fragment (your fragments' hosting
    /// and AI, and your agents' models, bill you)
    Ledger {
        #[command(subcommand)]
        sub: Option<LedgerCmd>,
    },
    /// A fragment's monthly cap, which you own: past it, AI steps and agent
    /// turns stop for everyone but you until next month (default $5)
    Cap {
        name: String,
        /// Dollars a month, or `default`
        usd: String,
    },
    /// Pause an operation's triggers (calls still work)
    Pause { name: String, op: String },
    /// Unpause an operation's triggers (also after an auto-pause)
    Unpause { name: String, op: String },
    /// Re-run a held or blocked run with its original input (after fixing the code)
    Replay { name: String, run: u64 },
    /// Rotate a fragment's tokens (the owner, or their agent for them; default: the inbox token and the share link)
    Rotate {
        name: String,
        /// rotate only the inbox token
        #[arg(long)]
        inbox: bool,
        /// rotate only the view token
        #[arg(long)]
        view: bool,
    },
    /// Manage secrets (values via env var of same name, or stdin)
    Secret {
        #[command(subcommand)]
        sub: SecretCmd,
    },
    /// Agents: make one, talk to it, point it at a chat (on the platform's
    /// host; FRAGMENT_AGENTS names another)
    Agent {
        #[command(subcommand)]
        sub: AgentCmd,
    },
    /// A fragment's members: list, add, remove, or leave
    Members {
        #[command(subcommand)]
        sub: MembersCmd,
    },
    /// Invites: a token that makes whoever redeems it a member
    Invite {
        #[command(subcommand)]
        sub: InviteCmd,
    },
    /// Join a fragment with an invite token
    Join { name: String, token: String },
    /// Call an operation; prints its result (a retry with the same --id is a replay)
    Call {
        name: String,
        op: String,
        /// The input, as JSON; @<file> reads it from a file, - from stdin
        /// (for inputs over the 128 KiB a Linux argument holds)
        #[arg(long, default_value = "{}")]
        input: String,
        /// The operation id (default: a fresh one)
        #[arg(long)]
        id: Option<String>,
    },
    /// A fragment's blobs: bytes by their hash, which its pages read at
    /// `__blob/<sha256>` (viewers and up)
    Blob {
        #[command(subcommand)]
        sub: BlobCmd,
    },
    /// Post a record to a channel fragment.json declares with a post role;
    /// prints the record (a retry with the same --id appends nothing)
    Post {
        name: String,
        channel: String,
        /// The record's body, as JSON
        #[arg(long)]
        body: String,
        /// The post's id (default: a fresh one)
        #[arg(long)]
        id: Option<String>,
    },
    /// List a fragment's channels, or read one (--follow keeps streaming)
    Channel {
        name: String,
        channel: Option<String>,
        /// Records after this sequence number
        #[arg(long, default_value = "0")]
        after: i64,
        /// Keep streaming new records (one JSON line each)
        #[arg(long)]
        follow: bool,
    },
    /// Show or set who can see a fragment: public | link | members
    Visibility { name: String, value: Option<String> },
    /// Write one text file to main through the platform (no folder, no git):
    /// what an agent on a computer uses. `fragment deploy` puts it live.
    Write {
        name: String,
        /// The repo path, relative (`site/index.html`, `garden/raw/note.md`)
        path: String,
        /// Its text from a file, or `-` for stdin
        #[arg(long, conflicts_with = "text")]
        from: Option<PathBuf>,
        /// Its text
        #[arg(long)]
        text: Option<String>,
        /// The commit's message
        #[arg(long)]
        message: Option<String>,
    },
    /// Post to a fragment's inbox (webhook-style, token auth)
    Inbox {
        name: String,
        #[arg(long)]
        token: String,
        #[arg(long)]
        payload: String,
        #[arg(long, default_value = "fragment-cli")]
        source: String,
    },
    /// Print the canonical and inbox URLs
    Open { name: String },
    /// Print the agent guide (start here if you are an agent)
    Guide,
    /// Print a SKILL.md that teaches a coding agent (Claude Code, Codex) to use fragment
    Skill,
    /// Scaffold a fragment folder from a template
    New {
        /// Target directory (created if missing)
        dir: Option<PathBuf>,
        /// Template name: todo (default) | inbox | notes
        #[arg(long)]
        template: Option<String>,
        /// List available templates
        #[arg(long)]
        list: bool,
    },
}

#[derive(Subcommand)]
enum AgentCmd {
    /// Make an agent you own; prints its npub (add it to fragments as a member)
    Create {
        name: String,
        /// Its model tier: cheap (the default) or medium
        #[arg(long)]
        model: Option<String>,
        #[arg(long)]
        instructions: Option<String>,
    },
    /// Its turn's state and its recent messages
    Show { name: String },
    /// Say something: starts a turn (or steers the running one) and waits for the answer
    Say {
        name: String,
        text: String,
        /// Return once the turn has started
        #[arg(long)]
        no_wait: bool,
    },
    /// Stop the running turn
    Stop { name: String },
    /// The tools its memberships give it (fragment operations)
    Tools { name: String },
    /// Follow a fragment's channel: others' messages start turns, answers go back through the reply operation
    Listen {
        name: String,
        fragment: String,
        #[arg(long, default_value = "chat")]
        channel: String,
        #[arg(long, default_value = "say")]
        reply: String,
    },
}

#[derive(Subcommand)]
enum LedgerCmd {
    /// Grant someone credit (the deployment's operators): a username, an
    /// identity (id:…), or `me`
    Grant {
        who: String,
        usd: f64,
        /// Why, for whoever reads the ledger later
        #[arg(long)]
        why: String,
        /// The grant's id (again: the same grant, made once)
        #[arg(long)]
        id: Option<String>,
    },
}

/// Dollars, as the ledger's micro-dollars: positive, at most a cent's
/// millionth apart from what was typed.
fn micros_of(usd: f64) -> Result<i64> {
    if !usd.is_finite() || usd <= 0.0 || usd > 1e9 {
        return Err(usage("a positive number of dollars"));
    }
    Ok((usd * USD as f64).round() as i64)
}

/// What a standing stops, for people.
fn standing_text(s: &Standing) -> String {
    match s {
        Standing::Ok => "agents run, AI runs, fragments take writes".into(),
        Standing::AgentsStopped { why } => format!("agents and AI are stopped ({})", serde_json::to_value(why).unwrap_or_default().as_str().unwrap_or("")),
        Standing::ReadOnly { why } => format!("your fragments are read-only ({})", serde_json::to_value(why).unwrap_or_default().as_str().unwrap_or("")),
    }
}

#[derive(Subcommand)]
enum KeysCmd {
    /// Your identity's keys (the same as `fragment whoami`)
    List,
    /// Make a new key, add it to you (proving you hold it), switch this
    /// machine to it, and revoke the old one
    Rotate,
    /// Revoke one of your keys (never the last)
    Revoke { npub: String },
}

#[derive(Subcommand)]
enum BlobCmd {
    /// Upload a file as a blob (editors; typed by its extension); prints
    /// its sha256. A blob nothing names is deleted after a week
    Put {
        name: String,
        file: PathBuf,
    },
}

#[derive(Subcommand)]
enum MembersCmd {
    /// List members and their roles
    List { name: String },
    /// Add a member, or change their role (the owner, or their agent for them)
    Add {
        name: String,
        /// identity (id:…), npub, 64-hex key, or NIP-05 name (name@domain)
        who: String,
        /// viewer | editor
        #[arg(long, default_value = "viewer")]
        role: String,
        /// Lend the member's agents nothing: only the person acts with it
        #[arg(long)]
        people_only: bool,
    },
    /// Remove a member (the owner, or their agent for them)
    Rm { name: String, who: String },
    /// Leave a fragment you are a member of
    Leave { name: String },
}

#[derive(Subcommand)]
enum InviteCmd {
    /// Make an invite (the owner, or their agent for them); prints the token once
    Create {
        name: String,
        /// viewer | editor
        #[arg(long, default_value = "viewer")]
        role: String,
        /// how many people may join with it
        #[arg(long, default_value = "1")]
        uses: u32,
        /// lifetime in seconds (default 7 days, at most 30)
        #[arg(long)]
        ttl: Option<i64>,
    },
    /// List open invites (the owner, or their agent for them; tokens are never shown again)
    List { name: String },
    /// Revoke an invite by id (the owner, or their agent for them)
    Revoke { name: String, id: String },
}

#[derive(Subcommand)]
enum SecretCmd {
    /// Set: value from argv, else env var of the same name, else stdin
    Set { name: String, key: String, value: Option<String> },
    List { name: String },
    Rm { name: String, key: String },
}

struct Config {
    host: Option<String>,
    secret_key: Option<String>,
    /// optional code.storage server override (backend-swap knob; the
    /// storage-token response is the default source)
    codestorage: Option<String>,
    agents: Option<String>,
}

fn config_path() -> PathBuf {
    dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("fragment").join("config.json")
}

/// Writes a secret into its own file, readable by its owner only (0600).
fn write_secret_file(path: &Path, secret: &str) -> Result<()> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(path)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        writeln!(f, "{secret}")?;
    }
    #[cfg(not(unix))]
    std::fs::write(path, format!("{secret}\n"))?;
    Ok(())
}

/// Sets one key of the config, keeping the others; the file holds the
/// secret key, so it stays 0600.
fn save_config(key: &str, value: &str) -> Result<PathBuf> {
    let p = config_path();
    let mut obj: Value = std::fs::read(&p).ok().and_then(|b| serde_json::from_slice(&b).ok()).filter(Value::is_object).unwrap_or(json!({}));
    obj[key] = json!(value);
    write_secret_file(&p, &serde_json::to_string_pretty(&obj)?)?;
    Ok(p)
}

fn print_identity(v: &IdentityView, this_key: &str) {
    println!("identity: {} ({})", v.id, v.kind.as_str());
    match &v.username {
        Some(u) => println!("username: {u} (your fragments are <name>.{u})"),
        None if v.kind == fragment_proto::IdentityKind::Person => println!("username: none yet (fragment username <name>, or on the host's page)"),
        None => {}
    }
    for k in &v.keys {
        let state = match (k.npub == this_key, k.revoked_at) {
            (true, _) => "this key",
            (false, None) => "active",
            (false, Some(_)) => "revoked",
        };
        println!("  {}  {state}", k.npub);
    }
    for a in &v.agents {
        println!("  agent {a}");
    }
}

/// Whom `members add|rm` names: an identity (`id:…`), or a key (an npub,
/// 64 hex, or a NIP-05 name).
fn member_named(who: String) -> Result<String> {
    if who.starts_with("id:") {
        return Ok(who);
    }
    auth::resolve_npub(&who)
}

fn load_config() -> Config {
    let v: Value = std::fs::read(config_path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or(json!({}));
    let text = |k: &str| v[k].as_str().map(str::to_string);
    Config { host: text("host"), secret_key: text("secret_key"), codestorage: text("codestorage").map(|u| u.trim_end_matches('/').to_string()), agents: text("agents") }
}

/// code.storage server override: FRAGMENT_CODESTORAGE_URL env, then the
/// config file's `codestorage` key; else the host's storage-token response
/// decides. Plain URL so the backend stays swappable.
fn codestorage_override() -> Option<String> {
    if let Ok(u) = std::env::var("FRAGMENT_CODESTORAGE_URL") {
        if !u.trim().is_empty() {
            return Some(u.trim().trim_end_matches('/').to_string());
        }
    }
    load_config().codestorage
}

fn resolve_host(cli_host: &Option<String>, cfg: &Config) -> String {
    cli_host
        .clone()
        .or_else(|| std::env::var("FRAGMENT_HOST").ok())
        .or_else(|| cfg.host.clone())
        .unwrap_or_else(|| "https://fragment.club".to_string())
}

/// Where agents answer: FRAGMENT_AGENTS, else the config's `agents`, else
/// the platform itself (the agents' script is co-hosted in its fleet).
fn agents_client(verbose: bool) -> Result<api::Client> {
    let host = std::env::var("FRAGMENT_AGENTS").ok().filter(|h| !h.trim().is_empty()).or_else(|| load_config().agents);
    require_client(&host, verbose)
}

/// State reads `fragment agent say` makes at most while a turn runs: each
/// waits up to `AGENT_STATE_WAIT_MS_MAX`, so about ten minutes in all.
const SAY_STATE_READS_MAX: u64 = 600_000 / AGENT_STATE_WAIT_MS_MAX + 1;
// a read that waits the longest still answers inside a request's timeout
const _: () = assert!(AGENT_STATE_WAIT_MS_MAX + 5_000 <= api::REQUEST_TIMEOUT_BASE.as_millis() as u64);

/// The agent mode, inside a computer (docs/computers.md, cli/GUIDE.md "As an
/// agent"): `FRAGMENT_AS_AGENT` names the agent fragment the computer's
/// egress signs each request as, and `FRAGMENT_FOR` the person it acts for.
/// Neither set: this machine's key signs, as ever.
fn agent_mode() -> Result<Option<api::AgentMode>> {
    agent_mode_of(std::env::var("FRAGMENT_AS_AGENT").ok(), std::env::var("FRAGMENT_FOR").ok())
}

fn agent_mode_of(agent: Option<String>, acting_for: Option<String>) -> Result<Option<api::AgentMode>> {
    let set = |v: Option<String>| v.map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
    let (agent, acting_for) = (set(agent), set(acting_for));
    let Some(agent) = agent else {
        return match acting_for {
            Some(_) => Err(usage("FRAGMENT_FOR names whom an agent acts for: set FRAGMENT_AS_AGENT too")),
            None => Ok(None),
        };
    };
    if !fragment_proto::valid_fragment_name(&agent) {
        return Err(usage(format!("FRAGMENT_AS_AGENT names an agent fragment (<label>.<username>), not {agent:?}")));
    }
    // an identity as the platform names one (`id:` and its opaque id: the
    // platform checks it exactly, and only an agent's owner is honored)
    let identity = |s: &str| s.strip_prefix("id:").is_some_and(|rest| (1..=64).contains(&rest.len()) && rest.bytes().all(|b| b.is_ascii_alphanumeric()));
    if let Some(who) = &acting_for {
        if !identity(who) {
            return Err(usage(format!("FRAGMENT_FOR names an identity (id:…), not {who:?}")));
        }
    }
    Ok(Some(api::AgentMode { agent, acting_for }))
}

/// Where an agent reaches the platform: `--host`, else `FRAGMENT_HOST`, else
/// the computer's `FRAGMENT_API`. Never the config's host or the default:
/// an agent's requests carry no signature until its computer's egress
/// signs them, so they mean nothing anywhere else.
fn agent_host(cli_host: &Option<String>, fragment_host: Option<String>, fragment_api: Option<String>) -> Result<String> {
    let set = |v: Option<String>| v.map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
    set(cli_host.clone())
        .or_else(|| set(fragment_host))
        .or_else(|| set(fragment_api))
        .ok_or_else(|| usage("an agent reaches the platform through its computer's API: set FRAGMENT_HOST (or FRAGMENT_API) to it, e.g. http://api.fragment.internal"))
}

/// The platform's origin for a link a person opens (a join page, a webhook):
/// the one the fragment's status names, else the host this CLI calls (an
/// older host's status names none). From inside a computer the host is its
/// internal API, which no person can open.
fn platform_of(c: &api::Client, status: &FragmentStatus) -> String {
    let named = status.urls.platform.trim_end_matches('/');
    if named.is_empty() { c.host.clone() } else { named.to_string() }
}

fn require_client(cli_host: &Option<String>, verbose: bool) -> Result<api::Client> {
    if let Some(mode) = agent_mode()? {
        let host = agent_host(cli_host, std::env::var("FRAGMENT_HOST").ok(), std::env::var("FRAGMENT_API").ok())?;
        let mut c = api::Client::new(&host, api::Signer::Agent(mode));
        c.verbose = verbose;
        return Ok(c);
    }
    let cfg = load_config();
    let host = resolve_host(cli_host, &cfg);
    let sk = cfg.secret_key.ok_or_else(|| {
        anyhow::Error::new(CodedError {
            code: Code::AuthFailed,
            msg: "no keypair — run `fragment login` first".into(),
        })
    })?;
    let id = auth::Identity::from_secret_hex(&sk).ok_or_else(|| anyhow!("the config's secret_key is not a 64-hex secp256k1 secret ({})", config_path().display()))?;
    let mut c = api::Client::new(&host, id);
    c.verbose = verbose;
    Ok(c)
}

/// Did the operator ask for machine output? `--json` may sit anywhere on the
/// line (even after a token that failed to parse), so scan raw argv; the env
/// var is the documented equivalent.
fn json_env_flag() -> bool {
    std::env::var("FRAGMENT_OUTPUT").map(|v| v.eq_ignore_ascii_case("json")).unwrap_or(false)
        || std::env::args().any(|a| a == "--json")
}

// ---------- machine envelope (--json) ----------
// Success: ONE line {"ok":true,"data":…} on stdout, exit 0.
// Failure: {"ok":false,"error":{code,message,hint,id?}} on stdout, exit 1
// (2 for usage-class errors). Human mode prints `error: …` on stderr, and
// what to do next.

fn emit_ok(data: &impl Serialize) {
    println!(
        "{{\"ok\":true,\"data\":{}}}",
        serde_json::to_string(data).unwrap_or_else(|_| "null".into())
    );
}

fn ok_exit(data: &impl Serialize) -> ! {
    emit_ok(data);
    std::process::exit(0);
}

/// In `--json` mode: the success envelope, and exit 0.
fn json_exit(j: bool, data: &impl Serialize) {
    if j {
        ok_exit(data);
    }
}

/// A mistake in how the command was called: `invalid_usage`, exit 2.
fn usage(msg: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(CodedError { code: Code::InvalidUsage, msg: msg.into() })
}

/// The code of an error from anywhere in run(): a `CodedError` names its
/// own (the host's refusals and usage mistakes are coded where they
/// happen); a request that never got an answer is unavailable; anything
/// else is a failure here.
fn classify_err(e: &anyhow::Error) -> Code {
    if let Some(ce) = e.downcast_ref::<CodedError>() {
        return ce.code;
    }
    if e.chain().any(|c| c.downcast_ref::<reqwest::Error>().is_some()) {
        return Code::Unavailable;
    }
    Code::ServerError
}

/// The id of an operation call or a post that failed (the error's
/// context): a retry with it replays it, so the failure always names it.
#[derive(Debug)]
struct CallId {
    id: String,
    /// The command that sent it: `call` or `post`.
    command: &'static str,
}

impl CallId {
    fn call(id: &str) -> CallId {
        CallId { id: id.to_string(), command: "call" }
    }

    fn post(id: &str) -> CallId {
        CallId { id: id.to_string(), command: "post" }
    }
}

impl std::fmt::Display for CallId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let what = if self.command == "post" { "post" } else { "operation" };
        write!(f, "{what} id {}", self.id)
    }
}

/// The `--json` failure envelope's `error` for an error from anywhere in
/// run(), and its code. A failed operation call carries its id, and when
/// its outcome is unknown the hint says how to find it out.
fn error_body(e: &anyhow::Error) -> (Value, Code) {
    let code = classify_err(e);
    let call = e.downcast_ref::<CallId>();
    let hint = match call {
        Some(CallId { id, command }) if code == Code::OutcomeUnknown => {
            format!("the {command} may have run: `fragment {command}` it again with --id {id}, which replays it and never runs it twice")
        }
        _ => code.hint().to_string(),
    };
    let mut err = json!({ "code": code.as_str(), "message": format!("{e:#}"), "hint": hint });
    if let Some(CallId { id, .. }) = call {
        err["id"] = json!(id);
    }
    (err, code)
}

fn main() {
    let json_mode = json_env_flag();
    let result = match Cli::try_parse() {
        Ok(cli) => run(cli),
        // help and version (exit 0), and clap's own usage text for a person (exit 2)
        Err(e) if !json_mode || !e.use_stderr() => e.exit(),
        Err(e) => Err(usage(e.to_string())),
    };
    let Err(e) = result else { return };
    let (err, code) = error_body(&e);
    if json_mode {
        println!("{{\"ok\":false,\"error\":{err}}}");
    } else {
        eprintln!("error: {e:#}");
        // what the host refused (or a call whose outcome is unknown), and
        // what to do about it
        if let (Some(_), Some(hint)) = (e.downcast_ref::<CodedError>(), err["hint"].as_str()) {
            eprintln!("hint: {hint}");
        }
    }
    std::process::exit(code.exit_status());
}

fn run(cli: Cli) -> Result<()> {
    let j = cli.json || json_env_flag();

    match cli.cmd {
        Cmd::Login { force, no_wait, no_browser } => {
            // an agent has no key to log in with: its computer signs for it
            if let Some(mode) = agent_mode()? {
                return Err(usage(format!("{} is an agent and needs no login: its computer signs each request as it (FRAGMENT_AS_AGENT)", mode.agent)));
            }
            // the key this machine signs with: the one it has, or a new one
            let key_existed = !force && load_config().secret_key.is_some();
            if !key_existed {
                save_config("secret_key", &auth::Identity::generate().secret_hex())?;
            }
            let c = require_client(&cli.host, cli.verbose)?;
            // whose is this key? (401 until someone signed in approves it)
            let me = || -> Result<Option<IdentityView>> {
                let r = c.get("/api/identities/me")?;
                match r.status {
                    401 => Ok(None),
                    _ => c.call_as(r).map(Some),
                }
            };
            let key = c.key()?;
            let mut done = me()?;
            if done.is_none() {
                // the link carries this key's own proof (ten minutes good):
                // approving it in a signed-in browser adds the key at once
                let npub = key.npub();
                let approve = format!("{}/cli/approve", c.host);
                let proof = key.nip98_header("POST", &approve, &[]);
                let proof = proof.strip_prefix("Nostr ").unwrap_or(&proof);
                let url = format!("{}/cli?key={npub}&proof={}", c.host, encode_q(proof));
                let tail = &npub[npub.len() - 8..];
                if no_wait {
                    json_exit(j, &json!({ "npub": npub, "pending": true, "approve": url }));
                    println!("approve this key in a browser where you are signed in (the link is good for ten minutes):\n  {url}");
                    println!("its key ends in {tail}; then run `fragment login` again");
                    return Ok(());
                }
                eprintln!("sign in and approve this key (the link is good for ten minutes):\n  {url}\nthe page should show a key ending in {tail}");
                if !no_browser {
                    let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
                    let _ = std::process::Command::new(opener).arg(&url).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status();
                }
                let t0 = std::time::Instant::now();
                while done.is_none() {
                    if t0.elapsed() > std::time::Duration::from_secs(600) {
                        anyhow::bail!("no approval in ten minutes; run `fragment login` again for a fresh link");
                    }
                    std::thread::sleep(std::time::Duration::from_secs(2));
                    done = me()?;
                }
            }
            let v = done.expect("approved");
            // never echo the key itself
            json_exit(
                j,
                &json!({ "npub": key.npub(), "id": v.id, "kind": v.kind, "owner": v.owner, "host": c.host, "config": config_path().display().to_string(), "existing": key_existed }),
            );
            println!("logged in as {} on {}", v.id, c.host);
            println!("key: {}", key.npub());
            return Ok(());
        }
        Cmd::Host { url } => {
            let cfg = load_config();
            match url {
                Some(url) => {
                    let url = url.trim_end_matches('/').to_string();
                    if !url.starts_with("https://") && !url.starts_with("http://") {
                        return Err(usage("host must be an http(s) URL, e.g. http://127.0.0.1:8790"));
                    }
                    let p = save_config("host", &url)?;
                    json_exit(j, &json!({ "host": url, "config": p.display().to_string() }));
                    println!("default host set: {url}");
                    println!("config: {}", p.display());
                }
                None => {
                    json_exit(j, &json!({ "host": resolve_host(&cli.host, &cfg) }));
                    println!("host: {}", resolve_host(&cli.host, &cfg));
                }
            }
            return Ok(());
        }
        Cmd::Guide => {
            print!("{GUIDE}");
            return Ok(());
        }
        Cmd::Skill => {
            print!("{SKILL}");
            return Ok(());
        }
        Cmd::New { dir, template, list } => {
            if list {
                for (name, files) in TEMPLATES {
                    println!("{name} ({} files)", files.len());
                }
                return Ok(());
            }
            let dir = dir.ok_or_else(|| usage("usage: fragment new <dir> [--template <name>]"))?;
            let tpl_name = template.as_deref().unwrap_or("todo");
            let (created, skipped) = scaffold(&dir, tpl_name)?;
            for rel in &created {
                println!("  {rel}");
            }
            let created = created.len();
            println!("scaffolded '{tpl_name}' into {} ({created} files{})", dir.display(), if skipped > 0 { format!(", {skipped} existing left alone") } else { String::new() });
            println!("next:");
            println!("  fragment init <name> --template <tpl>  (scaffold + create + deploy in one step)");
            return Ok(());
        }
        _ => {}
    }

    let c = require_client(&cli.host, cli.verbose)?;

    match cli.cmd {
        Cmd::Username { username, release } => {
            if release {
                let u = username.unwrap_or_default();
                let v = c.call(c.delete(&format!("/api/users/{u}"))?)?;
                json_exit(j, &v);
                println!("released {u} ({}): its person chooses a username again", v["identity"].as_str().unwrap_or(""));
                return Ok(());
            }
            let v = match username {
                Some(u) => c.call(c.put_json("/api/identities/me/username", &json!({ "username": u }))?)?,
                None => c.call(c.get("/api/identities/me")?)?,
            };
            json_exit(j, &v);
            match v["username"].as_str() {
                Some(u) => println!("username: {u}"),
                None => println!("no username yet: fragment username <name>"),
            }
        }
        Cmd::Whoami | Cmd::Keys { sub: None | Some(KeysCmd::List) } => {
            let v: IdentityView = c.call_as(c.get("/api/identities/me")?)?;
            if let Some(mode) = c.agent() {
                // an agent: no key here, its computer's egress signs as it
                json_exit(j, &json!({ "agent": mode.agent, "for": mode.acting_for, "host": c.host, "identity": v }));
                print_identity(&v, "");
                println!("agent: {} (its computer signs each request as it)", mode.agent);
                if let Some(who) = &mode.acting_for {
                    println!("acting for: {who}");
                }
                println!("host: {}", c.host);
                return Ok(());
            }
            let npub = c.key()?.npub();
            json_exit(j, &json!({ "npub": npub, "host": c.host, "identity": v }));
            print_identity(&v, &npub);
            println!("host: {}", c.host);
        }
        Cmd::Keys { sub: Some(KeysCmd::Rotate) } => {
            let old = c;
            let old_key = old.key()?.clone();
            let fresh = auth::Identity::generate();
            // 1. the old key adds the new one, which proves itself inside
            let url = format!("{}/api/identities/me/keys", old.host);
            let proof = fresh.proof("POST", &url, old_key.pubkey_hex());
            old.call_as::<IdentityView>(old.post_json("/api/identities/me/keys", &json!({ "proof": proof }))?)?;
            // 2. this machine switches to it (both keys work until step 3)
            save_config("secret_key", &fresh.secret_hex())?;
            let new = require_client(&cli.host, cli.verbose)?;
            // 3. the new key revokes the old one
            let revoked = new.call_as::<IdentityView>(new.delete(&format!("/api/identities/me/keys/{}", old_key.pubkey_hex()))?);
            if let Err(e) = &revoked {
                eprintln!("the new key is in use, but revoking the old one failed: {e}\nrevoke it: fragment keys revoke {}", old_key.npub());
            }
            let v = revoked?;
            let new_npub = new.key()?.npub();
            json_exit(j, &json!({ "npub": new_npub, "revoked": old_key.npub(), "identity": v }));
            println!("{new_npub} replaces {} (revoked); every grant stays with {}", old_key.npub(), v.id);
        }
        Cmd::Keys { sub: Some(KeysCmd::Revoke { npub }) } => {
            let hex = fragment_core::npub::parse(&npub).ok_or_else(|| anyhow!("{npub} is not an npub or a 64-hex key"))?;
            if hex == c.key()?.pubkey_hex() {
                anyhow::bail!("that is the key this machine signs with: rotate it instead (`fragment keys rotate`)");
            }
            let v: IdentityView = c.call_as(c.delete(&format!("/api/identities/me/keys/{hex}"))?)?;
            json_exit(j, &v);
            println!("revoked {npub}");
        }
        Cmd::Create { name, visibility, show_tokens, template, title } => {
            let visibility = match visibility.as_deref() {
                Some(v) => Some(Visibility::parse(v).ok_or_else(|| usage(format!("--visibility is public, link, or members, not {v:?}")))?),
                None => None,
            };
            let body = fragment_proto::CreateFragment { name: name.clone(), visibility, template, title };
            let v: Created = c.call_as(c.post_json("/api/fragments", &body)?)?;
            if j {
                // its tokens are credentials: on request only (a transcript keeps what is printed)
                let mut data = serde_json::to_value(&v)?;
                if !show_tokens {
                    for token in ["viewToken", "inboxToken", "webhookSecret"] {
                        data.as_object_mut().expect("Created is an object").remove(token);
                    }
                }
                ok_exit(&data);
            }
            println!("created fragment {}", v.name);
            println!("  npub:         {}", v.npub);
            println!("  canonical:    {}", v.canonical);
            if show_tokens {
                let st: FragmentStatus = c.call_as(c.get(&format!("/api/f/{}/status", v.name))?)?;
                println!("  share link:   {}", share_link(&v.canonical, &v.view_token));
                println!("  webhook URL:  {}/api/f/{}/inbox?t={}", platform_of(&c, &st), v.name, v.inbox_token);
            } else {
                println!("  its share link and webhook URL: fragment open {}", v.name);
            }
        }
        Cmd::List => {
            let v: FragmentList = c.call_as(c.get("/api/fragments")?)?;
            json_exit(j, &v);
            for f in &v.fragments {
                println!("{} ({})", f.name, f.role.as_str());
            }
        }
        Cmd::Status { name } => {
            let v: FragmentStatus = c.call_as(c.get(&format!("/api/f/{name}/status"))?)?;
            json_exit(j, &v);
            println!("{}", serde_json::to_string_pretty(&v)?);
        }
        Cmd::Events { name, since, tail } => {
            // the host picks the newest for --tail: a page read from the
            // start and cut here showed the oldest events once the log
            // outgrew one page
            let query = match tail {
                Some(n) => format!("tail={n}"),
                None => format!("since={since}"),
            };
            let v = c.call(c.get(&format!("/api/f/{name}/events?{query}"))?)?;
            let evs = v["events"].as_array().cloned().unwrap_or_default();
            json_exit(j, &json!({ "events": evs }));
            for e in evs {
                let at = e["at"].as_u64().unwrap_or(0);
                let time = chrono_like(at / 1000);
                println!("[{}] {} {} — {}", e["id"], time, e["kind"].as_str().unwrap_or(""), e["summary"].as_str().unwrap_or(""));
            }
        }
        Cmd::Manifest { name } => {
            let v = c.call(c.get(&format!("/api/f/{name}/manifest"))?)?;
            json_exit(j, &v);
            println!("{}", serde_json::to_string_pretty(&v)?);
        }
        Cmd::ManifestSet { name, file } => {
            let bytes = std::fs::read(&file).with_context(|| format!("reading {}", file.display()))?;
            let v: Value = serde_json::from_slice(&bytes)
                .with_context(|| format!("{} is not valid JSON", file.display()))?;
            // fragment.json is a git file at the repo root: manifest-set is
            // edit-and-commit (expected-parent CAS, bounded retries)
            let writer = writer_id(&c);
            let tip = sync::commit_single_file(&c, &name, "fragment.json", serde_json::to_vec(&v)?, &format!("manifest-set {name}"), &writer, codestorage_override().as_deref())
                .map_err(cs_anyhow)?;
            json_exit(j, &json!({ "updated": true, "commit": tip, "manifest": v }));
            println!("manifest updated (commit {})", &tip[..8.min(tip.len())]);
        }
        Cmd::Sync {
            name, dir, watch, mode, prune, live, mirror_from, apply_mass_delete,
            rebuild_state, no_live, install, uninstall,
        } => {
            // never stream JSON envelopes mid-run: watch prints progress
            // lines forever; a json consumer would choke on line 2
            if j && watch {
                return Err(usage("sync --watch streams progress lines continuously, so --json does not apply: run single passes with --json (`fragment sync <name> --dir .`), or drop --json to watch"));
            }
            if install || uninstall {
                install_sync_unit(&name, &dir, install, mirror_from.as_deref())?;
                return Ok(());
            }
            if rebuild_state {
                let p = dir.join(".fragment").join("state.json");
                std::fs::remove_file(&p).ok();
                println!("state cleared: {}", p.display());
            }
            let opts = SyncOptions {
                mirror_from,
                mode: match mode.as_deref() {
                    _ if live => Mode::Pull,
                    Some("push") => Mode::Push,
                    Some("pull") => Mode::Pull,
                    Some("mirror") | None => Mode::Mirror,
                    other => return Err(usage(format!("--mode must be push|pull|mirror, got {other:?}"))),
                },
                apply_mass_delete,
                prune: prune || live,
                live,
                writer_id: writer_id(&c),
                codestorage: codestorage_override(),
            };
            if watch {
                watch::run(&c, &name, &dir, &opts, !no_live)?;
                return Ok(());
            }
            let report = sync::sync_once(&c, &name, &dir, &opts).map_err(cs_anyhow)?;
            // scriptable exit codes survive the envelope (3 conflicts, 4 guard)
            if j {
                emit_ok(&report);
            } else {
                println!("sync {} ({})", name, dir.display());
                report.print();
            }
            std::process::exit(report.exit_code());
        }
        Cmd::Rm { name } => {
            c.call(c.delete(&format!("/api/f/{name}"))?)?;
            json_exit(j, &json!({ "deleted": true, "name": name }));
            println!("deleted fragment {name} (the repo stays; the name is reusable)");
        }
        Cmd::Verify { name, dir } => {
            let report = sync::verify(&c, &name, &dir, codestorage_override().as_deref()).map_err(cs_anyhow)?;
            if j {
                emit_ok(&report);
            } else {
                println!("verify {} ({})", name, dir.display());
                report.print();
            }
            std::process::exit(report.exit_code());
        }
        // no folder to sync and no preview: the platform moves live itself
        // (POST …/deploy), so nothing here talks to git (an agent on a computer)
        Cmd::Deploy { name, dir: None, note, preview: false } => {
            let r = c.post_json(&format!("/api/f/{name}/deploy"), &json!({ "note": note }))?;
            let v: Value = c.call_as(r)?;
            let live_tip = v["live"].as_str().unwrap_or("").to_string();
            let st: FragmentStatus = c.call_as(c.get(&format!("/api/f/{name}/status"))?)?;
            if let Some(why) = st.code.error.as_deref().filter(|_| st.code.sha.as_deref() != Some(live_tip.as_str())) {
                return Err(anyhow::Error::new(CodedError {
                    code: Code::InvalidRequest,
                    msg: format!("live moved to {}, but the platform refused its code, so the last good code keeps serving: {why}", &live_tip[..12.min(live_tip.len())]),
                }));
            }
            let live_url = &st.urls.canonical;
            json_exit(j, &json!({ "live": live_url, "liveTip": live_tip, "mainTip": live_tip }));
            println!("live: {live_url}");
            if let (Visibility::Link, Some(tok)) = (st.visibility, &st.view_token) {
                println!("share link: {}", share_link(live_url, tok));
            }
        }
        Cmd::Write { name, path, from, text, message } => {
            let text = match (from, text) {
                (Some(f), None) if f.as_os_str() == "-" => {
                    let mut t = String::new();
                    std::io::Read::read_to_string(&mut std::io::stdin(), &mut t).context("reading stdin (a file is text)")?;
                    t
                }
                (Some(f), None) => std::fs::read_to_string(&f).with_context(|| format!("reading {} (a file written this way is text; sync a folder for others)", f.display()))?,
                (None, Some(t)) => t,
                _ => return Err(usage("name the file's text: --text TEXT, or --from FILE (- for stdin)")),
            };
            if text.len() > WRITE_MAX_BYTES {
                return Err(usage(format!("a file written this way is at most {} KiB; sync a folder for larger ones", WRITE_MAX_BYTES / 1024)));
            }
            let msg = message.unwrap_or_else(|| format!("write {path}"));
            let r = c.post_json(&format!("/api/f/{name}/files"), &json!({ "files": [{ "path": path, "text": text }], "message": msg }))?;
            let v: Value = c.call_as(r)?;
            json_exit(j, &json!({ "path": path, "commit": v["commit"] }));
            println!("wrote {path} to main ({}); `fragment deploy {name}` puts it live", v["commit"].as_str().map(|c| &c[..8.min(c.len())]).unwrap_or("?"));
        }
        Cmd::Deploy { name, dir, note, preview } => {
            let deployed = deploy(&c, &name, dir.as_deref(), note.as_deref(), preview, codestorage_override().as_deref())?;
            let synced = match &deployed {
                Deployed::Guarded(report) => Some(report),
                Deployed::Preview { synced, .. } | Deployed::Live { synced, .. } => synced.as_ref(),
            };
            if let (Some(report), false) = (synced, j) {
                report.print();
            }
            let (live_tip, main_tip) = match deployed {
                Deployed::Guarded(_) => {
                    let dir = dir.as_deref().unwrap_or(Path::new("."));
                    anyhow::bail!("sync refused a mass deletion — deploy aborted before moving live. If the deletions are intended, run `fragment sync {} --dir {} --apply-mass-delete` first, then deploy again.", name, dir.display());
                }
                Deployed::Preview { slug, sha, .. } => {
                    json_exit(j, &json!({ "preview": slug, "sha": sha }));
                    println!("preview: {slug} (ephemeral ref at {})", &sha[..12.min(sha.len())]);
                    println!("go live with: fragment deploy {name}");
                    return Ok(());
                }
                Deployed::Live { live_tip, main_tip, .. } => (live_tip, main_tip),
            };
            let st: FragmentStatus = c.call_as(c.get(&format!("/api/f/{name}/status"))?)?;
            // the refresh installed live (or refused its code) before it answered
            if let Some(why) = st.code.error.as_deref().filter(|_| st.code.sha.as_deref() != Some(live_tip.as_str())) {
                return Err(anyhow::Error::new(CodedError {
                    code: Code::InvalidRequest,
                    msg: format!("live moved to {}, but the platform refused its code, so the last good code keeps serving: {why}", &live_tip[..12.min(live_tip.len())]),
                }));
            }
            let live_url = &st.urls.canonical;
            json_exit(j, &json!({ "live": live_url, "liveTip": live_tip, "mainTip": main_tip }));
            println!("live: {live_url}");
            if let (Visibility::Link, Some(tok)) = (st.visibility, &st.view_token) {
                println!("share link: {}", share_link(live_url, tok));
            }
        }
        Cmd::Rollback { name, to } => {
            let storage = CodeStorage::connect(&c, &name, codestorage_override().as_deref()).map_err(cs_anyhow)?;
            let history = storage.list_commits(LIVE, 30).map_err(cs_anyhow)?;
            let live_tip = history.first().map(|cm| cm.sha.clone())
                .ok_or_else(|| anyhow!("live has no deploys yet (see `fragment deploy {name}`)"))?;
            let target = match to {
                Some(s) => s,
                None => history.get(1).map(|cm| cm.sha.clone())
                    .ok_or_else(|| anyhow!("no earlier deploy to roll back to (see `fragment drafts {name}`)"))?,
            };
            let author = Author::writer(&writer_id(&c));
            let new_tip = storage.restore_live(&target, &live_tip, &format!("rollback {name} to {target}"), &author)
                .map_err(cs_anyhow)?;
            sync::refresh_pins(&c, &name);
            json_exit(j, &json!({ "rolledBackTo": target, "liveTip": new_tip }));
            println!("rolled back to {}: live is now {}", &target[..8.min(target.len())], &new_tip[..8.min(new_tip.len())]);
        }
        Cmd::Drafts { name } => {
            let storage = CodeStorage::connect(&c, &name, codestorage_override().as_deref()).map_err(cs_anyhow)?;
            let commits = storage.list_commits(LIVE, 30).map_err(cs_anyhow)?;
            json_exit(j, &json!({ "deploys": commits }));
            if commits.is_empty() {
                println!("(no deploys yet)");
                return Ok(());
            }
            for (i, cm) in commits.iter().enumerate() {
                println!(
                    "{} {}{}  {}",
                    &cm.sha[..8.min(cm.sha.len())],
                    if i == 0 { "[live] " } else { "" },
                    cm.message,
                    cm.date,
                );
            }
            println!("roll back with: fragment rollback {name} --to <sha>");
        }

        Cmd::Init { name, template } => {
            let dir = std::env::current_dir()?.join(&name);
            if dir.exists() {
                anyhow::bail!("{} already exists", dir.display());
            }
            let tpl_name = template.as_deref().unwrap_or("todo");
            scaffold(&dir, tpl_name)?;
            // stamp the fragment's name into the manifest BEFORE the first
            // sync — fragment.json is a git file and rides the commit
            let mf = dir.join("fragment.json");
            if mf.exists() {
                let mut m: Value = serde_json::from_str(&std::fs::read_to_string(&mf)?)?;
                if let Value::Object(o) = &mut m {
                    o.insert("name".into(), Value::String(name.clone()));
                }
                std::fs::write(&mf, serde_json::to_vec_pretty(&m)?)?;
            }
            if !j {
                println!("scaffolded '{tpl_name}' into {}", dir.display());
            }
            let created = c.post_json("/api/fragments", &json!({ "name": name })).and_then(|r| c.call(r));
            if let Err(e) = created {
                // nothing was created, so leave nothing here either: the same init can be retried
                let _ = std::fs::remove_dir_all(&dir);
                return Err(e);
            }
            // push the scaffold, then point live at it — the first deploy
            // is the real site, not an empty one
            let Deployed::Live { synced: Some(report), .. } = deploy(&c, &name, Some(&dir), None, false, codestorage_override().as_deref())? else {
                anyhow::bail!("the first deploy of {name} did not go live");
            };
            if !j {
                report.print();
            }
            let st: FragmentStatus = c.call_as(c.get(&format!("/api/f/{name}/status"))?)?;
            let canon = st.urls.canonical.clone();
            let shared = match (st.visibility, &st.view_token) {
                (Visibility::Link, Some(tok)) => Some(share_link(&canon, tok)),
                _ => None,
            };
            let webhook = st.inbox_token.as_ref().map(|tok| format!("{}/api/f/{}/inbox?t={tok}", platform_of(&c, &st), name));
            if j {
                let mut data = json!({
                    "canonical": canon,
                    "webhookUrl": webhook,
                    "folder": dir.display().to_string(),
                    "status": st,
                });
                if let Some(link) = &shared {
                    data["shareLink"] = json!(link);
                }
                ok_exit(&data);
            }
            println!("live: {}", canon);
            if let Some(link) = &shared {
                println!("share link: {link}");
            }
            if let Some(url) = &webhook {
                println!("webhook URL: {url}");
            }
            println!("folder: {}", dir.display());
        }
        Cmd::Runs { name, run: Some(id), .. } => {
            let v: Run = c.call_as(c.get(&format!("/api/f/{name}/runs/{id}"))?)?;
            json_exit(j, &v);
            println!("{}", serde_json::to_string_pretty(&v)?);
        }
        Cmd::Ledger { sub } => match sub {
            None => {
                let v: LedgerStatus = c.call_as(c.get("/api/ledger")?)?;
                json_exit(j, &v);
                let plan = serde_json::to_value(v.plan)?.as_str().unwrap_or("").to_string();
                println!("{}: {} available ({} balance, {} held by calls running now), plan {plan}", v.month, dollars(v.available_micros.max(0)), dollars(v.balance_micros), dollars(v.reserved_micros));
                println!("{}", standing_text(&v.standing));
                for f in &v.fragments {
                    println!("  {}\t{} of its {} cap", f.fragment, dollars(f.spent_micros), dollars(f.cap_micros));
                }
            }
            Some(LedgerCmd::Grant { who, usd, why, id }) => {
                let me: IdentityView = c.call_as(c.get("/api/identities/me")?)?;
                let id = id.unwrap_or_else(|| format!("cli-{:016x}", rand::random::<u64>()));
                let body = json!({ "id": id, "micros": micros_of(usd)?, "by": me.id, "why": why });
                c.call(c.post_json(&format!("/api/ledger/{who}/grant"), &body)?)?;
                json_exit(j, &json!({ "granted": body }));
                println!("granted {who} {} ({id})", dollars(micros_of(usd)?));
            }
        },
        Cmd::Cap { name, usd } => {
            let micros = match usd.as_str() {
                "default" => None,
                n => Some(micros_of(n.parse::<f64>().map_err(|_| usage("a cap is dollars, or `default`"))?)?),
            };
            let id = format!("cli-{:016x}", rand::random::<u64>());
            let v = c.call(c.put_json(&format!("/api/f/{name}/cap"), &json!({ "id": id, "micros": micros }))?)?;
            json_exit(j, &v);
            println!("{name}: {} a month{}", dollars(v["capMicros"].as_i64().unwrap_or(0)), if v["default"] == true { " (the default)" } else { "" });
        }

        Cmd::Triggers { name } => {
            let v = c.call(c.get(&format!("/api/f/{name}/triggers"))?)?;
            json_exit(j, &v);
            let rows = v["triggers"].as_array().cloned().unwrap_or_default();
            for t in &rows {
                let on = ["cron", "channel", "files"]
                    .iter()
                    .find_map(|k| t[*k].as_str().map(|v| format!("{k} {v}")))
                    .unwrap_or_default();
                let next = t["nextAt"].as_u64().map(|n| format!("\tnext {} UTC", chrono_like(n / 1000))).unwrap_or_default();
                let paused = if t["paused"].as_bool().unwrap_or(false) { "\tPAUSED" } else { "" };
                println!("{on}\t→ {}{next}{paused}", t["run"].as_str().unwrap_or("?"));
            }
            if rows.is_empty() {
                println!("(no triggers)");
            }
        }
        Cmd::Runs { name, run: None, status, limit } => {
            let mut path = format!("/api/f/{name}/runs?limit={limit}");
            if let Some(s) = &status {
                path.push_str(&format!("&status={}", encode_q(s)));
            }
            let v: RunList = c.call_as(c.get(&path)?)?;
            json_exit(j, &v);
            for r in &v.runs {
                let cost = r.cost_micros.map(dollars).unwrap_or_default();
                println!("#{}\t{}\t{}\t{}\tattempt {}\t{cost}", r.id, r.via.as_str(), r.op, r.status.as_str(), r.attempt);
                if let Some(e) = &r.error {
                    println!("  {}", e.chars().take(120).collect::<String>());
                }
            }
            if v.runs.is_empty() {
                println!("(no runs)");
            }
            let held = v.counts.get(fragment_proto::RunStatus::Held.as_str()).copied().unwrap_or(0);
            if held > 0 {
                println!("\n{held} held run(s) — `fragment replay {name} <run-id>` after fixing");
            }
        }
        Cmd::Pause { name, op } => {
            let v = c.call(c.post_json(&format!("/api/f/{name}/pause"), &json!({ "op": op, "paused": true }))?)?;
            json_exit(j, &v);
            println!("paused the triggers of '{op}' (calls still work; unpause with `fragment unpause`)");
        }
        Cmd::Unpause { name, op } => {
            let v = c.call(c.post_json(&format!("/api/f/{name}/pause"), &json!({ "op": op, "paused": false }))?)?;
            json_exit(j, &v);
            println!("unpaused '{op}'");
        }
        Cmd::Replay { name, run } => {
            let v = c.call(c.post_json(&format!("/api/f/{name}/replay"), &json!({ "run": run }))?)?;
            json_exit(j, &v);
            println!("run #{run} queued again (attempt {}); follow it with `fragment runs {name} {run}`", v["attempt"]);
        }
        Cmd::Rotate { name, inbox, view } => {
            // flags narrow the default both-scopes rotation (the webhook
            // secret is code.storage's to know: rotate it only by asking the cell)
            let scopes = match (inbox, view) {
                (true, false) => vec!["inbox"],
                (false, true) => vec!["view"],
                _ => vec!["inbox", "view"],
            };
            let body = json!({ "scopes": scopes });
            let v: Rotated = c.call_as(c.post_json(&format!("/api/f/{name}/rotate"), &body)?)?;
            // the whole answer, webhook secret included: the code.storage
            // push HMAC is only ever visible at create/rotate, and machine
            // consumers (dev harnesses registering push webhooks) need it
            json_exit(j, &v);
            let st: FragmentStatus = c.call_as(c.get(&format!("/api/f/{name}/status"))?)?;
            println!("rotated: {}", v.rotated.join(", "));
            println!("New webhook URL: {}/api/f/{}/inbox?t={}", platform_of(&c, &st), st.name, v.inbox_token);
            println!("New share link: {}", share_link(&st.urls.canonical, &v.view_token));
        }
        Cmd::Secret { sub } => match sub {
            SecretCmd::Set { name, key, value: argv_value } => {
                let value = match argv_value {
                    Some(v) if !v.is_empty() => v,
                    None => match std::env::var(&key) {
                        Ok(v) if !v.is_empty() => v,
                        _ => {
                            eprint!("value for {key} (stdin): ");
                            let mut buf = String::new();
                            std::io::stdin().read_to_string(&mut buf)?;
                            buf.trim().to_string()
                        }
                    },
                    Some(_) => String::new(),
                };
                if value.is_empty() {
                    return Err(usage("empty secret value"));
                }
                c.call(c.put_bytes(&format!("/api/f/{name}/secrets/{key}"), value.into_bytes())?)?;
                // names only — values never travel back
                json_exit(j, &json!({ "name": name, "key": key, "set": true }));
                println!("secret {key} set on {name}");
            }
            SecretCmd::List { name } => {
                let v = c.call(c.get(&format!("/api/f/{name}/secrets"))?)?;
                json_exit(j, &v);
                for n in v["names"].as_array().cloned().unwrap_or_default() {
                    println!("{}", n.as_str().unwrap_or(""));
                }
            }
            SecretCmd::Rm { name, key } => {
                c.call(c.delete(&format!("/api/f/{name}/secrets/{key}"))?)?;
                json_exit(j, &json!({ "name": name, "key": key, "removed": true }));
                println!("secret {key} removed");
            }
        },
        Cmd::Inbox { name, token, payload, source } => {
            let payload_v: Value = serde_json::from_str(&payload).unwrap_or(Value::String(payload));
            // inbox is token-gated, no nostr signature
            let url = format!("{}/api/f/{}/inbox?t={}", c.host, name, token);
            let body = serde_json::to_vec(&json!({ "source": source, "payload": payload_v }))?;
            let mut req = reqwest::blocking::Client::new().post(&url).body(body).header("content-type", "application/json");
            // a computer's egress answers only a request that names its agent
            if let Some(mode) = c.agent() {
                req = req.header(api::AGENT_HEADER, &mode.agent);
            }
            let resp = req.send()?;
            let resp = api::Resp { status: resp.status().as_u16(), body: resp.bytes()?.to_vec() };
            let v = c.call(resp)?;
            json_exit(j, &v);
            println!("{}", serde_json::to_string(&v)?);
        }
        Cmd::Open { name } => {
            let v: FragmentStatus = c.call_as(c.get(&format!("/api/f/{name}/status"))?)?;
            let canon = &v.urls.canonical;
            let link = match v.visibility {
                Visibility::Public => canon.clone(),
                _ => format!("{canon}?view={}", v.view_token.as_deref().unwrap_or("")),
            };
            let webhook = format!("{}/api/f/{}/inbox?t={}", platform_of(&c, &v), v.name, v.inbox_token.as_deref().unwrap_or(""));
            json_exit(j, &json!({ "canonical": link, "shareLink": link, "webhookUrl": webhook }));
            println!("canonical:   {link}");
            println!("share link:  {link}");
            println!("webhook URL: {webhook}");
        }
        Cmd::Agent { sub } => {
            let a = agents_client(cli.verbose)?;
            match sub {
                AgentCmd::Create { name, model, instructions } => {
                    let mut body = json!({ "name": name });
                    if let Some(m) = model {
                        body["model"] = json!(m);
                    }
                    if let Some(i) = instructions {
                        body["instructions"] = json!(i);
                    }
                    // made and registered as yours in one request (the platform does both)
                    let v = a.call(a.post_json("/api/agents", &body)?)?;
                    json_exit(j, &v);
                    let id = v["id"].as_str().unwrap_or("");
                    println!("agent {} ({}): {id}", v["name"].as_str().unwrap_or(""), v["model"].as_str().unwrap_or(""));
                    println!("  npub: {}", v["npub"].as_str().unwrap_or(""));
                    println!("  you own it: you can read whatever it can read");
                    println!("give it a fragment: fragment members add <fragment> {id} --role editor");
                }
                AgentCmd::Show { name } => {
                    let v = a.call(a.get(&format!("/api/a/{name}"))?)?;
                    json_exit(j, &v);
                    println!("{} ({}) {}: {}", v["name"].as_str().unwrap_or(""), v["model"].as_str().unwrap_or(""), v["npub"].as_str().unwrap_or(""), v["outcome"].as_str().unwrap_or("idle"));
                    if let Some(e) = v["error"].as_str().filter(|e| !e.is_empty()) {
                        println!("  error: {e}");
                    }
                    let messages = v["messages"].as_array().cloned().unwrap_or_default();
                    for m in messages.iter().rev().take(10).rev() {
                        let tools: Vec<&str> = m["tool_requests"].as_array().into_iter().flatten().filter_map(|t| t["name"].as_str()).collect();
                        let text = m["text"].as_str().unwrap_or("");
                        if !text.is_empty() {
                            println!("  {}: {text}", m["role"].as_str().unwrap_or(""));
                        } else if !tools.is_empty() {
                            println!("  {} calls {}", m["role"].as_str().unwrap_or(""), tools.join(", "));
                        }
                    }
                }
                AgentCmd::Say { name, text, no_wait } => {
                    let v = a.call(a.post_json(&format!("/api/a/{name}/turns"), &json!({ "text": text }))?)?;
                    if no_wait {
                        json_exit(j, &v);
                        println!("{}", if v["steered"] == true { "steered the running turn" } else { "started" });
                        return Ok(());
                    }
                    // a steer is answered by the running turn: wait for it too
                    let mut ended: Option<AgentState> = None;
                    for _ in 0..SAY_STATE_READS_MAX {
                        let state: AgentState = a.call_as(a.get(&format!("/api/a/{name}/state?wait_ms={AGENT_STATE_WAIT_MS_MAX}"))?)?;
                        if !state.active {
                            ended = Some(state);
                            break;
                        }
                    }
                    let state = ended.ok_or_else(|| anyhow!("the turn is still running after 10 minutes (fragment agent show {name})"))?;
                    let answer = state.answer.clone().unwrap_or_default();
                    json_exit(j, &json!({ "outcome": state.outcome, "answer": answer, "error": state.error.clone().unwrap_or_default() }));
                    match state.outcome {
                        Some(TurnOutcome::Idle) => println!("{answer}"),
                        Some(other) => println!("({}) {}", other.as_str(), state.error.as_deref().unwrap_or("")),
                        None => {}
                    }
                }
                AgentCmd::Stop { name } => {
                    let v = a.call(a.post_json(&format!("/api/a/{name}/stop"), &json!({}))?)?;
                    json_exit(j, &v);
                    println!("{}", if v["active"] == true { "stopping" } else { "no turn was running" });
                }
                AgentCmd::Tools { name } => {
                    let v = a.call(a.get(&format!("/api/a/{name}/tools"))?)?;
                    json_exit(j, &v);
                    for t in v["tools"].as_array().cloned().unwrap_or_default() {
                        println!("{}", t.as_str().unwrap_or(""));
                    }
                }
                AgentCmd::Listen { name, fragment, channel, reply } => {
                    let v = a.call(a.post_json(&format!("/api/a/{name}/listen"), &json!({ "fragment": fragment, "channel": channel, "reply": reply }))?)?;
                    json_exit(j, &v);
                    println!("{name} follows {fragment}'s {channel} channel and answers through {reply}");
                }
            }
        }
        Cmd::Members { sub } => match sub {
            MembersCmd::List { name } => {
                let v: MemberList = c.call_as(c.get(&format!("/api/f/{name}/members"))?)?;
                json_exit(j, &v);
                for m in &v.members {
                    // an agent's owner, named with what it is
                    let owned = match (&m.owner, m.kind) {
                        (Some(o), Some(kind)) => format!("\t{} of {o}", kind.as_str()),
                        (Some(o), None) => format!("\towned by {o}"),
                        (None, _) => String::new(),
                    };
                    println!("{}\t{}{owned}", m.role.as_str(), m.principal);
                }
            }
            MembersCmd::Add { name, who, role, people_only } => {
                let who = member_named(who)?;
                let role = fragment_proto::Role::parse(&role).ok_or_else(|| usage(format!("--role is viewer or editor, not {role:?}")))?;
                let v: Member = c.call_as(c.put_bytes(&format!("/api/f/{name}/members/{who}"), serde_json::to_vec(&fragment_proto::SetRole { role, people_only })?)?)?;
                json_exit(j, &v);
                println!("{} is now {} on {name}", v.principal, v.role.as_str());
                if let Some(owner) = &v.owner {
                    println!("  an agent: its owner {owner} can read {name} too");
                }
            }
            MembersCmd::Rm { name, who } => {
                let who = member_named(who)?;
                let v = c.call(c.delete(&format!("/api/f/{name}/members/{who}"))?)?;
                json_exit(j, &v);
                println!("removed {who} from {name}");
            }
            MembersCmd::Leave { name } => {
                let v = c.call(c.delete(&format!("/api/f/{name}/members/me"))?)?;
                json_exit(j, &v);
                println!("left {name}");
            }
        },
        Cmd::Invite { sub } => match sub {
            InviteCmd::Create { name, role, uses, ttl } => {
                let role = fragment_proto::Role::parse(&role).ok_or_else(|| usage(format!("--role is viewer or editor, not {role:?}")))?;
                let body = fragment_proto::CreateInvite { role, uses: Some(uses), ttl_s: ttl, invitee: None };
                let v: Invite = c.call_as(c.post_json(&format!("/api/f/{name}/invites"), &body)?)?;
                // the create is the one answer that carries the token
                let token = v.token.clone().ok_or_else(|| anyhow!("the host made invite {} but did not answer its token", v.id))?;
                // the link a person opens in a browser: the platform's join
                // page (they sign in, see what it grants, then join)
                let status: FragmentStatus = c.call_as(c.get(&format!("/api/f/{name}/status"))?)?;
                let link = format!("{}/join/{}?token={token}", platform_of(&c, &status), status.name);
                if j {
                    let mut out = serde_json::to_value(&v)?;
                    out["link"] = json!(link);
                    ok_exit(&out);
                }
                println!("invite {} ({}, {uses} use{})", v.id, v.role.as_str(), if uses == 1 { "" } else { "s" });
                println!("open in a browser: {link}");
                println!("or from a CLI: fragment join {name} {token}");
            }
            InviteCmd::List { name } => {
                let v: InviteList = c.call_as(c.get(&format!("/api/f/{name}/invites"))?)?;
                json_exit(j, &v);
                for i in &v.invites {
                    let expires_s = u64::try_from(i.expires_at / 1000).unwrap_or(0);
                    println!("{}\t{}\t{} left\texpires {}", i.id, i.role.as_str(), i.uses_left, chrono_like(expires_s));
                }
            }
            InviteCmd::Revoke { name, id } => {
                let v = c.call(c.delete(&format!("/api/f/{name}/invites/{id}"))?)?;
                json_exit(j, &v);
                println!("revoked invite {id}");
            }
        },
        Cmd::Join { name, token } => {
            let v = c.call(c.post_json(&format!("/api/f/{name}/join"), &json!({ "token": token }))?)?;
            json_exit(j, &v);
            if v["joined"].as_bool().unwrap_or(false) {
                println!("joined {name} as {}", v["role"].as_str().unwrap_or(""));
            } else {
                println!("already a member of {name} ({})", v["role"].as_str().unwrap_or(""));
            }
        }
        Cmd::Call { name, op, input, id } => {
            let input = match (input.as_str(), input.strip_prefix('@')) {
                ("-", _) => std::io::read_to_string(std::io::stdin()).map_err(|e| usage(format!("--input -: reading stdin: {e}")))?,
                (_, Some(path)) => std::fs::read_to_string(path).map_err(|e| usage(format!("--input @{path}: {e}")))?,
                _ => input,
            };
            let input: Value = serde_json::from_str(&input).map_err(|e| usage(format!("--input must be JSON: {e}")))?;
            let id = id.unwrap_or_else(|| format!("cli-{:016x}", rand::random::<u64>()));
            // the id makes the call safe to send again: it is retried like a
            // read, and a failure names it, so a retry by hand replays it too
            let call = fragment_proto::OpCall { id: id.clone(), input };
            let v: OpResult = c
                .post_json_by_id(&format!("/api/f/{name}/ops/{op}"), &call)
                .and_then(|r| c.call_as(r))
                .map_err(|e| e.context(CallId::call(&id)))?;
            json_exit(j, &json!({ "id": id, "result": v.result, "replayed": v.replayed }));
            println!("{}", serde_json::to_string_pretty(&v.result)?);
            if v.replayed {
                eprintln!("(replayed: operation {id} had already run)");
            }
        }
        Cmd::Blob { sub: BlobCmd::Put { name, file } } => {
            let bytes = std::fs::read(&file).with_context(|| format!("reading {}", file.display()))?;
            let sha = sync::sha256_hex(&bytes);
            let kind = fragment_core::site::mime_for_path(&file.to_string_lossy());
            let path = format!("/api/f/{name}/blobs/{sha}");
            let v = c.call(c.put_blob(&path, bytes, Some(kind))?)?;
            json_exit(j, &v);
            println!("{sha}");
        }
        Cmd::Post { name, channel, body, id } => {
            let body: Value = serde_json::from_str(&body).map_err(|e| usage(format!("--body must be JSON: {e}")))?;
            let id = id.unwrap_or_else(|| format!("cli-{:016x}", rand::random::<u64>()));
            // the id makes the post safe to send again, as a call's does
            let post = fragment_proto::PostRecord { id: id.clone(), body };
            let v: Posted = c
                .post_json_by_id(&format!("/api/f/{name}/channels/{channel}"), &post)
                .and_then(|r| c.call_as(r))
                .map_err(|e| e.context(CallId::post(&id)))?;
            json_exit(j, &json!({ "id": id, "record": v.record, "replayed": v.replayed }));
            println!("{}", serde_json::to_string(&v.record)?);
            if v.replayed {
                eprintln!("(replayed: post {id} had already appended this record)");
            }
        }
        Cmd::Channel { name, channel: None, .. } => {
            let v = c.call(c.get(&format!("/api/f/{name}/channels"))?)?;
            json_exit(j, &v);
            for ch in v["channels"].as_array().cloned().unwrap_or_default() {
                let signed_in = if ch["signedIn"] == true { " (signed in)" } else { "" };
                let post = ch["post"].as_str().map(|p| format!(", {p} posts{signed_in}")).unwrap_or_default();
                println!("{}\t{}{post}\t{} records", ch["name"].as_str().unwrap_or(""), ch["read"].as_str().unwrap_or(""), ch["seq"]);
            }
        }
        Cmd::Channel { name, channel: Some(channel), after, follow } => {
            if follow {
                if j {
                    return Err(usage("--follow streams JSON lines; --json does not apply"));
                }
                // the live socket takes a full name (`<label>.<username>`); the
                // signed API resolves a bare label to one of yours, so ask it
                let name = if name.contains('.') { name } else { c.call_as::<FragmentStatus>(c.get(&format!("/api/f/{name}/status"))?)?.name };
                watch::follow_channel(&c, &name, &channel, after)?;
                return Ok(());
            }
            let v: ChannelPage = c.call_as(c.get(&format!("/api/f/{name}/channels/{channel}?after={after}"))?)?;
            json_exit(j, &v);
            for r in &v.records {
                println!("{}", serde_json::to_string(r)?);
            }
        }
        Cmd::Visibility { name, value } => {
            let visibility = match value {
                None => c.call_as::<FragmentStatus>(c.get(&format!("/api/f/{name}/status"))?)?.visibility,
                Some(vis) => {
                    let visibility = Visibility::parse(&vis).ok_or_else(|| usage(format!("visibility is public, link, or members, not {vis:?}")))?;
                    c.call(c.put_bytes(&format!("/api/f/{name}/visibility"), serde_json::to_vec(&fragment_proto::SetVisibility { visibility })?)?)?;
                    visibility
                }
            };
            json_exit(j, &json!({ "visibility": visibility }));
            println!("{name}: {}", visibility.as_str());
        }
        Cmd::Login { .. } | Cmd::Host { .. } | Cmd::Guide | Cmd::Skill | Cmd::New { .. } => unreachable!(),
    }
    Ok(())
}

/// A share link: the canonical URL (with its trailing slash: a path-mode
/// URL without one redirects) and the view token.
fn share_link(canonical: &str, token: &str) -> String {
    let base = if canonical.ends_with('/') { canonical.to_string() } else { format!("{canonical}/") };
    format!("{base}?view={token}")
}

/// What `fragment deploy` did.
enum Deployed {
    /// the folder's sync refused a mass deletion, so nothing moved
    Guarded(sync::Report),
    /// an ephemeral ref at main's tip
    Preview { synced: Option<sync::Report>, slug: String, sha: String },
    Live { synced: Option<sync::Report>, live_tip: String, main_tip: String },
}

/// `fragment deploy`: syncs `dir` first when one is given, then points
/// live (or a new preview ref) at main's tip. One storage token serves the
/// sync and the ref move, main's head is the one the sync ended on, and
/// the cell's pins get one nudge for everything that moved.
fn deploy(c: &api::Client, name: &str, dir: Option<&Path>, note: Option<&str>, preview: bool, codestorage: Option<&str>) -> Result<Deployed> {
    let writer = writer_id(c);
    let storage = CodeStorage::connect(c, name, codestorage).map_err(cs_anyhow)?;
    let synced = match dir {
        Some(dir) => Some(sync::pass(c, &storage, name, dir, &SyncOptions { writer_id: writer.clone(), ..Default::default() }).map_err(cs_anyhow)?),
        None => None,
    };
    let (main_tip, landed) = match synced {
        Some(report) if report.mass_delete_guard.is_some() => return Ok(Deployed::Guarded(report)),
        // fragment.json rides the commit (it is a git file at the repo
        // root): files and machinery go live together
        Some(ref report) => (report.head.clone(), report.landed),
        None => (storage.branch_head(MAIN).map_err(cs_anyhow)?, false),
    };
    let main_tip = main_tip.ok_or_else(|| anyhow!("nothing to deploy: main has no commits (sync a folder with --dir first)"))?;
    if preview {
        // ephemeral ref at main's tip: unguessable, invisible to clones,
        // promoted by deploying. There is no served URL — the ref IS the
        // preview.
        let slug = format!("preview/{:012x}", rand::random::<u64>());
        let sha = storage.create_branch(&main_tip, &slug, true).map_err(cs_anyhow)?;
        // a preview moves no pin, but the sync's commit moved main
        if landed {
            sync::refresh_pins(c, name);
        }
        return Ok(Deployed::Preview { synced, slug, sha });
    }
    // the platform moves live (POST …/deploy), under the fragment's plane
    // lock: after a rollback that takes two steps (docs/api.md), and no
    // pin ever serves the files live holds between them
    let r = c.post_json(&format!("/api/f/{name}/deploy"), &json!({ "note": note }))?;
    let v: Value = c.call_as(r)?;
    let live_tip = v["live"].as_str().filter(|s| !s.is_empty()).ok_or_else(|| anyhow!("the platform's deploy named no live commit: {v}"))?.to_string();
    // the sync's commit and the live move, in one nudge: serving sees THIS
    // deploy now, not at the next poll backstop
    sync::refresh_pins(c, name);
    Ok(Deployed::Live { synced, live_tip, main_tip })
}

fn writer_id(c: &api::Client) -> String {
    c.writer_id()
}

/// typed sync/code.storage errors -> anyhow. CAS rejections map to the
/// stable "conflict" machine code, and a write whose answer was lost to
/// "outcome_unknown", so `--json` consumers can branch.
fn cs_anyhow(e: impl Into<crate::sync::SyncError>) -> anyhow::Error {
    let e = e.into();
    match e {
        crate::sync::SyncError::Cs(CsError::CasRejected { .. }) => anyhow::Error::new(CodedError { code: Code::Conflict, msg: e.to_string() }),
        crate::sync::SyncError::Cs(CsError::OutcomeUnknown(_)) => anyhow::Error::new(CodedError { code: Code::OutcomeUnknown, msg: e.to_string() }),
        // the host's refusal keeps its code (a viewer's sync is forbidden)
        crate::sync::SyncError::Cs(CsError::Host(refused)) | crate::sync::SyncError::Host(refused) => anyhow::Error::new(refused),
        other => anyhow!("{other}"),
    }
}

/// unix seconds -> "YYYY-MM-DD HH:MM:SSZ" (UTC)
fn chrono_like(secs: u64) -> String {
    let (y, m, d) = fragment_core::cron::civil((secs / 86_400) as i64);
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}Z", secs % 86_400 / 3600, secs % 3600 / 60, secs % 60)
}

// ---------- sync unit (keep a folder live without a terminal) ----------
// Writes a LaunchAgent (macOS) or systemd user unit (Linux) for one
// fragment+folder pair. Uses the CLI's own absolute path and an explicit
// PATH — launchd and systemd both run with minimal environments (the
// agent-built watch.sh failed on exactly this).

fn install_sync_unit(name: &str, dir: &Path, install: bool, mirror_from: Option<&Path>) -> Result<()> {
    let dir = match dir.canonicalize() {
        Ok(d) => d,
        Err(_) => anyhow::bail!("no such directory: {}", dir.display()),
    };
    // the unit runs in `dir`, so a relative source is made absolute here
    let mirror_from = match mirror_from.filter(|_| install) {
        Some(m) => Some(m.canonicalize().with_context(|| format!("no such directory: {}", m.display()))?),
        None => None,
    };
    let home = std::env::var("HOME").context("HOME not set")?;
    let exe = std::env::current_exe()
        .and_then(|p| p.canonicalize())
        .context("cannot resolve the fragment binary path")?;
    let log = dir.join(".fragment").join("watch.log");

    if cfg!(target_os = "macos") {
        let label = format!("sh.finite.fragment-sync.{name}");
        let plist_dir = PathBuf::from(&home).join("Library").join("LaunchAgents");
        let plist = plist_dir.join(format!("{label}.plist"));
        let _ = std::process::Command::new("launchctl")
            .arg("bootout")
            .arg(format!("gui/{}/{}", uid()?, label))
            .status();
        if install {
            std::fs::create_dir_all(&plist_dir)?;
            std::fs::create_dir_all(dir.join(".fragment"))?;
            std::fs::write(&plist, sync_unit_file(true, name, &exe, &dir, mirror_from.as_deref(), &home))?;
            // bootstrap can race the bootout above (async port teardown) —
            // give it a beat and retry once before giving up
            let mut ok = false;
            for attempt in 0..2 {
                if attempt > 0 {
                    std::thread::sleep(std::time::Duration::from_millis(1200));
                }
                let status = std::process::Command::new("launchctl")
                    .arg("bootstrap")
                    .arg(format!("gui/{}", uid()?))
                    .arg(&plist)
                    .status()
                    .context("launchctl bootstrap failed")?;
                if status.success() {
                    ok = true;
                    break;
                }
            }
            if !ok {
                anyhow::bail!("launchctl bootstrap failed — try: launchctl bootstrap gui/{} {}", uid()?, plist.display());
            }
            println!("installed LaunchAgent {label}");
            println!("  syncs {name} <-> {} every 3s, starting now and after reboot", dir.display());
            println!("  log: {}", log.display());
            println!("  remove with: fragment sync {name} --dir {} --uninstall", dir.display());
        } else {
            let _ = std::fs::remove_file(&plist);
            println!("removed LaunchAgent {label}");
        }
    } else {
        let unit = format!("fragment-sync-{name}.service");
        let dir_units = PathBuf::from(&home).join(".config").join("systemd").join("user");
        let path = dir_units.join(&unit);
        let _ = std::process::Command::new("systemctl")
            .args(["--user", "disable", "--now", &unit])
            .status();
        if install {
            std::fs::create_dir_all(&dir_units)?;
            std::fs::create_dir_all(dir.join(".fragment"))?;
            std::fs::write(&path, sync_unit_file(false, name, &exe, &dir, mirror_from.as_deref(), &home))?;
            let run = |args: &[&str]| -> Result<()> {
                let st = std::process::Command::new("systemctl")
                    .arg("--user")
                    .args(args)
                    .status()
                    .with_context(|| format!("systemctl --user {:?}", args))?;
                if !st.success() { anyhow::bail!("systemctl --user {:?} failed", args); }
                Ok(())
            };
            run(&["daemon-reload"])?;
            run(&["enable", "--now", &unit])?;
            println!("installed systemd user unit {unit}");
            println!("  log: journalctl --user -u {unit} -f");
            println!("  remove with: fragment sync {name} --dir {} --uninstall", dir.display());
        } else {
            let _ = std::fs::remove_file(&path);
            let _ = std::process::Command::new("systemctl").args(["--user", "daemon-reload"]).status();
            println!("removed systemd user unit {unit}");
        }
    }
    Ok(())
}

/// Writes a template's files into `dir`, never over a file that exists:
/// the paths it wrote, and how many it left alone.
fn scaffold(dir: &Path, tpl_name: &str) -> Result<(Vec<&'static str>, usize)> {
    let (_, files) = TEMPLATES.iter().find(|(n, _)| *n == tpl_name).ok_or_else(|| usage(format!("unknown template '{tpl_name}' (use `fragment new --list`)")))?;
    if dir.exists() && !dir.is_dir() {
        anyhow::bail!("{} exists and is not a directory", dir.display());
    }
    let (mut created, mut skipped) = (Vec::new(), 0);
    for (rel, bytes) in files.iter() {
        let target = dir.join(rel);
        if target.exists() {
            skipped += 1;
            continue;
        }
        std::fs::create_dir_all(target.parent().unwrap_or(dir))?;
        std::fs::write(&target, bytes)?;
        created.push(*rel);
    }
    Ok((created, skipped))
}

/// The unit file that keeps `dir` synced: a LaunchAgent plist on macOS, a
/// systemd user unit elsewhere. It runs this CLI by its absolute path with
/// `sync --watch`, and with `--mirror-from` when the install had one.
fn sync_unit_file(macos: bool, name: &str, exe: &Path, dir: &Path, mirror_from: Option<&Path>, home: &str) -> String {
    let mut args = vec![exe.display().to_string(), "sync".into(), name.into(), "--dir".into(), dir.display().to_string(), "--watch".into()];
    if let Some(m) = mirror_from {
        args.extend(["--mirror-from".into(), m.display().to_string()]);
    }
    let homebin = format!("{home}/.local/bin:{home}/.cargo/bin");
    if !macos {
        return r#"[Unit]
Description=fragment sync __NAME__
After=network-online.target

[Service]
ExecStart=__ARGS__
WorkingDirectory=__DIR__
Environment=PATH=__HOMEBIN__:/usr/local/bin:/usr/bin:/bin
Restart=always

[Install]
WantedBy=default.target
"#
        .replace("__NAME__", name)
        .replace("__ARGS__", &args.join(" "))
        .replace("__DIR__", &dir.display().to_string())
        .replace("__HOMEBIN__", &homebin);
    }
    r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>__LABEL__</string>
  <key>ProgramArguments</key>
  <array>
__ARGS__  </array>
  <key>WorkingDirectory</key><string>__DIR__</string>
  <key>EnvironmentVariables</key>
  <dict>
    <key>PATH</key><string>__HOMEBIN__:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin</string>
    <key>HOME</key><string>__HOME__</string>
  </dict>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>StandardOutPath</key><string>__LOG__</string>
  <key>StandardErrorPath</key><string>__LOG__</string>
</dict>
</plist>
"#
    .replace("__LABEL__", &format!("sh.finite.fragment-sync.{name}"))
    .replace("__ARGS__", &args.iter().map(|a| format!("    <string>{a}</string>\n")).collect::<String>())
    .replace("__DIR__", &dir.display().to_string())
    .replace("__HOMEBIN__", &homebin)
    .replace("__HOME__", home)
    .replace("__LOG__", &dir.join(".fragment").join("watch.log").display().to_string())
}

fn uid() -> Result<String> {
    let out = std::process::Command::new("id").arg("-u").output().context("id -u failed")?;
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn code_of(r: Result<impl std::fmt::Debug>) -> Code {
        classify_err(&r.expect_err("a failure"))
    }

    /// Goal: the agent mode is on only when the computer names an agent, and
    /// says exactly what is wrong otherwise. Valid: an agent, alone or acting
    /// for an identity. Invalid: a name that is no agent fragment's, a `for`
    /// that is no identity, a `for` with no agent.
    #[test]
    fn the_agent_mode_reads_what_the_computer_names() {
        let s = |v: &str| Some(v.to_string());
        assert_eq!(agent_mode_of(None, None).unwrap(), None, "no agent: this machine's key signs");
        assert_eq!(agent_mode_of(s(""), s("  ")).unwrap(), None, "empty is unset");
        assert_eq!(agent_mode_of(s("juniper.paul"), None).unwrap(), Some(api::AgentMode { agent: "juniper.paul".into(), acting_for: None }));
        let id = "id:0123456789abcdef0123456789abcdef";
        assert_eq!(agent_mode_of(s(" juniper.paul "), s(id)).unwrap(), Some(api::AgentMode { agent: "juniper.paul".into(), acting_for: s(id) }));
        for bad in ["juniper", "Juniper.Paul", "a/b.paul", "juniper.paul?for=x"] {
            assert_eq!(code_of(agent_mode_of(s(bad), None)), Code::InvalidUsage, "{bad}");
        }
        for bad in ["paul", "id:", "id:a b", "npub1xyz", "id:paul&x=1"] {
            assert_eq!(code_of(agent_mode_of(s("juniper.paul"), s(bad))), Code::InvalidUsage, "{bad}");
        }
        assert_eq!(code_of(agent_mode_of(None, s(id))), Code::InvalidUsage, "for whom, with no agent?");
    }

    /// An agent reaches the platform only through its computer: `--host`,
    /// then `FRAGMENT_HOST`, then `FRAGMENT_API`, and never a default.
    #[test]
    fn an_agent_calls_its_computers_api() {
        let s = |v: &str| Some(v.to_string());
        assert_eq!(agent_host(&s("http://a"), s("http://b"), s("http://c")).unwrap(), "http://a");
        assert_eq!(agent_host(&None, s("http://b"), s("http://c")).unwrap(), "http://b");
        assert_eq!(agent_host(&None, None, s("http://api.fragment.internal")).unwrap(), "http://api.fragment.internal");
        assert_eq!(code_of(agent_host(&None, None, None)), Code::InvalidUsage);
        assert_eq!(code_of(agent_host(&None, s(" "), None)), Code::InvalidUsage);
    }

    /// A link a person opens names the platform's own origin, which a
    /// status says, never the internal host a computer's CLI calls.
    #[test]
    fn links_for_people_name_the_platform() {
        let status = |platform: &str| -> FragmentStatus {
            serde_json::from_value(json!({
                "name": "g.paul", "npub": "n", "owner": "id:p", "role": "owner", "visibility": "link", "repo": "r",
                "pins": { "main": null, "live": null }, "counts": { "files": 0, "events": 0, "members": 1 },
                "code": { "sha": null, "operations": {}, "error": null }, "viewToken": null, "inboxToken": null,
                "urls": { "canonical": "https://g--paul.fragment.boats/", "platform": platform },
            }))
            .expect("a status")
        };
        let agent = api::Client::new("http://api.fragment.internal", api::Signer::Agent(api::AgentMode { agent: "j.paul".into(), acting_for: None }));
        assert_eq!(platform_of(&agent, &status("https://fragment.club/")), "https://fragment.club");
        assert_eq!(platform_of(&agent, &status("")), "http://api.fragment.internal", "an older host names none");
    }

    /// Goal: a deploy of a folder mints one storage token, reads main's
    /// head once (its sync's), lists once, and nudges the pins once.
    /// Method: count the fake's requests for a first deploy and for one
    /// after an edit. Before, each minted two tokens, read main's head four
    /// times, listed twice, and refreshed twice.
    #[test]
    fn a_deploy_mints_one_token_and_refreshes_once() {
        let mock = crate::mockcs::start();
        mock.seed_repo("t", &[]);
        let c = api::Client::new(&mock.url, auth::fixed(7));
        let dir = std::env::temp_dir().join(format!("fragment-deploy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("site")).unwrap();
        std::fs::write(dir.join("site/index.html"), "<h1>one</h1>").unwrap();
        mock.take_requests("");
        let count = |routes: &[(&str, u32)]| -> std::collections::BTreeMap<String, u32> { routes.iter().map(|(r, n)| (r.to_string(), *n)).collect() };

        let Deployed::Live { live_tip, main_tip, .. } = deploy(&c, "t", Some(&dir), None, false, None).unwrap() else { panic!("a live deploy") };
        assert_eq!((Some(&live_tip), Some(&main_tip)), (mock.branch("t", "live").as_ref(), mock.branch("t", "main").as_ref()));
        assert_eq!(
            mock.take_requests(""),
            count(&[("GET storage-token", 1), ("GET branch", 1), ("GET files/metadata", 1), ("POST commit-pack", 1), ("POST deploy", 1), ("POST refresh", 1)]),
            "main's head, one listing, one token, the platform's deploy, one nudge"
        );

        std::fs::write(dir.join("site/index.html"), "<h1>two</h1>").unwrap();
        let Deployed::Live { live_tip, .. } = deploy(&c, "t", Some(&dir), Some("two"), false, None).unwrap() else { panic!("a live deploy") };
        assert_eq!(mock.file_at("t", "live", "site/index.html").unwrap(), b"<h1>two</h1>");
        assert_eq!(Some(live_tip), mock.branch("t", "live"));
        assert_eq!(
            mock.take_requests(""),
            count(&[("GET storage-token", 1), ("GET branch", 1), ("GET files/metadata", 1), ("POST commit-pack", 1), ("POST deploy", 1), ("POST refresh", 1)])
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    fn coded(code: Code) -> anyhow::Error {
        anyhow::Error::new(CodedError { code, msg: "the answer was lost".into() })
    }

    /// Goal: a failed operation call always names its id, and one whose
    /// outcome is unknown says to call it again with that id. Method: the
    /// envelope of a call's outcome_unknown, of its refusal, and of the same
    /// outcome_unknown from a write with no id.
    #[test]
    fn a_failed_call_names_its_id() {
        let (err, code) = error_body(&coded(Code::OutcomeUnknown).context(CallId::call("cli-00000000000000ab")));
        assert_eq!((err["code"].as_str(), code.exit_status()), (Some("outcome_unknown"), 1));
        assert_eq!(err["id"], "cli-00000000000000ab");
        assert!(err["hint"].as_str().is_some_and(|h| h.contains("--id cli-00000000000000ab")), "{err}");
        assert!(err["message"].as_str().is_some_and(|m| m.starts_with("operation id cli-00000000000000ab: ")), "{err}");

        let (err, _) = error_body(&coded(Code::Forbidden).context(CallId::call("mine")));
        assert_eq!((err["code"].as_str(), err["id"].as_str()), (Some("forbidden"), Some("mine")));
        assert!(!err["hint"].as_str().unwrap_or("").contains("--id"), "a refusal is not retried by id: {err}");

        let (err, _) = error_body(&coded(Code::OutcomeUnknown));
        assert!(err.get("id").is_none() && !err["hint"].as_str().unwrap_or("").contains("--id"), "{err}");
    }

    /// Goal: `fragment host <url>` changes the host and keeps the config's
    /// other keys (it rewrote the file with `host` and `secret_key` only,
    /// so `codestorage` and `agents` were lost). Method: the config lives
    /// under HOME, so this test runs the command in a copy of itself with a
    /// HOME of its own, then reads the file.
    #[test]
    fn host_keeps_the_other_config_keys() {
        if std::env::var_os("FRAGMENT_TEST_RUN_HOST").is_some() {
            return run(Cli::parse_from(["fragment", "host", "http://127.0.0.1:2"])).unwrap();
        }
        let home = std::env::temp_dir().join(format!("fragment-host-config-{}", std::process::id()));
        let dir = home.join(if cfg!(target_os = "macos") { "Library/Application Support" } else { ".config" }).join("fragment");
        std::fs::create_dir_all(&dir).unwrap();
        let config = |host: &str| json!({ "host": host, "secret_key": "07".repeat(32), "codestorage": "http://127.0.0.1:3", "agents": "http://127.0.0.1:4" });
        std::fs::write(dir.join("config.json"), config("http://127.0.0.1:1").to_string()).unwrap();
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "tests::host_keeps_the_other_config_keys"])
            .env("FRAGMENT_TEST_RUN_HOST", "1")
            .env("HOME", &home)
            .env_remove("XDG_CONFIG_HOME")
            .env_remove("FRAGMENT_OUTPUT")
            .output()
            .unwrap();
        let after: Value = serde_json::from_slice(&std::fs::read(dir.join("config.json")).unwrap()).unwrap();
        std::fs::remove_dir_all(&home).ok();
        assert!(child.status.success(), "{}", String::from_utf8_lossy(&child.stdout));
        assert_eq!(after, config("http://127.0.0.1:2"));
    }

    /// Goal: `sync --install --mirror-from <src>` installs a unit that runs
    /// with `--mirror-from <src>`, as a LaunchAgent and as a systemd unit.
    /// Method: both unit files, with a source and without. Neither template
    /// had a place for it, so an installed watcher never overlaid its
    /// source.
    #[test]
    fn an_installed_unit_carries_its_mirror_source() {
        let unit = |macos, src: Option<&str>| sync_unit_file(macos, "n", Path::new("/bin/fragment"), Path::new("/notes"), src.map(Path::new), "/home/p");
        let plist = unit(true, Some("/vault"));
        assert!(plist.contains("    <string>--watch</string>\n    <string>--mirror-from</string>\n    <string>/vault</string>\n  </array>"), "{plist}");
        let systemd = unit(false, Some("/vault"));
        assert!(systemd.contains("\nExecStart=/bin/fragment sync n --dir /notes --watch --mirror-from /vault\n"), "{systemd}");
        assert!(!unit(true, None).contains("--mirror-from") && !unit(false, None).contains("--mirror-from"));
    }
}
