mod api;
mod auth;
mod blobs;
mod builder;
mod codestorage;
mod sync;
mod watch;

#[cfg(test)]
mod mockcs;

include!(concat!(env!("OUT_DIR"), "/templates.rs"));

use crate::api::encode_q;
use crate::codestorage::{Author, CodeStorage, CsError, LIVE, MAIN, MAX_CAS_ATTEMPTS};
use crate::sync::{Mode, SyncOptions};
use anyhow::{anyhow, Context, Result};
use clap::{Parser, Subcommand};
use serde_json::{json, Value};
use std::io::Read;
use std::path::{Path, PathBuf};

const GUIDE: &str = include_str!("../GUIDE.md");

#[derive(Parser)]
#[command(name = "fragment", version, about = "make and run fragments: folder + sqlite + urls + workflows, on celld")]
struct Cli {
    /// Host base URL (else FRAGMENT_HOST, else config, else http://127.0.0.1:8789)
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
    /// Generate a nostr keypair and store it (--force to replace)
    Login {
        #[arg(long)]
        force: bool,
    },
    /// Print your npub and host
    Whoami,
    /// Set the default host (or show it, with no argument)
    Host {
        /// Base URL, e.g. https://fragment.club
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
    /// Status of a fragment (counts, live snapshot, crons, share link)
    Status { name: String },
    /// Read the event log (--since is an event ID cursor; --tail shows the last N)
    Events {
        name: String,
        /// Show events after this event ID
        #[arg(long, default_value = "0", conflicts_with = "tail")]
        since: u64,
        /// Show only the last N events
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
    /// Delete a fragment you own (registry row + all cell data; name reusable)
    Rm {
        name: String,
        /// drop just the registry row when the cell itself is unwedgeable
        #[arg(long)]
        force_registry: bool,
    },
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
        #[arg(long)]
        template: Option<String>,
    },
    /// Run a workflow now
    Run { name: String, workflow: String, #[arg(long)] input: Option<String> },
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
    /// Pause an operation's triggers (calls still work)
    Pause { name: String, workflow: String },
    /// Unpause an operation's triggers (also after an auto-pause)
    Unpause { name: String, workflow: String },
    /// Re-run a held or blocked run with its original input (after fixing the code)
    Replay { name: String, run: u64 },
    /// Rotate a fragment's tokens (owner-only; default rotates both scopes)
    Rotate {
        name: String,
        /// rotate only the inbox token
        #[arg(long)]
        inbox: bool,
        /// rotate only the view token
        #[arg(long)]
        view: bool,
    },
    /// List a fragment's rooms, or read one room's recent messages
    Rooms {
        name: String,
        /// Room to read; omit to list all rooms with counts
        room: Option<String>,
        /// How many recent messages to show (with ROOM)
        #[arg(long, default_value = "30")]
        tail: u64,
    },
    /// Manage secrets (values via env var of same name, or stdin)
    Secret {
        #[command(subcommand)]
        sub: SecretCmd,
    },
    /// Grant a role to an npub
    /// Grant a role to an npub or NIP-05 name (name@domain)
    Grant { name: String, #[arg(long)] editor: Vec<String>, #[arg(long)] viewer: Vec<String> },
    /// Revoke a role from an npub or NIP-05 name
    Revoke { name: String, #[arg(long)] editor: Vec<String>, #[arg(long)] viewer: Vec<String> },
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
        /// Template name (basic | vault | dropzone)
        #[arg(long)]
        template: Option<String>,
        /// List available templates
        #[arg(long)]
        list: bool,
    },
}

#[derive(Subcommand)]
enum MembersCmd {
    /// List members and their roles
    List { name: String },
    /// Add a member, or change their role (owner only)
    Add {
        name: String,
        /// npub, 64-hex key, or NIP-05 name (name@domain)
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
}

fn config_path() -> PathBuf {
    dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("fragment").join("config.json")
}

fn load_config() -> Config {
    let p = config_path();
    if let Ok(bytes) = std::fs::read(&p) {
        let v: Value = serde_json::from_slice(&bytes).unwrap_or(json!({}));
        Config {
            host: v["host"].as_str().map(|s| s.to_string()),
            secret_key: v["secret_key"].as_str().map(|s| s.to_string()),
            codestorage: v["codestorage"].as_str().map(|s| s.trim_end_matches('/').to_string()),
        }
    } else {
        Config { host: None, secret_key: None, codestorage: None }
    }
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
        .unwrap_or_else(|| "http://127.0.0.1:8789".to_string())
}

fn require_client(cli_host: &Option<String>, verbose: bool) -> Result<api::Client> {
    let cfg = load_config();
    let host = resolve_host(cli_host, &cfg);
    let sk = cfg.secret_key.ok_or_else(|| {
        anyhow::Error::new(api::CodedError {
            code: "auth_failed",
            msg: "no keypair — run `fragment login` first".into(),
        })
    })?;
    let bytes = hex::decode(&sk).context("config secret_key is not hex")?;
    let arr: [u8; 32] = bytes.try_into().map_err(|_| anyhow!("config secret_key must be 32 bytes"))?;
    let c = api::Client::new(&host, auth::Identity::from_secret(arr));
    Ok(if verbose { c.with_verbose() } else { c })
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
// Failure: {"ok":false,"error":{code,message,hint?}} on stdout, exit 1
// (2 for usage-class errors). Human mode prints exactly as before.

fn emit_ok(data: &Value) {
    println!(
        "{{\"ok\":true,\"data\":{}}}",
        serde_json::to_string(data).unwrap_or_else(|_| "null".into())
    );
}

fn ok_exit(data: &Value) -> ! {
    emit_ok(data);
    std::process::exit(0);
}

fn fail_json(code: &str, msg: &str, hint: Option<&str>, exit_code: i32) -> ! {
    let mut err = json!({ "code": code, "message": msg });
    if let Some(h) = hint {
        err["hint"] = json!(h);
    }
    println!(
        "{{\"ok\":false,\"error\":{}}}",
        serde_json::to_string(&err).unwrap_or_else(|_| "{\"code\":\"server_error\"}".into())
    );
    std::process::exit(exit_code);
}

/// Read the first key that exists out of snake_case/camelCase spellings
/// (runtime payloads have used both shapes over their life).
fn jstr_any(v: &Value, keys: &[&str]) -> String {
    for k in keys {
        if let Some(s) = v[*k].as_str() {
            return s.to_string();
        }
    }
    String::new()
}

fn summary_of(v: &Value) -> String {
    v["error"].as_str().unwrap_or("unknown error").to_string()
}

/// Next-action strings per stable error code (only shown under --json).
fn default_hint(code: &str) -> Option<&'static str> {
    match code {
        "auth_failed" => Some("run `fragment login`, or point at another host with --host / `fragment host <url>`"),
        "forbidden" => Some("your npub lacks a role here — ask the owner to `fragment grant` you"),
        "not_found" => Some("check the name with `fragment list`"),
        "name_taken" => Some("pick another name, or remove the existing fragment with `fragment rm <name>`"),
        "conflict" => Some("re-sync (`fragment sync`) and reapply your change"),
        "too_large" => Some("cell bodies cap at 1 MB — keep media in a bucket/CDN and link it"),
        "rate_limited" => Some("back off and retry shortly"),
        "unavailable" => Some("usually transient; retrying is safe"),
        "server_error" => Some("see `fragment events <name>` if it persists"),
        "invalid_usage" => Some("see `fragment --help`"),
        _ => None,
    }
}

/// Pick (code, exit_class) for an arbitrary error from anywhere in run().
/// CodedError wins (mapped at the HTTP boundary); local misuse gets
/// invalid_usage; everything else is a generic failure.
fn classify_err(e: &anyhow::Error) -> (&'static str, i32) {
    if let Some(ce) = e.downcast_ref::<api::CodedError>() {
        return (ce.code, if ce.code == "invalid_usage" { 2 } else { 1 });
    }
    let msg = format!("{e:#}");
    // connect-level failures that escaped mapping still count as unavailable
    if e.chain().any(|c| c.downcast_ref::<reqwest::Error>().is_some()) {
        return ("unavailable", 1);
    }
    // usage-style bails: bad flag values, bad inline JSON input
    if msg.starts_with("--") || msg.contains("usage:") || msg.contains("usage: fragment") {
        return ("invalid_usage", 2);
    }
    ("server_error", 1)
}

fn main() {
    let json_mode = json_env_flag();
    let cli = match Cli::try_parse() {
        Ok(c) => c,
        Err(e) => {
            if !e.use_stderr() {
                e.exit(); // help/version: print them, exit 0
            }
            if json_mode {
                fail_json(
                    "invalid_usage",
                    &e.to_string(),
                    default_hint("invalid_usage"),
                    2,
                );
            }
            e.exit(); // clap's own usage text + exit code 2
        }
    };
    let verbose = cli.verbose;
    let host = cli.host.clone();
    if let Err(e) = run(cli) {
        let (code, class) = classify_err(&e);
        if json_mode {
            fail_json(code, &format!("{e:#}"), default_hint(code), class);
        }
        eprintln!("error: {e:#}");
        std::process::exit(class);
    }
    let _ = (verbose, host);
}

fn run(cli: Cli) -> Result<()> {
    // env var is a first-class equivalent of passing --json on every command
    let j = cli.json || json_env_flag();

    match cli.cmd {
        Cmd::Login { force } => {
            let p = config_path();
            if p.exists() && !force {
                let cfg = load_config();
                if let Some(sk) = cfg.secret_key {
                    let bytes = hex::decode(sk)?;
                    let id = auth::Identity::from_secret(bytes.try_into().map_err(|_| anyhow!("bad key"))?);
                    if j {
                        // never echo the key itself
                        ok_exit(&json!({ "npub": id.npub(), "config": p.display().to_string(), "existing": true }));
                    }
                    println!("already logged in as {}", id.npub());
                    println!("(use --force to replace the key)");
                    return Ok(());
                }
            }
            let id = auth::Identity::generate();
            std::fs::create_dir_all(p.parent().unwrap())?;
            let f = std::fs::File::create(&p)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600))?;
            }
            serde_json::to_writer_pretty(f, &json!({ "secret_key": hex::encode(id.secret) }))?;
            if j {
                ok_exit(&json!({ "npub": id.npub(), "config": p.display().to_string() }));
            }
            println!("logged in: {}", id.npub());
            println!("config: {}", p.display());
            return Ok(());
        }
        Cmd::Whoami => {
            let c = require_client(&cli.host, cli.verbose)?;
            if j {
                ok_exit(&json!({ "npub": c.id.npub(), "host": c.host }));
            }
            println!("npub: {}", c.id.npub());
            println!("host: {}", c.host);
            return Ok(());
        }
        Cmd::Host { url } => {
            let cfg = load_config();
            match url {
                Some(url) => {
                    let url = url.trim_end_matches('/').to_string();
                    if !url.starts_with("https://") && !url.starts_with("http://") {
                        anyhow::bail!("host must be an http(s) URL, e.g. https://fragment.club");
                    }
                    let p = config_path();
                    std::fs::create_dir_all(p.parent().unwrap())?;
                    let mut obj = json!({ "host": url });
                    if let Some(sk) = &cfg.secret_key {
                        obj["secret_key"] = json!(sk);
                    }
                    // the file holds the secret key: keep it 0600
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::OpenOptionsExt;
                        let mut f = std::fs::OpenOptions::new()
                            .write(true).create(true).truncate(true).mode(0o600)
                            .open(&p)?;
                        serde_json::to_writer_pretty(&mut f, &obj)?;
                    }
                    #[cfg(not(unix))]
                    {
                        let f = std::fs::File::create(&p)?;
                        serde_json::to_writer_pretty(f, &obj)?;
                    }
                    if j {
                        ok_exit(&json!({ "host": url, "config": p.display().to_string() }));
                    }
                    println!("default host set: {url}");
                    println!("config: {}", p.display());
                }
                None => {
                    if j {
                        ok_exit(&json!({ "host": resolve_host(&cli.host, &cfg) }));
                    }
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
                for (name, files) in TEMPLATES.iter().filter(|(n, _)| *n != "libs") {
                    println!("{name} ({} files)", files.len());
                }
                return Ok(());
            }
            let dir = dir.ok_or_else(|| anyhow!("usage: fragment new <dir> [--template <name>]"))?;
            let tpl_name = template.as_deref().unwrap_or("basic");
            let tpl = TEMPLATES
                .iter()
                .find(|(n, _)| *n == tpl_name)
                .filter(|(n, _)| *n != "libs")
                .ok_or_else(|| anyhow!("unknown template '{tpl_name}' (use --list)"))?;
            if dir.exists() && !dir.is_dir() {
                anyhow::bail!("{} exists and is not a directory", dir.display());
            }
            std::fs::create_dir_all(&dir)?;
            let mut created = 0usize;
            let mut skipped = 0usize;
            for (rel, bytes) in tpl.1 {
                let target = dir.join(rel);
                if target.exists() {
                    skipped += 1; // never clobber existing files
                    continue;
                }
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(&target, bytes)?;
                println!("  {}", rel);
                created += 1;
            }
            println!("scaffolded '{tpl_name}' into {} ({created} files{})", dir.display(), if skipped > 0 { format!(", {skipped} existing left alone") } else { String::new() });
            println!("next:");
            println!("  fragment init <name> --template <tpl>  (scaffold + create + deploy in one step)");
            return Ok(());
        }
        _ => {}
    }

    let c = require_client(&cli.host, cli.verbose)?;

    match cli.cmd {
        Cmd::Create { name, visibility } => {
            // Client-side fragment identity (ROADMAP level-c fix): the
            // npub keypair is generated HERE, and the secret crosses the
            // wire exactly once as `fragmentSecret` (the field the create
            // route reads), inside the creator's NIP-98-authenticated
            // request; the runtime stores it wrapped and the CLI drops it.
            let fid = auth::Identity::generate();
            let mut body = json!({ "name": name, "fragmentSecret": hex::encode(fid.secret) });
            if let Some(vis) = &visibility {
                body["visibility"] = json!(vis);
            }
            let v = c.call(c.post_json("/api/fragments", &body)?)?;
            let npub = v["npub"].as_str().unwrap_or_default().to_string();
            let npub = if npub.is_empty() { fid.npub() } else { npub };
            if j {
                ok_exit(&v);
            }
            println!("created fragment {}", v["name"].as_str().unwrap_or(&name));
            println!("  npub:         {npub}");
            let canon2 = v["canonical"].as_str().filter(|s| s.starts_with("http")).map(|s| s.to_string())
                .unwrap_or_else(|| format!("{}/f/{}/", c.host, name));
            println!("  share link:   {}?view={}", canon2.trim_end_matches('/'), v["viewToken"].as_str().unwrap_or(""));
            println!("  webhook URL:  {}/api/f/{}/inbox?t={}", c.host.trim_end_matches('/'), name, v["inboxToken"].as_str().unwrap_or(""));
            let canon = v["canonical"].as_str().filter(|s| s.starts_with("http")).map(|s| s.to_string())
                .unwrap_or_else(|| format!("{}/f/{}/", c.host, name));
            println!("  canonical:   {}", canon);
        }
        Cmd::List => {
            let v = c.call(c.get("/api/fragments")?)?;
            if j {
                ok_exit(&v);
            }
            for f in v["fragments"].as_array().cloned().unwrap_or_default() {
                println!("{} ({})", f["name"].as_str().unwrap_or(""), f["role"].as_str().unwrap_or(""));
            }
        }
        Cmd::Status { name } => {
            let v = c.call(c.get(&format!("/api/f/{name}/status"))?)?;
            if j {
                ok_exit(&v);
            }
            println!("{}", serde_json::to_string_pretty(&v)?);
        }
        Cmd::Events { name, since, tail } => {
            let v = c.call(c.get(&format!("/api/f/{name}/events?since={since}"))?)?;
            let mut evs = v["events"].as_array().cloned().unwrap_or_default();
            if let Some(n) = tail {
                let n = n as usize;
                if evs.len() > n {
                    evs = evs.split_off(evs.len() - n);
                }
            }
            if j {
                ok_exit(&json!({ "events": evs }));
            }
            for e in evs {
                let at = e["at"].as_u64().unwrap_or(0);
                let time = chrono_like(at / 1000);
                println!("[{}] {} {} — {}", e["id"], time, e["kind"].as_str().unwrap_or(""), e["summary"].as_str().unwrap_or(""));
            }
        }
        Cmd::Manifest { name } => {
            let v = c.call(c.get(&format!("/api/f/{name}/manifest"))?)?;
            if j {
                ok_exit(&v);
            }
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
            if j {
                ok_exit(&json!({ "updated": true, "commit": tip, "manifest": v }));
            }
            println!("manifest updated (commit {})", &tip[..8.min(tip.len())]);
        }
        Cmd::Sync {
            name, dir, watch, mode, prune, mirror_from, apply_mass_delete,
            rebuild_state, no_live, install, uninstall,
        } => {
            // never stream JSON envelopes mid-run: watch prints progress
            // lines forever; a json consumer would choke on line 2
            if j && watch {
                fail_json(
                    "invalid_usage",
                    "sync --watch streams progress lines continuously — --json is not supported there",
                    Some("run single passes with --json (`fragment sync <name> --dir .`), or drop --json to watch"),
                    2,
                );
            }
            if install || uninstall {
                install_sync_unit(&name, &dir, install, mirror_from.as_deref().and_then(|p| p.to_str()))?;
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
                    other => anyhow::bail!("--mode must be push|pull|mirror, got {other:?}"),
                },
                apply_mass_delete,
                prune,
                verify: false,
                writer_id: writer_id(&c),
                codestorage: codestorage_override(),
            };
            if watch {
                let cfg = watch::WatchConfig { live: !no_live };
                watch::run(&c, &name, &dir, &opts, &cfg)?;
                return Ok(());
            }
            let report = sync::sync_once(&c, &name, &dir, &opts).map_err(cs_anyhow)?;
            if j {
                emit_ok(&serde_json::to_value(&report).unwrap_or_default());
                // scriptable exit codes survive the envelope (3 conflicts, 4 guard)
                std::process::exit(report.exit_code());
            }
            println!("sync {} ({})", name, dir.display());
            report.print();
            std::process::exit(report.exit_code());
        }
        Cmd::Rm { name, force_registry } => {
            match c.call(c.delete(&format!("/api/f/{name}"))?) {
                Ok(_) => {}
                Err(_) if force_registry => {
                    // the cell is unwedgeable (poisoned state from an older
                    // era); drop the registry row and leave whatever cell
                    // data exists to the bucket's own GC
                    c.call(c.post_json(
                        "/api/f/_registry/__registry/delete",
                        &serde_json::json!({ "name": name }),
                    )?)
                    .context("registry delete request failed")?;
                    if j { ok_exit(&json!({ "deleted": true, "name": name, "mode": "registry-only" })); }
                    println!("deleted registry row for {name} (cell data left to GC; the name is reusable)");
                    return Ok(());
                }
                Err(e) => return Err(e),
            }
            if j {
                ok_exit(&json!({ "deleted": true, "name": name }));
            }
            println!("deleted fragment {name} (registry row + all data; the name is reusable)");
        }
        Cmd::Verify { name, dir } => {
            let report = sync::verify(&c, &name, &dir, codestorage_override().as_deref()).map_err(cs_anyhow)?;
            if j {
                emit_ok(&serde_json::to_value(&report).unwrap_or_default());
                std::process::exit(report.exit_code());
            }
            println!("verify {} ({})", name, dir.display());
            report.print();
            std::process::exit(report.exit_code());
        }
        Cmd::Deploy { name, dir, note, preview } => {
            let writer = writer_id(&c);
            let cs = codestorage_override();
            if let Some(dir) = dir.as_deref() {
                let report = sync::sync_once(&c, &name, dir, &SyncOptions { writer_id: writer.clone(), codestorage: cs.clone(), ..Default::default() })
                    .map_err(cs_anyhow)?;
                if report.mass_delete_guard.is_some() {
                    report.print();
                    anyhow::bail!("sync refused a mass deletion — deploy aborted before moving live. If the deletions are intended, run `fragment sync {} --dir {} --apply-mass-delete` first, then deploy again.", name, dir.display());
                }
                report.print();
                // fragment.json rides the commit (it is a git file at the
                // repo root) — files and machinery go live together
            }
            // secrets declared in code but never set: the 3-round news
            // failure — workflows reference ctx.secrets.X, nobody runs
            // `fragment secret set`, every run holds. Catch it at deploy.
            if let Some(d) = &dir {
                let mut referenced: Vec<String> = Vec::new();
                let wf_dir = d.join("workflows");
                if wf_dir.exists() {
                    for entry in walkdir::WalkDir::new(&wf_dir).max_depth(2) {
                        let entry = match entry { Ok(e) => e, Err(_) => continue };
                        if !entry.file_type().is_file() { continue; }
                        let body = match std::fs::read_to_string(entry.path()) { Ok(b) => b, Err(_) => continue };
                        for cap in body.match_indices("ctx.secrets.").map(|(i, _)| i) {
                            let rest = &body[cap + "ctx.secrets.".len()..];
                            let name: String = rest.chars().take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '_').collect();
                            if name.len() > 1 && !referenced.contains(&name) {
                                referenced.push(name);
                            }
                        }
                    }
                }
                if !referenced.is_empty() {
                    let listed = c.get(&format!("/api/f/{name}/secrets"))?;
                    let set = c.call(listed)?;
                    let have: Vec<String> = set["names"].as_array().cloned().unwrap_or_default().iter()
                        .map(|v| v.as_str().unwrap_or("").to_string()).collect();
                    for r in referenced {
                        if !have.contains(&r) {
                            eprintln!("WARNING: workflows reference ctx.secrets.{r} but it is NOT set — every run will fail until you do:\n  fragment secret set {name} {r}");
                        }
                    }
                }
            }
            let storage = CodeStorage::connect(&c, &name, cs.as_deref()).map_err(cs_anyhow)?;
            let main_tip = storage.branch_head(MAIN).map_err(cs_anyhow)?
                .ok_or_else(|| anyhow!("nothing to deploy: main has no commits (sync a folder with --dir first)"))?;
            let author = Author::writer(&writer);
            if preview {
                // ephemeral ref at main's tip: unguessable, invisible to
                // clones, promoted by deploying. There is no served URL —
                // the ref IS the preview.
                let slug = format!("preview/{:012x}", rand::random::<u64>());
                let sha = storage.create_branch(&main_tip, &slug, true).map_err(cs_anyhow)?;
                if j {
                    ok_exit(&json!({ "preview": slug, "sha": sha }));
                }
                println!("preview: {slug} (ephemeral ref at {})", &sha[..12.min(sha.len())]);
                println!("go live with: fragment deploy {name}");
                return Ok(());
            }
            let msg = format!("deploy {name}{}", note.as_deref().map(|n| format!(": {n}")).unwrap_or_default());
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
            // the live move is an external ref change from the cell's
            // perspective — nudge the pins so serving sees THIS deploy now,
            // not at the next poll backstop (best-effort; the poll covers)
            if let Err(e) = c.post_json(&format!("/api/f/{name}/refresh"), &json!({})) {
                eprintln!("warning: cell pin refresh failed ({e:#}); the poll backstop will catch up");
            }
            let st = c.call(c.get(&format!("/api/f/{name}/status"))?)?;
            let live_url = st["urls"]["canonical"].as_str().filter(|s| s.starts_with("http")).map(|s| s.to_string())
                .unwrap_or_else(|| format!("{}/f/{}/", c.host.trim_end_matches('/'), name));
            if j {
                ok_exit(&json!({ "live": live_url, "liveTip": live_tip, "mainTip": main_tip }));
            }
            println!("live: {live_url}");
            if st["visibility"].as_str() == Some("link") {
                if let Some(tok) = st["viewToken"].as_str() {
                    let canon = st["urls"]["canonical"].as_str().filter(|s| s.starts_with("http"))
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| format!("{}/f/{}/", c.host, name));
                    println!("share link: {}?view={}", canon.trim_end_matches('/'), tok);
                }
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
            if let Err(e) = c.post_json(&format!("/api/f/{name}/refresh"), &json!({})) {
                eprintln!("warning: cell pin refresh failed ({e:#}); the poll backstop will catch up");
            }
            if j {
                ok_exit(&json!({ "rolledBackTo": target, "liveTip": new_tip }));
            }
            println!("rolled back to {}: live is now {}", &target[..8.min(target.len())], &new_tip[..8.min(new_tip.len())]);
        }
        Cmd::Drafts { name } => {
            let storage = CodeStorage::connect(&c, &name, codestorage_override().as_deref()).map_err(cs_anyhow)?;
            let commits = storage.list_commits(LIVE, 30).map_err(cs_anyhow)?;
            if j {
                ok_exit(&json!({ "deploys": commits }));
            }
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
            // scaffold (reuse the New machinery) + create + deploy
            let dir = std::env::current_dir()?.join(&name);
            if dir.exists() {
                anyhow::bail!("{} already exists", dir.display());
            }
            let tpl_name = template.as_deref().unwrap_or("basic");
            let tpl = TEMPLATES
                .iter()
                .find(|(n, _)| *n == tpl_name)
                .filter(|(n, _)| *n != "libs")
                .ok_or_else(|| anyhow!("unknown template '{tpl_name}' (use `fragment new --list`)"))?;
            std::fs::create_dir_all(&dir)?;
            for (rel, bytes) in tpl.1 {
                let target = dir.join(rel);
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(&target, bytes)?;
            }
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
            println!("scaffolded '{tpl_name}' into {}", dir.display());
            // client-side npub; secret crosses the wire once (fragmentSecret),
            // wrapped at rest
            let fid = auth::Identity::generate();
            let created = c.post_json("/api/fragments", &json!({ "name": name, "fragmentSecret": hex::encode(fid.secret) })).and_then(|r| c.call(r));
            if let Err(e) = created {
                // nothing was created, so leave nothing here either: the same init can be retried
                let _ = std::fs::remove_dir_all(&dir);
                return Err(e);
            }
            // push the scaffold, then point live at it — the first deploy
            // is the real site, not an empty one
            let writer = writer_id(&c);
            let cs = codestorage_override();
            let report = sync::sync_once(&c, &name, &dir, &SyncOptions { writer_id: writer.clone(), codestorage: cs.clone(), ..Default::default() })
                .map_err(cs_anyhow)?;
            report.print();
            let storage = CodeStorage::connect(&c, &name, cs.as_deref()).map_err(cs_anyhow)?;
            let main_tip = storage.branch_head(MAIN).map_err(cs_anyhow)?
                .ok_or_else(|| anyhow!("first sync produced no commits"))?;
            storage.create_branch(&main_tip, LIVE, false).map_err(cs_anyhow)?;
            // the new live ref is a git move the cell learns of now, not at its next poll
            if let Err(e) = c.post_json(&format!("/api/f/{name}/refresh"), &json!({})) {
                eprintln!("warning: cell pin refresh failed ({e:#}); the poll backstop will catch up");
            }
            let st = c.call(c.get(&format!("/api/f/{name}/status"))?)?;
            let canon = st["urls"]["canonical"].as_str().filter(|s| s.starts_with("http"))
                .map(|s| s.to_string())
                .unwrap_or_else(|| format!("{}/f/{}/", c.host, name));
            if j {
                // synthesized composite: the human output's three URLs plus the full status
                let mut data = json!({
                    "canonical": canon,
                    "shareLink": format!("{}?view={}", canon.trim_end_matches('/'), st["viewToken"].as_str().unwrap_or("")),
                    "webhookUrl": format!("{}/api/f/{}/inbox?t={}", c.host.trim_end_matches('/'), name, st["inboxToken"].as_str().unwrap_or("")),
                    "folder": dir.display().to_string(),
                    "status": st,
                });
                if st["visibility"].as_str() != Some("link") {
                    data.as_object_mut().unwrap().remove("shareLink");
                }
                ok_exit(&data);
            }
            println!("live: {}", canon);
            if st["visibility"].as_str() == Some("link") {
                if let Some(tok) = st["viewToken"].as_str() {
                    println!("share link: {}?view={}", canon.trim_end_matches('/'), tok);
                }
            }
            if let Some(tok) = st["inboxToken"].as_str() {
                println!("webhook URL: {}/api/f/{}/inbox?t={}", c.host.trim_end_matches('/'), name, tok);
            }
            println!("folder: {}", dir.display());
        }
        Cmd::Run { name, workflow, input } => {
            let input_v: Value = match input.as_deref() {
                Some(s) => serde_json::from_str(s).context("--input must be valid JSON")?,
                None => Value::Null,
            };
            let v = c.call(c.post_json(&format!("/api/f/{name}/run"), &json!({ "workflow": workflow, "input": input_v }))?)?;
            if j {
                ok_exit(&v);
            }
            if v["ok"].as_bool().unwrap_or(false) {
                println!("ok. output: {}", serde_json::to_string_pretty(&v["output"])?);
            } else {
                println!("FAILED: {}", v["error"].as_str().unwrap_or(""));
            }
            for e in v["events"].as_array().cloned().unwrap_or_default() {
                println!("  [{}] {}", e["kind"].as_str().unwrap_or(""), e["summary"].as_str().unwrap_or(""));
            }
        }
        Cmd::Runs { name, run: Some(id), .. } => {
            let v = c.call(c.get(&format!("/api/f/{name}/runs/{id}"))?)?;
            if j {
                ok_exit(&v);
            }
            println!("{}", serde_json::to_string_pretty(&v)?);
        }
        Cmd::Triggers { name } => {
            let v = c.call(c.get(&format!("/api/f/{name}/triggers"))?)?;
            if j {
                ok_exit(&v);
            }
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
            let v = c.call(c.get(&path)?)?;
            if j {
                ok_exit(&v);
            }
            let rows = v["runs"].as_array().cloned().unwrap_or_default();
            for r in &rows {
                let dur = r["durationMs"].as_u64().map(|d| format!("{d}ms")).unwrap_or_default();
                // the Rust cell names the operation (`op`); the TypeScript runtime the workflow (`wf`)
                let tries = match r["maxAttempts"].as_u64() {
                    Some(max) => format!("try {}/{max}", r["attempt"].as_u64().unwrap_or(0)),
                    None => format!("attempt {}", r["attempt"].as_u64().unwrap_or(0)),
                };
                println!(
                    "#{}\t{}\t{}\t{}\t{}\t{}",
                    r["id"].as_u64().unwrap_or(0),
                    r["via"].as_str().unwrap_or("?"),
                    r["op"].as_str().or(r["wf"].as_str()).unwrap_or("?"),
                    r["status"].as_str().unwrap_or("?"),
                    tries,
                    dur,
                );
                if let Some(e) = r["error"].as_str() {
                    println!("  {}", e.chars().take(120).collect::<String>());
                }
            }
            if rows.is_empty() {
                println!("(no runs)");
            }
            let held = v["counts"]["held"].as_u64().unwrap_or(0);
            if held > 0 {
                println!("\n{held} held run(s) — `fragment replay {name} <run-id>` after fixing");
            }
        }
        Cmd::Pause { name, workflow } => {
            let v = c.call(c.post_json(&format!("/api/f/{name}/pause"), &json!({ "workflow": workflow, "paused": true }))?)?;
            if j {
                ok_exit(&v);
            }
            println!("paused '{workflow}' (manual runs still work; unpause with `fragment unpause`)");
        }
        Cmd::Unpause { name, workflow } => {
            let v = c.call(c.post_json(&format!("/api/f/{name}/pause"), &json!({ "workflow": workflow, "paused": false }))?)?;
            if j {
                ok_exit(&v);
            }
            println!("unpaused '{workflow}'");
        }
        Cmd::Replay { name, run } => {
            let v = c.call(c.post_json(&format!("/api/f/{name}/replay"), &json!({ "run": run }))?)?;
            if j {
                ok_exit(&v);
            }
            if v["ok"].as_bool().unwrap_or(false) {
                println!("replayed run #{run} → ok");
            } else {
                println!("replayed run #{run} → error: {}", v["error"].as_str().unwrap_or("unknown"));
            }
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
            let body = if scopes.is_empty() { json!({}) } else { json!({ "scopes": scopes }) };
            let v = c.call(c.post_json(&format!("/api/f/{name}/rotate"), &body)?)?;
            let it = jstr_any(&v, &["inbox_token", "inboxToken"]);
            let vt = jstr_any(&v, &["view_token", "viewToken"]);
            let wh = jstr_any(&v, &["webhook_secret", "webhookSecret"]);
            let rotated = match v["rotated"].as_array() {
                Some(a) => a.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(", "),
                None => String::from("both"),
            };
            let canon = format!("{}/f/{}/", c.host.trim_end_matches('/'), name);
            if j {
                // webhook_secret included: the code.storage push HMAC is
                // only ever visible at create/rotate, and machine consumers
                // (dev harnesses registering push webhooks) need it back
                ok_exit(&json!({ "inbox_token": it, "view_token": vt, "webhook_secret": wh, "rotated": v["rotated"].clone() }));
            }
            println!("rotated: {rotated}");
            println!("New webhook URL: {}/api/f/{}/inbox?t={}", c.host.trim_end_matches('/'), name, it);
            println!("New share link: {}?view={}", canon.trim_end_matches('/'), vt);
        }
        Cmd::Rooms { name, room, tail } => {
            match room {
                None => {
                    let v = c.call(c.get(&format!("/api/f/{name}/rooms"))?)?;
                    if j {
                        ok_exit(&v);
                    }
                    for r in v["rooms"].as_array().cloned().unwrap_or_default() {
                        let last = r["last_at"].as_u64().unwrap_or(0);
                        println!(
                            "{}\t{}\t{}",
                            r["room"].as_str().unwrap_or(""),
                            r["count"].as_u64().unwrap_or(0),
                            chrono_like(last / 1000),
                        );
                    }
                }
                Some(room) => {
                    let enc = encode_q(&room).replace('/', "%2F");
                    let v = c.call(c.get(&format!("/api/f/{name}/rooms/{enc}/messages?limit={tail}"))?)?;
                    if j {
                        ok_exit(&v);
                    }
                    for m in v["messages"].as_array().cloned().unwrap_or_default() {
                        let data = serde_json::to_string(&m["data"]).unwrap_or_default();
                        let preview: String = data.chars().take(80).collect::<String>();
                        let preview = if data.chars().count() > 80 { format!("{preview}…") } else { preview };
                        let at = m["at"].as_u64().unwrap_or(0);
                        println!(
                            "{}\t{}  {}",
                            m["sender"].as_str().unwrap_or("-"),
                            chrono_like(at / 1000),
                            preview,
                        );
                    }
                }
            }
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
                    anyhow::bail!("empty secret value");
                }
                c.call(c.put_bytes(&format!("/api/f/{name}/secrets/{key}"), value.into_bytes())?)?;
                if j {
                    // names only — values never travel back
                    ok_exit(&json!({ "name": name, "key": key, "set": true }));
                }
                println!("secret {key} set on {name}");
            }
            SecretCmd::List { name } => {
                let v = c.call(c.get(&format!("/api/f/{name}/secrets"))?)?;
                if j {
                    ok_exit(&v);
                }
                for n in v["names"].as_array().cloned().unwrap_or_default() {
                    println!("{}", n.as_str().unwrap_or(""));
                }
            }
            SecretCmd::Rm { name, key } => {
                c.call(c.delete(&format!("/api/f/{name}/secrets/{key}"))?)?;
                if j {
                    ok_exit(&json!({ "name": name, "key": key, "removed": true }));
                }
                println!("secret {key} removed");
            }
        },
        Cmd::Grant { name, editor, viewer } => edit_roles(&c, &name, editor, viewer, true, j),
        Cmd::Revoke { name, editor, viewer } => edit_roles(&c, &name, editor, viewer, false, j),
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
            let status = resp.status().as_u16();
            let bytes = resp.bytes()?;
            let v: Value = serde_json::from_slice(&bytes)
                .unwrap_or(Value::String(String::from_utf8_lossy(&bytes).to_string()));
            if j {
                if (200..300).contains(&status) {
                    ok_exit(&v);
                }
                let code = api::code_for(status, &summary_of(&v));
                fail_json(code, &format!("http {}: {}", status, summary_of(&v)), default_hint(code), 1);
            }
            println!("{}", serde_json::to_string(&v)?);
        }
        Cmd::Open { name } => {
            let v = c.call(c.get(&format!("/api/f/{name}/status"))?)?;
            let public = v["visibility"].as_str() == Some("public");
            let view = v["viewToken"].as_str().unwrap_or("");
            let inbox = v["inboxToken"].as_str().unwrap_or("");
            let suffix = if public { "" } else { "?view=" };
            let view_part = if public { "" } else { view };
            let server_canon = v["urls"]["canonical"].as_str().filter(|s| s.starts_with("http"));
            let canon = server_canon.map(|s| s.to_string()).unwrap_or_else(|| format!("{}/f/{}/", c.host, name));
            if j {
                ok_exit(&json!({
                    "canonical": format!("{}{}{}", canon, suffix, view_part),
                    "shareLink": format!("{}{}{}", canon, suffix, view_part),
                    "webhookUrl": format!("{}/api/f/{}/inbox?t={}", c.host, name, inbox),
                    "rooms": format!("{}/f/{}/__room/<room>{}{}", c.host, name, suffix, view_part),
                }));
            }
            println!("canonical:   {}{}{}", canon, suffix, view_part);
            println!("share link:   {}{}{}", canon, suffix, view_part);
            println!("webhook URL:  {}/api/f/{}/inbox?t={}", c.host, name, inbox);
            println!("rooms:       {}/f/{}/__room/<room>{}{}", c.host, name, suffix, view_part);
        }
        Cmd::Members { sub } => match sub {
            MembersCmd::List { name } => {
                let v = c.call(c.get(&format!("/api/f/{name}/members"))?)?;
                if j {
                    ok_exit(&v);
                }
                for m in v["members"].as_array().cloned().unwrap_or_default() {
                    println!("{}\t{}", m["role"].as_str().unwrap_or(""), m["principal"].as_str().unwrap_or(""));
                }
            }
            MembersCmd::Add { name, who, role } => {
                let who = auth::resolve_npub(&who)?;
                let v = c.call(c.put_bytes(&format!("/api/f/{name}/members/{who}"), serde_json::to_vec(&json!({ "role": role }))?)?)?;
                if j {
                    ok_exit(&v);
                }
                println!("{who} is now {role} on {name}");
            }
            MembersCmd::Rm { name, who } => {
                let who = auth::resolve_npub(&who)?;
                let v = c.call(c.delete(&format!("/api/f/{name}/members/{who}"))?)?;
                if j {
                    ok_exit(&v);
                }
                println!("removed {who} from {name}");
            }
            MembersCmd::Leave { name } => {
                let me = c.id.npub();
                let v = c.call(c.delete(&format!("/api/f/{name}/members/{me}"))?)?;
                if j {
                    ok_exit(&v);
                }
                println!("left {name}");
            }
        },
        Cmd::Invite { sub } => match sub {
            InviteCmd::Create { name, role, uses, ttl } => {
                let mut body = json!({ "role": role, "uses": uses });
                if let Some(t) = ttl {
                    body["ttlS"] = json!(t);
                }
                let v = c.call(c.post_json(&format!("/api/f/{name}/invites"), &body)?)?;
                if j {
                    ok_exit(&v);
                }
                let token = v["token"].as_str().unwrap_or("");
                println!("invite {} ({role}, {uses} use{})", v["id"].as_str().unwrap_or(""), if uses == 1 { "" } else { "s" });
                println!("join with: fragment join {name} {token}");
            }
            InviteCmd::List { name } => {
                let v = c.call(c.get(&format!("/api/f/{name}/invites"))?)?;
                if j {
                    ok_exit(&v);
                }
                for i in v["invites"].as_array().cloned().unwrap_or_default() {
                    println!(
                        "{}\t{}\t{} left\texpires {}",
                        i["id"].as_str().unwrap_or(""),
                        i["role"].as_str().unwrap_or(""),
                        i["usesLeft"].as_u64().unwrap_or(0),
                        chrono_like(i["expiresAt"].as_u64().unwrap_or(0) / 1000),
                    );
                }
            }
            InviteCmd::Revoke { name, id } => {
                let v = c.call(c.delete(&format!("/api/f/{name}/invites/{id}"))?)?;
                if j {
                    ok_exit(&v);
                }
                println!("revoked invite {id}");
            }
        },
        Cmd::Join { name, token } => {
            let v = c.call(c.post_json(&format!("/api/f/{name}/join"), &json!({ "token": token }))?)?;
            if j {
                ok_exit(&v);
            }
            if v["joined"].as_bool().unwrap_or(false) {
                println!("joined {name} as {}", v["role"].as_str().unwrap_or(""));
            } else {
                println!("already a member of {name} ({})", v["role"].as_str().unwrap_or(""));
            }
        }
        Cmd::Call { name, op, input, id } => {
            let input: Value = serde_json::from_str(&input).context("--input must be JSON")?;
            let id = id.unwrap_or_else(|| format!("cli-{:016x}", rand::random::<u64>()));
            let v = c.call(c.post_json(&format!("/api/f/{name}/ops/{op}"), &json!({ "id": id, "input": input }))?)?;
            if j {
                ok_exit(&json!({ "id": id, "result": v["result"], "replayed": v["replayed"] }));
            }
            println!("{}", serde_json::to_string_pretty(&v["result"])?);
            if v["replayed"].as_bool().unwrap_or(false) {
                eprintln!("(replayed: operation {id} had already run)");
            }
        }
        Cmd::Channel { name, channel: None, .. } => {
            let v = c.call(c.get(&format!("/api/f/{name}/channels"))?)?;
            if j {
                ok_exit(&v);
            }
            for ch in v["channels"].as_array().cloned().unwrap_or_default() {
                println!("{}\t{}\t{} records", ch["name"].as_str().unwrap_or(""), ch["read"].as_str().unwrap_or(""), ch["seq"]);
            }
        }
        Cmd::Channel { name, channel: Some(channel), after, follow } => {
            if follow {
                if j {
                    fail_json("invalid_usage", "--follow streams JSON lines; --json does not apply", None, 2);
                }
                watch::follow_channel(&c, &name, &channel, after)?;
                return Ok(());
            }
            let v = c.call(c.get(&format!("/api/f/{name}/channels/{channel}?after={after}"))?)?;
            if j {
                ok_exit(&v);
            }
            for r in v["records"].as_array().cloned().unwrap_or_default() {
                println!("{}", serde_json::to_string(&r)?);
            }
        }
        Cmd::Visibility { name, value } => {
            let v = match value {
                None => {
                    let st = c.call(c.get(&format!("/api/f/{name}/status"))?)?;
                    json!({ "visibility": st["visibility"] })
                }
                Some(vis) => c.call(c.put_bytes(&format!("/api/f/{name}/visibility"), serde_json::to_vec(&json!({ "visibility": vis }))?)?)?,
            };
            if j {
                ok_exit(&v);
            }
            println!("{name}: {}", v["visibility"].as_str().unwrap_or(""));
        }
        Cmd::Login { .. } | Cmd::Whoami | Cmd::Host { .. } | Cmd::Guide | Cmd::New { .. } => unreachable!(),
    }
    Ok(())
}

/// 8 hex chars of the user's pubkey — the writer identity that names
/// conflict copies and signs commits
fn writer_id(c: &api::Client) -> String {
    c.id.pubkey_hex.chars().take(8).collect()
}

/// typed sync/code.storage errors -> anyhow. CAS rejections map to the
/// stable "conflict" machine code so `--json` consumers can branch.
fn cs_anyhow(e: impl Into<crate::sync::SyncError>) -> anyhow::Error {
    let e = e.into();
    match e {
        crate::sync::SyncError::Cs(CsError::CasRejected { .. }) => anyhow::Error::new(api::CodedError {
            code: "conflict",
            msg: e.to_string(),
        }),
        other => anyhow!("{other}"),
    }
}

fn edit_roles(c: &api::Client, name: &str, editors: Vec<String>, viewers: Vec<String>, add: bool, j: bool) {
    // The Rust cell keeps members in the cell (fragment.json no longer
    // grants access): grant and revoke become member changes there. The
    // TypeScript runtime has no members route and keeps the manifest path.
    if c.get(&format!("/api/f/{name}/members")).map(|r| r.ok()).unwrap_or(false) {
        if let Err(e) = (|| -> Result<()> {
            let mut changed = Vec::new();
            for (list, role) in [(editors, "editor"), (viewers, "viewer")] {
                for who in list {
                    let who = auth::resolve_npub(&who)?;
                    if add {
                        c.call(c.put_bytes(&format!("/api/f/{name}/members/{who}"), serde_json::to_vec(&json!({ "role": role }))?)?)?;
                    } else {
                        c.call(c.delete(&format!("/api/f/{name}/members/{who}"))?)?;
                    }
                    changed.push(json!({ "principal": who, "role": if add { role } else { "removed" } }));
                }
            }
            if j {
                ok_exit(&json!({ "members": changed }));
            }
            println!("members updated on {name}");
            Ok(())
        })() {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
        return;
    }
    if let Err(e) = (|| -> Result<()> {
        // fragment.json is a git file at the repo root: grant/revoke is
        // read-manifest, edit roles, commit — the same edit-and-commit
        // shape manifest-set uses (the PUT /manifest route is gone)
        let mut m = c.call(c.get(&format!("/api/f/{name}/manifest"))?)?;
        for (list, key) in [(editors, "editors"), (viewers, "viewers")] {
            let mut cur: Vec<String> = m[key]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect();
            for npub in list {
                // npubs or NIP-05 names (name@domain); resolved to canonical
                // npubs via the well-known path before touching the manifest.
                let npub = auth::resolve_npub(&npub)?;
                if add {
                    if !cur.contains(&npub) {
                        cur.push(npub);
                    }
                } else {
                    cur.retain(|x| x != &npub);
                }
            }
            m[key] = json!(cur);
        }
        let writer = writer_id(c);
        let tip = sync::commit_single_file(
            c,
            name,
            "fragment.json",
            serde_json::to_vec(&m)?,
            &format!("roles {name}"),
            &writer,
            codestorage_override().as_deref(),
        )
        .map_err(cs_anyhow)?;
        if j {
            ok_exit(&m);
        } else {
            println!("roles updated on {name} (commit {})", &tip[..8.min(tip.len())]);
        }
        Ok(())
    })() {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

/// unix seconds -> "YYYY-MM-DD HH:MM:SS" (UTC), no chrono dependency
fn chrono_like(secs: u64) -> String {
    let days = secs / 86400;
    let rem = secs % 86400;
    let (h, mi, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    // civil-from-days (Howard Hinnant's algorithm)
    let z = days as i64 + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02} {h:02}:{mi:02}:{s:02}Z")
}

// ---------- sync unit (keep a folder live without a terminal) ----------
// Writes a LaunchAgent (macOS) or systemd user unit (Linux) for one
// fragment+folder pair. Uses the CLI's own absolute path and an explicit
// PATH — launchd and systemd both run with minimal environments (the
// agent-built watch.sh failed on exactly this).

fn install_sync_unit(name: &str, dir: &Path, install: bool, mirror_from: Option<&str>) -> Result<()> {
    let dir = match dir.canonicalize() {
        Ok(d) => d,
        Err(_) => anyhow::bail!("no such directory: {}", dir.display()),
    };
    let home = std::env::var("HOME").context("HOME not set")?;
    let exe = std::env::current_exe()
        .and_then(|p| p.canonicalize())
        .context("cannot resolve the fragment binary path")?;
    let log = dir.join(".fragment").join("watch.log");
    let mirror_arg = mirror_from
        .map(|m| format!("    <string>--mirror-from</string>\n    <string>{m}</string>\n"))
        .unwrap_or_default();

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
            .replace("    __MIRROR__\n", &mirror_arg)
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
                .replace("__HOMEBIN__", &format!("{}/.local/bin:{}/.cargo/bin", home, home))
            .replace("    __MIRROR__\n", &mirror_arg);
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

fn uid() -> Result<String> {
    let out = std::process::Command::new("id").arg("-u").output().context("id -u failed")?;
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}
