//! `cargo xtask deploy --config <file> [--branch <name>]` and `cargo xtask
//! teardown --config <file> --branch <name>` (docs/cloudflare-v1.md,
//! decisions 4 and 20).
//!
//! A deployment's configuration is a file outside the repo (JSONC, keys as
//! in `Deployment`), its secrets files named by path in it, so anyone
//! deploys their own without a fork. From it and the checked-in configs
//! (`cell/wrangler.jsonc`, `agent/wrangler.jsonc`) this renders each
//! Worker's own wrangler config under `target/deploy/<name>/`, makes the
//! bucket and queues it names, and runs `wrangler deploy` with the secrets
//! in a file of mode 600 that is removed after.
//!
//! A branch deployment (`--branch b`) is a complete copy beside the others
//! in one account and zone: its Workers, Durable Objects, Workflow, queues
//! and bucket are named for it, its platform is `b.<zone>`, and its
//! fragments are `<label>--<username>--b.<zone>` (one wildcard DNS record
//! and certificate cover them all). Its repos are named `b--…` in the
//! code.storage org, so it never touches another deployment's.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};
use fragment_devstack as devstack;
use serde::Deserialize;
use serde_json::{json, Value};

/// A deployment's settings (its config file). Unknown keys are refused, so
/// a misspelt one never silently does nothing.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Deployment {
    /// The Cloudflare account it deploys to.
    account_id: String,
    /// The zone its routes are on (it must be proxied by Cloudflare).
    zone: String,
    /// The platform's host and the fragments' suffix, for a deployment of
    /// its own (production: `fragment.club`, `fragment.boats`). A branch
    /// deployment's come from the zone.
    platform_host: Option<String>,
    fragment_suffix: Option<String>,
    /// A Cloudflare API token with DNS Edit on the zones, by path: with it,
    /// a deploy makes the proxied record its routes need when it is
    /// missing (xtask/src/dns.rs). Without it, they are made by hand.
    dns_token_file: Option<PathBuf>,
    /// Files holding the secrets.
    host_secret_file: PathBuf,
    codestorage: CodeStorage,
    workos: WorkOs,
    /// Who may grant credit, set plans, and release usernames (npubs).
    #[serde(default)]
    operators: Vec<String>,
    /// The AI Gateway the model route calls through (its id, named: never
    /// `default`, which makes one that logs). Without it, models are off.
    ai_gateway: Option<String>,
    /// A new person's plan: `guest` (the default), `seat`, or `seat_always_on`.
    default_plan: Option<String>,
    /// Computers (docs/computers.md): the images they run, and the one a
    /// new computer is pinned to. Without it, the deployment makes none.
    computers: Option<Computers>,
    /// The WorkOS Pipes providers a computer's swap offers, each with its
    /// hosts (`{"github": ["api.github.com"]}`; decision 22).
    #[serde(default)]
    connections: BTreeMap<String, Vec<String>>,
    /// The operator's keys a computer's swap offers (decision 37).
    #[serde(default)]
    operator_keys: BTreeMap<String, OperatorKey>,
    /// The price book's version: raise it with every change to a key's
    /// price, or ledgers made before keep the book they have.
    price_book_version: Option<u32>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Computers {
    /// The image a new computer is pinned to (one of `images`).
    default_image: String,
    /// By name: each image's Dockerfile and build context, relative to the
    /// repo (`images/hermes/Dockerfile`, `.`: the Hermes image carries the CLI,
    /// so it builds from the repo root), and its build variables.
    images: BTreeMap<String, Image>,
    // No size here: a Durable Object's container is sized as it starts, at
    // the instance its awake time is priced at (fragment_core::price
    // `instance_size`; decision 13's 2 vCPU and 6 GiB), and wrangler
    // refuses `instance_type` for one.
}

#[derive(Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct Image {
    dockerfile: String,
    build_context: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    build_vars: BTreeMap<String, String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OperatorKey {
    hosts: Vec<String>,
    /// The file holding the key.
    key_file: PathBuf,
    /// What its use costs at list price: `micros` per `per` calls (the
    /// margin is added). A key with no price is not lent.
    price: KeyPrice,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct KeyPrice {
    micros: i64,
    per: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CodeStorage {
    org: String,
    private_key_file: PathBuf,
    /// The API base (default `https://api.<org>.code.storage`).
    api: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkOs {
    client_id_file: PathBuf,
    api_key_file: PathBuf,
}

/// What `--branch` may be: a DNS label short enough that
/// `<label>--<username>--<branch>` fits in one (63 bytes).
fn valid_branch(b: &str) -> bool {
    (1..=16).contains(&b.len())
        && b.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
        && !b.starts_with('-')
        && !b.ends_with('-')
        && !b.contains("--")
}

fn expand(path: &Path) -> PathBuf {
    match path.strip_prefix("~") {
        Ok(rest) => PathBuf::from(std::env::var_os("HOME").expect("HOME is set")).join(rest),
        Err(_) => path.to_path_buf(),
    }
}

/// A secret's value, read from its file (never printed).
fn read_secret(path: &Path) -> Result<String> {
    let path = expand(path);
    let text = fs::read_to_string(&path).with_context(|| format!("read the secret file {}", path.display()))?;
    let text = text.trim().to_string();
    anyhow::ensure!(!text.is_empty(), "the secret file {} is empty", path.display());
    Ok(text)
}

/// The names and hosts one deployment has.
struct Names {
    /// `fragment` or `fragment-<branch>`: the platform Worker's.
    cell: String,
    agent: String,
    jobs: String,
    deliveries: String,
    dead: String,
    /// The meters' queue (cell/src/meter.rs).
    ledger: String,
    bucket: String,
    platform_host: String,
    suffix: String,
    /// `--<branch>`, for a branch.
    label_suffix: Option<String>,
    routes: Vec<String>,
    /// The hosts those routes answer on, which must be proxied.
    dns: Vec<crate::dns::Wanted>,
    /// Repos this deployment makes are named with this before them.
    repo_prefix: Option<String>,
}

fn names(d: &Deployment, branch: Option<&str>) -> Result<Names> {
    match (branch, &d.platform_host, &d.fragment_suffix) {
        (Some(b), None, None) => {
            anyhow::ensure!(valid_branch(b), "a branch is 1-16 of a-z, 0-9 and single dashes inside, not {b:?}");
            Ok(Names {
                cell: format!("fragment-{b}"),
                agent: format!("fragment-agent-{b}"),
                jobs: format!("fragment-jobs-{b}"),
                deliveries: format!("fragment-deliveries-{b}"),
                dead: format!("fragment-deliveries-dead-{b}"),
                ledger: format!("fragment-ledger-{b}"),
                bucket: format!("fragment-blobs-{b}"),
                platform_host: format!("{b}.{}", d.zone),
                suffix: d.zone.clone(),
                label_suffix: Some(format!("--{b}")),
                routes: vec![format!("{b}.{}/*", d.zone), format!("*--{b}.{}/*", d.zone)],
                // one wildcard covers every branch's platform and fragments
                dns: vec![crate::dns::Wanted { zone: d.zone.clone(), name: format!("*.{}", d.zone) }],
                repo_prefix: Some(format!("{b}--")),
            })
        }
        (None, Some(platform), Some(suffix)) => Ok(Names {
            cell: "fragment".into(),
            agent: "fragment-agent".into(),
            jobs: "fragment-jobs".into(),
            deliveries: "fragment-deliveries".into(),
            dead: "fragment-deliveries-dead".into(),
            ledger: "fragment-ledger".into(),
            bucket: "fragment-blobs".into(),
            platform_host: platform.clone(),
            suffix: suffix.clone(),
            label_suffix: None,
            routes: vec![format!("{platform}/*"), format!("*.{suffix}/*")],
            // each its own zone on the account (the config's comment says so)
            dns: vec![
                crate::dns::Wanted { zone: platform.clone(), name: platform.clone() },
                crate::dns::Wanted { zone: suffix.clone(), name: format!("*.{suffix}") },
            ],
            repo_prefix: None,
        }),
        (Some(_), _, _) => bail!("a config with platform_host and fragment_suffix deploys one deployment of its own: no --branch"),
        (None, _, _) => bail!("name a --branch (or give the config both platform_host and fragment_suffix)"),
    }
}

fn load(config: &Path) -> Result<Deployment> {
    let text = fs::read_to_string(config).with_context(|| format!("read {}", config.display()))?;
    let d: Deployment = serde_json::from_str(&devstack::strip_comments(&text)).with_context(|| format!("parse {}", config.display()))?;
    checked(d).with_context(|| format!("check {}", config.display()))
}

/// What the cell would refuse at its first request, refused before a
/// deploy: the swap's names and hosts, and a default image it has.
fn checked(d: Deployment) -> Result<Deployment> {
    fragment_core::swap::parse_hosts(&serde_json::to_string(&d.connections)?).map_err(|e| anyhow::anyhow!("connections: {e}"))?;
    let hosts: BTreeMap<&String, &Vec<String>> = d.operator_keys.iter().map(|(name, k)| (name, &k.hosts)).collect();
    fragment_core::swap::parse_hosts(&serde_json::to_string(&hosts)?).map_err(|e| anyhow::anyhow!("operator_keys: {e}"))?;
    if let Some((name, _)) = d.operator_keys.iter().find(|(_, k)| k.price.micros < 1 || k.price.per < 1) {
        bail!("operator_keys: {name}'s price is at least one micro-dollar per at least one call");
    }
    if let Some(c) = &d.computers {
        if !c.images.contains_key(&c.default_image) {
            bail!("computers: the default image {:?} is not one of its images", c.default_image);
        }
    }
    Ok(d)
}

fn args(rest: &[String]) -> Result<(PathBuf, Option<String>)> {
    let usage = || anyhow::anyhow!("usage: cargo xtask deploy|teardown --config <file> [--branch <name>]");
    let (mut config, mut branch) = (None, None);
    let mut it = rest.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--config" => config = Some(PathBuf::from(it.next().ok_or_else(usage)?)),
            "--branch" => branch = Some(it.next().ok_or_else(usage)?.clone()),
            _ => return Err(usage()),
        }
    }
    Ok((config.ok_or_else(usage)?, branch))
}

fn wrangler(tools: &devstack::Tools, account: &str) -> Command {
    let mut c = Command::new(&tools.wrangler);
    c.env("CLOUDFLARE_ACCOUNT_ID", account).env("WRANGLER_SEND_METRICS", "false").current_dir(devstack::repo_root());
    c
}

/// Runs wrangler; an answer that says the thing exists already is success
/// (making the bucket and queues is idempotent).
fn ensure(cmd: &mut Command, what: &str) -> Result<()> {
    let out = cmd.stdin(Stdio::null()).output().with_context(|| format!("run wrangler for {what}"))?;
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    if out.status.success() || text.contains("already exists") || text.contains("already taken") {
        return Ok(());
    }
    bail!("{what}: {}", text.trim())
}

fn git_head() -> Result<String> {
    let out = Command::new("git").args(["rev-parse", "--short=12", "HEAD"]).current_dir(devstack::repo_root()).output()?;
    anyhow::ensure!(out.status.success(), "git rev-parse HEAD");
    let dirty = Command::new("git").args(["status", "--porcelain"]).current_dir(devstack::repo_root()).output()?;
    let sha = String::from_utf8_lossy(&out.stdout).trim().to_string();
    Ok(if dirty.stdout.is_empty() { sha } else { format!("{sha}-dirty") })
}

/// A file only its owner reads, removed when dropped.
struct SecretFile(PathBuf);

impl SecretFile {
    fn write(path: PathBuf, value: &Value) -> Result<SecretFile> {
        let _ = fs::remove_file(&path);
        let mut f = fs::OpenOptions::new().create_new(true).write(true).mode(0o600).open(&path)?;
        f.write_all(value.to_string().as_bytes())?;
        Ok(SecretFile(path))
    }
}

impl Drop for SecretFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

pub fn deploy(rest: &[String]) -> Result<()> {
    let (config, branch) = args(rest)?;
    let d = load(&config)?;
    let n = names(&d, branch.as_deref())?;
    // every secret is read before anything is built or made
    let host_secret = read_secret(&d.host_secret_file)?;
    let org_key = read_secret(&d.codestorage.private_key_file)?;
    let workos_client = read_secret(&d.workos.client_id_file)?;
    let workos_key = read_secret(&d.workos.api_key_file)?;
    let dns_token = d.dns_token_file.as_deref().map(read_secret).transpose()?;
    if let Some(p) = &d.default_plan {
        anyhow::ensure!(matches!(p.as_str(), "guest" | "seat" | "seat_always_on"), "default_plan is guest, seat or seat_always_on, not {p:?}");
    }
    anyhow::ensure!(d.ai_gateway.as_deref() != Some("default"), "ai_gateway names the deployment's own gateway: `default` makes one that logs");
    let operator_keys: Vec<(String, String)> =
        d.operator_keys.iter().map(|(name, k)| Ok((fragment_core::swap::key_secret_name(name), read_secret(&k.key_file)?))).collect::<Result<_>>()?;
    let tools = devstack::Tools::locate()?;
    crate::build()?;
    let deploy_id = git_head()?;
    let dir = devstack::repo_root().join("target/deploy").join(&n.cell);
    fs::create_dir_all(&dir)?;
    let root = devstack::repo_root();

    let platform_url = format!("https://{}", n.platform_host);
    let mut agent = devstack::read_config(&root.join("agent"))?;
    agent["name"] = json!(n.agent);
    agent["main"] = json!(root.join("agent/build/index.js"));
    agent["workers_dev"] = json!(false);
    agent["preview_urls"] = json!(false);
    agent["observability"] = json!({ "enabled": true });
    agent["vars"] = json!({ "FRAGMENT_API": platform_url, "AGENT_URL": platform_url });
    let agent_config = dir.join("agent.json");
    fs::write(&agent_config, serde_json::to_string_pretty(&agent)?)?;

    let mut cell = devstack::read_config(&root.join("cell"))?;
    let c = cell.as_object_mut().context("cell/wrangler.jsonc is an object")?;
    c.remove("build");
    c.insert("name".into(), json!(n.cell));
    c.insert("main".into(), json!(root.join("cell/entry.mjs")));
    c.insert("workers_dev".into(), json!(false));
    c.insert("preview_urls".into(), json!(false));
    c.insert("observability".into(), json!({ "enabled": true }));
    c.insert("routes".into(), json!(n.routes.iter().map(|p| json!({ "pattern": p, "zone_name": d.zone })).collect::<Vec<_>>()));
    c.insert("workflows".into(), json!([{ "name": n.jobs, "binding": "JOBS", "class_name": "Job" }]));
    c.insert("r2_buckets".into(), json!([{ "binding": "BLOBS", "bucket_name": n.bucket }]));
    // every queue the checked-in config names, renamed for this deployment
    let renamed = |q: &Value| -> Result<Value> {
        match q.as_str() {
            Some("fragment-deliveries") => Ok(json!(n.deliveries)),
            Some("fragment-deliveries-dead") => Ok(json!(n.dead)),
            Some("fragment-ledger") => Ok(json!(n.ledger)),
            other => bail!("cell/wrangler.jsonc names a queue this deploy does not know: {other:?}"),
        }
    };
    let queues = &mut c["queues"];
    for list in ["producers", "consumers"] {
        for q in queues[list].as_array_mut().context("cell/wrangler.jsonc lists its queues")? {
            q["queue"] = renamed(&q["queue"])?;
            if !q["dead_letter_queue"].is_null() {
                q["dead_letter_queue"] = renamed(&q["dead_letter_queue"])?;
            }
        }
    }
    c.insert("services".into(), json!([{ "binding": "AGENTS", "service": n.agent }]));
    // the deployment's own images in place of the e2e's stubs, or none
    match &d.computers {
        Some(computers) => {
            let container = &mut c["containers"][0];
            let mut images = serde_json::Map::new();
            for (name, image) in &computers.images {
                let mut i = json!(image);
                i["dockerfile"] = json!(root.join(&image.dockerfile));
                i["build_context"] = json!(root.join(&image.build_context));
                images.insert(name.clone(), i);
            }
            container["images"] = Value::Object(images);
        }
        None => {
            c.remove("containers");
        }
    }
    let mut vars = json!({
        "FRAGMENT_DEPLOY_ID": deploy_id,
        "FRAGMENT_HOST_SUFFIX": n.suffix,
        "FRAGMENT_PLATFORM_URL": platform_url,
        "CODESTORAGE_ORG": d.codestorage.org,
        "WORKOS_CLIENT_ID": workos_client,
    });
    let v = vars.as_object_mut().expect("an object");
    if let Some(s) = &n.label_suffix {
        v.insert("FRAGMENT_HOST_LABEL_SUFFIX".into(), json!(s));
    }
    if let Some(p) = &n.repo_prefix {
        v.insert("CODESTORAGE_REPO_PREFIX".into(), json!(p));
    }
    if let Some(api) = &d.codestorage.api {
        v.insert("CODESTORAGE_API_URL".into(), json!(api));
    }
    if !d.operators.is_empty() {
        v.insert("FRAGMENT_OPERATORS".into(), json!(d.operators.join(",")));
    }
    if let Some(g) = &d.ai_gateway {
        v.insert("AI_GATEWAY_ID".into(), json!(g));
    }
    if let Some(p) = &d.default_plan {
        v.insert("FRAGMENT_DEFAULT_PLAN".into(), json!(p));
    }
    if let Some(computers) = &d.computers {
        v.insert("FRAGMENT_COMPUTER_IMAGE".into(), json!(computers.default_image));
    }
    if !d.connections.is_empty() {
        v.insert("FRAGMENT_CONNECTIONS".into(), Value::String(serde_json::to_string(&d.connections)?));
    }
    if !d.operator_keys.is_empty() {
        let hosts: BTreeMap<&String, &Vec<String>> = d.operator_keys.iter().map(|(name, k)| (name, &k.hosts)).collect();
        v.insert("FRAGMENT_OPERATOR_KEYS".into(), Value::String(serde_json::to_string(&hosts)?));
        let prices: Vec<Value> = d.operator_keys.iter().map(|(name, k)| json!({ "key": name, "micros": k.price.micros, "per": k.price.per })).collect();
        v.insert("FRAGMENT_KEY_PRICES".into(), Value::String(serde_json::to_string(&prices)?));
    }
    if let Some(version) = d.price_book_version {
        v.insert("FRAGMENT_PRICE_BOOK_VERSION".into(), json!(version.to_string()));
    }
    c.insert("vars".into(), vars);
    let cell_config = dir.join("cell.json");
    fs::write(&cell_config, serde_json::to_string_pretty(&cell)?)?;

    println!("deploying {} ({deploy_id}) to {platform_url}", n.cell);
    match &dns_token {
        Some(token) => crate::dns::ensure(token, &d.account_id, &n.dns)?,
        None => println!("dns: no dns_token_file, so these must be proxied by hand: {}", n.dns.iter().map(|w| w.name.as_str()).collect::<Vec<_>>().join(", ")),
    }
    ensure(wrangler(&tools, &d.account_id).args(["r2", "bucket", "create", &n.bucket]), "the bucket")?;
    for q in [&n.dead, &n.deliveries, &n.ledger] {
        ensure(wrangler(&tools, &d.account_id).args(["queues", "create", q]), "a queue")?;
    }
    let agent_secrets = SecretFile::write(dir.join("agent-secrets.json"), &json!({ "FRAGMENT_HOST_SECRET": host_secret }))?;
    crate::run(wrangler(&tools, &d.account_id).arg("deploy").arg("-c").arg(&agent_config).arg("--secrets-file").arg(&agent_secrets.0))?;
    drop(agent_secrets);
    let mut secrets = json!({
        "FRAGMENT_HOST_SECRET": host_secret,
        "CODESTORAGE_PRIVATE_KEY": org_key,
        "WORKOS_API_KEY": workos_key,
    });
    for (name, key) in operator_keys {
        secrets[name] = json!(key);
    }
    let cell_secrets = SecretFile::write(dir.join("cell-secrets.json"), &secrets)?;
    crate::run(wrangler(&tools, &d.account_id).arg("deploy").arg("-c").arg(&cell_config).arg("--secrets-file").arg(&cell_secrets.0))?;
    drop(cell_secrets);
    println!("deployed {deploy_id}:");
    println!("  the platform  {platform_url}/  (sign-in redirect: {platform_url}/auth/callback)");
    println!("  fragments     https://<label>--<username>{}.{}/", n.label_suffix.as_deref().unwrap_or(""), n.suffix);
    println!("  check         curl -sI {platform_url}/healthz | grep x-fragment-deploy");
    Ok(())
}

/// Removes a branch deployment: its Workers (their Durable Objects and
/// data with them), its Workflow and its queues. Its bucket is emptied by
/// the deployment's own blob grace and left: R2 refuses to delete a bucket
/// with objects in it. Irreversible: ask before running it.
pub fn teardown(rest: &[String]) -> Result<()> {
    let (config, branch) = args(rest)?;
    let d = load(&config)?;
    anyhow::ensure!(branch.is_some(), "teardown removes a branch deployment: name its --branch");
    let n = names(&d, branch.as_deref())?;
    let tools = devstack::Tools::locate()?;
    for worker in [&n.cell, &n.agent] {
        ensure(wrangler(&tools, &d.account_id).args(["delete", "--name", worker, "--force"]), "a Worker")?;
    }
    ensure(wrangler(&tools, &d.account_id).args(["workflows", "delete", &n.jobs]), "the Workflow")?;
    for q in [&n.deliveries, &n.dead, &n.ledger] {
        ensure(wrangler(&tools, &d.account_id).args(["queues", "delete", q, "--force"]), "a queue")?;
    }
    println!("removed {}; its bucket {} stays (empty it, then `wrangler r2 bucket delete {}`)", n.cell, n.bucket, n.bucket);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn deployment(platform: Option<&str>, suffix: Option<&str>) -> Deployment {
        Deployment {
            account_id: "acct".into(),
            zone: "finite.place".into(),
            platform_host: platform.map(str::to_string),
            fragment_suffix: suffix.map(str::to_string),
            dns_token_file: None,
            host_secret_file: "h".into(),
            codestorage: CodeStorage { org: "o".into(), private_key_file: "k".into(), api: None },
            workos: WorkOs { client_id_file: "c".into(), api_key_file: "a".into() },
            operators: vec![],
            ai_gateway: None,
            default_plan: None,
            computers: None,
            connections: BTreeMap::new(),
            operator_keys: BTreeMap::new(),
            price_book_version: None,
        }
    }

    /// A branch is a copy of its own beside the others: every name is
    /// marked, and its hosts are single labels under the zone.
    #[test]
    fn a_branch_names_everything_for_itself() {
        let n = names(&deployment(None, None), Some("dev")).unwrap();
        assert_eq!((n.cell.as_str(), n.agent.as_str(), n.bucket.as_str()), ("fragment-dev", "fragment-agent-dev", "fragment-blobs-dev"));
        assert_eq!((n.deliveries.as_str(), n.dead.as_str(), n.ledger.as_str()), ("fragment-deliveries-dev", "fragment-deliveries-dead-dev", "fragment-ledger-dev"));

        assert_eq!(n.platform_host, "dev.finite.place");
        assert_eq!(n.routes, ["dev.finite.place/*", "*--dev.finite.place/*"]);
        // one wildcard in the zone covers the platform and every fragment
        assert_eq!(n.dns, [crate::dns::Wanted { zone: "finite.place".into(), name: "*.finite.place".into() }]);
        assert_eq!((n.label_suffix.as_deref(), n.repo_prefix.as_deref()), (Some("--dev"), Some("dev--")));
        for bad in ["", "Dev", "a--b", "-a", "a-", "a.b", "seventeen-letters"] {
            assert!(names(&deployment(None, None), Some(bad)).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn a_deployment_of_its_own_takes_no_branch() {
        let d = deployment(Some("fragment.club"), Some("fragment.boats"));
        let n = names(&d, None).unwrap();
        assert_eq!(n.routes, ["fragment.club/*", "*.fragment.boats/*"]);
        assert_eq!(
            n.dns,
            [
                crate::dns::Wanted { zone: "fragment.club".into(), name: "fragment.club".into() },
                crate::dns::Wanted { zone: "fragment.boats".into(), name: "*.fragment.boats".into() },
            ]
        );
        assert_eq!((n.label_suffix, n.repo_prefix), (None, None));
        assert!(names(&d, Some("b")).is_err());
        assert!(names(&deployment(None, None), None).is_err());
        assert!(names(&deployment(Some("fragment.club"), None), None).is_err());
    }

    #[test]
    fn the_example_config_parses() {
        let text = fs::read_to_string(devstack::repo_root().join("deploy/example.jsonc")).unwrap();
        let d: Deployment = serde_json::from_str(&devstack::strip_comments(&text)).unwrap();
        assert!(names(&d, Some("dev")).is_ok());
        let d = checked(d).unwrap();
        assert_eq!(d.computers.as_ref().map(|c| c.default_image.as_str()), Some("hermes"));
    }

    /// OpenRouter went (a hard cut): a config that still names its key is
    /// refused, not deployed without it.
    #[test]
    fn a_config_naming_openrouter_is_refused() {
        let text = fs::read_to_string(devstack::repo_root().join("deploy/example.jsonc")).unwrap();
        let mut v: Value = serde_json::from_str(&devstack::strip_comments(&text)).unwrap();
        v["openrouter_api_key_file"] = json!("~/.config/fragment/secrets/openrouter-api-key");
        let refused = serde_json::from_value::<Deployment>(v).err().map(|e| e.to_string()).unwrap_or_default();
        assert!(refused.contains("unknown field `openrouter_api_key_file`"), "{refused}");
    }

    /// A swap's names and hosts, and a default image, are checked before
    /// anything deploys.
    #[test]
    fn a_bad_swap_or_image_is_refused_before_a_deploy() {
        let mut d = deployment(None, None);
        d.connections.insert("GitHub".into(), vec!["api.github.com".into()]);
        assert!(checked(d).is_err());
        let mut d = deployment(None, None);
        d.operator_keys.insert("search".into(), OperatorKey { hosts: vec!["localhost".into()], key_file: "k".into(), price: KeyPrice { micros: 1, per: 1 } });
        assert!(checked(d).is_err());
        let mut d = deployment(None, None);
        d.computers = Some(Computers { default_image: "hermes".into(), images: BTreeMap::new() });
        assert!(checked(d).is_err());
        assert!(checked(deployment(None, None)).is_ok());
    }
}
