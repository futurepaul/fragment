//! `cargo xtask deploy --config <file> [--branch <name>]` and `cargo xtask
//! teardown --config <file> --branch <name>` (docs/cloudflare-v1.md,
//! decisions 4 and 20).
//!
//! A deployment's configuration is a file outside the repo (JSONC, keys as
//! in `Deployment`), so anyone deploys their own without a fork. Its
//! secrets live in the account's Cloudflare Secrets Store (docs/secrets.md;
//! `cargo xtask secret` sets them), and the config names each by its store
//! name: the deploy reads no secret's value. It first lists the store
//! (read-only) and refuses, before anything is built or made, when a
//! secret the config names is not there. From the config and the
//! checked-in config (`cell/wrangler.jsonc`) it renders the Worker's own
//! wrangler config under `target/deploy/<name>/`, the store secrets bound
//! by name (`secrets_store_secrets`), makes the bucket and queues it
//! names, and runs `wrangler deploy`. Only a branch's
//! test secret is still a Worker secret, uploaded from a file of mode 600
//! that is removed after.
//!
//! A branch deployment (`--branch b`) is a complete copy beside the others
//! in one account and zone: its Worker, Durable Objects, Workflow, queues
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
    /// missing (xtask/src/dns.rs). Without it, they are made by hand. The
    /// deploying machine's own credential, which xtask itself uses: no
    /// Worker holds it, so it is no store secret.
    dns_token_file: Option<PathBuf>,
    /// The host secret's name in the account's Secrets Store: 32 random
    /// bytes or more (`cargo xtask secret gen`), which seal every value at
    /// rest. Rotated by name, never in place (docs/secrets.md).
    host_secret: String,
    /// The host secret before a rotation, by name: bound while values it
    /// sealed are resealed under `host_secret`.
    host_secret_previous: Option<String>,
    codestorage: CodeStorage,
    workos: WorkOs,
    /// Who may grant credit, set plans, release usernames, and wipe a
    /// person (npubs, or identities). The hosted e2e's `wipe` signs with an
    /// operator key among them, its file named on its command line
    /// (`--operator-key-file`), never here: the key is the runner's.
    #[serde(default)]
    operators: Vec<String>,
    /// The AI Gateway the model route calls through (its id, named: never
    /// `default`, which makes one that logs). Without it, models are off.
    ai_gateway: Option<String>,
    /// The model the route's `vision` runs (`FRAGMENT_VISION_MODEL`): a
    /// runtime's calls about an image, Hermes' screenshots among them.
    /// GLM-5.3 Flash unless named; one the price book does not price is
    /// refused (`fragment_core::models::vision_model`).
    vision_model: Option<String>,
    /// A new person's plan: `guest` (the default), `seat`, or `seat_always_on`.
    default_plan: Option<String>,
    /// Where a person whose computer will not start gets help
    /// (`FRAGMENT_SUPPORT_URL`): an `https:` page or a `mailto:` address
    /// (`fragment_core::computer::support_url_ok`), linked from the shell.
    support_url: Option<String>,
    /// Computers (docs/computers.md): the images they run, and the one a
    /// new computer is pinned to. Without it, the deployment makes none.
    computers: Option<Computers>,
    /// The provider catalog a computer's swap offers (decisions 22 and 37;
    /// `fragment_core::catalog`): each row a connection, an operator key or
    /// an own key, with its hosts, placements, environment variables and
    /// (an operator key's) price, and an operator key's `key`: its name in
    /// the store.
    #[serde(default)]
    providers: Vec<Value>,
    /// The price book's version: raise it with every change to a key's
    /// price, or ledgers made before keep the book they have.
    price_book_version: Option<u32>,
    /// A branch deployment's test levers (`FRAGMENT_TEST_SECRET`, docs/secrets.md):
    /// the file of a secret of 32 bytes or more, with which the hosted e2e
    /// signs its people in and pulls its levers there. Only a `--branch`
    /// deploy takes it: a deployment of its own (production) is refused.
    /// A file, not a store secret: the hosted e2e runner sends its value,
    /// and a store gives no value back.
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
    /// How long a computer whose sleep's save keeps failing stays awake
    /// before it sleeps unsaved (`FRAGMENT_COMPUTER_UNSAVED_MAX_MS`; thirty
    /// minutes when unset: docs/computers.md).
    #[serde(default)]
    unsaved_max_ms: Option<u64>,
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

/// The deployment's catalog, and the store secret each operator key's
/// value is in: a row's `key` is the deploy's (it binds it), the rest the
/// cell's (`FRAGMENT_PROVIDERS`). An operator key names one; no other row
/// may.
fn catalog_of(d: &Deployment) -> Result<(fragment_core::catalog::Catalog, Vec<(String, String)>)> {
    let mut rows = vec![];
    let mut keys = vec![];
    for row in &d.providers {
        let mut row = row.clone();
        let name = row["name"].as_str().unwrap_or("?").to_string();
        let key = row.as_object_mut().and_then(|o| o.remove("key"));
        match (row["kind"].as_str(), key) {
            (Some("operator"), Some(Value::String(k))) => keys.push((name.clone(), k)),
            (Some("operator"), _) => bail!("providers: the operator key {name} names its key (its secret's name in the store)"),
            (_, Some(_)) => bail!("providers: {name} is no operator key, so it names no key"),
            (_, None) => {}
        }
        rows.push(serde_json::from_value(row).with_context(|| format!("providers: {name}"))?);
    }
    let catalog = fragment_core::catalog::Catalog::of(rows).map_err(|e| anyhow::anyhow!("providers: {e}"))?;
    Ok((catalog, keys))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CodeStorage {
    org: String,
    /// The org's signing key (PKCS#8 P-256, in PEM): its name in the store.
    private_key: String,
    /// The API base (default `https://api.<org>.code.storage`).
    api: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkOs {
    /// The environment's client id and API key: their names in the store.
    client_id: String,
    api_key: String,
}

/// The store secrets the deployment's Workers are bound to, by name.
fn bound(d: &Deployment) -> Result<devstack::store::Bound> {
    let (_, operator_keys) = catalog_of(d)?;
    Ok(devstack::store::Bound {
        host_secret: d.host_secret.clone(),
        host_secret_previous: d.host_secret_previous.clone(),
        codestorage_key: d.codestorage.private_key.clone(),
        workos: Some((d.workos.client_id.clone(), d.workos.api_key.clone())),
        operator_keys,
    })
}

/// Each store secret the config names, with the field that names it.
fn named_secrets(b: &devstack::store::Bound) -> Vec<(String, &str)> {
    let mut named = vec![("host_secret".to_string(), b.host_secret.as_str())];
    if let Some(previous) = &b.host_secret_previous {
        named.push(("host_secret_previous".into(), previous.as_str()));
    }
    named.push(("codestorage.private_key".into(), b.codestorage_key.as_str()));
    if let Some((client, key)) = &b.workos {
        named.push(("workos.client_id".into(), client.as_str()));
        named.push(("workos.api_key".into(), key.as_str()));
    }
    for (provider, name) in &b.operator_keys {
        named.push((format!("providers: {provider}'s key"), name.as_str()));
    }
    named
}

/// Every name the config gives a store secret is one (the store's
/// characters, `devstack::store::valid_name`), and the host secret and the
/// one before it are two.
fn check_names(b: &devstack::store::Bound) -> Result<()> {
    for (field, name) in named_secrets(b) {
        if !devstack::store::valid_name(name) {
            let path = if name.contains('/') || name.starts_with('~') {
                " It looks like a file's path: the secrets are no files now. Move the file into the store (cargo xtask secret set <name> --config <file> --from-file <path>) and name it here"
            } else {
                ""
            };
            bail!("{field} is {name:?}, which is no Secrets Store secret's name (1-{} of A-Z, a-z, 0-9, _ and -).{path}", devstack::store::NAME_MAX_BYTES);
        }
    }
    if b.host_secret_previous.as_deref() == Some(b.host_secret.as_str()) {
        bail!("host_secret_previous names the host secret itself: a rotation names the new one as host_secret and the one before as host_secret_previous");
    }
    Ok(())
}

/// What a deploy says when the store lacks secrets the config names (or
/// there is no store): each one, the field naming it, and the command that
/// sets it.
fn refusal(config: &Path, store: Option<&devstack::store::AccountStore>, missing: &[(String, &str)]) -> String {
    assert!(!missing.is_empty(), "a refusal names what is missing");
    let head = match store {
        Some(s) => format!("the account's Secrets Store {} ({}) lacks {} of the secrets this config names", s.name, s.id, missing.len()),
        None => "the account has no Secrets Store yet (the first `cargo xtask secret set` makes it), so it lacks every secret this config names".to_string(),
    };
    format!(
        "{head}; nothing was built or deployed. Set each, then deploy again:\n{}\n(set asks for the value in a terminal, reads it from standard input, or takes --from-file <path>; `cargo xtask secret gen <name>` makes a host secret; docs/secrets.md)",
        set_commands(config, missing).join("\n")
    )
}

/// A line for each missing secret: the field naming it, and the command
/// that sets it.
pub(crate) fn set_commands<S: AsRef<str>>(config: &Path, missing: &[(String, S)]) -> Vec<String> {
    let width = missing.iter().map(|(field, _)| field.len()).max().unwrap_or(0);
    missing.iter().map(|(field, name)| format!("  {field:width$}  cargo xtask secret set {} --config {}", name.as_ref(), config.display())).collect()
}

/// The account's store, holding every secret `bound` names: read-only,
/// before anything is built or made. Refused (`refusal`) otherwise.
fn preflight(tools: &devstack::Tools, d: &Deployment, config: &Path, bound: &devstack::store::Bound) -> Result<devstack::store::AccountStore> {
    let named = named_secrets(bound);
    let Some(store) = devstack::store::account_store(tools, &d.account_id)? else { bail!("{}", refusal(config, None, &named)) };
    let listed = devstack::store::list(tools, &devstack::store::Store::Account { account_id: &d.account_id, store_id: &store.id })?;
    let missing = devstack::store::missing(&named, &listed);
    if !missing.is_empty() {
        bail!("{}", refusal(config, Some(&store), &missing));
    }
    Ok(store)
}

use devstack::valid_branch;

pub(crate) fn expand(path: &Path) -> PathBuf {
    match path.strip_prefix("~") {
        Ok(rest) => PathBuf::from(std::env::var_os("HOME").expect("HOME is set")).join(rest),
        Err(_) => path.to_path_buf(),
    }
}

/// A local file's secret (the DNS token's, a branch's test secret's; never
/// printed): the two the deploying machine holds itself.
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
    let v = read_json(config)?;
    let d: Deployment = serde_json::from_value(v).with_context(|| format!("parse {}", config.display()))?;
    checked(d).with_context(|| format!("check {}", config.display()))
}

/// A config file's JSON (its comments dropped), unchecked.
fn read_json(config: &Path) -> Result<Value> {
    let text = fs::read_to_string(config).with_context(|| format!("read {}", config.display()))?;
    serde_json::from_str(&devstack::strip_comments(&text)).with_context(|| format!("parse {}", config.display()))
}

/// What `cargo xtask secret` takes from a deployment's config: read
/// leniently, so a config that does not load as a deploy loads it still
/// names its account (and `secret list` says why it does not load).
pub(crate) struct SecretsOf {
    pub account_id: String,
    /// The host secret's names (`host_secret`, `host_secret_previous`):
    /// secrets never set again in place.
    pub host_secrets: Vec<String>,
    /// Each store secret the config names, with the field naming it; or
    /// why the config does not load as a deploy loads it.
    pub named: Result<Vec<(String, String)>, String>,
}

pub(crate) fn secrets_of(config: &Path) -> Result<SecretsOf> {
    let v = read_json(config)?;
    let account_id = v["account_id"].as_str().filter(|a| !a.is_empty()).with_context(|| format!("{} names no account_id", config.display()))?.to_string();
    let host_secrets = ["host_secret", "host_secret_previous"].iter().filter_map(|k| v[*k].as_str().map(str::to_string)).collect();
    let named = load(config).and_then(|d| bound(&d)).map(|b| named_secrets(&b).into_iter().map(|(f, n)| (f, n.to_string())).collect()).map_err(|e| format!("{e:#}"));
    Ok(SecretsOf { account_id, host_secrets, named })
}

/// What the cell would refuse at its first request, refused before a
/// deploy: the provider catalog, a default image it has, its store
/// secrets' names, and a vision model the price book prices.
fn checked(d: Deployment) -> Result<Deployment> {
    check_names(&bound(&d)?)?;
    if let Some(m) = &d.vision_model {
        fragment_core::models::vision_model(Some(m), &fragment_core::price::PriceBook::defaults()).map_err(|e| anyhow::anyhow!("vision_model: {e}"))?;
    }
    if let Some(c) = &d.computers {
        if !c.images.contains_key(&c.default_image) {
            bail!("computers: the default image {:?} is not one of its images", c.default_image);
        }
    }
    if let Some(u) = d.support_url.as_deref().filter(|u| !fragment_core::computer::support_url_ok(u)) {
        bail!("support_url: {u:?} is no https: page or mailto: address of at most {} visible characters", fragment_core::computer::SUPPORT_URL_MAX_BYTES);
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
    // the two local files are read before anything is built or made
    let dns_token = d.dns_token_file.as_deref().map(read_secret).transpose()?;
    let test_secret = test_secret_file(&d, branch.as_deref())?.map(read_test_secret).transpose()?;
    if let Some(p) = &d.default_plan {
        anyhow::ensure!(matches!(p.as_str(), "guest" | "seat" | "seat_always_on"), "default_plan is guest, seat or seat_always_on, not {p:?}");
    }
    anyhow::ensure!(d.ai_gateway.as_deref() != Some("default"), "ai_gateway names the deployment's own gateway: `default` makes one that logs");
    let bound = bound(&d)?;
    let tools = devstack::Tools::locate()?;
    // and every store secret the Worker is bound to is there (read-only)
    let store = preflight(&tools, &d, &config, &bound)?;
    println!("secrets: the {} the config names are in the account's Secrets Store {} ({})", named_secrets(&bound).len(), store.name, store.id);
    crate::build()?;
    let deploy_id = git_head()?;
    let dir = devstack::repo_root().join("target/deploy").join(&n.cell);
    fs::create_dir_all(&dir)?;
    let cell = worker_config(&d, &n, &store.id, &deploy_id, &devstack::repo_root())?;
    let cell_config = dir.join("cell.json");
    fs::write(&cell_config, serde_json::to_string_pretty(&cell)?)?;
    let platform_url = format!("https://{}", n.platform_host);

    println!("deploying {} ({deploy_id}) to {platform_url}", n.cell);
    match &dns_token {
        Some(token) => crate::dns::ensure(token, &d.account_id, &n.dns)?,
        None => println!("dns: no dns_token_file, so these must be proxied by hand: {}", n.dns.iter().map(|w| w.name.as_str()).collect::<Vec<_>>().join(", ")),
    }
    ensure(wrangler(&tools, &d.account_id)?.args(["r2", "bucket", "create", &n.bucket]), "the bucket")?;
    for q in [&n.dead, &n.deliveries, &n.ledger] {
        ensure(wrangler(&tools, &d.account_id)?.args(["queues", "create", q]), "a queue")?;
    }
    let mut deploy_cell = wrangler(&tools, &d.account_id)?;
    deploy_cell.arg("deploy").arg("-c").arg(&cell_config);
    // a preview's levers (cell/src/levers.rs), a branch's alone (checked
    // above), are the one Worker secret left
    let test_secrets = match &test_secret {
        Some(secret) => {
            assert!(n.label_suffix.is_some(), "only a branch deployment takes a test secret");
            let file = SecretFile::write(dir.join("cell-secrets.json"), &json!({ "FRAGMENT_TEST_SECRET": secret }))?;
            deploy_cell.arg("--secrets-file").arg(&file.0);
            Some(file)
        }
        None => None,
    };
    crate::run(&mut deploy_cell)?;
    drop(test_secrets);
    println!("deployed {deploy_id}:");
    println!("  the platform  {platform_url}/  (sign-in redirect: {platform_url}/auth/callback)");
    if test_secret.is_some() {
        println!("  test levers   on (test_secret_file): cargo xtask e2e --hosted --config <this config> --branch {}", branch.as_deref().unwrap_or(""));
    }
    println!("  fragments     https://<label>--<username>{}.{}/", n.label_suffix.as_deref().unwrap_or(""), n.suffix);
    println!("  check         curl -sI {platform_url}/healthz | grep x-fragment-deploy");
    Ok(())
}

/// The platform Worker's config, rendered from the deployment's and the
/// checked-in one (`root`'s `cell/`), its store secrets bound by name in
/// the store `store_id`. No value of a secret is in it.
fn worker_config(d: &Deployment, n: &Names, store_id: &str, deploy_id: &str, root: &Path) -> Result<Value> {
    let bound = bound(d)?;
    let (catalog, _) = catalog_of(d)?;
    let platform_url = format!("https://{}", n.platform_host);
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
    if let Some(u) = &d.support_url {
        v.insert("FRAGMENT_SUPPORT_URL".into(), json!(u));
    }
    if let Some(m) = &d.vision_model {
        v.insert("FRAGMENT_VISION_MODEL".into(), json!(m.trim()));
    }
    if let Some(computers) = &d.computers {
        v.insert("FRAGMENT_COMPUTER_IMAGE".into(), json!(computers.default_image));
        if let Some(ms) = computers.unsaved_max_ms {
            v.insert("FRAGMENT_COMPUTER_UNSAVED_MAX_MS".into(), json!(ms.to_string()));
        }
    }
    if !catalog.is_empty() {
        v.insert("FRAGMENT_PROVIDERS".into(), Value::String(serde_json::to_string(catalog.providers())?));
    }
    if let Some(version) = d.price_book_version {
        v.insert("FRAGMENT_PRICE_BOOK_VERSION".into(), json!(version.to_string()));
    }
    c.insert("vars".into(), vars);
    anyhow::ensure!(!c.contains_key("secrets_store_secrets"), "cell/wrangler.jsonc binds no secrets of its own: a deployment's are bound here");
    c.insert("secrets_store_secrets".into(), devstack::store::bindings_json(store_id, &bound.cell()));
    Ok(cell)
}

/// `cargo xtask e2e --hosted --config <file> --branch <b> [--only … |
/// --except …] [--dry-run | --sweep [<run>] | --sweep-all] [--max-paid-calls
/// <n>]`: the suite's
/// own arguments for that branch deployment, from its config: the zone, the
/// test secret's file (named, never read here), and what the deployment
/// offers (computers, models). A deployment of its own is refused: the
/// hosted lane runs on a preview.
pub fn hosted_e2e_args(rest: &[String]) -> Result<Vec<String>> {
    let usage = || {
        anyhow::anyhow!(
            "usage: cargo xtask e2e --hosted --config <file> --branch <name> [--only <section>[,...] | --except <section>[,...]] [--dry-run | --sweep [<run>] | --sweep-all] [--max-paid-calls <n>] [--operator-key-file <file>]"
        )
    };
    let (mut config, mut branch, mut passed, mut operator_key_file) = (None, None, vec![], None);
    let mut it = rest.iter().peekable();
    // bounded: each pass takes one argument at least
    while let Some(a) = it.next() {
        match a.as_str() {
            "--hosted" => {}
            "--config" => config = Some(PathBuf::from(it.next().ok_or_else(usage)?)),
            "--branch" => branch = Some(it.next().ok_or_else(usage)?.clone()),
            // an operator key's file (`fragment operator key` makes one): the
            // runner reads it by path; the deployment lists its npub
            "--operator-key-file" => operator_key_file = Some(PathBuf::from(it.next().ok_or_else(usage)?)),
            "--only" | "--except" | "--max-paid-calls" => {
                passed.push(a.clone());
                passed.push(it.next().ok_or_else(usage)?.clone());
            }
            "--dry-run" | "--sweep-all" => passed.push(a.clone()),
            // the run it names, when the next argument is not another flag
            // (the suite checks it is one)
            "--sweep" => {
                passed.push(a.clone());
                passed.extend(it.next_if(|next| !next.starts_with("--")).cloned());
            }
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
    // the run reads the key, and checks it is one of these
    if let Some(file) = &operator_key_file {
        anyhow::ensure!(!d.operators.is_empty(), "--operator-key-file names a key the deployment's operators list: its config's operators are empty");
        args.extend(["--operator-key-file".to_string(), expand(file).to_string_lossy().into_owned(), "--operators".into(), d.operators.join(",")]);
    }
    let offers: Vec<&str> = [(d.computers.is_some(), "computers"), (d.ai_gateway.is_some(), "models")].into_iter().filter(|(on, _)| *on).map(|(_, o)| o).collect();
    if !offers.is_empty() {
        args.extend(["--offers".to_string(), offers.join(",")]);
    }
    args.extend(passed);
    Ok(args)
}

/// Removes a branch deployment: its Worker (its Durable Objects and
/// data with them), its Workflow and its queues. Its bucket is emptied by
/// the deployment's own blob grace and left: R2 refuses to delete a bucket
/// with objects in it. Irreversible: ask before running it.
pub fn teardown(rest: &[String]) -> Result<()> {
    let (config, branch) = args(rest)?;
    let d = load(&config)?;
    anyhow::ensure!(branch.is_some(), "teardown removes a branch deployment: name its --branch");
    let n = names(&d, branch.as_deref())?;
    let tools = devstack::Tools::locate()?;
    // Cloudflare deletes neither first: not a Worker that still consumes a
    // queue (10064), nor a queue a Worker still binds. So its consumers go,
    // then the Worker, then its queues.
    for q in [&n.deliveries, &n.dead, &n.ledger] {
        ensure(wrangler(&tools, &d.account_id)?.args(["queues", "consumer", "remove", q, &n.cell]), "a queue's consumer")?;
    }
    ensure(wrangler(&tools, &d.account_id)?.args(["delete", "--name", &n.cell, "--force"]), "the Worker")?;
    ensure(wrangler(&tools, &d.account_id)?.args(["workflows", "delete", &n.jobs]), "the Workflow")?;
    for q in [&n.deliveries, &n.dead, &n.ledger] {
        ensure(wrangler(&tools, &d.account_id)?.args(["queues", "delete", q]), "a queue")?;
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
            host_secret: "fragment-host-secret".into(),
            host_secret_previous: None,
            codestorage: CodeStorage { org: "o".into(), private_key: "fragment-codestorage-private-key".into(), api: None },
            workos: WorkOs { client_id: "fragment-workos-client-id".into(), api_key: "fragment-workos-api-key".into() },
            operators: vec![],
            ai_gateway: None,
            vision_model: None,
            default_plan: None,
            support_url: None,
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
        // a sweep: one run's (named, or the last that finished here), or all
        let base = ["--hosted", "--zone", "finite.place", "--branch", "p5", "--secret-file", "/run/secrets/p5-test"];
        let with = |more: &[&str]| base.iter().chain(more).map(|s| s.to_string()).collect::<Vec<_>>();
        let sweep = |more: &[&str]| args(&[&["--hosted", "--config", path.to_str().unwrap(), "--branch", "p5"][..], more].concat()).unwrap();
        assert_eq!(sweep(&["--sweep", "c58b2a"]), with(&["--sweep", "c58b2a"]));
        assert_eq!(sweep(&["--sweep"]), with(&["--sweep"]));
        assert_eq!(sweep(&["--sweep", "--max-paid-calls", "0"]), with(&["--sweep", "--max-paid-calls", "0"]), "a flag after it is not its run");
        assert_eq!(sweep(&["--sweep-all"]), with(&["--sweep-all"]));
        // an operator key's file (named, not read) and the operators it is among
        let npub = fragment_core::npub::encode(&"ab".repeat(32));
        v["operators"] = json!([npub]);
        let path = config_file("hosted-args-operator", &v);
        let got = args(&["--hosted", "--config", path.to_str().unwrap(), "--branch", "p5", "--operator-key-file", "/run/secrets/p5-operator"]).unwrap();
        assert_eq!(got, with(&["--operator-key-file", "/run/secrets/p5-operator", "--operators", &npub]));
        v["operators"] = json!([]);
        let path = config_file("hosted-args-operator-unlisted", &v);
        assert!(args(&["--hosted", "--config", path.to_str().unwrap(), "--branch", "p5", "--operator-key-file", "/k"]).is_err(), "a key among no operators");
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
        assert_eq!((n.cell.as_str(), n.bucket.as_str()), ("fragment-dev", "fragment-blobs-dev"));
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
    /// list prices, each key's store secret named; and every name they give
    /// a secret is the one dev and the e2e bind locally
    /// (`store::Bound::conventional`), so the three never drift apart.
    #[test]
    fn the_example_and_e2e_configs_parse() {
        for file in ["deploy/example.jsonc", "deploy/e2e.jsonc"] {
            let path = devstack::repo_root().join(file);
            let d = load(&path).unwrap();
            assert!(names(&d, Some("dev")).is_ok(), "{file}");
            assert_eq!(d.computers.as_ref().map(|c| c.default_image.as_str()), Some("hermes"), "{file}");
            let (catalog, keys) = catalog_of(&d).unwrap();
            let names: Vec<&str> = catalog.providers().iter().map(|p| p.name.as_str()).collect();
            assert_eq!(names, ["google", "perplexity", "google-places", "xai", "elevenlabs"], "{file}");
            assert_eq!(keys.len(), 4, "{file}: each operator key's store secret");
            assert!(catalog.key_prices().iter().all(|k| fragment_core::price::default_key_price(&k.key) == Some((k.micros, k.per))), "{file}: at list");
            let conventional = devstack::store::Bound::conventional(true, &["perplexity", "google-places", "xai", "elevenlabs"]);
            assert_eq!(bound(&d).unwrap(), conventional, "{file}: the names dev and the e2e bind");
        }
    }

    /// Every name a config gives a store secret is one: a path (the old
    /// fields' values pasted into the new ones) or anything outside the
    /// store's characters is refused with its field, and a rotation names
    /// two host secrets, not one twice.
    #[test]
    fn a_name_that_is_no_store_name_is_refused() {
        let refused = |edit: &dyn Fn(&mut Deployment)| {
            let mut d = deployment(None, None);
            edit(&mut d);
            checked(d).err().map(|e| e.to_string()).unwrap_or_default()
        };
        let r = refused(&|d| d.host_secret = "~/.config/fragment/secrets/host-secret".into());
        assert!(r.contains("host_secret is \"~/.config") && r.contains("looks like a file's path"), "{r}");
        let r = refused(&|d| d.workos.api_key = "workos api key".into());
        assert!(r.contains("workos.api_key is \"workos api key\"") && !r.contains("file's path"), "{r}");
        let r = refused(&|d| d.codestorage.private_key = String::new());
        assert!(r.contains("codestorage.private_key"), "{r}");
        let r = refused(&|d| d.host_secret_previous = Some("fragment-host-secret".into()));
        assert!(r.contains("names the host secret itself"), "{r}");
        let r = refused(&|d| d.providers = vec![json!({ "name": "xai", "kind": "operator", "hosts": ["api.x.ai"], "placements": [{ "header": "authorization", "format": "Bearer {}" }], "env": ["XAI_API_KEY"], "key": "x/y" })]);
        assert!(r.contains("providers: xai's key is \"x/y\""), "{r}");
        assert!(refused(&|d| d.host_secret_previous = Some("fragment-host-secret-2026-09".into())).is_empty(), "a rotation's two names");
    }

    /// The preflight: a store holding every secret the config names lets
    /// the deploy on; one lacking some is refused, naming each missing
    /// secret, its field, and the command that sets it; no store at all is
    /// refused the same way, every secret named.
    #[test]
    fn the_preflight_names_each_missing_secret() {
        let mut d = deployment(None, None);
        d.host_secret_previous = Some("fragment-host-secret-old".into());
        d.providers = vec![json!({ "name": "xai", "kind": "operator", "hosts": ["api.x.ai"], "placements": [{ "header": "authorization", "format": "Bearer {}" }], "env": ["XAI_API_KEY"], "key": "fragment-xai-api-key" })];
        let b = bound(&checked(d).unwrap()).unwrap();
        let named = named_secrets(&b);
        let listed = |names: &[&str]| names.iter().map(|n| devstack::store::Listed { name: n.to_string(), id: "0".into(), created: String::new(), modified: String::new() }).collect::<Vec<_>>();
        let all = ["fragment-host-secret", "fragment-host-secret-old", "fragment-codestorage-private-key", "fragment-workos-client-id", "fragment-workos-api-key", "fragment-xai-api-key", "another-projects-secret"];
        assert!(devstack::store::missing(&named, &listed(&all)).is_empty(), "every one there: the deploy goes on");

        let missing = devstack::store::missing(&named, &listed(&all[..4]));
        let config = Path::new("/home/p/.config/finite-next/e2e.jsonc");
        let store = devstack::store::AccountStore { name: "fragment".into(), id: "0f0e".into() };
        let said = refusal(config, Some(&store), &missing);
        assert!(said.starts_with("the account's Secrets Store fragment (0f0e) lacks 2 of the secrets this config names; nothing was built or deployed"), "{said}");
        assert!(said.contains("workos.api_key        cargo xtask secret set fragment-workos-api-key --config /home/p/.config/finite-next/e2e.jsonc"), "{said}");
        assert!(said.contains("providers: xai's key  cargo xtask secret set fragment-xai-api-key --config /home/p/.config/finite-next/e2e.jsonc"), "{said}");
        assert!(!said.contains("fragment-host-secret "), "what is there is not named: {said}");

        let said = refusal(config, None, &named);
        assert!(said.starts_with("the account has no Secrets Store yet"), "{said}");
        assert_eq!(said.matches("cargo xtask secret set ").count(), named.len(), "{said}");
    }

    /// The Worker's config a deploy writes: every secret a binding to its
    /// store secret by name, no secret's value and no old name anywhere in
    /// it, and the WorkOS client id no longer a variable.
    #[test]
    fn the_deploy_binds_its_secrets_by_name() {
        let text = fs::read_to_string(devstack::repo_root().join("deploy/e2e.jsonc")).unwrap();
        let mut v: Value = serde_json::from_str(&devstack::strip_comments(&text)).unwrap();
        v["host_secret_previous"] = json!("fragment-host-secret-old");
        let d = load(&config_file("bindings", &v)).unwrap();
        let n = names(&d, Some("p5")).unwrap();
        let cell = worker_config(&d, &n, "0f0e0d0c", "abc123", &devstack::repo_root()).unwrap();
        let bound: Vec<(&str, &str)> = cell["secrets_store_secrets"].as_array().unwrap().iter().map(|b| (b["binding"].as_str().unwrap(), b["secret_name"].as_str().unwrap())).collect();
        assert_eq!(
            bound,
            [
                ("HOST_SECRET", "fragment-host-secret"),
                ("HOST_SECRET_PREVIOUS", "fragment-host-secret-old"),
                ("CODESTORAGE_KEY", "fragment-codestorage-private-key"),
                ("WORKOS_CLIENT", "fragment-workos-client-id"),
                ("WORKOS_KEY", "fragment-workos-api-key"),
                ("OPERATOR_KEY_PERPLEXITY", "fragment-perplexity-api-key"),
                ("OPERATOR_KEY_GOOGLE_PLACES", "fragment-google-places-api-key"),
                ("OPERATOR_KEY_XAI", "fragment-xai-api-key"),
                ("OPERATOR_KEY_ELEVENLABS", "fragment-elevenlabs-api-key"),
            ]
        );
        assert!(cell["secrets_store_secrets"].as_array().unwrap().iter().all(|b| b["store_id"] == "0f0e0d0c"));
        assert!(cell["vars"].get("WORKOS_CLIENT_ID").is_none(), "the client id is a binding now");
        let providers = cell["vars"]["FRAGMENT_PROVIDERS"].as_str().unwrap();
        assert!(bound[5..].iter().all(|(_, name)| !providers.contains(name)), "a key's store name is the deploy's, not the cell's");
        let whole = cell.to_string();
        for old in ["FRAGMENT_HOST_SECRET", "CODESTORAGE_PRIVATE_KEY", "WORKOS_API_KEY", "FRAGMENT_KEY_", "key_file", "secrets/"] {
            assert!(!whole.contains(old), "{old} is in a rendered config");
        }
    }

    /// A field gone in a hard cut is refused, never deployed without:
    /// OpenRouter's key, and the secrets' files the store replaced (a
    /// provider's among them).
    #[test]
    fn a_config_naming_a_field_gone_is_refused() {
        let text = fs::read_to_string(devstack::repo_root().join("deploy/example.jsonc")).unwrap();
        let fresh: Value = serde_json::from_str(&devstack::strip_comments(&text)).unwrap();
        // each field, and the object (a JSON pointer) it is added to
        for (field, within) in [("openrouter_api_key_file", ""), ("host_secret_file", ""), ("key_file", "/providers/1")] {
            let mut v = fresh.clone();
            v.pointer_mut(within).and_then(Value::as_object_mut).expect("the config has it").insert(field.into(), json!("~/f"));
            let refused = load(&config_file(field, &v)).err().map(|e| format!("{e:#}")).unwrap_or_default();
            assert!(refused.contains(&format!("unknown field `{field}`")), "{refused}");
        }
        assert!(load(&config_file("fresh", &fresh)).is_ok());
    }

    /// The provider catalog, its keys' store names, and a default image are
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
        let d = with(vec![row(json!({ "key": "fragment-perplexity-api-key" }))]);
        let (catalog, keys) = catalog_of(&d).unwrap();
        assert_eq!(catalog.get("perplexity").unwrap().price.map(|p| (p.micros, p.per)), Some((5_000_000, 1_000)), "the price book's list price");
        assert_eq!(keys, vec![("perplexity".to_string(), "fragment-perplexity-api-key".to_string())]);
        assert!(!serde_json::to_string(catalog.providers()).unwrap().contains("fragment-perplexity-api-key"), "the key's store name is the deploy's, never the cell's");
        assert!(checked(d).is_ok());
        assert!(checked(with(vec![row(json!({}))])).is_err(), "an operator key names its store secret");
        assert!(checked(with(vec![row(json!({ "key": "k", "name": "Perplexity" }))])).is_err());
        assert!(checked(with(vec![row(json!({ "key": "k", "hosts": ["localhost"] }))])).is_err());
        let google = json!({ "name": "google", "kind": "connection", "hosts": ["www.googleapis.com"], "placements": [{ "header": "authorization", "format": "Bearer {}" }], "env": ["GOOGLE_OAUTH_ACCESS_TOKEN"], "key": "k" });
        assert!(checked(with(vec![google])).is_err(), "a connection has no key");
        let mut d = deployment(None, None);
        d.computers = Some(Computers { default_image: "hermes".into(), images: BTreeMap::new(), unsaved_max_ms: None });
        assert!(checked(d).is_err());
        assert!(checked(deployment(None, None)).is_ok());
    }

    /// The vision model is one the price book prices, refused before a
    /// deploy otherwise (the cell would refuse it at its first request);
    /// named, it is the cell's `FRAGMENT_VISION_MODEL`, and unnamed the
    /// cell's default (GLM-5.3 Flash).
    #[test]
    fn a_vision_model_the_book_does_not_price_is_refused() {
        let with = |m: Option<&str>| {
            let mut d = deployment(None, None);
            d.vision_model = m.map(str::to_string);
            d
        };
        let refused = checked(with(Some("@cf/deepseek-ai/deepseek-v4-flash-0731"))).err().map(|e| format!("{e:#}")).unwrap_or_default();
        assert!(refused.contains("vision_model: the vision model \"@cf/deepseek-ai/deepseek-v4-flash-0731\" is not in the price book"), "{refused}");
        assert!(checked(with(Some("@cf/zai-org/glm-5.3-flash"))).is_ok());
        assert!(checked(with(None)).is_ok());
        let text = fs::read_to_string(devstack::repo_root().join("deploy/e2e.jsonc")).unwrap();
        let mut v: Value = serde_json::from_str(&devstack::strip_comments(&text)).unwrap();
        let rendered = |v: &Value, test: &str| {
            let d = load(&config_file(test, v)).unwrap();
            let n = names(&d, Some("p5")).unwrap();
            worker_config(&d, &n, "0f0e0d0c", "abc123", &devstack::repo_root()).unwrap()["vars"].clone()
        };
        assert!(rendered(&v, "vision-none").get("FRAGMENT_VISION_MODEL").is_none(), "unnamed: the cell's default");
        v["vision_model"] = json!("@cf/zai-org/glm-5.3-flash");
        assert_eq!(rendered(&v, "vision-named")["FRAGMENT_VISION_MODEL"], "@cf/zai-org/glm-5.3-flash");
        v["vision_model"] = json!("@cf/meta/llama-4-scout-17b-16e-instruct");
        let refused = load(&config_file("vision-unpriced", &v)).err().map(|e| format!("{e:#}")).unwrap_or_default();
        assert!(refused.contains("is not in the price book"), "{refused}");
    }

    /// The support link a person whose computer will not start is shown:
    /// an https page or a mailto address, refused before a deploy
    /// otherwise; named, it is the cell's `FRAGMENT_SUPPORT_URL`.
    #[test]
    fn a_support_link_is_checked_and_rendered() {
        let with = |u: Option<&str>| {
            let mut d = deployment(None, None);
            d.support_url = u.map(str::to_string);
            d
        };
        assert!(checked(with(Some("https://help.example.dev"))).is_ok() && checked(with(Some("mailto:help@example.dev"))).is_ok() && checked(with(None)).is_ok());
        let refused = checked(with(Some("javascript:alert(1)"))).err().map(|e| format!("{e:#}")).unwrap_or_default();
        assert!(refused.contains("support_url: \"javascript:alert(1)\" is no https: page or mailto: address"), "{refused}");
        let text = fs::read_to_string(devstack::repo_root().join("deploy/e2e.jsonc")).unwrap();
        let mut v: Value = serde_json::from_str(&devstack::strip_comments(&text)).unwrap();
        v["support_url"] = json!("mailto:help@example.dev");
        let d = load(&config_file("support-named", &v)).unwrap();
        let n = names(&d, Some("p5")).unwrap();
        assert_eq!(worker_config(&d, &n, "0f0e0d0c", "abc123", &devstack::repo_root()).unwrap()["vars"]["FRAGMENT_SUPPORT_URL"], "mailto:help@example.dev");
    }
}
