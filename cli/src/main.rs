mod api;
mod auth;
mod sync;
mod watch;

include!(concat!(env!("OUT_DIR"), "/templates.rs"));

use crate::api::encode_q;
use crate::sync::{ConflictStrategy, Mode, SyncOptions};
use anyhow::{anyhow, Context, Result};
use clap::{Parser, Subcommand};
use serde_json::{json, Value};
use std::io::Read;
use std::path::PathBuf;

const GUIDE: &str = include_str!("../GUIDE.md");

#[derive(Parser)]
#[command(name = "fragment", version, about = "make and run fragments: folder + sqlite + urls + workflows, on celld")]
struct Cli {
    /// Host base URL (else FRAGMENT_HOST, else config, else http://127.0.0.1:8789)
    #[arg(long, global = true)]
    host: Option<String>,
    /// Machine-readable output (pass through server JSON)
    #[arg(long, global = true)]
    json: bool,
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
    Create { name: String },
    /// List fragments you have a role on
    List,
    /// Status of a fragment (counts, blessed draft, cron schedule, tokens)
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
    /// Sync a local folder with the fragment (default: bidirectional mirror)
    Sync {
        name: String,
        #[arg(long, default_value = ".")]
        dir: PathBuf,
        /// Keep syncing continuously (OS events + live channel + sweeps)
        #[arg(long)]
        watch: bool,
        /// push: local→remote only; pull: remote→local only (never deletes
        /// without --prune); mirror: bidirectional (default)
        #[arg(long)]
        mode: Option<String>,
        /// In pull mode, apply remote deletions locally too
        #[arg(long)]
        prune: bool,
        /// Conflict style: markers (default) or copy
        #[arg(long)]
        conflict_strategy: Option<String>,
        /// Allow a mass deletion to propagate (the guard refuses otherwise)
        #[arg(long)]
        apply_mass_delete: bool,
        /// Delete local state and start fresh (folder moved/replaced)
        #[arg(long)]
        rebuild_state: bool,
        /// Watch tuning: event debounce ms (50–5000, default 300)
        #[arg(long)]
        debounce_ms: Option<u64>,
        /// Watch tuning: poll fallback interval seconds (default 5)
        #[arg(long)]
        poll_interval: Option<u64>,
        /// Watch tuning: full rescan sweep seconds (default 60)
        #[arg(long)]
        rescan_secs: Option<u64>,
        /// Watch backend: auto (default), native, or poll
        #[arg(long)]
        watch_backend: Option<String>,
        /// Disable the live __watch channel in continuous mode
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
    Rm { name: String },
    /// Sync (if a dir is given) then publish a draft; prints the draft URL
    Publish {
        name: String,
        #[arg(long)]
        dir: Option<PathBuf>,
        #[arg(long)]
        note: Option<String>,
        /// Bless the draft immediately (continuous-publish style)
        #[arg(long)]
        bless: bool,
    },
    /// List drafts
    Drafts { name: String },
    /// Promote a draft to the canonical URL
    Bless { name: String, slug: String },
    /// Run a workflow now
    Run { name: String, workflow: String, #[arg(long)] input: Option<String> },
    /// List workflow runs (the run history; --status held shows parked failures)
    Runs {
        name: String,
        /// Filter by status: running | backoff | success | held | skipped | blocked
        #[arg(long)]
        status: Option<String>,
        #[arg(long, default_value = "30")]
        limit: u64,
    },
    /// Pause a workflow (auto-triggers stop; manual runs still work)
    Pause { name: String, workflow: String },
    /// Unpause a workflow (also clears the auto-pause breaker)
    Unpause { name: String, workflow: String },
    /// Re-run a held run with its original input (after fixing the workflow)
    Replay { name: String, run: u64 },
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
    /// Print the canonical, draft, and inbox URLs
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
enum SecretCmd {
    Set { name: String, key: String },
    List { name: String },
    Rm { name: String, key: String },
}

struct Config {
    host: Option<String>,
    secret_key: Option<String>,
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
        }
    } else {
        Config { host: None, secret_key: None }
    }
}

fn resolve_host(cli_host: &Option<String>, cfg: &Config) -> String {
    cli_host
        .clone()
        .or_else(|| std::env::var("FRAGMENT_HOST").ok())
        .or_else(|| cfg.host.clone())
        .unwrap_or_else(|| "http://127.0.0.1:8789".to_string())
}

fn require_client(cli_host: &Option<String>) -> Result<api::Client> {
    let cfg = load_config();
    let host = resolve_host(cli_host, &cfg);
    let sk = cfg.secret_key.ok_or_else(|| anyhow!("no keypair — run `fragment login` first"))?;
    let bytes = hex::decode(&sk).context("config secret_key is not hex")?;
    let arr: [u8; 32] = bytes.try_into().map_err(|_| anyhow!("config secret_key must be 32 bytes"))?;
    Ok(api::Client::new(&host, auth::Identity::from_secret(arr)))
}

fn out(cli: &api::Client, v: Value, json_flag: bool) -> Result<()> {
    let _ = cli;
    if json_flag {
        println!("{}", serde_json::to_string(&v)?);
    } else {
        println!("{}", serde_json::to_string_pretty(&v)?);
    }
    Ok(())
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.cmd {
        Cmd::Login { force } => {
            let p = config_path();
            if p.exists() && !force {
                let cfg = load_config();
                if let Some(sk) = cfg.secret_key {
                    let bytes = hex::decode(sk)?;
                    let id = auth::Identity::from_secret(bytes.try_into().map_err(|_| anyhow!("bad key"))?);
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
            println!("logged in: {}", id.npub());
            println!("config: {}", p.display());
            return Ok(());
        }
        Cmd::Whoami => {
            let c = require_client(&cli.host)?;
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
                    println!("default host set: {url}");
                    println!("config: {}", p.display());
                }
                None => println!("host: {}", resolve_host(&cli.host, &cfg)),
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
            println!("  fragment create <name>");
            println!("  fragment manifest-set <name> fragment.json");
            println!("  fragment publish <name> --dir . --bless");
            return Ok(());
        }
        _ => {}
    }

    let c = require_client(&cli.host)?;
    let j = cli.json;

    match cli.cmd {
        Cmd::Create { name } => {
            let v = c.call(c.post_json("/api/fragments", &json!({ "name": name }))?)?;
            if j {
                return out(&c, v, true);
            }
            println!("created fragment {}", v["name"].as_str().unwrap_or(&name));
            println!("  npub:        {}", v["npub"].as_str().unwrap_or(""));
            println!("  view token:  {}", v["viewToken"].as_str().unwrap_or(""));
            println!("  inbox token: {}", v["inboxToken"].as_str().unwrap_or(""));
            let canon = v["canonical"].as_str().filter(|s| s.starts_with("http")).map(|s| s.to_string())
                .unwrap_or_else(|| format!("{}/f/{}/", c.host, name));
            println!("  canonical:   {}", canon);
        }
        Cmd::List => {
            let v = c.call(c.get("/api/fragments")?)?;
            if j {
                return out(&c, v, true);
            }
            for f in v["fragments"].as_array().cloned().unwrap_or_default() {
                println!("{} ({})", f["name"].as_str().unwrap_or(""), f["role"].as_str().unwrap_or(""));
            }
        }
        Cmd::Status { name } => {
            let v = c.call(c.get(&format!("/api/f/{name}/status"))?)?;
            if j {
                return out(&c, v, true);
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
                return out(&c, json!({ "events": evs }), true);
            }
            for e in evs {
                let at = e["at"].as_u64().unwrap_or(0);
                let time = chrono_like(at / 1000);
                println!("[{}] {} {} — {}", e["id"], time, e["kind"].as_str().unwrap_or(""), e["summary"].as_str().unwrap_or(""));
            }
        }
        Cmd::Manifest { name } => {
            let v = c.call(c.get(&format!("/api/f/{name}/manifest"))?)?;
            println!("{}", serde_json::to_string_pretty(&v)?);
        }
        Cmd::ManifestSet { name, file } => {
            let v: Value = serde_json::from_slice(&std::fs::read(&file).with_context(|| format!("reading {}", file.display()))?)?;
            c.call(c.put_json(&format!("/api/f/{name}/manifest"), &v)?)?;
            println!("manifest updated");
        }
        Cmd::Sync {
            name, dir, watch, mode, prune, conflict_strategy, apply_mass_delete,
            rebuild_state, debounce_ms, poll_interval, rescan_secs, watch_backend,
            no_live, install, uninstall,
        } => {
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
                mode: match mode.as_deref() {
                    Some("push") => Mode::Push,
                    Some("pull") => Mode::Pull,
                    Some("mirror") | None => Mode::Mirror,
                    other => anyhow::bail!("--mode must be push|pull|mirror, got {other:?}"),
                },
                strategy: match conflict_strategy.as_deref() {
                    Some("copy") => ConflictStrategy::Copy,
                    _ => ConflictStrategy::Markers,
                },
                apply_mass_delete,
                prune,
                verify: false,
                writer_id: c.id.pubkey_hex.chars().take(8).collect(),
            };
            if watch {
                let cfg = watch::WatchConfig {
                    debounce_ms: debounce_ms.unwrap_or(300),
                    poll_interval: poll_interval.unwrap_or(5),
                    rescan_secs: rescan_secs.unwrap_or(60),
                    backend: watch_backend.clone().unwrap_or_else(|| "auto".into()),
                    live: !no_live,
                };
                watch::run(&c, &name, &dir, &opts, &cfg)?;
                return Ok(());
            }
            let report = sync::sync_once(&c, &name, &dir, &opts)?;
            if j {
                return out(&c, serde_json::to_value(&report).unwrap_or_default(), true);
            }
            println!("sync {} ({})", name, dir.display());
            report.print();
            std::process::exit(report.exit_code());
        }
        Cmd::Rm { name } => {
            let v = c.call(c.delete(&format!("/api/f/{name}"))?)?;
            if j {
                return out(&c, v, true);
            }
            println!("deleted fragment {name} (registry row + all data; the name is reusable)");
        }
        Cmd::Verify { name, dir } => {
            let report = sync::verify(&c, &name, &dir)?;
            if j {
                return out(&c, serde_json::to_value(&report).unwrap_or_default(), true);
            }
            println!("verify {} ({})", name, dir.display());
            report.print();
            std::process::exit(report.exit_code());
        }
        Cmd::Publish { name, dir, note, bless } => {
            if let Some(dir) = dir {
                let report = sync::sync_once(&c, &name, &dir, &SyncOptions::default())?;
                report.print();
            }
            let v = c.call(c.post_json(&format!("/api/f/{name}/drafts"), &json!({ "note": note }))?)?;
            if v.get("warning").and_then(|w| w.as_str()).is_some() {
                eprintln!("WARNING: {} — this draft will 404 at every URL", v["warning"].as_str().unwrap_or(""));
            }
            let slug = v["slug"].as_str().unwrap_or("").to_string();
            if bless && !slug.is_empty() {
                let b = c.call(c.post_json(&format!("/api/f/{name}/bless"), &json!({ "slug": slug }))?)?;
                if j {
                    let mut vv = v;
                    vv["blessed"] = b["url"].clone();
                    return out(&c, vv, true);
                }
                println!("draft published: {}/d/{}/", c.host, slug);
                let bu = b["url"].as_str().unwrap_or("");
                let blessed = if bu.starts_with("http") { bu.to_string() } else { format!("{}{}", c.host, bu) };
                println!("blessed: {}", blessed);
            } else {
                if j {
                    return out(&c, v, true);
                }
                println!("draft published: {}/d/{}/", c.host, slug);
                println!("bless with: fragment bless {} {}", name, slug);
            }
        }
        Cmd::Drafts { name } => {
            let v = c.call(c.get(&format!("/api/f/{name}/drafts"))?)?;
            if j {
                return out(&c, v, true);
            }
            for d in v["drafts"].as_array().cloned().unwrap_or_default() {
                println!(
                    "{} {}{}  {}",
                    d["slug"].as_str().unwrap_or(""),
                    if d["blessed"].as_bool().unwrap_or(false) { "[blessed] " } else { "" },
                    d["note"].as_str().unwrap_or(""),
                    d["at"].as_u64().map(|ms| chrono_like(ms / 1000)).unwrap_or_default(),
                );
            }
        }
        Cmd::Bless { name, slug } => {
            let v = c.call(c.post_json(&format!("/api/f/{name}/bless"), &json!({ "slug": slug }))?)?;
            let bu = v["url"].as_str().unwrap_or("");
            let blessed = if bu.starts_with("http") { bu.to_string() } else { format!("{}{}", c.host, bu) };
            println!("blessed: {}", blessed);
        }
        Cmd::Run { name, workflow, input } => {
            let input_v: Value = match input.as_deref() {
                Some(s) => serde_json::from_str(s).context("--input must be valid JSON")?,
                None => Value::Null,
            };
            let v = c.call(c.post_json(&format!("/api/f/{name}/run"), &json!({ "workflow": workflow, "input": input_v }))?)?;
            if j {
                return out(&c, v, true);
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
        Cmd::Runs { name, status, limit } => {
            let mut path = format!("/api/f/{name}/runs?limit={limit}");
            if let Some(s) = &status {
                path.push_str(&format!("&status={}", encode_q(s)));
            }
            let v = c.call(c.get(&path)?)?;
            if j {
                return out(&c, v, true);
            }
            let rows = v["runs"].as_array().cloned().unwrap_or_default();
            for r in &rows {
                let dur = r["durationMs"].as_u64().map(|d| format!("{d}ms")).unwrap_or_default();
                println!(
                    "#{}\t{}\t{}\t{}\ttry {}/{}\t{}",
                    r["id"].as_u64().unwrap_or(0),
                    r["via"].as_str().unwrap_or("?"),
                    r["wf"].as_str().unwrap_or("?"),
                    r["status"].as_str().unwrap_or("?"),
                    r["attempt"].as_u64().unwrap_or(0),
                    r["maxAttempts"].as_u64().unwrap_or(0),
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
                return out(&c, v, true);
            }
            println!("paused '{workflow}' (manual runs still work; unpause with `fragment unpause`)");
        }
        Cmd::Unpause { name, workflow } => {
            let v = c.call(c.post_json(&format!("/api/f/{name}/pause"), &json!({ "workflow": workflow, "paused": false }))?)?;
            if j {
                return out(&c, v, true);
            }
            println!("unpaused '{workflow}'");
        }
        Cmd::Replay { name, run } => {
            let v = c.call(c.post_json(&format!("/api/f/{name}/replay"), &json!({ "run": run }))?)?;
            if j {
                return out(&c, v, true);
            }
            if v["ok"].as_bool().unwrap_or(false) {
                println!("replayed run #{run} → ok");
            } else {
                println!("replayed run #{run} → error: {}", v["error"].as_str().unwrap_or("unknown"));
            }
        }
        Cmd::Secret { sub } => match sub {
            SecretCmd::Set { name, key } => {
                let value = match std::env::var(&key) {
                    Ok(v) => v,
                    Err(_) => {
                        eprint!("value for {key} (stdin): ");
                        let mut buf = String::new();
                        std::io::stdin().read_to_string(&mut buf)?;
                        buf.trim().to_string()
                    }
                };
                if value.is_empty() {
                    anyhow::bail!("empty secret value");
                }
                c.call(c.put_bytes(&format!("/api/f/{name}/secrets/{key}"), value.into_bytes())?)?;
                println!("secret {key} set on {name}");
            }
            SecretCmd::List { name } => {
                let v = c.call(c.get(&format!("/api/f/{name}/secrets"))?)?;
                for n in v["names"].as_array().cloned().unwrap_or_default() {
                    println!("{}", n.as_str().unwrap_or(""));
                }
            }
            SecretCmd::Rm { name, key } => {
                c.call(c.delete(&format!("/api/f/{name}/secrets/{key}"))?)?;
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
            let v: Value = serde_json::from_slice(&resp.bytes()?)?;
            out(&c, v, true)?;
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
            println!("canonical:  {}{}{}", canon, suffix, view_part);
            println!("drafts at:  {}/d/<slug>/", c.host);
            println!("inbox:      {}/api/f/{}/inbox?t={}", c.host, name, inbox);
            println!("rooms:      {}/f/{}/__room/<room>{}{}", c.host, name, suffix, view_part);
        }
        Cmd::Login { .. } | Cmd::Whoami | Cmd::Host { .. } | Cmd::Guide | Cmd::New { .. } => unreachable!(),
    }
    Ok(())
}

fn edit_roles(c: &api::Client, name: &str, editors: Vec<String>, viewers: Vec<String>, add: bool, j: bool) {
    if let Err(e) = (|| -> Result<()> {
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
        c.call(c.put_json(&format!("/api/f/{name}/manifest"), &m)?)?;
        if !j {
            println!("roles updated on {name}");
        } else {
            println!("{}", serde_json::to_string(&m)?);
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

fn install_sync_unit(name: &str, dir: &PathBuf, install: bool) -> Result<()> {
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

fn uid() -> Result<String> {
    let out = std::process::Command::new("id").arg("-u").output().context("id -u failed")?;
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}
