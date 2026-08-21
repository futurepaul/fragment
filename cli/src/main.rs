mod api;
mod auth;
mod sync;

use anyhow::{anyhow, Context, Result};
use clap::{Parser, Subcommand};
use serde_json::{json, Value};
use std::io::Read;
use std::path::PathBuf;

const GUIDE: &str = include_str!("../GUIDE.md");

#[derive(Parser)]
#[command(name = "fragment", about = "make and run fragments: folder + sqlite + urls + workflows, on celld")]
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
    /// Sync a local folder with the fragment (bidirectional)
    Sync {
        name: String,
        #[arg(long, default_value = ".")]
        dir: PathBuf,
        /// Re-sync every N seconds
        #[arg(long)]
        watch: Option<u64>,
    },
    /// Sync (if a dir is given) then publish a draft; prints the draft URL
    Publish {
        name: String,
        #[arg(long)]
        dir: Option<PathBuf>,
        #[arg(long)]
        note: Option<String>,
    },
    /// List drafts
    Drafts { name: String },
    /// Promote a draft to the canonical URL
    Bless { name: String, slug: String },
    /// Run a workflow now
    Run { name: String, workflow: String, #[arg(long)] input: Option<String> },
    /// Manage secrets (values via env var of same name, or stdin)
    Secret {
        #[command(subcommand)]
        sub: SecretCmd,
    },
    /// Grant a role to an npub
    Grant { name: String, #[arg(long)] editor: Vec<String>, #[arg(long)] viewer: Vec<String> },
    /// Revoke a role from an npub
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
        Cmd::Guide => {
            print!("{GUIDE}");
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
            println!("  canonical:   {}/f/{}/", c.host, name);
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
        Cmd::Sync { name, dir, watch } => {
            loop {
                let report = sync::sync_once(&c, &name, &dir)?;
                println!("sync {} ({})", name, dir.display());
                report.print();
                match watch {
                    Some(secs) => std::thread::sleep(std::time::Duration::from_secs(secs)),
                    None => break,
                }
            }
        }
        Cmd::Publish { name, dir, note } => {
            if let Some(dir) = dir {
                let report = sync::sync_once(&c, &name, &dir)?;
                report.print();
            }
            let v = c.call(c.post_json(&format!("/api/f/{name}/drafts"), &json!({ "note": note }))?)?;
            if j {
                return out(&c, v, true);
            }
            let slug = v["slug"].as_str().unwrap_or("");
            println!("draft published: {}/d/{}/", c.host, slug);
            println!("bless with: fragment bless {} {}", name, slug);
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
            println!("blessed: {}{}", c.host, v["url"].as_str().unwrap_or(""));
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
            println!("canonical:  {}/f/{}/{}{}", c.host, name, suffix, view_part);
            println!("drafts at:  {}/d/<slug>/", c.host);
            println!("inbox:      {}/api/f/{}/inbox?t={}", c.host, name, inbox);
            println!("rooms:      {}/f/{}/__room/<room>{}{}", c.host, name, suffix, view_part);
        }
        Cmd::Login { .. } | Cmd::Whoami | Cmd::Guide => unreachable!(),
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
