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
    /// The provider catalog a computer's swap offers (decisions 22 and 37;
    /// `fragment_core::catalog`): each row a connection, an operator key or
    /// an own key, with its hosts, placements, environment variables and
    /// (an operator key's) price, and an operator key's `key_file`.
    #[serde(default)]
    providers: Vec<Value>,
    /// The price book's version: raise it with every change to a key's
    /// price, or ledgers made before keep the book they have.
    price_book_version: Option<u32>,
    /// A branch deployment's test levers (`FRAGMENT_TEST_SECRET`, docs/secrets.md):
    /// the file of a secret of 32 bytes or more, with which the hosted e2e
    /// signs its people in and pulls its levers there. Only a `--branch`
    /// deploy takes it: a deployment of its own (production) is refused.
    test_secret_file: Option<PathBuf>,
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

/// The deployment's catalog, and the file each operator key's value is
/// in: a row's `key_file` is the deploy's (its secret), the rest the
/// cell's (`FRAGMENT_PROVIDERS`). An operator key names one; no other row
/// may.
fn catalog_of(d: &Deployment) -> Result<(fragment_core::catalog::Catalog, Vec<(String, PathBuf)>)> {
    let mut rows = vec![];
    let mut key_files = vec![];
    for row in &d.providers {
        let mut row = row.clone();
        let name = row["name"].as_str().unwrap_or("?").to_string();
        let key_file = row.as_object_mut().and_then(|o| o.remove("key_file"));
        match (row["kind"].as_str(), key_file) {
            (Some("operator"), Some(Value::String(f))) => key_files.push((name.clone(), PathBuf::from(f))),
            (Some("operator"), _) => bail!("providers: the operator key {name} names its key_file"),
            (_, Some(_)) => bail!("providers: {name} is no operator key, so it names no key_file"),
            (_, None) => {}
        }
        rows.push(serde_json::from_value(row).with_context(|| format!("providers: {name}"))?);
    }
    let catalog = fragment_core::catalog::Catalog::of(rows).map_err(|e| anyhow::anyhow!("providers: {e}"))?;
    Ok((catalog, key_files))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CodeStorage {
    org: String,
    private_key_file: PathBuf,
    /// The API base (default `https://api.<org>.code.storage`).
    api: Option<String>,
}

/// The WorkOS environment: Pipes' connections (its client id and API
/// key), and sign-in through its AuthKit, an OAuth application's OpenID
/// Connect provider at the AuthKit domain (docs/self-host.md, seam 4).
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkOs {
    client_id_file: PathBuf,
    api_key_file: PathBuf,
    /// The AuthKit domain (`<name>.authkit.app`, or the custom domain):
    /// the issuer is `https://<it>`.
    authkit_domain: String,
    /// The OAuth application's client id and secret (WorkOS Connect,
    /// first-party, its redirect URI `<platform>/auth/callback`).
    oauth_client_id_file: PathBuf,
    oauth_client_secret_file: PathBuf,
}

/// AuthKit's issuer for a domain: a bare host name, nothing else.
fn authkit_issuer(domain: &str) -> Result<String> {
    let host = domain.trim();
    anyhow::ensure!(
        !host.is_empty() && host.len() <= 253 && host.contains('.') && host.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-'),
        "workos.authkit_domain is the AuthKit domain's host name alone (e.g. example.authkit.app), not {domain:?}"
    );
    Ok(format!("https://{host}"))
}

use devstack::valid_branch;

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

/// The test secret's file a deploy of `branch` takes, if the config names
/// one: a branch deployment's alone. A deployment of its own is refused
/// outright (its levers would be production's), before anything is read.
fn test_secret_file<'a>(d: &'a Deployment, branch: Option<&str>) -> Result<Option<&'a Path>> {
    match (&d.test_secret_file, branch) {
        (None, _) => Ok(None),
        (Some(_), None) => bail!("test_secret_file is a branch deployment's (a preview's): a deployment of its own never has test levers. Remove it, or deploy a --branch"),
        (Some(file), Some(_)) => Ok(Some(file.as_path())),
    }
}

/// The test secret, from its file, checked as the cell checks it (never printed).
fn read_test_secret(file: &Path) -> Result<String> {
    let secret = read_secret(file)?;
    fragment_core::levers::check(&secret).map_err(|why| anyhow::anyhow!("test_secret_file {}: {}", file.display(), why.message()))?;
    Ok(secret)
}

fn load(config: &Path) -> Result<Deployment> {
    let text = fs::read_to_string(config).with_context(|| format!("read {}", config.display()))?;
    let d: Deployment = serde_json::from_str(&devstack::strip_comments(&text)).with_context(|| format!("parse {}", config.display()))?;
    checked(d).with_context(|| format!("check {}", config.display()))
}

/// What the cell would refuse at its first request, refused before a
/// deploy: the provider catalog, and a default image it has.
fn checked(d: Deployment) -> Result<Deployment> {
    catalog_of(&d)?;
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

/// wrangler on the pinned Node (`devstack::Tools::wrangler`), for `account`.
fn wrangler(tools: &devstack::Tools, account: &str) -> Result<Command> {
    let mut c = tools.wrangler()?;
    c.env("CLOUDFLARE_ACCOUNT_ID", account).current_dir(devstack::repo_root());
    Ok(c)
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
    let signin = devstack::OidcVars::authkit(
        &authkit_issuer(&d.workos.authkit_domain)?,
        &read_secret(&d.workos.oauth_client_id_file)?,
        &read_secret(&d.workos.oauth_client_secret_file)?,
        &workos_client,
    );
    let dns_token = d.dns_token_file.as_deref().map(read_secret).transpose()?;
    let test_secret = test_secret_file(&d, branch.as_deref())?.map(read_test_secret).transpose()?;
    if let Some(p) = &d.default_plan {
        anyhow::ensure!(matches!(p.as_str(), "guest" | "seat" | "seat_always_on"), "default_plan is guest, seat or seat_always_on, not {p:?}");
    }
    anyhow::ensure!(d.ai_gateway.as_deref() != Some("default"), "ai_gateway names the deployment's own gateway: `default` makes one that logs");
    let (catalog, key_files) = catalog_of(&d)?;
    let operator_keys: Vec<(String, String)> = key_files.iter().map(|(name, file)| Ok((fragment_core::catalog::key_secret_name(name), read_secret(file)?))).collect::<Result<_>>()?;
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
    if !catalog.is_empty() {
        v.insert("FRAGMENT_PROVIDERS".into(), Value::String(serde_json::to_string(catalog.providers())?));
    }
    if let Some(version) = d.price_book_version {
        v.insert("FRAGMENT_PRICE_BOOK_VERSION".into(), json!(version.to_string()));
    }
    for (k, value) in signin.vars() {
        v.insert(k.into(), json!(value));
    }
    c.insert("vars".into(), vars);
    let cell_config = dir.join("cell.json");
    fs::write(&cell_config, serde_json::to_string_pretty(&cell)?)?;

    println!("deploying {} ({deploy_id}) to {platform_url}", n.cell);
    match &dns_token {
        Some(token) => crate::dns::ensure(token, &d.account_id, &n.dns)?,
        None => println!("dns: no dns_token_file, so these must be proxied by hand: {}", n.dns.iter().map(|w| w.name.as_str()).collect::<Vec<_>>().join(", ")),
    }
    ensure(wrangler(&tools, &d.account_id)?.args(["r2", "bucket", "create", &n.bucket]), "the bucket")?;
    for q in [&n.dead, &n.deliveries, &n.ledger] {
        ensure(wrangler(&tools, &d.account_id)?.args(["queues", "create", q]), "a queue")?;
    }
    let agent_secrets = SecretFile::write(dir.join("agent-secrets.json"), &json!({ "FRAGMENT_HOST_SECRET": host_secret }))?;
    crate::run(wrangler(&tools, &d.account_id)?.arg("deploy").arg("-c").arg(&agent_config).arg("--secrets-file").arg(&agent_secrets.0))?;
    drop(agent_secrets);
    let mut secrets = json!({
        "FRAGMENT_HOST_SECRET": host_secret,
        "CODESTORAGE_PRIVATE_KEY": org_key,
        "WORKOS_API_KEY": workos_key,
        "FRAGMENT_OIDC_CLIENT_SECRET": signin.client_secret.as_deref().expect("AuthKit's application has a secret"),
    });
    for (name, key) in operator_keys {
        secrets[name] = json!(key);
    }
    // a preview's levers (cell/src/levers.rs): a branch's alone, checked above
    if let Some(secret) = &test_secret {
        assert!(n.label_suffix.is_some(), "only a branch deployment takes a test secret");
        secrets["FRAGMENT_TEST_SECRET"] = json!(secret);
    }
    let cell_secrets = SecretFile::write(dir.join("cell-secrets.json"), &secrets)?;
    crate::run(wrangler(&tools, &d.account_id)?.arg("deploy").arg("-c").arg(&cell_config).arg("--secrets-file").arg(&cell_secrets.0))?;
    drop(cell_secrets);
    println!("deployed {deploy_id}:");
    println!("  the platform  {platform_url}/  (sign-in redirect: {platform_url}/auth/callback)");
    if test_secret.is_some() {
        println!("  test levers   on (test_secret_file): cargo xtask e2e --hosted --config <this config> --branch {}", branch.as_deref().unwrap_or(""));
    }
    println!("  fragments     https://<label>--<username>{}.{}/", n.label_suffix.as_deref().unwrap_or(""), n.suffix);
    println!("  check         curl -sI {platform_url}/healthz | grep x-fragment-deploy");
    Ok(())
}

/// `cargo xtask e2e --hosted --config <file> --branch <b> [--only … |
/// --except …] [--dry-run | --sweep] [--max-paid-calls <n>]`: the suite's
/// own arguments for that branch deployment, from its config: the zone, the
/// test secret's file (named, never read here), and what the deployment
/// offers (computers, models). A deployment of its own is refused: the
/// hosted lane runs on a preview.
pub fn hosted_e2e_args(rest: &[String]) -> Result<Vec<String>> {
    let usage = || anyhow::anyhow!("usage: cargo xtask e2e --hosted --config <file> --branch <name> [--only <section>[,...] | --except <section>[,...]] [--dry-run | --sweep] [--max-paid-calls <n>]");
    let (mut config, mut branch, mut passed) = (None, None, vec![]);
    let mut it = rest.iter();
    // bounded: each pass takes one argument at least
    while let Some(a) = it.next() {
        match a.as_str() {
            "--hosted" => {}
            "--config" => config = Some(PathBuf::from(it.next().ok_or_else(usage)?)),
            "--branch" => branch = Some(it.next().ok_or_else(usage)?.clone()),
            "--only" | "--except" | "--max-paid-calls" => {
                passed.push(a.clone());
                passed.push(it.next().ok_or_else(usage)?.clone());
            }
            "--dry-run" | "--sweep" => passed.push(a.clone()),
            _ => return Err(usage()),
        }
    }
    let (config, branch) = (config.ok_or_else(usage)?, branch.ok_or_else(usage)?);
    let d = load(&config)?;
    // a deployment of its own takes no --branch: production never runs it
    names(&d, Some(&branch)).context("the hosted lane runs on a branch deployment (a preview)")?;
    let mut args = vec!["--hosted".to_string(), "--zone".into(), d.zone.clone(), "--branch".into(), branch.clone()];
    if let Some(file) = test_secret_file(&d, Some(&branch))? {
        args.extend(["--secret-file".to_string(), expand(file).to_string_lossy().into_owned()]);
    }
    let offers: Vec<&str> = [(d.computers.is_some(), "computers"), (d.ai_gateway.is_some(), "models")].into_iter().filter(|(on, _)| *on).map(|(_, o)| o).collect();
    if !offers.is_empty() {
        args.extend(["--offers".to_string(), offers.join(",")]);
    }
    args.extend(passed);
    Ok(args)
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
        ensure(wrangler(&tools, &d.account_id)?.args(["delete", "--name", worker, "--force"]), "a Worker")?;
    }
    ensure(wrangler(&tools, &d.account_id)?.args(["workflows", "delete", &n.jobs]), "the Workflow")?;
    for q in [&n.deliveries, &n.dead, &n.ledger] {
        ensure(wrangler(&tools, &d.account_id)?.args(["queues", "delete", q, "--force"]), "a queue")?;
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
            workos: WorkOs {
                client_id_file: "c".into(),
                api_key_file: "a".into(),
                authkit_domain: "example.authkit.app".into(),
                oauth_client_id_file: "o".into(),
                oauth_client_secret_file: "s".into(),
            },
            operators: vec![],
            ai_gateway: None,
            default_plan: None,
            computers: None,
            providers: vec![],
            price_book_version: None,
            test_secret_file: None,
        }
    }

    /// A config file of `body` in this test's own scratch, and its path.
    fn config_file(test: &str, body: &Value) -> PathBuf {
        let dir = devstack::repo_root().join("target/xtask-tests").join(test);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("deploy.jsonc");
        fs::write(&path, body.to_string()).unwrap();
        path
    }

    /// The test levers are a preview's: a deployment of its own (production)
    /// that names a test secret is refused before anything is read, a
    /// branch takes it, and a config without one has none.
    #[test]
    fn a_test_secret_is_a_branch_deployments_alone() {
        let mut production = deployment(Some("fragment.club"), Some("fragment.boats"));
        production.test_secret_file = Some("~/.config/fragment/secrets/test-secret".into());
        let refused = test_secret_file(&production, None).err().map(|e| e.to_string()).unwrap_or_default();
        assert!(refused.contains("never has test levers"), "{refused}");
        // nor may it be deployed as a branch to slip one in
        assert!(names(&production, Some("p5")).is_err());

        let mut branch = deployment(None, None);
        assert!(matches!(test_secret_file(&branch, Some("p5")), Ok(None)), "no field, no levers");
        branch.test_secret_file = Some("/run/secrets/test".into());
        assert_eq!(test_secret_file(&branch, Some("p5")).unwrap(), Some(Path::new("/run/secrets/test")));
        assert!(test_secret_file(&branch, None).is_err(), "a branch config deployed without --branch is a deployment of its own");
    }

    /// A test secret is checked as the cell checks it: too short is refused.
    #[test]
    fn a_short_test_secret_is_refused() {
        let dir = devstack::repo_root().join("target/xtask-tests/short-secret");
        fs::create_dir_all(&dir).unwrap();
        let (short, long) = (dir.join("short"), dir.join("long"));
        fs::write(&short, "too-short\n").unwrap();
        fs::write(&long, format!("{}\n", "a1".repeat(32))).unwrap();
        let refused = read_test_secret(&short).err().map(|e| e.to_string()).unwrap_or_default();
        assert!(refused.contains("at least 32 bytes") && !refused.contains("too-short"), "{refused}");
        assert_eq!(read_test_secret(&long).unwrap().len(), 64);
    }

    /// The hosted e2e reads its preview from the deployment's config: its
    /// zone, the secret's file (named, not read), and what it offers. A
    /// deployment of its own is refused.
    #[test]
    fn the_hosted_e2e_reads_its_preview_from_the_config() {
        let text = fs::read_to_string(devstack::repo_root().join("deploy/example.jsonc")).unwrap();
        let mut v: Value = serde_json::from_str(&devstack::strip_comments(&text)).unwrap();
        v["zone"] = json!("finite.place");
        v["ai_gateway"] = json!("fragment-dev");
        v["test_secret_file"] = json!("/run/secrets/p5-test");
        let args = |rest: &[&str]| hosted_e2e_args(&rest.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        let path = config_file("hosted-args", &v);
        let got = args(&["--hosted", "--config", path.to_str().unwrap(), "--branch", "p5", "--dry-run", "--only", "computers,templates"]).unwrap();
        assert_eq!(
            got,
            ["--hosted", "--zone", "finite.place", "--branch", "p5", "--secret-file", "/run/secrets/p5-test", "--offers", "computers,models", "--dry-run", "--only", "computers,templates"]
        );
        // no gateway, no computers: it offers neither
        v.as_object_mut().unwrap().remove("ai_gateway");
        v.as_object_mut().unwrap().remove("computers");
        let path = config_file("hosted-args-bare", &v);
        let got = args(&["--hosted", "--config", path.to_str().unwrap(), "--branch", "p5"]).unwrap();
        assert_eq!(got, ["--hosted", "--zone", "finite.place", "--branch", "p5", "--secret-file", "/run/secrets/p5-test"]);
        // production: a deployment of its own runs no hosted lane
        v["platform_host"] = json!("fragment.club");
        v["fragment_suffix"] = json!("fragment.boats");
        let path = config_file("hosted-args-production", &v);
        assert!(args(&["--hosted", "--config", path.to_str().unwrap(), "--branch", "p5"]).is_err());
        assert!(args(&["--hosted", "--config", path.to_str().unwrap()]).is_err(), "a branch is named");
        assert!(args(&["--hosted", "--branch", "p5"]).is_err(), "a config is named");
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

    /// The example and the hosted e2e's configs parse and check, each with
    /// the platform's catalog: Google, and the four operator keys at their
    /// list prices, each key's file named.
    #[test]
    fn the_example_and_e2e_configs_parse() {
        for file in ["deploy/example.jsonc", "deploy/e2e.jsonc"] {
            let text = fs::read_to_string(devstack::repo_root().join(file)).unwrap();
            let d: Deployment = serde_json::from_str(&devstack::strip_comments(&text)).unwrap();
            assert!(names(&d, Some("dev")).is_ok(), "{file}");
            let d = checked(d).unwrap();
            assert_eq!(d.computers.as_ref().map(|c| c.default_image.as_str()), Some("hermes"), "{file}");
            let (catalog, files) = catalog_of(&d).unwrap();
            let names: Vec<&str> = catalog.providers().iter().map(|p| p.name.as_str()).collect();
            assert_eq!(names, ["google", "perplexity", "google-places", "xai", "elevenlabs"], "{file}");
            assert_eq!(files.len(), 4, "{file}: each operator key's file");
            assert!(catalog.key_prices().iter().all(|k| fragment_core::price::default_key_price(&k.key) == Some((k.micros, k.per))), "{file}: at list");
        }
    }

    /// Sign-in is AuthKit's OpenID Connect provider at the AuthKit domain,
    /// its people keyed as the environment's, as before; a domain that is
    /// more than a host name is refused.
    #[test]
    fn workos_signs_in_through_authkit_keyed_as_before() {
        assert_eq!(authkit_issuer("example.authkit.app").unwrap(), "https://example.authkit.app");
        assert_eq!(authkit_issuer(" auth.example.com ").unwrap(), "https://auth.example.com");
        for bad in ["", "https://example.authkit.app", "example.authkit.app/", "authkit", "exa mple.authkit.app", "example.authkit.app?x"] {
            assert!(authkit_issuer(bad).is_err(), "{bad:?}");
        }
        let signin = devstack::OidcVars::authkit("https://example.authkit.app", "client_app", "sk_app", "client_env");
        let vars: BTreeMap<&str, &str> = signin.vars().into_iter().collect();
        assert_eq!(vars["FRAGMENT_OIDC_ISSUER"], "https://example.authkit.app");
        assert_eq!(vars["FRAGMENT_OIDC_CLIENT_ID"], "client_app");
        assert_eq!(vars["FRAGMENT_OIDC_AUTH"], "client_secret_post", "the client in the body, as WorkOS's reference has it");
        assert_eq!(vars["FRAGMENT_OIDC_KEYED_AS"], "workos:client_env", "the people WorkOS signed in before keep their key");
        assert!(!vars.contains_key("FRAGMENT_OIDC_CLIENT_SECRET") && !vars.values().any(|v| v.contains("sk_app")), "the secret is a secret, never a variable");
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

    /// The provider catalog, its key files, and a default image are
    /// checked before anything deploys.
    #[test]
    fn a_bad_catalog_or_image_is_refused_before_a_deploy() {
        let row = |extra: Value| {
            let mut r = json!({ "name": "perplexity", "kind": "operator", "hosts": ["api.perplexity.ai"], "placements": [{ "header": "authorization", "format": "Bearer {}" }], "env": ["PERPLEXITY_API_KEY"] });
            r.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
            r
        };
        let with = |rows: Vec<Value>| {
            let mut d = deployment(None, None);
            d.providers = rows;
            d
        };
        let d = with(vec![row(json!({ "key_file": "~/.config/fragment/secrets/perplexity-api-key" }))]);
        let (catalog, files) = catalog_of(&d).unwrap();
        assert_eq!(catalog.get("perplexity").unwrap().price.map(|p| (p.micros, p.per)), Some((5_000_000, 1_000)), "the price book's list price");
        assert_eq!(files, vec![("perplexity".to_string(), PathBuf::from("~/.config/fragment/secrets/perplexity-api-key"))]);
        assert!(!serde_json::to_string(catalog.providers()).unwrap().contains("key_file"), "the key's file is the deploy's, never the cell's");
        assert!(checked(d).is_ok());
        assert!(checked(with(vec![row(json!({}))])).is_err(), "an operator key names its file");
        assert!(checked(with(vec![row(json!({ "key_file": "k", "name": "Perplexity" }))])).is_err());
        assert!(checked(with(vec![row(json!({ "key_file": "k", "hosts": ["localhost"] }))])).is_err());
        let google = json!({ "name": "google", "kind": "connection", "hosts": ["www.googleapis.com"], "placements": [{ "header": "authorization", "format": "Bearer {}" }], "env": ["GOOGLE_OAUTH_ACCESS_TOKEN"], "key_file": "k" });
        assert!(checked(with(vec![google])).is_err(), "a connection has no key file");
        let mut d = deployment(None, None);
        d.computers = Some(Computers { default_image: "hermes".into(), images: BTreeMap::new() });
        assert!(checked(d).is_err());
        assert!(checked(deployment(None, None)).is_ok());
    }
}
