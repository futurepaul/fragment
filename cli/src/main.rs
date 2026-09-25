mod api;
mod auth;
mod blobs;
mod builder;
mod codestorage;
mod sync;
mod watch;

#[cfg(test)]
mod mockcs;

use fragment_templates::ALL as TEMPLATES;

use crate::api::{encode_q, Code, CodedError};
use crate::codestorage::{Author, CodeStorage, CsError, LIVE, MAIN, MAX_CAS_ATTEMPTS};
use crate::sync::{Mode, SyncOptions};
use anyhow::{anyhow, Context, Result};
use clap::{Parser, Subcommand};
use fragment_core::budget::dollars;
use fragment_proto::limits::AGENT_STATE_WAIT_MS_MAX;
use fragment_proto::{
    AgentState, BudgetView, ChannelPage, Created, FragmentList, FragmentStatus, IdentityView, Invite, InviteList, Member, MemberList, OpResult, Posted,
    Rotated, Run, RunList, TurnOutcome, Visibility,
};
use serde::Serialize;
use serde_json::{json, Value};
use std::io::Read;
use std::path::{Path, PathBuf};

const GUIDE: &str = include_str!("../GUIDE.md");

#[derive(Parser)]
#[command(name = "fragment", version, about = "make and run fragments: a folder in git, an app of operations and jobs, channels, members; on celld")]
struct Cli {
    /// Host base URL (else FRAGMENT_HOST, else config, else http://127.0.0.1:8790)
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
    /// Create a fragment
    Create {
        name: String,
        /// public | link (default) | members
        #[arg(long)]
        visibility: Option<String>,
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
    /// Compile a fragment folder: TypeScript sources -> runnable files,
    /// hashed site assets, and a parse gate on everything served
    Build {
        /// The fragment folder (default: current directory)
        dir: Option<String>,
    },
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
    /// Your AI budget this month: what is left, and what spent it (the
    /// fragments you own pay for their AI unless they have their own key)
    Budget {
        #[command(subcommand)]
        sub: Option<BudgetCmd>,
    },
    /// Pause an operation's triggers (calls still work)
    Pause { name: String, op: String },
    /// Unpause an operation's triggers (also after an auto-pause)
    Unpause { name: String, op: String },
    /// Re-run a held or blocked run with its original input (after fixing the code)
    Replay { name: String, run: u64 },
    /// Rotate a fragment's tokens (owner-only; default: the inbox token and the share link)
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
    /// A computer an agent works on: `fragment computer serve` runs goose's
    /// developer tools for the agent that attaches it (`fragment agent computer`)
    Computer {
        #[command(subcommand)]
        sub: ComputerCmd,
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
        /// The input, as JSON
        #[arg(long, default_value = "{}")]
        input: String,
        /// The operation id (default: a fresh one)
        #[arg(long)]
        id: Option<String>,
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
        /// An OpenRouter model id (default z-ai/glm-5.3-flash)
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
    /// Attach a computer (`fragment computer serve`): its shell and file tools join the agent's turns
    Computer {
        name: String,
        /// The computer's URL (`fragment computer serve`)
        #[arg(long, required_unless_present_any = ["detach", "connect"])]
        url: Option<String>,
        /// A file holding its token: the one `fragment computer serve` made,
        /// or, with --connect, where to write the new connect token
        #[arg(long, required_unless_present = "detach")]
        token_file: Option<PathBuf>,
        /// The computer connects out to the agent instead (`fragment computer
        /// connect`): no public URL. Writes its connect token to --token-file
        #[arg(long, conflicts_with = "url")]
        connect: bool,
        /// The project directory its tools work in, on the computer
        #[arg(long, default_value = "work")]
        cwd: String,
        /// Detach the computer instead
        #[arg(long, conflicts_with_all = ["url", "token_file", "connect"])]
        detach: bool,
    },
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
enum ComputerCmd {
    /// Answer an agent without a public URL: poll it for work, and run it here
    /// (goose's developer tools and screenshots)
    Connect {
        /// The agent: its URL (`fragment agent computer --connect` prints it)
        #[arg(long)]
        agent: String,
        /// The connect token its owner was given
        #[arg(long)]
        token_file: PathBuf,
        /// Projects live under this directory, one per attached cwd
        #[arg(long, default_value = "work")]
        work: PathBuf,
        /// The call journal
        #[arg(long, default_value = ".fragment-computer")]
        state: PathBuf,
    },
    /// Serve goose's developer tools (shell, write, edit, tree) over HTTP to the agent that attaches this computer
    Serve {
        #[arg(long, default_value = "0.0.0.0:8080")]
        listen: std::net::SocketAddr,
        /// Projects live under this directory, one per attached cwd
        #[arg(long, default_value = "work")]
        work: PathBuf,
        /// The call journal
        #[arg(long, default_value = ".fragment-computer")]
        state: PathBuf,
        /// The bearer token an agent presents; made here (0600) if missing
        #[arg(long, default_value = ".fragment-computer/token")]
        token_file: PathBuf,
    },
}

#[derive(Subcommand)]
enum BudgetCmd {
    /// Every paid step this month (or --period YYYY-MM)
    Usage {
        #[arg(long)]
        period: Option<String>,
    },
    /// Add dollars to someone's month (the fleet's operators)
    TopUp { who: String, usd: f64 },
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
enum MembersCmd {
    /// List members and their roles
    List { name: String },
    /// Add a member, or change their role (owner only)
    Add {
        name: String,
        /// identity (id:…), npub, 64-hex key, or NIP-05 name (name@domain)
        who: String,
        /// viewer | editor
        #[arg(long, default_value = "viewer")]
        role: String,
    },
    /// Remove a member (owner only)
    Rm { name: String, who: String },
    /// Leave a fragment you are a member of
    Leave { name: String },
}

#[derive(Subcommand)]
enum InviteCmd {
    /// Make an invite (owner only); prints the token once
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
    /// List open invites (owner only; tokens are never shown again)
    List { name: String },
    /// Revoke an invite by id (owner only)
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

/// What a person's identity looks like in a terminal.
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
        .unwrap_or_else(|| "http://127.0.0.1:8790".to_string())
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

fn require_client(cli_host: &Option<String>, verbose: bool) -> Result<api::Client> {
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
            let mut done = me()?;
            if done.is_none() {
                // the link carries this key's own proof (ten minutes good):
                // approving it in a signed-in browser adds the key at once
                let npub = c.id.npub();
                let proof = c.id.nip98_header("POST", &format!("{}/cli/approve", c.host), &[]);
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
            json_exit(j, &json!({ "npub": c.id.npub(), "id": v.id, "host": c.host, "config": config_path().display().to_string(), "existing": key_existed }));
            println!("logged in as {} on {}", v.id, c.host);
            println!("key: {}", c.id.npub());
            return Ok(());
        }
        Cmd::Username { username, release } => {
            let c = require_client(&cli.host, cli.verbose)?;
            if release {
                let u = username.unwrap_or_default();
                let v = c.call(c.delete(&format!("/api/users/{u}"))?)?;
                if j {
                    ok_exit(&v);
                }
                println!("released {u} ({}): its person chooses a username again", v["identity"].as_str().unwrap_or(""));
                return Ok(());
            }
            let v = match username {
                Some(u) => c.call(c.put_json("/api/identities/me/username", &json!({ "username": u }))?)?,
                None => c.call(c.get("/api/identities/me")?)?,
            };
            if j {
                ok_exit(&v);
            }
            match v["username"].as_str() {
                Some(u) => println!("username: {u}"),
                None => println!("no username yet: fragment username <name>"),
            }
            return Ok(());
        }
        Cmd::Whoami | Cmd::Keys { sub: None | Some(KeysCmd::List) } => {
            let c = require_client(&cli.host, cli.verbose)?;
            let v: IdentityView = c.call_as(c.get("/api/identities/me")?)?;
            if j {
                ok_exit(&json!({ "npub": c.id.npub(), "host": c.host, "identity": v }));
            }
            print_identity(&v, &c.id.npub());
            println!("host: {}", c.host);
            return Ok(());
        }
        Cmd::Keys { sub: Some(KeysCmd::Rotate) } => {
            let old = require_client(&cli.host, cli.verbose)?;
            let fresh = auth::Identity::generate();
            // 1. the old key adds the new one, which proves itself inside
            let url = format!("{}/api/identities/me/keys", old.host);
            let proof = fresh.proof("POST", &url, old.id.pubkey_hex());
            old.call_as::<IdentityView>(old.post_json("/api/identities/me/keys", &json!({ "proof": proof }))?)?;
            // 2. this machine switches to it (both keys work until step 3)
            save_config("secret_key", &fresh.secret_hex())?;
            let new = require_client(&cli.host, cli.verbose)?;
            // 3. the new key revokes the old one
            let revoked = new.call_as::<IdentityView>(new.delete(&format!("/api/identities/me/keys/{}", old.id.pubkey_hex()))?);
            if let Err(e) = &revoked {
                eprintln!("the new key is in use, but revoking the old one failed: {e}\nrevoke it: fragment keys revoke {}", old.id.npub());
            }
            let v = revoked?;
            if j {
                ok_exit(&json!({ "npub": new.id.npub(), "revoked": old.id.npub(), "identity": v }));
            }
            println!("{} replaces {} (revoked); every grant stays with {}", new.id.npub(), old.id.npub(), v.id);
            return Ok(());
        }
        Cmd::Keys { sub: Some(KeysCmd::Revoke { npub }) } => {
            let c = require_client(&cli.host, cli.verbose)?;
            let hex = fragment_core::npub::parse(&npub).ok_or_else(|| anyhow!("{npub} is not an npub or a 64-hex key"))?;
            if hex == c.id.pubkey_hex() {
                anyhow::bail!("that is the key this machine signs with: rotate it instead (`fragment keys rotate`)");
            }
            let v: IdentityView = c.call_as(c.delete(&format!("/api/identities/me/keys/{hex}"))?)?;
            if j {
                ok_exit(&v);
            }
            println!("revoked {npub}");
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
        Cmd::Computer { sub: ComputerCmd::Connect { agent, token_file, work, state } } => {
            #[cfg(feature = "computer")]
            return fragment_computer::connect(fragment_computer::ConnectArgs { agent, work, state, token_file });
            #[cfg(not(feature = "computer"))]
            {
                let _ = (agent, work, state, token_file);
                anyhow::bail!("this fragment was built without computers: cargo install --path cli --features computer");
            }
        }
        Cmd::Computer { sub: ComputerCmd::Serve { listen, work, state, token_file } } => {
            #[cfg(feature = "computer")]
            return fragment_computer::serve(fragment_computer::ServeArgs { listen, work, state, token_file });
            #[cfg(not(feature = "computer"))]
            {
                let _ = (listen, work, state, token_file);
                anyhow::bail!("this fragment was built without computers: cargo install --path cli --features computer");
            }
        }
        _ => {}
    }

    let c = require_client(&cli.host, cli.verbose)?;

    match cli.cmd {
        Cmd::Create { name, visibility } => {
            // the fragment's own key is made by the platform (its KEYS)
            let visibility = match visibility.as_deref() {
                Some(v) => Some(Visibility::parse(v).ok_or_else(|| usage(format!("--visibility is public, link, or members, not {v:?}")))?),
                None => None,
            };
            let body = fragment_proto::CreateFragment { name: name.clone(), visibility, template: None };
            let v: Created = c.call_as(c.post_json("/api/fragments", &body)?)?;
            json_exit(j, &v);
            println!("created fragment {}", v.name);
            println!("  npub:         {}", v.npub);
            println!("  share link:   {}", share_link(&v.canonical, &v.view_token));
            println!("  webhook URL:  {}/api/f/{}/inbox?t={}", c.host, v.name, v.inbox_token);
            println!("  canonical:   {}", v.canonical);
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
            name, dir, watch, mode, prune, mirror_from, apply_mass_delete,
            rebuild_state, no_live, install, uninstall,
        } => {
            // never stream JSON envelopes mid-run: watch prints progress
            // lines forever; a json consumer would choke on line 2
            if j && watch {
                return Err(usage("sync --watch streams progress lines continuously, so --json does not apply: run single passes with --json (`fragment sync <name> --dir .`), or drop --json to watch"));
            }
            if install || uninstall {
                install_sync_unit(&name, &dir, install)?;
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
                    Some("push") => Mode::Push,
                    Some("pull") => Mode::Pull,
                    Some("mirror") | None => Mode::Mirror,
                    other => return Err(usage(format!("--mode must be push|pull|mirror, got {other:?}"))),
                },
                apply_mass_delete,
                prune,
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

        Cmd::Build { dir } => {
            let dir = match dir {
                Some(d) => std::path::PathBuf::from(d),
                None => std::env::current_dir()?,
            };
            builder::run(&dir)?;
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
            let webhook = st.inbox_token.as_ref().map(|tok| format!("{}/api/f/{}/inbox?t={tok}", c.host, name));
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
        Cmd::Budget { sub } => match sub {
            None => {
                let v: BudgetView = c.call_as(c.get("/api/budget")?)?;
                json_exit(j, &v);
                println!(
                    "{}: {} of {} left ({} spent{})",
                    v.period,
                    dollars(v.remaining_micros.max(0)),
                    dollars(v.allowance_micros),
                    dollars(v.spent_micros),
                    if v.reserved_micros > 0 { format!(", {} held by steps running now", dollars(v.reserved_micros)) } else { String::new() }
                );
                if v.warn {
                    println!("most of this month's budget is used: paid steps stop when it runs out");
                }
                for u in v.usage.iter().take(10) {
                    println!("  {}\t{}\t{}\t{}", dollars(u.quantity), u.kind, u.fragment, u.state.as_str());
                }
            }
            Some(BudgetCmd::Usage { period }) => {
                let path = match period {
                    Some(p) => format!("/api/budget/usage?period={}", encode_q(&p)),
                    None => "/api/budget/usage".to_string(),
                };
                let v: BudgetView = c.call_as(c.get(&path)?)?;
                json_exit(j, &v);
                for u in &v.usage {
                    println!("{}\t{}\t{}\t{}\t{}", dollars(u.quantity), u.state.as_str(), u.kind, u.model.as_deref().unwrap_or(""), u.source_ref);
                }
            }
            Some(BudgetCmd::TopUp { who, usd }) => {
                let v: BudgetView = c.call_as(c.post_json(&format!("/api/budget/{who}/top-up"), &json!({ "usd": usd }))?)?;
                json_exit(j, &v);
                println!("{who}: {} of {} left in {}", dollars(v.remaining_micros), dollars(v.allowance_micros), v.period);
            }
        },
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
            // flags narrow the default both-scopes rotation
            let mut scopes: Vec<&str> = Vec::new();
            if inbox {
                scopes.push("inbox");
            }
            if view {
                scopes.push("view");
            }
            if scopes.is_empty() {
                // the webhook secret is code.storage's to know: rotate it only by asking the cell
                scopes = vec!["inbox", "view"];
            }
            let body = json!({ "scopes": scopes });
            let v: Rotated = c.call_as(c.post_json(&format!("/api/f/{name}/rotate"), &body)?)?;
            // the whole answer, webhook secret included: the code.storage
            // push HMAC is only ever visible at create/rotate, and machine
            // consumers (dev harnesses registering push webhooks) need it
            json_exit(j, &v);
            let canon = format!("{}/f/{}/", c.host, name);
            println!("rotated: {}", v.rotated.join(", "));
            println!("New webhook URL: {}/api/f/{}/inbox?t={}", c.host, name, v.inbox_token);
            println!("New share link: {}", share_link(&canon, &v.view_token));
        }
        Cmd::Secret { sub } => match sub {
            SecretCmd::Set { name, key, value: argv_value } => {
                // value from argv, else env var of the same name, else stdin
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
            let resp = reqwest::blocking::Client::new()
                .post(&url)
                .body(body)
                .header("content-type", "application/json")
                .send()?;
            let resp = api::Resp { status: resp.status().as_u16(), body: resp.bytes()?.to_vec() };
            let v = c.call(resp)?;
            json_exit(j, &v);
            println!("{}", serde_json::to_string(&v)?);
        }
        Cmd::Open { name } => {
            let v: FragmentStatus = c.call_as(c.get(&format!("/api/f/{name}/status"))?)?;
            let public = v.visibility == Visibility::Public;
            let view = v.view_token.as_deref().unwrap_or("");
            let inbox = v.inbox_token.as_deref().unwrap_or("");
            let suffix = if public { "" } else { "?view=" };
            let view_part = if public { "" } else { view };
            let canon = &v.urls.canonical;
            if j {
                ok_exit(&json!({
                    "canonical": format!("{}{}{}", canon, suffix, view_part),
                    "shareLink": format!("{}{}{}", canon, suffix, view_part),
                    "webhookUrl": format!("{}/api/f/{}/inbox?t={}", c.host, name, inbox),
                }));
            }
            println!("canonical:   {}{}{}", canon, suffix, view_part);
            println!("share link:  {}{}{}", canon, suffix, view_part);
            println!("webhook URL: {}/api/f/{}/inbox?t={}", c.host, name, inbox);
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
                    // a steer is answered by the running turn: wait for it too.
                    // Each read waits in the agent's cell until the turn ends
                    // (or AGENT_STATE_WAIT_MS_MAX), so this is a read about
                    // every 25 s, for about ten minutes at most.
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
                AgentCmd::Computer { name, url, token_file, connect, cwd, detach } => {
                    let path = format!("/api/a/{name}/computer");
                    if connect {
                        let token_file = token_file.unwrap_or_default();
                        let v = a.call(a.put_json(&path, &json!({ "connect": true, "cwd": cwd }))?)?;
                        let token = v["token"].as_str().ok_or_else(|| anyhow!("the agent answered no connect token"))?;
                        write_secret_file(&token_file, token)?;
                        let agent = v["agent"].as_str().unwrap_or("");
                        json_exit(j, &json!({ "agent": agent, "cwd": v["cwd"], "tokenFile": token_file }));
                        println!("{name} takes a computer that connects out; its token is in {} (0600).", token_file.display());
                        println!("on the computer: fragment computer connect --agent {agent} --token-file <that file>");
                        return Ok(());
                    }
                    if detach {
                        let v = a.call(a.delete(&path)?)?;
                        json_exit(j, &v);
                        println!("{}", if v["detached"] == true { "detached" } else { "no computer was attached" });
                        return Ok(());
                    }
                    let (url, token_file) = (url.unwrap_or_default(), token_file.unwrap_or_default());
                    let token = std::fs::read_to_string(&token_file).with_context(|| format!("reading {}", token_file.display()))?.trim().to_string();
                    let v = a.call(a.put_json(&path, &json!({ "url": url, "token": token, "cwd": cwd }))?)?;
                    json_exit(j, &v);
                    let tools: Vec<&str> = v["tools"].as_array().into_iter().flatten().filter_map(|t| t.as_str()).collect();
                    println!("{name} works on {} in {} ({})", v["url"].as_str().unwrap_or(""), v["cwd"].as_str().unwrap_or(""), tools.join(", "));
                }
            }
        }
        Cmd::Members { sub } => match sub {
            MembersCmd::List { name } => {
                let v: MemberList = c.call_as(c.get(&format!("/api/f/{name}/members"))?)?;
                json_exit(j, &v);
                for m in &v.members {
                    let agent = match &m.owner {
                        Some(o) => format!("\tagent of {o}"),
                        None => String::new(),
                    };
                    println!("{}\t{}{agent}", m.role.as_str(), m.principal);
                }
            }
            MembersCmd::Add { name, who, role } => {
                let who = if who.starts_with("id:") { who } else { auth::resolve_npub(&who)? };
                let role = fragment_proto::Role::parse(&role).ok_or_else(|| usage(format!("--role is viewer or editor, not {role:?}")))?;
                let v: Member = c.call_as(c.put_bytes(&format!("/api/f/{name}/members/{who}"), serde_json::to_vec(&fragment_proto::SetRole { role })?)?)?;
                json_exit(j, &v);
                println!("{} is now {} on {name}", v.principal, v.role.as_str());
                if let Some(owner) = &v.owner {
                    println!("  an agent: its owner {owner} can read {name} too");
                }
            }
            MembersCmd::Rm { name, who } => {
                let who = if who.starts_with("id:") { who } else { auth::resolve_npub(&who)? };
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
                let link = format!("{}/join/{}?token={token}", c.host.trim_end_matches('/'), status.name);
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
                let post = ch["post"].as_str().map(|p| format!(", {p} posts")).unwrap_or_default();
                println!("{}\t{}{post}\t{} records", ch["name"].as_str().unwrap_or(""), ch["read"].as_str().unwrap_or(""), ch["seq"]);
            }
        }
        Cmd::Channel { name, channel: Some(channel), after, follow } => {
            if follow {
                if j {
                    return Err(usage("--follow streams JSON lines; --json does not apply"));
                }
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
        Cmd::Login { .. } | Cmd::Whoami | Cmd::Username { .. } | Cmd::Keys { .. } | Cmd::Host { .. } | Cmd::Guide | Cmd::New { .. } | Cmd::Computer { .. } => unreachable!(),
    }
    Ok(())
}

/// A share link: the canonical URL (with its trailing slash: a path-mode
/// URL without one redirects) and the view token.
fn share_link(canonical: &str, token: &str) -> String {
    let base = if canonical.ends_with('/') { canonical.to_string() } else { format!("{canonical}/") };
    format!("{base}?view={token}")
}

/// 8 hex chars of the user's pubkey — the writer identity that names
/// conflict copies and signs commits
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
    let author = Author::writer(&writer);
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
    let msg = format!("deploy {name}{}", note.map(|n| format!(": {n}")).unwrap_or_default());
    // move live to main's tip; create it on first deploy; bounded
    // target_moved retries after that
    let live_tip = match storage.branch_head(LIVE).map_err(cs_anyhow)? {
        None => storage.create_branch(&main_tip, LIVE, false).map_err(cs_anyhow)?,
        Some(t) if t == main_tip => t,
        Some(mut expected) => {
            let mut landed: Option<String> = None;
            for _attempt in 0..MAX_CAS_ATTEMPTS {
                match storage.promote_live(&expected, &msg, &author) {
                    Ok(new_tip) => {
                        landed = Some(new_tip);
                        break;
                    }
                    Err(CsError::CasRejected { .. }) => {
                        expected = storage.branch_head(LIVE).map_err(cs_anyhow)?
                            .ok_or_else(|| anyhow!("the live ref vanished mid-deploy"))?;
                        continue;
                    }
                    Err(e) => return Err(cs_anyhow(e)),
                }
            }
            landed.ok_or_else(|| anyhow!("live kept moving under {MAX_CAS_ATTEMPTS} deploy attempts; re-run"))?
        }
    };
    // the sync's commit and the live move, in one nudge: serving sees THIS
    // deploy now, not at the next poll backstop
    sync::refresh_pins(c, name);
    Ok(Deployed::Live { synced, live_tip, main_tip })
}

fn writer_id(c: &api::Client) -> String {
    c.id.pubkey_hex().chars().take(8).collect()
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

fn install_sync_unit(name: &str, dir: &Path, install: bool) -> Result<()> {
    let dir = match dir.canonicalize() {
        Ok(d) => d,
        Err(_) => anyhow::bail!("no such directory: {}", dir.display()),
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
            let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>__LABEL__</string>
  <key>ProgramArguments</key>
  <array>
    <string>__EXE__</string>
    <string>sync</string>
    <string>__NAME__</string>
    <string>--dir</string>
    <string>__DIR__</string>
    <string>--watch</string>
  </array>
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
            .replace("__LABEL__", &label)
            .replace("__EXE__", &exe.display().to_string())
            .replace("__NAME__", name)
            .replace("__DIR__", &dir.display().to_string())
            .replace("__HOMEBIN__", &format!("{}/.local/bin:{}/.cargo/bin", home, home))
            .replace("__HOME__", &home)
            .replace("__LOG__", &log.display().to_string());
            std::fs::write(&plist, xml)?;
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
            let ini = r#"[Unit]
Description=fragment sync __NAME__
After=network-online.target

[Service]
ExecStart=__EXE__ sync __NAME__ --dir __DIR__ --watch
WorkingDirectory=__DIR__
Environment=PATH=__HOMEBIN__:/usr/local/bin:/usr/bin:/bin
Restart=always

[Install]
WantedBy=default.target
"#
                .replace("__NAME__", name)
                .replace("__EXE__", &exe.display().to_string())
                .replace("__DIR__", &dir.display().to_string())
                .replace("__HOMEBIN__", &format!("{}/.local/bin:{}/.cargo/bin", home, home));
            std::fs::write(&path, ini)?;
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

fn uid() -> Result<String> {
    let out = std::process::Command::new("id").arg("-u").output().context("id -u failed")?;
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Goal: a deploy of a folder mints one storage token, reads main's
    /// head once (its sync's), lists once, and nudges the pins once.
    /// Method: count the fake's requests for a first deploy and for one
    /// after an edit. Before, each minted two tokens, read main's head four
    /// times, listed twice, and refreshed twice.
    #[test]
    fn a_deploy_mints_one_token_and_refreshes_once() {
        let mock = crate::mockcs::MockServer::start();
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
            count(&[("GET storage-token", 1), ("GET branch", 2), ("GET files/metadata", 1), ("POST commit-pack", 1), ("POST branches/create", 1), ("POST refresh", 1)]),
            "main's head and live's, one listing, one token, one nudge"
        );

        std::fs::write(dir.join("site/index.html"), "<h1>two</h1>").unwrap();
        let Deployed::Live { live_tip, .. } = deploy(&c, "t", Some(&dir), Some("two"), false, None).unwrap() else { panic!("a live deploy") };
        assert_eq!(mock.file_at("t", "live", "site/index.html").unwrap(), b"<h1>two</h1>");
        assert_eq!(Some(live_tip), mock.branch("t", "live"));
        assert_eq!(
            mock.take_requests(""),
            count(&[("GET storage-token", 1), ("GET branch", 2), ("GET files/metadata", 1), ("POST commit-pack", 1), ("POST merge", 1), ("POST refresh", 1)])
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
}
