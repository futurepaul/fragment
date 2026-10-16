//! `cargo xtask deploy --config <file> [--branch <name>] [--pinned-only]`
//! and `cargo xtask teardown --config <file> --branch <name>`
//! (docs/cloudflare-v1.md, decisions 4 and 20).
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
//! fragments are `<name>--b.<zone>` (one wildcard DNS record
//! and certificate cover them all). Its repos are named `b--…` in the
//! code.storage org, so it never touches another deployment's.
//!
//! A deployment of its own names its hosts instead of a zone: the
//! platform's (`platform_host`) and the fragments' suffix
//! (`fragment_suffix`), each a zone's own name, so its routes are in two
//! zones (or one, named twice), each in its host's.

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
    /// A branch deployment's zone, which its routes are on: its platform is
    /// `<branch>.<zone>`. A deployment of its own names none (refused).
    zone: Option<String>,
    /// The platform's host and the fragments' suffix, for a deployment of
    /// its own (production: `fragment.club`, `fragment.boats`): each a
    /// zone's own name on the account (the same one, for one zone for
    /// both), and each route is in its host's zone.
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
    /// Seats sold through Stripe (docs/billing.md). Without it, seats are
    /// comped by operators only.
    stripe: Option<StripeDeploy>,
    /// Who may grant credit, set plans, and wipe a
    /// person (npubs, or identities). The hosted e2e's `wipe` signs with an
    /// operator key among them, its file named on its command line
    /// (`--operator-key-file`), never here: the key is the runner's.
    #[serde(default)]
    operators: Vec<String>,
    /// The AI Gateway the model route calls through (its id, named: never
    /// `default`, which makes one that logs). Without it, models are off.
    ai_gateway: Option<String>,
    /// Where the model route's names run (`FRAGMENT_MODELS`, less the keys;
    /// docs/computers.md, Models; decision 61): an object of the slots it
    /// names (`cheap`, `medium`, `fallback`: a tier's call is made on it
    /// once more when its own fails before answering, `vision`: a runtime's
    /// calls about an image), each `{"provider": "workers_ai", "model":
    /// <catalog id>}` or `{"provider": "openai", "base_url", "key": <its
    /// secret's name in the store>, "model", "body"?: <the API's own fields
    /// every call carries>}`. Each slot unnamed is on Workers AI as before
    /// (`fragment_core::models::Models::defaults`). A slot, provider, URL,
    /// model or body the cell would refuse is refused before a deploy, as
    /// is an OpenAI-compatible API without a key, and slots past a Workers
    /// variable's 5 KB.
    models: Option<Value>,
    /// A new person's plan: `guest` (the default), `seat`, or `seat_always_on`.
    default_plan: Option<String>,
    /// Where a person whose computer will not start gets help
    /// (`FRAGMENT_SUPPORT_URL`): an `https:` page or a `mailto:` address
    /// (`fragment_core::computer::support_url_ok`), linked from the shell.
    support_url: Option<String>,
    /// The address the platform's mail comes from (`FRAGMENT_MAIL_FROM`;
    /// cell/src/mail.rs), on a domain onboarded to Cloudflare Email
    /// Sending (`wrangler email sending enable <domain>`). Without it, the
    /// deployment sends no mail.
    mail_from: Option<String>,
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
    /// By name: each image built at every deploy (its Dockerfile and build
    /// context, relative to the repo: `images/hermes/Dockerfile`, `.`, as the
    /// Hermes image carries the CLI and builds from the repo root; and its
    /// build variables), or kept at a digest the account's registry holds
    /// (`{"image": …@sha256:…}`): the stable image a canary is ahead of
    /// (docs/computers.md, "Stable and canary").
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

/// One image computers may be pinned to, as wrangler takes a Durable
/// Object container's named image: built from the tree at every deploy, or
/// a digest already in the account's registry, which a deploy keeps as it
/// is (wrangler skips its build and prepares it, and refuses the deploy
/// when the account's registry does not have it).
#[derive(Deserialize, serde::Serialize)]
#[serde(untagged)]
enum Image {
    Built(Built),
    Pinned(Pinned),
}

#[derive(Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct Built {
    dockerfile: String,
    build_context: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    build_vars: BTreeMap<String, String>,
}

#[derive(Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct Pinned {
    /// `registry.cloudflare.com/<account>/<repository>@sha256:<64 hex>`: a
    /// reference a deploy printed, or a computer's (docs/computers.md).
    image: String,
}

/// The registry wrangler pushes a deployment's images to.
const REGISTRY: &str = "registry.cloudflare.com";

/// Whether `reference` is an image pinned by digest in `account`'s
/// registry: `registry.cloudflare.com/<account>/<repository>@sha256:` and 64
/// lowercase hex digits, its repository's components lowercase letters and
/// digits joined by `.`, `_` or `-` (what wrangler takes). Another
/// account's image (a preview's, at a production deploy) is refused here:
/// the account deployed to could not run it.
fn pinned_ok(reference: &str, account: &str) -> bool {
    let Some((repository, hex)) = reference.strip_prefix(REGISTRY).and_then(|r| r.strip_prefix('/')).and_then(|r| r.strip_prefix(account)).and_then(|r| r.strip_prefix('/')).and_then(|r| r.split_once("@sha256:")) else {
        return false;
    };
    let component = |c: &str| {
        let alnum = |b: u8| b.is_ascii_lowercase() || b.is_ascii_digit();
        !c.is_empty() && c.bytes().all(|b| alnum(b) || b"._-".contains(&b)) && c.bytes().next().is_some_and(alnum) && c.bytes().last().is_some_and(alnum)
    };
    !account.is_empty() && repository.split('/').all(component) && hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
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

/// Each slot of the model route on an OpenAI-compatible API, and its key's
/// name in the store.
type SlotKeys = Vec<(String, String)>;

/// The deployment's models (`models`): the cell's (`FRAGMENT_MODELS`, each
/// slot's upstream less its key) and the store secret each slot on an
/// OpenAI-compatible API names for its key. Such a slot names one; a
/// Workers AI slot names none. Checked as the cell checks them
/// (`fragment_core::models::Models::parse`), on no local address.
fn models_of(d: &Deployment) -> Result<(Option<Value>, SlotKeys)> {
    let Some(models) = &d.models else { return Ok((None, vec![])) };
    let Some(slots) = models.as_object() else { bail!("models is an object: the slots it names ({}), each where it runs", fragment_core::models::SLOTS.join(", ")) };
    let mut cell = serde_json::Map::new();
    let mut keys = vec![];
    for (slot, row) in slots {
        let mut row = row.clone();
        let key = row.as_object_mut().and_then(|o| o.remove("key"));
        match (row["provider"].as_str(), key) {
            (Some("openai"), Some(Value::String(k))) => keys.push((slot.clone(), k)),
            (Some("openai"), Some(_)) => bail!("models.{slot}: its key is its secret's name in the store, a string"),
            (Some("openai"), None) => bail!("models.{slot}: an OpenAI-compatible API names its key: its secret's name in the store (cargo xtask secret set <name> --config <file>)"),
            (Some("workers_ai"), Some(_)) => bail!("models.{slot}: only an OpenAI-compatible API (provider openai) names a key"),
            (Some("workers_ai"), None) => {}
            (other, _) => bail!(
                "models.{slot}: its provider is workers_ai (a Workers AI model, through the gateway) or openai (an OpenAI-compatible API), not {}",
                other.map_or("none".to_string(), |p| format!("{p:?}"))
            ),
        }
        cell.insert(slot.clone(), row);
    }
    let cell = Value::Object(cell);
    let text = cell.to_string();
    fragment_core::models::Models::parse(Some(&text), &fragment_core::price::PriceBook::defaults(), false).map_err(|e| anyhow::anyhow!("{e}"))?;
    if text.len() > WORKER_VAR_MAX_BYTES {
        bail!("models is {} bytes as the cell's FRAGMENT_MODELS, past a Workers variable's {WORKER_VAR_MAX_BYTES}: shorten the slots' bodies", text.len());
    }
    Ok((Some(cell), keys))
}

/// A Workers variable's largest value: 5 KB (developers.cloudflare.com/
/// workers/platform/limits, Environment variables), taken as 5,000 bytes.
const WORKER_VAR_MAX_BYTES: usize = 5_000;

/// Fields a hard cut took out of the config, and what replaced each: a
/// config naming one is refused, saying so.
const FIELDS_GONE: [(&str, &str); 2] = [
    ("vision_model", "models.vision: {\"provider\": \"workers_ai\", \"model\": <its id>} (2026-10-14, decision 61)"),
    ("fallback_model", "models.fallback: {\"provider\": \"workers_ai\", \"model\": <its id>} (2026-10-14, decision 61)"),
];

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
struct StripeDeploy {
    /// The account's restricted key and this deployment's webhook signing
    /// secret: their names in the store.
    key: String,
    webhook_secret: String,
    /// The deployment's own portal configuration (`bpc_…`), never the
    /// account's default: finite-mono's is.
    portal: Option<String>,
    /// Stripe's automatic tax on a Checkout (decision 54; default true): off
    /// for an account with no tax settings, as a sandbox may be.
    tax: Option<bool>,
    /// The account's restricted key's file, for xtask's own calls (`stripe
    /// check|setup`, a branch's webhook endpoint): the deploying machine's,
    /// as the DNS token is. A branch deployment with it gets an endpoint of
    /// its own at each deploy, its secret a Worker secret (decision 58).
    key_file: Option<PathBuf>,
    /// The API's base (default https://api.stripe.com; tests: the fake).
    api: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkOs {
    /// The environment's client id and API key: their names in the store.
    client_id: String,
    api_key: String,
}

/// Whether a deploy makes the deployment's webhook endpoint itself: a
/// branch's, when the config names the key xtask calls Stripe with
/// (decision 58). Its secret is then a Worker secret, not the store's.
fn makes_endpoint(d: &Deployment, branch: Option<&str>) -> bool {
    branch.is_some() && d.stripe.as_ref().is_some_and(|s| s.key_file.is_some())
}

/// The store secrets the deployment's Workers are bound to, by name.
fn bound(d: &Deployment) -> Result<devstack::store::Bound> {
    bound_for(d, None)
}

/// `bound`, for a deploy of `branch` (one that makes its endpoint binds no
/// store secret for it).
fn bound_for(d: &Deployment, branch: Option<&str>) -> Result<devstack::store::Bound> {
    let (_, operator_keys) = catalog_of(d)?;
    let (_, model_keys) = models_of(d)?;
    Ok(devstack::store::Bound {
        host_secret: d.host_secret.clone(),
        host_secret_previous: d.host_secret_previous.clone(),
        codestorage_key: d.codestorage.private_key.clone(),
        workos: Some((d.workos.client_id.clone(), d.workos.api_key.clone())),
        stripe: d.stripe.as_ref().map(|s| (s.key.clone(), (!makes_endpoint(d, branch)).then(|| s.webhook_secret.clone()))),
        operator_keys,
        model_keys,
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
    if let Some((key, webhook)) = &b.stripe {
        named.push(("stripe.key".into(), key.as_str()));
        if let Some(w) = webhook {
            named.push(("stripe.webhook_secret".into(), w.as_str()));
        }
    }
    // a secret two slots name is one line, naming both
    let first_model_key = named.len();
    for (slot, name) in &b.model_keys {
        match named[first_model_key..].iter_mut().find(|(_, n)| *n == name.as_str()) {
            Some((field, _)) => field.push_str(&format!(", models.{slot}.key")),
            None => named.push((format!("models.{slot}.key"), name.as_str())),
        }
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
    routes: Vec<Route>,
    /// The hosts those routes answer on, which must be proxied.
    dns: Vec<crate::dns::Wanted>,
    /// Repos this deployment makes are named with this before them.
    repo_prefix: Option<String>,
}

/// A Worker route, and the zone it is in (wrangler's `zone_name`).
#[derive(Debug, PartialEq, Eq)]
struct Route {
    pattern: String,
    zone: String,
}

fn names(d: &Deployment, branch: Option<&str>) -> Result<Names> {
    match (branch, &d.platform_host, &d.fragment_suffix) {
        (Some(b), None, None) => {
            anyhow::ensure!(valid_branch(b), "a branch is 1-16 of a-z, 0-9 and single dashes inside, not {b:?}");
            let zone = d.zone.as_deref().context("a --branch deploys at <branch>.<zone>: the config names its zone")?;
            Ok(Names {
                cell: format!("fragment-{b}"),
                jobs: format!("fragment-jobs-{b}"),
                deliveries: format!("fragment-deliveries-{b}"),
                dead: format!("fragment-deliveries-dead-{b}"),
                ledger: format!("fragment-ledger-{b}"),
                bucket: format!("fragment-blobs-{b}"),
                platform_host: format!("{b}.{zone}"),
                suffix: zone.to_string(),
                label_suffix: Some(format!("--{b}")),
                routes: vec![Route { pattern: format!("{b}.{zone}/*"), zone: zone.into() }, Route { pattern: format!("*--{b}.{zone}/*"), zone: zone.into() }],
                // one wildcard covers every branch's platform and fragments
                dns: vec![crate::dns::Wanted { zone: zone.into(), name: format!("*.{zone}") }],
                repo_prefix: Some(format!("{b}--")),
            })
        }
        (None, Some(platform), Some(suffix)) => {
            if let Some(zone) = &d.zone {
                bail!("zone ({zone}) is a branch deployment's: a deployment of its own is on its hosts' zones, platform_host's and fragment_suffix's. Remove it");
            }
            Ok(Names {
                cell: "fragment".into(),
                jobs: "fragment-jobs".into(),
                deliveries: "fragment-deliveries".into(),
                dead: "fragment-deliveries-dead".into(),
                ledger: "fragment-ledger".into(),
                bucket: "fragment-blobs".into(),
                platform_host: platform.clone(),
                suffix: suffix.clone(),
                label_suffix: None,
                // each host a zone's own name: its route is in that zone
                routes: vec![Route { pattern: format!("{platform}/*"), zone: platform.clone() }, Route { pattern: format!("*.{suffix}/*"), zone: suffix.clone() }],
                dns: vec![
                    crate::dns::Wanted { zone: platform.clone(), name: platform.clone() },
                    crate::dns::Wanted { zone: suffix.clone(), name: format!("*.{suffix}") },
                ],
                repo_prefix: None,
            })
        }
        (Some(_), _, _) => bail!("a config naming platform_host or fragment_suffix deploys one deployment of its own: no --branch"),
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
    for (field, now) in FIELDS_GONE {
        if v.get(field).is_some() {
            bail!("{}: {field} is gone; name it as {now}", config.display());
        }
    }
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
/// secrets' names, and its models (`models_of`, through `bound`).
fn checked(d: Deployment) -> Result<Deployment> {
    check_names(&bound(&d)?)?;
    if let Some(c) = &d.computers {
        if !c.images.contains_key(&c.default_image) {
            bail!("computers: the default image {:?} is not one of its images", c.default_image);
        }
        for (name, image) in &c.images {
            if let Image::Pinned(p) = image {
                if !pinned_ok(&p.image, &d.account_id) {
                    bail!("computers: the image {name:?} is {:?}, not one pinned by digest in this account's registry ({REGISTRY}/{}/<repository>@sha256:<64 hex>)", p.image, d.account_id);
                }
            }
        }
    }
    if let Some(u) = d.support_url.as_deref().filter(|u| !fragment_core::computer::support_url_ok(u)) {
        bail!("support_url: {u:?} is no https: page or mailto: address of at most {} visible characters", fragment_core::computer::SUPPORT_URL_MAX_BYTES);
    }
    if let Some(f) = &d.mail_from {
        fragment_core::mail::Sender::parse(f).map_err(|e| anyhow::anyhow!("mail_from: {e}"))?;
    }
    Ok(d)
}

/// A deploy's or a teardown's arguments.
struct Args {
    config: PathBuf,
    branch: Option<String>,
    /// A deploy's `--pinned-only` (`pinned_only`).
    pinned_only: bool,
}

fn args(rest: &[String]) -> Result<Args> {
    let usage = || anyhow::anyhow!("usage: cargo xtask deploy --config <file> [--branch <name>] [--pinned-only] | teardown --config <file> --branch <name>");
    let (mut config, mut branch, mut pinned_only) = (None, None, false);
    let mut it = rest.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--config" => config = Some(PathBuf::from(it.next().ok_or_else(usage)?)),
            "--branch" => branch = Some(it.next().ok_or_else(usage)?.clone()),
            "--pinned-only" => pinned_only = true,
            _ => return Err(usage()),
        }
    }
    Ok(Args { config: config.ok_or_else(usage)?, branch, pinned_only })
}

/// `--pinned-only`, CI's deploy (.github/workflows/ci.yml): every computer
/// image the config names is kept at its digest, so the deploy builds none.
/// A runner keeps no layer cache: a build there stamps every layer anew,
/// pushes the whole base (gigabytes) and makes a new digest at every
/// deploy, which tells each computer pinned to it that an update is ready
/// (docs/computers.md, "What changes a version"). An image built from the
/// tree is refused, by name, before anything is read, built or made.
fn pinned_only(d: &Deployment) -> Result<()> {
    let built: Vec<&str> = d.computers.iter().flat_map(|c| &c.images).filter(|(_, i)| matches!(i, Image::Built(_))).map(|(name, _)| name.as_str()).collect();
    if !built.is_empty() {
        bail!(
            "--pinned-only builds no image, and this config builds {} from the tree: pin each at a digest this account's registry holds ({REGISTRY}/{}/<repository>@sha256:<64 hex>; docs/computers.md, \"Stable and canary\"), or deploy it without --pinned-only where Docker runs",
            built.join(", "),
            d.account_id
        );
    }
    Ok(())
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

/// `cargo xtask stripe check|setup --config <file> [--branch <name>]
/// [--webhook-secret-file <path>]` (xtask/src/stripe.rs).
pub fn stripe(rest: &[String]) -> Result<()> {
    let usage = || anyhow::anyhow!("usage: cargo xtask stripe check|setup --config <file> [--branch <name>] [--webhook-secret-file <path>]");
    let (verb, rest) = rest.split_first().ok_or_else(usage)?;
    let (mut config, mut branch, mut secret_file) = (None, None, None);
    let mut it = rest.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--config" => config = Some(PathBuf::from(it.next().ok_or_else(usage)?)),
            "--branch" => branch = Some(it.next().ok_or_else(usage)?.clone()),
            "--webhook-secret-file" => secret_file = Some(PathBuf::from(it.next().ok_or_else(usage)?)),
            _ => return Err(usage()),
        }
    }
    let d = load(&config.ok_or_else(usage)?)?;
    let n = names(&d, branch.as_deref())?;
    let st = d.stripe.as_ref().context("the config has no `stripe`: this deployment sells no seats")?;
    let key_file = st.key_file.as_deref().context("stripe.key_file names the account's restricted key's file, for xtask's own calls")?;
    let s = crate::stripe::Stripe::new(st.api.as_deref().unwrap_or("https://api.stripe.com"), &read_secret(key_file)?)?;
    let platform = format!("https://{}", n.platform_host);
    match verb.as_str() {
        "check" => {
            let wrong = crate::stripe::check(&s, &platform, st.portal.as_deref())?;
            if wrong.is_empty() {
                println!("stripe: {platform}'s prices, portal and webhook endpoint are as docs/billing.md wants them");
                return Ok(());
            }
            for w in &wrong {
                println!("  {w}");
            }
            bail!("stripe: {} thing(s) to fix", wrong.len())
        }
        "setup" => {
            let made = crate::stripe::setup(&s, &platform, st.portal.as_deref(), secret_file.as_deref())?;
            for p in &made.prices {
                println!("made the price {p}");
            }
            if let Some(p) = &made.portal {
                println!("made the portal configuration {p}: name it in the config's stripe.portal");
            }
            match (&made.endpoint, &secret_file) {
                (Some((id, file)), _) => println!(
                    "made the webhook endpoint {id} at {}: its signing secret is in {} (0600); store it: cargo xtask secret set {} --config <this config> --from-file {}",
                    crate::stripe::endpoint_url(&platform),
                    file.display(),
                    st.webhook_secret,
                    file.display()
                ),
                (None, None) if branch.is_none() => println!("no endpoint made: name --webhook-secret-file <path> for its signing secret"),
                _ => {}
            }
            Ok(())
        }
        _ => Err(usage()),
    }
}

pub fn deploy(rest: &[String]) -> Result<()> {
    let Args { config, branch, pinned_only: pinned } = args(rest)?;
    let d = load(&config)?;
    if pinned {
        pinned_only(&d)?;
    }
    let n = names(&d, branch.as_deref())?;
    // the two local files are read before anything is built or made
    let dns_token = d.dns_token_file.as_deref().map(read_secret).transpose()?;
    let test_secret = test_secret_file(&d, branch.as_deref())?.map(read_test_secret).transpose()?;
    if let Some(p) = &d.default_plan {
        anyhow::ensure!(matches!(p.as_str(), "guest" | "seat" | "seat_always_on"), "default_plan is guest, seat or seat_always_on, not {p:?}");
    }
    anyhow::ensure!(d.ai_gateway.as_deref() != Some("default"), "ai_gateway names the deployment's own gateway: `default` makes one that logs");
    let bound = bound_for(&d, branch.as_deref())?;
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
    // a branch's own Stripe webhook endpoint, made anew (decision 58): its
    // secret is a Worker secret, as the test secret is
    let webhook = match (makes_endpoint(&d, branch.as_deref()), &d.stripe) {
        (true, Some(st)) => {
            let key = read_secret(st.key_file.as_deref().expect("makes_endpoint checked it"))?;
            let s = crate::stripe::Stripe::new(st.api.as_deref().unwrap_or("https://api.stripe.com"), &key)?;
            let secret = crate::stripe::branch_endpoint(&s, &platform_url)?;
            println!("stripe: made {}'s own webhook endpoint (its secret a Worker secret)", crate::stripe::endpoint_url(&platform_url));
            Some(secret)
        }
        _ => None,
    };
    // a preview's levers (cell/src/levers.rs) and its Stripe endpoint's
    // secret, a branch's alone (checked above), are the Worker secrets left
    let mut worker_secrets = serde_json::Map::new();
    if let Some(secret) = &test_secret {
        assert!(n.label_suffix.is_some(), "only a branch deployment takes a test secret");
        worker_secrets.insert("FRAGMENT_TEST_SECRET".into(), json!(secret));
    }
    if let Some(secret) = &webhook {
        assert!(n.label_suffix.is_some(), "only a branch deployment makes its endpoint");
        worker_secrets.insert("FRAGMENT_STRIPE_WEBHOOK".into(), json!(secret));
    }
    let test_secrets = match worker_secrets.is_empty() {
        false => {
            let file = SecretFile::write(dir.join("cell-secrets.json"), &Value::Object(worker_secrets))?;
            deploy_cell.arg("--secrets-file").arg(&file.0);
            Some(file)
        }
        true => None,
    };
    crate::run(&mut deploy_cell)?;
    drop(test_secrets);
    println!("deployed {deploy_id}:");
    println!("  the platform  {platform_url}/  (sign-in redirect: {platform_url}/auth/callback)");
    if test_secret.is_some() {
        println!("  test levers   on (test_secret_file): cargo xtask e2e --hosted --config <this config> --branch {}", branch.as_deref().unwrap_or(""));
    }
    println!("  fragments     https://<name>{}.{}/", n.label_suffix.as_deref().unwrap_or(""), n.suffix);
    println!("  check         curl -sI {platform_url}/healthz | grep x-fragment-deploy");
    Ok(())
}

/// The platform Worker's config, rendered from the deployment's and the
/// checked-in one (`root`'s `cell/`), its store secrets bound by name in
/// the store `store_id`. No value of a secret is in it.
fn worker_config(d: &Deployment, n: &Names, store_id: &str, deploy_id: &str, root: &Path) -> Result<Value> {
    let bound = bound_for(d, n.label_suffix.as_deref())?;
    let (catalog, _) = catalog_of(d)?;
    let platform_url = format!("https://{}", n.platform_host);
    let mut cell = devstack::read_config(&root.join("cell"))?;
    let c = cell.as_object_mut().context("cell/wrangler.jsonc is an object")?;
    c.remove("build");
    c.insert("name".into(), json!(n.cell));
    c.insert("main".into(), json!(root.join("cell/entry.mjs")));
    // the release's files the build wrote (build.rs), uploaded with the
    // Worker: from the config written under target/deploy, by its path
    let assets = c.get_mut("assets").and_then(Value::as_object_mut).context("cell/wrangler.jsonc names the cell's Static Assets")?;
    let dir = assets.get("directory").and_then(Value::as_str).context("the cell's Static Assets name their directory")?;
    let dir = root.join("cell").join(dir);
    assets.insert("directory".into(), json!(dir));
    c.insert("workers_dev".into(), json!(false));
    c.insert("preview_urls".into(), json!(false));
    c.insert("observability".into(), json!({ "enabled": true }));
    c.insert("routes".into(), json!(n.routes.iter().map(|r| json!({ "pattern": r.pattern, "zone_name": r.zone })).collect::<Vec<_>>()));
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
    // the deployment's own images in place of the e2e's stubs, or none: one
    // built from the tree, or kept at its digest
    match &d.computers {
        Some(computers) => {
            let container = &mut c["containers"][0];
            let mut images = serde_json::Map::new();
            for (name, image) in &computers.images {
                let mut i = json!(image);
                if let Image::Built(b) = image {
                    i["dockerfile"] = json!(root.join(&b.dockerfile));
                    i["build_context"] = json!(root.join(&b.build_context));
                }
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
    if let Some(f) = &d.mail_from {
        v.insert("FRAGMENT_MAIL_FROM".into(), json!(f.trim()));
    }
    if let Some(s) = &d.stripe {
        if let Some(p) = &s.portal {
            v.insert("FRAGMENT_STRIPE_PORTAL".into(), json!(p));
        }
        if s.tax == Some(false) {
            v.insert("FRAGMENT_STRIPE_TAX".into(), json!("off"));
        }
    }
    if let (Some(models), _) = models_of(d)? {
        v.insert("FRAGMENT_MODELS".into(), Value::String(models.to_string()));
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
/// <n>] [--candidate <image>]`: the suite's
/// own arguments for that branch deployment, from its config: the zone, the
/// test secret's file (named, never read here), and what the deployment
/// offers (computers, models). A deployment of its own is refused: the
/// hosted lane runs on a preview. A candidate is one of the config's
/// images other than its default (docs/computers.md, "Stable and canary").
pub fn hosted_e2e_args(rest: &[String]) -> Result<Vec<String>> {
    let usage = || {
        anyhow::anyhow!(
            "usage: cargo xtask e2e --hosted --config <file> --branch <name> [--only <section>[,...] | --except <section>[,...]] [--dry-run | --sweep [<run>] | --sweep-all] [--max-paid-calls <n>] [--operator-key-file <file>] [--candidate <image>]"
        )
    };
    let (mut config, mut branch, mut passed, mut operator_key_file, mut candidate) = (None, None, vec![], None, None);
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
            "--candidate" => candidate = Some(it.next().ok_or_else(usage)?.clone()),
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
    let n = names(&d, Some(&branch)).context("the hosted lane runs on a branch deployment (a preview)")?;
    // a branch's suffix is its zone
    let mut args = vec!["--hosted".to_string(), "--zone".into(), n.suffix, "--branch".into(), branch.clone()];
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
    // a round's candidate: an image of the config's that new computers do
    // not start on, or there is nothing to try
    if let Some(image) = candidate {
        let c = d.computers.as_ref().context("--candidate names one of the deployment's computer images: its config has no `computers`")?;
        anyhow::ensure!(c.images.contains_key(&image), "--candidate {image:?} is not one of the config's images ({})", c.images.keys().cloned().collect::<Vec<_>>().join(", "));
        anyhow::ensure!(image != c.default_image, "--candidate {image:?} is the default image, which the hosted lane runs anyway: name the canary");
        args.extend(["--candidate".to_string(), image]);
    }
    args.extend(passed);
    Ok(args)
}

/// Removes a branch deployment: its Worker (its Durable Objects and
/// data with them), its Workflow and its queues. Its bucket is emptied by
/// the deployment's own blob grace and left: R2 refuses to delete a bucket
/// with objects in it. Irreversible: ask before running it.
pub fn teardown(rest: &[String]) -> Result<()> {
    let Args { config, branch, pinned_only } = args(rest)?;
    anyhow::ensure!(!pinned_only, "--pinned-only is a deploy's: a teardown builds nothing");
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

    /// A branch config (on the zone finite.place) with neither host, else a
    /// deployment of its own's (no zone).
    fn deployment(platform: Option<&str>, suffix: Option<&str>) -> Deployment {
        Deployment {
            account_id: "acct".into(),
            zone: (platform.is_none() && suffix.is_none()).then(|| "finite.place".into()),
            platform_host: platform.map(str::to_string),
            fragment_suffix: suffix.map(str::to_string),
            dns_token_file: None,
            host_secret: "fragment-host-secret".into(),
            host_secret_previous: None,
            codestorage: CodeStorage { org: "o".into(), private_key: "fragment-codestorage-private-key".into(), api: None },
            workos: WorkOs { client_id: "fragment-workos-client-id".into(), api_key: "fragment-workos-api-key".into() },
            stripe: None,
            operators: vec![],
            ai_gateway: None,
            models: None,
            default_plan: None,
            support_url: None,
            mail_from: None,
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
        // a round's candidate: one of the config's images, not its default
        let candidate = |v: &Value, test: &str, image: &str| {
            let path = config_file(test, v);
            args(&["--hosted", "--config", path.to_str().unwrap(), "--branch", "p5", "--candidate", image, "--only", "agent-restart"])
        };
        assert!(candidate(&v, "hosted-args-candidate-no-computers", "hermes-canary").is_err(), "a deployment that makes no computers");
        let build = json!({ "dockerfile": "images/hermes/Dockerfile", "build_context": "." });
        v["computers"] = json!({ "default_image": "hermes", "images": { "hermes": build, "hermes-canary": build } });
        assert_eq!(candidate(&v, "hosted-args-candidate", "hermes-canary").unwrap(), with(&["--offers", "computers", "--candidate", "hermes-canary", "--only", "agent-restart"]));
        assert!(candidate(&v, "hosted-args-candidate-default", "hermes").is_err(), "the default is what runs anyway");
        assert!(candidate(&v, "hosted-args-candidate-unknown", "stub-next").is_err(), "an image the config does not name");
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
        // both routes in the one zone, as the Worker's config renders them
        let cell = worker_config(&deployment(None, None), &n, "0f0e0d0c", "abc123", &devstack::repo_root()).unwrap();
        assert_eq!(cell["routes"], json!([{ "pattern": "dev.finite.place/*", "zone_name": "finite.place" }, { "pattern": "*--dev.finite.place/*", "zone_name": "finite.place" }]));
        // one wildcard in the zone covers the platform and every fragment
        assert_eq!(n.dns, [crate::dns::Wanted { zone: "finite.place".into(), name: "*.finite.place".into() }]);
        assert_eq!((n.label_suffix.as_deref(), n.repo_prefix.as_deref()), (Some("--dev"), Some("dev--")));
        for bad in ["", "Dev", "a--b", "-a", "a-", "a.b", "seventeen-letters"] {
            assert!(names(&deployment(None, None), Some(bad)).is_err(), "{bad:?}");
        }
        // a branch is at <branch>.<zone>: a config without a zone has none
        let mut zoneless = deployment(None, None);
        zoneless.zone = None;
        let refused = names(&zoneless, Some("dev")).err().map(|e| e.to_string()).unwrap_or_default();
        assert!(refused.contains("names its zone"), "{refused}");
    }

    /// A deployment of its own is on two zones: the platform's route and
    /// record in the platform's, the fragments' in the suffix's. It names
    /// no zone of its own, takes no branch, and names both hosts.
    #[test]
    fn a_deployment_of_its_own_routes_each_host_in_its_zone() {
        let d = deployment(Some("finite.chat"), Some("finite.place"));
        let n = names(&d, None).unwrap();
        assert_eq!((n.platform_host.as_str(), n.suffix.as_str()), ("finite.chat", "finite.place"));
        let cell = worker_config(&d, &n, "0f0e0d0c", "abc123", &devstack::repo_root()).unwrap();
        assert_eq!(cell["routes"], json!([{ "pattern": "finite.chat/*", "zone_name": "finite.chat" }, { "pattern": "*.finite.place/*", "zone_name": "finite.place" }]));
        assert_eq!((cell["vars"]["FRAGMENT_PLATFORM_URL"].as_str(), cell["vars"]["FRAGMENT_HOST_SUFFIX"].as_str()), (Some("https://finite.chat"), Some("finite.place")));
        assert!(cell["vars"].get("FRAGMENT_HOST_LABEL_SUFFIX").is_none() && cell["vars"].get("CODESTORAGE_REPO_PREFIX").is_none());
        assert_eq!(
            n.dns,
            [
                crate::dns::Wanted { zone: "finite.chat".into(), name: "finite.chat".into() },
                crate::dns::Wanted { zone: "finite.place".into(), name: "*.finite.place".into() },
            ]
        );
        assert_eq!((n.label_suffix, n.repo_prefix), (None, None));
        // one zone for both: each route in it
        let one = deployment(Some("example.dev"), Some("example.dev"));
        let n = names(&one, None).unwrap();
        assert_eq!(n.routes, [Route { pattern: "example.dev/*".into(), zone: "example.dev".into() }, Route { pattern: "*.example.dev/*".into(), zone: "example.dev".into() }]);
        // a zone beside its hosts is a branch config's: refused, never ignored
        let mut zoned = deployment(Some("finite.chat"), Some("finite.place"));
        zoned.zone = Some("finite.place".into());
        let refused = names(&zoned, None).err().map(|e| e.to_string()).unwrap_or_default();
        assert!(refused.contains("zone (finite.place) is a branch deployment's"), "{refused}");
        assert!(names(&d, Some("b")).is_err());
        assert!(names(&deployment(None, None), None).is_err());
        // one host alone is neither kind, with a branch or without
        for half in [deployment(Some("finite.chat"), None), deployment(None, Some("finite.place"))] {
            assert!(names(&half, None).err().is_some_and(|e| e.to_string().contains("both platform_host and fragment_suffix")));
            assert!(names(&half, Some("b")).err().is_some_and(|e| e.to_string().contains("no --branch")));
        }
    }

    /// The example and the hosted e2e's configs parse and check, each with
    /// the platform's catalog: Google, the four operator keys at their
    /// list prices, each key's store secret named, and the person's own
    /// OpenRouter, signed in to, with models; and every name they give
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
            assert_eq!(names, ["x", "firecrawl", "fal", "browser-use", "google", "perplexity", "google-places", "xai", "elevenlabs", "openrouter"], "{file}");
            let own = catalog.get("openrouter").unwrap();
            assert!(own.oauth.is_some() && own.models.as_ref().is_some_and(|m| m.offer.len() == 6), "{file}: OpenRouter is signed in to, and offers its models");
            assert_eq!(keys.len(), 8, "{file}: each operator key's store secret");
            assert!(catalog.key_prices().iter().all(|k| fragment_core::price::default_key_price(&k.key) == Some((k.micros, k.per))), "{file}: at list");
            // the example's tiers through OpenRouter (decision 61); a preview's on Workers AI
            let slots: &[&str] = if file == "deploy/example.jsonc" { &["cheap", "medium"] } else { &[] };
            let conventional = devstack::store::Bound::conventional(true, false, &["x", "firecrawl", "fal", "browser-use", "perplexity", "google-places", "xai", "elevenlabs"], slots);
            assert_eq!(bound(&d).unwrap(), conventional, "{file}: the names dev and the e2e bind");
        }
    }

    /// A deployment that sells seats binds Stripe's key and its endpoint's
    /// secret from the store; a branch whose deploy makes its own endpoint
    /// (it names the key's file) binds the key alone: its endpoint's secret
    /// is a Worker secret (decision 58).
    #[test]
    fn a_branch_that_makes_its_endpoint_binds_no_store_secret_for_it() {
        let mut d = deployment(None, None);
        d.stripe = Some(StripeDeploy { key: "fragment-stripe-key".into(), webhook_secret: "fragment-stripe-webhook-secret".into(), portal: None, tax: None, key_file: None, api: None });
        let named = |b: &devstack::store::Bound| named_secrets(b).into_iter().map(|(_, n)| n.to_string()).collect::<Vec<_>>();
        assert!(named(&bound_for(&d, Some("p5")).unwrap()).contains(&"fragment-stripe-webhook-secret".to_string()), "no key file: the store's");
        d.stripe.as_mut().unwrap().key_file = Some("/run/secrets/stripe".into());
        let production = named(&bound_for(&d, None).unwrap());
        let branch = named(&bound_for(&d, Some("p5")).unwrap());
        assert!(production.contains(&"fragment-stripe-webhook-secret".to_string()) && production.contains(&"fragment-stripe-key".to_string()));
        assert!(!branch.contains(&"fragment-stripe-webhook-secret".to_string()) && branch.contains(&"fragment-stripe-key".to_string()));
        assert!(makes_endpoint(&d, Some("p5")) && !makes_endpoint(&d, None));
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
                ("OPERATOR_KEY_X", "fragment-x-api-key"),
                ("OPERATOR_KEY_FIRECRAWL", "fragment-firecrawl-api-key"),
                ("OPERATOR_KEY_FAL", "fragment-fal-api-key"),
                ("OPERATOR_KEY_BROWSER_USE", "fragment-browser-use-api-key"),
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

    /// Goal: a deploy, a branch's too, uploads the release's files with the
    /// Worker, and serves none of them but through it. Method: the config it
    /// writes (under target/deploy, so paths are absolute) names the
    /// directory the build writes, bound as ASSETS, every request the
    /// Worker's first.
    #[test]
    fn the_deploy_uploads_the_release_files() {
        let own = deployment(Some("fragment.club"), Some("fragment.boats"));
        for (d, branch) in [(&own, None), (&deployment(None, None), Some("p5"))] {
            let cell = worker_config(d, &names(d, branch).unwrap(), "0f0e0d0c", "abc123", &devstack::repo_root()).unwrap();
            let assets = &cell["assets"];
            assert_eq!(assets["directory"], json!(devstack::repo_root().join("cell/build/assets")), "{branch:?}");
            assert_eq!((&assets["binding"], &assets["run_worker_first"]), (&json!("ASSETS"), &json!(true)), "{branch:?}");
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

    /// Stable kept at its digest beside a canary built from the tree
    /// (docs/computers.md, "Stable and canary"): the pinned image is given
    /// to wrangler as it is, nothing to build, and the built one with its
    /// paths made the repo's; a pinned image that is not a digest in this
    /// account's registry is refused before anything is built, and so is an
    /// entry that is both or neither.
    #[test]
    fn a_stable_image_is_kept_at_its_digest() {
        let text = fs::read_to_string(devstack::repo_root().join("deploy/e2e.jsonc")).unwrap();
        let mut v: Value = serde_json::from_str(&devstack::strip_comments(&text)).unwrap();
        let account = v["account_id"].as_str().unwrap().to_string();
        let digest = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08";
        let stable = format!("registry.cloudflare.com/{account}/fragment-preview-hermes@sha256:{digest}");
        let built = json!({ "dockerfile": "images/hermes/Dockerfile", "build_context": "." });
        v["computers"]["images"] = json!({ "hermes": { "image": stable }, "hermes-canary": built });
        let d = load(&config_file("pinned-stable", &v)).unwrap();
        let cell = worker_config(&d, &names(&d, Some("preview")).unwrap(), "0f0e0d0c", "abc123", &devstack::repo_root()).unwrap();
        let images = &cell["containers"][0]["images"];
        assert_eq!(images["hermes"], json!({ "image": stable }), "kept as it is: nothing to build");
        assert_eq!(images["hermes-canary"]["dockerfile"], json!(devstack::repo_root().join("images/hermes/Dockerfile")));
        assert_eq!(images["hermes-canary"]["build_context"], json!(devstack::repo_root().join(".")));
        assert_eq!(cell["vars"]["FRAGMENT_COMPUTER_IMAGE"], "hermes", "new computers start on stable");

        let refused = |hermes: Value, test: &str| {
            let mut v = v.clone();
            v["computers"]["images"]["hermes"] = hermes;
            load(&config_file(test, &v)).err().map(|e| format!("{e:#}")).unwrap_or_default()
        };
        for (bad, test) in [
            (format!("registry.cloudflare.com/{account}/fragment-preview-hermes:latest"), "pinned-tag"),
            (format!("registry.cloudflare.com/ffffffffffffffffffffffffffffffff/fragment-preview-hermes@sha256:{digest}"), "pinned-other-account"),
            (format!("docker.io/{account}/fragment-preview-hermes@sha256:{digest}"), "pinned-other-registry"),
            (format!("registry.cloudflare.com/{account}/fragment-preview-hermes@sha256:{}", &digest[..12]), "pinned-short"),
            (format!("registry.cloudflare.com/{account}/fragment-preview-hermes@sha256:{}", digest.to_uppercase()), "pinned-upper"),
            (format!("registry.cloudflare.com/{account}/Hermes@sha256:{digest}"), "pinned-repo-case"),
            (format!("registry.cloudflare.com/{account}/@sha256:{digest}"), "pinned-no-repo"),
        ] {
            let why = refused(json!({ "image": bad }), test);
            assert!(why.contains("computers: the image \"hermes\" is") && why.contains("not one pinned by digest in this account's registry"), "{bad}: {why}");
        }
        let both = refused(json!({ "image": stable, "dockerfile": "images/hermes/Dockerfile", "build_context": "." }), "pinned-and-built");
        assert!(both.contains("parse"), "both: {both}");
        assert!(refused(json!({}), "pinned-neither").contains("parse"), "neither");
        assert!(pinned_ok(&format!("registry.cloudflare.com/{account}/a/b.c_d-e@sha256:{digest}"), &account));
        assert!(!pinned_ok(&stable, ""), "no account");
    }

    /// Goal: CI's deploy (`--pinned-only`) builds no image. Method: the
    /// deploy itself, on a config whose DNS token's file is not there, so
    /// that a deploy that gets past its first checks stops at reading it.
    /// Invalid: a canary built from the tree beside a pinned stable, refused
    /// naming it before that read (and deployed by hand without the flag);
    /// the flag on a teardown. Valid: every image at its digest, or no
    /// computers.
    #[test]
    fn a_pinned_only_deploy_refuses_an_image_it_would_build() {
        let text = fs::read_to_string(devstack::repo_root().join("deploy/e2e.jsonc")).unwrap();
        let mut v: Value = serde_json::from_str(&devstack::strip_comments(&text)).unwrap();
        let account = v["account_id"].as_str().unwrap().to_string();
        let stable = json!({ "image": format!("registry.cloudflare.com/{account}/fragment-preview-hermes@sha256:{}", "9f".repeat(32)) });
        let built = json!({ "dockerfile": "images/hermes/Dockerfile", "build_context": "." });
        v["dns_token_file"] = json!(devstack::repo_root().join("target/xtask-tests/pinned-only/no-dns-token"));
        let deploy_with = |v: &Value, test: &str, more: &[&str]| {
            let path = config_file(test, v);
            let rest: Vec<String> = [&["--config", path.to_str().unwrap(), "--branch", "preview"][..], more].concat().iter().map(|s| s.to_string()).collect();
            deploy(&rest).err().map(|e| format!("{e:#}")).unwrap_or_default()
        };
        let read_the_token = "read the secret file";

        v["computers"]["images"] = json!({ "hermes": stable, "hermes-canary": built });
        let refused = deploy_with(&v, "pinned-only-canary-built", &["--pinned-only"]);
        assert!(refused.contains("--pinned-only builds no image, and this config builds hermes-canary from the tree") && !refused.contains(read_the_token), "{refused}");
        assert!(deploy_with(&v, "pinned-only-by-hand", &[]).contains(read_the_token), "without the flag it goes on");

        v["computers"]["images"] = json!({ "hermes": stable, "hermes-canary": stable });
        assert!(deploy_with(&v, "pinned-only-pinned", &["--pinned-only"]).contains(read_the_token), "every image at its digest");
        v.as_object_mut().unwrap().remove("computers");
        assert!(deploy_with(&v, "pinned-only-no-computers", &["--pinned-only"]).contains(read_the_token), "no computers, no image");

        let path = config_file("pinned-only-teardown", &v);
        let refused = teardown(&["--config", path.to_str().unwrap(), "--branch", "preview", "--pinned-only"].map(String::from)).err().map(|e| e.to_string()).unwrap_or_default();
        assert!(refused.contains("--pinned-only is a deploy's"), "{refused}");
    }

    /// The deployment's mail: its `mail_from` is the cell's
    /// `FRAGMENT_MAIL_FROM`, beside the Email Sending binding the cell's
    /// config declares; without one, the cell sends no mail.
    #[test]
    fn the_mail_comes_from_the_config_s_address() {
        let text = fs::read_to_string(devstack::repo_root().join("deploy/e2e.jsonc")).unwrap();
        let mut v: Value = serde_json::from_str(&devstack::strip_comments(&text)).unwrap();
        let rendered = |v: &Value, test: &str| {
            let d = load(&config_file(test, v)).unwrap();
            worker_config(&d, &names(&d, Some("p5")).unwrap(), "0f0e0d0c", "abc123", &devstack::repo_root()).unwrap()
        };
        v.as_object_mut().unwrap().remove("mail_from");
        let none = rendered(&v, "mail-none");
        assert!(none["vars"].get("FRAGMENT_MAIL_FROM").is_none());
        v["mail_from"] = json!(" fragment <mail@finite.place> ");
        let cell = rendered(&v, "mail-from");
        assert_eq!(cell["vars"]["FRAGMENT_MAIL_FROM"], "fragment <mail@finite.place>");
        assert_eq!(cell["send_email"], json!([{ "name": "EMAIL" }]));
        v["mail_from"] = json!("mail@finite.place");
        assert_eq!(rendered(&v, "mail-bare")["vars"]["FRAGMENT_MAIL_FROM"], "mail@finite.place");
        for from in ["", "not an address", "Fragment <mail@finite.place", "mail@finite.place\n", "\"Fragment\" <mail@finite.place>"] {
            v["mail_from"] = json!(from);
            let why = format!("{:#}", load(&config_file("mail-invalid", &v)).err().unwrap());
            assert!(why.contains("mail_from:"), "{why}");
        }
    }

    /// Finite's own deployment's `models` (deploy/example.jsonc; decision
    /// 61): its tiers on GLM-5.3 Flash through OpenRouter, each naming its
    /// key, which is bound for each slot (one secret, two bindings) and is
    /// nowhere in the cell's variable, and its provider routing, which is,
    /// under the ceiling the price book charges; its fallback and vision on
    /// Workers AI's GLM-5.3 Flash. A preview's (deploy/e2e.jsonc) names
    /// none: its tiers are on Workers AI, and it needs no OpenRouter key to
    /// deploy.
    #[test]
    fn a_deployments_models_bind_their_keys_and_render_without_them() {
        let rendered = |file: &str| {
            let d = load(&devstack::repo_root().join(file)).unwrap();
            let n = names(&d, Some("p5")).unwrap();
            (worker_config(&d, &n, "0f0e0d0c", "abc123", &devstack::repo_root()).unwrap(), bound(&d).unwrap())
        };
        let (cell, b) = rendered("deploy/example.jsonc");
        let models: Value = serde_json::from_str(cell["vars"]["FRAGMENT_MODELS"].as_str().unwrap()).unwrap();
        let routing = json!({ "provider": { "zdr": true, "sort": "throughput", "require_parameters": true, "quantizations": ["fp8", "bf16"], "max_price": { "prompt": 0.15, "completion": 0.5 } } });
        let routed = json!({ "provider": "openai", "base_url": "https://openrouter.ai/api/v1", "model": "z-ai/glm-5.3-flash", "body": routing });
        let flash = json!({ "provider": "workers_ai", "model": "@cf/zai-org/glm-5.3-flash" });
        assert_eq!(models, json!({ "cheap": routed, "medium": routed, "fallback": flash, "vision": flash }), "no key in the cell's variable, the routing in it");
        let parsed = fragment_core::models::Models::parse(Some(&models.to_string()), &fragment_core::price::PriceBook::defaults(), false).unwrap();
        assert_eq!(parsed.cheap.model(), fragment_core::models::OPENROUTER_FLASH);
        // the ceiling OpenRouter routes under is the one the price book charges, with OpenRouter's fee
        use fragment_core::price::{BP, CREDITS_FEE_BP_DEFAULT, OPENROUTER_FEE_BP};
        let row = fragment_core::price::PriceBook::defaults().models.into_iter().find(|m| m.model == fragment_core::models::OPENROUTER_FLASH).unwrap().price;
        let ceiling = |per_million: &Value| (per_million.as_f64().unwrap() * 1e6).round() as i64 * i64::from(BP + OPENROUTER_FEE_BP) / i64::from(BP + CREDITS_FEE_BP_DEFAULT);
        let max = &routing["provider"]["max_price"];
        assert!(row.input >= ceiling(&max["prompt"]) && row.cached_input >= row.input && row.cache_write >= row.input && row.output >= ceiling(&max["completion"]), "{row:?}");
        let bindings: Vec<(&str, &str)> = cell["secrets_store_secrets"].as_array().unwrap().iter().map(|b| (b["binding"].as_str().unwrap(), b["secret_name"].as_str().unwrap())).collect();
        assert!(bindings.contains(&("MODEL_KEY_CHEAP", "fragment-openrouter-key")) && bindings.contains(&("MODEL_KEY_MEDIUM", "fragment-openrouter-key")), "{bindings:?}");
        assert!(!bindings.iter().any(|(binding, _)| *binding == "MODEL_KEY_FALLBACK" || *binding == "MODEL_KEY_VISION"), "Workers AI needs no key");
        assert_eq!(b.model_keys, [("cheap".to_string(), "fragment-openrouter-key".to_string()), ("medium".into(), "fragment-openrouter-key".into())]);
        // a deploy without the key is refused, naming it once, with both slots
        let named = named_secrets(&b);
        assert_eq!(named.iter().filter(|(_, n)| *n == "fragment-openrouter-key").map(|(f, _)| f.as_str()).collect::<Vec<_>>(), ["models.cheap.key, models.medium.key"]);
        let all: Vec<&str> = named.iter().map(|(_, n)| *n).filter(|n| *n != "fragment-openrouter-key").collect();
        let listed: Vec<devstack::store::Listed> = all.iter().map(|n| devstack::store::Listed { name: n.to_string(), id: "0".into(), created: String::new(), modified: String::new() }).collect();
        let missing = devstack::store::missing(&named, &listed);
        let said = refusal(Path::new("/home/p/prod.jsonc"), Some(&devstack::store::AccountStore { name: "fragment".into(), id: "0f0e".into() }), &missing);
        assert!(said.contains("lacks 1 of the secrets") && said.contains("models.cheap.key, models.medium.key  cargo xtask secret set fragment-openrouter-key --config /home/p/prod.jsonc"), "{said}");

        // the preview's: every tier on Workers AI, no key named or bound
        let (cell, b) = rendered("deploy/e2e.jsonc");
        assert!(cell["vars"].get("FRAGMENT_MODELS").is_none(), "unnamed: the cell's defaults, every slot on Workers AI");
        assert!(b.model_keys.is_empty() && !cell.to_string().contains("MODEL_KEY_"));
    }

    /// Goal: a deployment's `models` the cell would refuse, or whose key the
    /// deploy could not bind, is refused before anything is read or made,
    /// saying which slot and why; the fields it replaced are refused, naming
    /// it. Invalid: an unknown provider, an OpenAI-compatible API without a
    /// key's name (or with one that is no string, or no store name), a key
    /// on a Workers AI slot, a bad base URL, an unknown slot, an unpriced
    /// model, `models` no object, and `vision_model` or `fallback_model`.
    /// Valid: a slot alone, rendered with the rest the cell's.
    #[test]
    fn a_deployments_models_out_of_shape_are_refused() {
        let with = |models: Value| {
            let mut d = deployment(None, None);
            d.models = Some(models);
            checked(d).err().map(|e| format!("{e:#}")).unwrap_or_default()
        };
        let row = |extra: Value| {
            let mut r = json!({ "provider": "openai", "base_url": "https://finite-private.finite.containers.tinfoil.dev/v1", "key": "fragment-finite-private-key", "model": "glm-5-3-flash" });
            r.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
            json!({ "cheap": r })
        };
        assert_eq!(with(row(json!({}))), "", "Finite Private's, as example.jsonc names it");
        let refused = |models: Value, says: &str| {
            let e = with(models.clone());
            assert!(e.contains(says), "{models}: {e}");
        };
        refused(row(json!({ "provider": "anthropic" })), "models.cheap: its provider is workers_ai (a Workers AI model, through the gateway) or openai (an OpenAI-compatible API), not \"anthropic\"");
        refused(json!({ "cheap": { "model": "glm-5-3-flash" } }), "models.cheap: its provider is workers_ai");
        refused(json!({ "cheap": { "provider": "openai", "base_url": "https://a.example/v1", "model": "glm-5-3-flash" } }), "models.cheap: an OpenAI-compatible API names its key");
        refused(row(json!({ "key": 7 })), "models.cheap: its key is its secret's name in the store, a string");
        refused(row(json!({ "key": "~/.config/fragment/secrets/finite-private" })), "models.cheap.key is \"~/.config/fragment/secrets/finite-private\", which is no Secrets Store secret's name");
        refused(json!({ "vision": { "provider": "workers_ai", "model": "@cf/zai-org/glm-5.3-flash", "key": "fragment-finite-private-key" } }), "models.vision: only an OpenAI-compatible API (provider openai) names a key");
        refused(row(json!({ "base_url": "http://finite-private.finite.containers.tinfoil.dev/v1" })), "models.cheap: base_url:");
        refused(row(json!({ "base_url": "http://127.0.0.1:4810/v1" })), "models.cheap: base_url:");
        refused(row(json!({ "base_url": "https://finite-private.finite.containers.tinfoil.dev/v1/" })), "without a query or a trailing /");
        refused(row(json!({ "model": "glm-6" })), "models.cheap: the model \"glm-6\" is not in the price book");
        refused(json!({ "high": { "provider": "workers_ai", "model": "@cf/zai-org/glm-5.3" } }), "unknown field `high`");
        refused(json!([]), "models is an object");
        // a body: the API's own fields, rendered as named; one the cell refuses, refused here
        refused(row(json!({ "body": "zdr" })), "body is a JSON object of the fields every call to this API carries");
        refused(row(json!({ "body": { "max_tokens": 64 } })), "models.cheap: body names max_tokens, which the route writes itself");
        refused(row(json!({ "body": { "x": "a".repeat(4096) } })), "models.cheap: body is 4104 bytes of JSON, past 4096");
        let big = row(json!({ "body": { "x": "a".repeat(2500) } }))["cheap"].clone();
        refused(json!({ "cheap": big, "medium": big }), "past a Workers variable's 5000: shorten the slots' bodies");
        assert_eq!(with(row(json!({ "body": { "provider": { "zdr": true } } }))), "", "a body within its bounds");
        // a slot alone: rendered as named, the rest the cell's
        let text = fs::read_to_string(devstack::repo_root().join("deploy/e2e.jsonc")).unwrap();
        let mut v: Value = serde_json::from_str(&devstack::strip_comments(&text)).unwrap();
        v["models"] = json!({ "fallback": { "provider": "workers_ai", "model": "@cf/zai-org/glm-5.3" } });
        let d = load(&config_file("models-fallback", &v)).unwrap();
        let n = names(&d, Some("p5")).unwrap();
        let vars = worker_config(&d, &n, "0f0e0d0c", "abc123", &devstack::repo_root()).unwrap()["vars"].clone();
        assert_eq!(vars["FRAGMENT_MODELS"], json!({ "fallback": { "provider": "workers_ai", "model": "@cf/zai-org/glm-5.3" } }).to_string());
        // the fields it replaced
        for (field, now) in [("vision_model", "models.vision"), ("fallback_model", "models.fallback")] {
            let mut v = v.clone();
            v[field] = json!("@cf/zai-org/glm-5.3-flash");
            let refused = load(&config_file(field, &v)).err().map(|e| format!("{e:#}")).unwrap_or_default();
            assert!(refused.contains(&format!("{field} is gone; name it as {now}")), "{refused}");
        }
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
