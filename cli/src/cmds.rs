//! Command implementations. Each takes a `Ctx` (host + identity + output mode).

use crate::auth::{npub_decode, Identity};
use crate::client::{url_encode, Client};
use crate::config::{self, Config};
use crate::sync::{self, SyncState, SyncSummary};
use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use std::io::Read;
use std::path::{Path, PathBuf};

pub struct Output {
    pub json: bool,
}

impl Output {
    /// Print `v` as JSON in --json mode, otherwise run the human formatter.
    pub fn emit(&self, v: &Value, human: impl FnOnce()) {
        if self.json {
            println!("{}", serde_json::to_string_pretty(v).expect("json"));
        } else {
            human();
        }
    }
}

pub struct Ctx {
    pub host: String,
    pub out: Output,
    cfg: Option<Config>,
}

impl Ctx {
    pub fn new(host: String, json: bool, cfg: Option<Config>) -> Self {
        Ctx {
            host,
            out: Output { json },
            cfg,
        }
    }

    /// Signed client, or an error telling the user to log in.
    pub fn client(&self) -> Result<Client> {
        let cfg = self
            .cfg
            .as_ref()
            .ok_or_else(|| anyhow!("not logged in — run `fragment login` first"))?;
        let id = Identity::from_secret_hex(&cfg.secret_key)?;
        Client::new(&self.host, Some(id))
    }

    /// Unsigned client (token-only endpoints).
    pub fn unsigned_client(&self) -> Result<Client> {
        Client::new(&self.host, None)
    }
}

// ---------- identity ----------

pub fn login(host: String, force: bool, out: &Output) -> Result<()> {
    let path = config::config_path()?;
    if path.exists() && !force {
        bail!(
            "already logged in ({} exists). Use `fragment login --force` to replace the key — \
             the old key's ownership of fragments will be lost.",
            path.display()
        );
    }
    let id = Identity::generate();
    let cfg = Config {
        secret_key: id.secret_hex(),
        host: host.clone(),
    };
    config::save(&cfg)?;
    let npub = id.npub()?;
    out.emit(
        &json!({ "npub": npub, "host": host, "config": path.display().to_string() }),
        || {
            println!("logged in");
            println!("  npub:   {npub}");
            println!("  host:   {host}");
            println!("  config: {} (mode 0600)", path.display());
        },
    );
    Ok(())
}

pub fn whoami(ctx: &Ctx) -> Result<()> {
    let cfg = ctx
        .cfg
        .as_ref()
        .ok_or_else(|| anyhow!("not logged in — run `fragment login` first"))?;
    let id = Identity::from_secret_hex(&cfg.secret_key)?;
    let npub = id.npub()?;
    ctx.out.emit(&json!({ "npub": npub, "host": ctx.host }), || {
        println!("npub: {npub}");
        println!("host: {}", ctx.host);
    });
    Ok(())
}

// ---------- fragments ----------

pub fn create(ctx: &Ctx, name: &str) -> Result<()> {
    let v: Value = ctx.client()?.json("POST", "/api/fragments", Some(&json!({ "name": name })))?;
    let npub = v["npub"].as_str().unwrap_or("");
    let canonical = format!("{}/f/{name}/", ctx.host);
    let inbox = match v["inboxToken"].as_str() {
        Some(t) => format!("{}/api/f/{name}/inbox?t={t}", ctx.host),
        None => format!("{}/api/f/{name}/inbox?t=<inboxToken>", ctx.host),
    };
    let view = v["viewToken"].as_str().map(|t| format!("{canonical}?view={t}"));
    let mut shown = v.clone();
    shown["canonicalUrl"] = json!(canonical);
    shown["inboxUrl"] = json!(inbox);
    ctx.out.emit(&shown, || {
        println!("created fragment '{name}'");
        println!("  npub:      {npub}");
        println!("  canonical: {canonical}");
        println!("  inbox:     {inbox}");
        if let Some(view) = view {
            println!("  view link: {view}");
        }
        println!("the host remembers everything; nothing was saved locally");
    });
    Ok(())
}

pub fn list(ctx: &Ctx) -> Result<()> {
    let v: Value = ctx.client()?.json("GET", "/api/fragments", None)?;
    let empty = vec![];
    let frags = v["fragments"].as_array().unwrap_or(&empty);
    ctx.out.emit(&v, || {
        if frags.is_empty() {
            println!("no fragments (create one with `fragment create <name>`)");
        }
        for f in frags {
            println!("{}\t{}", f["name"].as_str().unwrap_or("?"), f["role"].as_str().unwrap_or("?"));
        }
    });
    Ok(())
}

pub fn status(ctx: &Ctx, name: &str) -> Result<()> {
    let v: Value = ctx.client()?.json("GET", &format!("/api/f/{name}/status"), None)?;
    ctx.out.emit(&v, || {
        println!("{}", serde_json::to_string_pretty(&v).unwrap());
    });
    Ok(())
}

pub fn events(ctx: &Ctx, name: &str, since: Option<u64>) -> Result<()> {
    let v: Value = ctx.client()?.json(
        "GET",
        &format!("/api/f/{name}/events?since={}", since.unwrap_or(0)),
        None,
    )?;
    let empty = vec![];
    let evs = v["events"].as_array().unwrap_or(&empty);
    ctx.out.emit(&v, || {
        for e in evs {
            println!(
                "#{}\t{}\t{}\t{}",
                e["id"].as_u64().map(|i| i.to_string()).unwrap_or_else(|| "?".into()),
                e["at"].as_u64().map(|a| a.to_string()).unwrap_or_else(|| "?".into()),
                e["kind"].as_str().unwrap_or("?"),
                e["summary"].as_str().unwrap_or(""),
            );
        }
        if evs.is_empty() {
            println!("(no events)");
        }
    });
    Ok(())
}

// ---------- manifest ----------

pub fn manifest(ctx: &Ctx, name: &str) -> Result<()> {
    let v: Value = ctx.client()?.json("GET", &format!("/api/f/{name}/manifest"), None)?;
    println!("{}", serde_json::to_string_pretty(&v)?);
    Ok(())
}

pub fn manifest_set(ctx: &Ctx, name: &str, file: &Path) -> Result<()> {
    let text = std::fs::read_to_string(file).with_context(|| format!("cannot read {}", file.display()))?;
    let parsed: Value = serde_json::from_str(&text)
        .with_context(|| format!("{} is not valid JSON", file.display()))?;
    let v: Value = ctx
        .client()?
        .json("PUT", &format!("/api/f/{name}/manifest"), Some(&parsed))?;
    ctx.out.emit(&v, || println!("manifest updated for '{name}'"));
    Ok(())
}

/// grant/revoke are manifest edits: GET, modify editors/viewers, PUT.
pub fn grant_revoke(ctx: &Ctx, name: &str, editor: &Option<String>, viewer: &Option<String>, grant: bool) -> Result<()> {
    let (field, npub) = match (editor, viewer) {
        (Some(n), None) => ("editors", n),
        (None, Some(n)) => ("viewers", n),
        _ => bail!("specify exactly one of --editor <npub> or --viewer <npub>"),
    };
    npub_decode(npub).with_context(|| format!("'{npub}' is not a valid npub"))?;

    let client = ctx.client()?;
    let path = format!("/api/f/{name}/manifest");
    let mut m: Value = client.json("GET", &path, None)?;
    if !m.is_object() {
        bail!("server returned a non-object manifest");
    }
    let arr = m
        .as_object_mut()
        .unwrap()
        .entry(field.to_string())
        .or_insert_with(|| json!([]));
    let arr = arr
        .as_array_mut()
        .ok_or_else(|| anyhow!("manifest field '{field}' is not an array"))?;
    let already = arr.iter().any(|x| x.as_str() == Some(npub));
    if grant {
        if !already {
            arr.push(json!(npub));
        }
    } else {
        arr.retain(|x| x.as_str() != Some(npub));
    }
    let verb = if grant { "granted" } else { "revoked" };
    let role = field.trim_end_matches('s');
    let _: Value = client.json("PUT", &path, Some(&m))?;
    ctx.out.emit(&json!({ "ok": true, "manifest": m }), || {
        if !grant && already == false {
            println!("{npub} was not in {field} of '{name}' (no change)");
        } else {
            println!("{verb} {role} {npub} on '{name}'");
        }
    });
    Ok(())
}

// ---------- sync ----------

fn load_or_init_state(dir: &Path, name: &str, host: &str) -> Result<SyncState> {
    match sync::load_state(dir)? {
        Some(st) => {
            if st.name != name {
                bail!(
                    "{} is synced with fragment '{}', not '{name}'. Use a different --dir.",
                    dir.display(),
                    st.name
                );
            }
            Ok(st)
        }
        None => Ok(SyncState::new(name, host)),
    }
}

pub fn sync_once(ctx: &Ctx, name: &str, dir: &Path, push_only: bool) -> Result<SyncSummary> {
    let client = ctx.client()?;
    let mut state = load_or_init_state(dir, name, &ctx.host)?;
    let mut summary = SyncSummary::default();
    sync::push_phase(&client, name, dir, &mut state, &mut summary)?;
    if !push_only {
        sync::pull_phase(&client, name, dir, &mut state, &mut summary)?;
    }
    sync::save_state(dir, &state)?;
    Ok(summary)
}

fn print_summary(name: &str, s: &SyncSummary) {
    println!("sync '{name}': {} pushed, {} pulled, {} conflicts, {} unchanged",
        s.pushed.len(), s.pulled.len(), s.conflicts.len(), s.unchanged);
    for p in &s.pushed {
        println!("  pushed:  {p}");
    }
    for p in &s.pulled {
        println!("  pulled:  {p}");
    }
    for p in &s.deleted_remote {
        println!("  deleted remotely: {p}");
    }
    for p in &s.deleted_local {
        println!("  deleted locally:  {p}");
    }
    for p in &s.conflicts {
        println!("  CONFLICT: {p} (remote version saved as {p}.remote-<ts>; local kept)");
    }
}

pub fn sync_cmd(ctx: &Ctx, name: &str, dir: &Path, watch: Option<u64>) -> Result<()> {
    match watch {
        None => {
            let s = sync_once(ctx, name, dir, false)?;
            ctx.out.emit(&json!(s), || print_summary(name, &s));
            Ok(())
        }
        Some(secs) => {
            if secs == 0 {
                bail!("--watch interval must be >= 1 second");
            }
            loop {
                match sync_once(ctx, name, dir, false) {
                    Ok(s) => ctx.out.emit(&json!(s), || print_summary(name, &s)),
                    Err(e) => eprintln!("sync error: {e:#}"),
                }
                std::thread::sleep(std::time::Duration::from_secs(secs));
            }
        }
    }
}

// ---------- drafts / bless ----------

pub fn publish(ctx: &Ctx, name: &str, dir: &Path, note: &Option<String>) -> Result<()> {
    // sync first (push), then snapshot
    let s = sync_once(ctx, name, dir, true)?;
    if !s.is_clean() {
        eprintln!(
            "warning: sync had {} conflict(s); the draft snapshots the remote state",
            s.conflicts.len()
        );
    }
    let body = match note {
        Some(n) => json!({ "note": n }),
        None => json!({}),
    };
    let v: Value = ctx.client()?.json("POST", &format!("/api/f/{name}/drafts"), Some(&body))?;
    let url = v["url"].as_str().map(|u| absolutize(&ctx.host, u));
    ctx.out.emit(&v, || {
        print_summary(name, &s);
        println!("draft {}: {}", v["slug"].as_str().unwrap_or("?"), url.as_deref().unwrap_or("?"));
    });
    Ok(())
}

/// Server results may carry path-only URLs; make them absolute for printing.
fn absolutize(host: &str, url: &str) -> String {
    if url.starts_with("http://") || url.starts_with("https://") {
        url.to_string()
    } else {
        format!("{}{}", host.trim_end_matches('/'), if url.starts_with('/') { url.to_string() } else { format!("/{url}") })
    }
}

pub fn bless(ctx: &Ctx, name: &str, slug: &str) -> Result<()> {
    let v: Value = ctx.client()?.json("POST", &format!("/api/f/{name}/bless"), Some(&json!({ "slug": slug })))?;
    let url = v["url"].as_str().map(|u| absolutize(&ctx.host, u));
    ctx.out.emit(&v, || {
        println!("blessed {slug} for '{name}'");
        println!("canonical: {}", url.as_deref().unwrap_or("?"));
    });
    Ok(())
}

pub fn drafts(ctx: &Ctx, name: &str) -> Result<()> {
    let v: Value = ctx.client()?.json("GET", &format!("/api/f/{name}/drafts"), None)?;
    let empty = vec![];
    let ds = v["drafts"].as_array().unwrap_or(&empty);
    ctx.out.emit(&v, || {
        for d in ds {
            println!(
                "{}{}\t{}\t{}",
                d["slug"].as_str().unwrap_or("?"),
                if d["blessed"].as_bool().unwrap_or(false) { " (blessed)" } else { "" },
                d["at"].as_u64().map(|a| a.to_string()).unwrap_or_else(|| "?".into()),
                d["note"].as_str().unwrap_or(""),
            );
        }
        if ds.is_empty() {
            println!("(no drafts)");
        }
    });
    Ok(())
}

// ---------- workflows ----------

pub fn run(ctx: &Ctx, name: &str, workflow: &str, input: &Option<String>) -> Result<()> {
    let body = match input {
        Some(raw) => {
            let parsed: Value = serde_json::from_str(raw)
                .with_context(|| "--input must be valid JSON")?;
            json!({ "workflow": workflow, "input": parsed })
        }
        None => json!({ "workflow": workflow }),
    };
    let v: Value = ctx.client()?.json("POST", &format!("/api/f/{name}/run"), Some(&body))?;
    ctx.out.emit(&v, || {
        if v["ok"].as_bool().unwrap_or(false) {
            println!("ok");
        } else {
            println!("error: {}", v["error"].as_str().unwrap_or("unknown"));
        }
        if let Some(out) = v.get("output").filter(|o| !o.is_null()) {
            println!("output: {}", serde_json::to_string_pretty(out).unwrap());
        }
        if let Some(evs) = v["events"].as_array() {
            for e in evs {
                println!(
                    "event #{}\t{}\t{}",
                    e["id"].as_u64().map(|i| i.to_string()).unwrap_or_else(|| "?".into()),
                    e["kind"].as_str().unwrap_or("?"),
                    e["summary"].as_str().unwrap_or(""),
                );
            }
        }
    });
    Ok(())
}

// ---------- secrets ----------

pub fn secret_set(ctx: &Ctx, name: &str, key: &str) -> Result<()> {
    // value comes from the env var of the same name, or a stdin prompt — never argv
    let value = match std::env::var(key) {
        Ok(v) if !v.is_empty() => v,
        _ => {
            eprint!("value for {key} (stdin): ");
            let mut buf = String::new();
            std::io::stdin().read_to_string(&mut buf)?;
            let v = buf.trim_end_matches(['\n', '\r']).to_string();
            if v.is_empty() {
                bail!("empty value; set the ${key} env var or pipe the value on stdin");
            }
            v
        }
    };
    let v: Value = ctx.client()?.json(
        "PUT",
        &format!("/api/f/{name}/secrets/{}", url_encode(key)),
        Some(&json!(value)),
    )?;
    // ^ contract says raw body = value; send raw bytes instead of JSON:
    let _ = v;
    let resp = ctx.client()?.call(
        "PUT",
        &format!("/api/f/{name}/secrets/{}", url_encode(key)),
        Some(value.into_bytes()),
    )?;
    let v: Value = crate::client::expect_ok(resp, "PUT", "secrets")?.json()?;
    ctx.out.emit(&v, || println!("secret {key} set on '{name}'"));
    Ok(())
}

pub fn secret_list(ctx: &Ctx, name: &str) -> Result<()> {
    let v: Value = ctx.client()?.json("GET", &format!("/api/f/{name}/secrets"), None)?;
    let empty = vec![];
    let names = v["names"].as_array().unwrap_or(&empty);
    ctx.out.emit(&v, || {
        for n in names {
            println!("{}", n.as_str().unwrap_or("?"));
        }
        if names.is_empty() {
            println!("(no secrets set)");
        }
    });
    Ok(())
}

pub fn secret_rm(ctx: &Ctx, name: &str, key: &str) -> Result<()> {
    let v: Value = ctx.client()?.json(
        "DELETE",
        &format!("/api/f/{name}/secrets/{}", url_encode(key)),
        None,
    )?;
    ctx.out.emit(&v, || println!("secret {key} removed from '{name}'"));
    Ok(())
}

// ---------- misc ----------

pub fn inbox(ctx: &Ctx, name: &str, token: &str, payload: &str) -> Result<()> {
    let parsed: Value = serde_json::from_str(payload).with_context(|| "--payload must be valid JSON")?;
    let v: Value = ctx.unsigned_client().json(
        "POST",
        &format!("/api/f/{name}/inbox?t={}", url_encode(token)),
        Some(&json!({ "payload": parsed })),
    )?;
    ctx.out.emit(&v, || {
        println!("delivered to inbox of '{name}' (id {})", v["id"].as_u64().map(|i| i.to_string()).unwrap_or_else(|| "?".into()));
    });
    Ok(())
}

pub fn open(ctx: &Ctx, name: &str) -> Result<()> {
    let canonical = format!("{}/f/{name}/", ctx.host);
    let draft = format!("{}/d/<slug>/", ctx.host);
    let inbox = format!("{}/api/f/{name}/inbox?t=<inboxToken>", ctx.host);
    ctx.out.emit(
        &json!({ "canonical": canonical, "draft": draft, "inbox": inbox }),
        || {
            println!("canonical: {canonical}");
            println!("draft:     {draft}");
            println!("inbox:     {inbox}");
        },
    );
    Ok(())
}

pub fn guide(out: &Output) -> Result<()> {
    let _ = out;
    print!("{}", include_str!("../GUIDE.md"));
    Ok(())
}

pub fn default_dir() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}
