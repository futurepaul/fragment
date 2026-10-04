//! The fragment end-to-end suite: the real cell and agents' Worker under
//! `wrangler dev` (workerd, from staged copies under `target/e2e/<run>`, so
//! a running `xtask dev` is never touched), the code.storage fake from
//! `crates/fakes` (webhooks included), the real CLI, and signed HTTP the
//! way the CLI and a browser send it. The fakes stand only at vendor
//! boundaries (code.storage, WorkOS, the model, a push service): this is
//! the lower rung under the hosted lane.
//!
//! `cargo xtask e2e [--only <section>[,<section>...] | --except <section>[,...]]`.
//! Each section makes its own fragments, so any set can run alone. Every check prints `ok` or
//! `FAIL`, and a section that stops early is one FAIL, with the sections
//! after it still run; the process exits non-zero when any check fails.
//! A run's scratch (`target/e2e/<run>`: the staged cell, each node boot's
//! log, the lanes' directories and screenshots) is
//! removed when every check passes, and kept when one fails (or when
//! `FRAGMENT_E2E_KEEP` is set).
//!
//! The hosted lane (`--hosted`, hosted.rs) runs the same sections against
//! a branch deployment on real vendors: each section says what it needs
//! (needs.rs), and one that needs what a preview lacks (a fake, the node,
//! the whole deployment, local Docker) is a skip that says why.
//!
//! A local run's git may live outside it (`FRAGMENT_E2E_CODESTORE`,
//! store.rs): a code store already running, or macrofiche started for the
//! run. The lanes then read it through its REST API, and a check that pulls
//! one of the code.storage fake's levers is a skip that says which.

mod api;
mod browser;
mod hosted;
mod lanes;
mod needs;
mod store;

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use base64::Engine;
use fragment_devstack as devstack;
use fragment_fakes::codestorage::{self as fake, CodeStorage};
use fragment_nip98::Keys;
use serde_json::{json, Value};

use api::Api;
use devstack::summary::{self, Shard};
pub use needs::Need;

pub const SUFFIX: &str = "fragment.localhost";
/// The fragments' suffix on a node shaped as fragment.club is since the
/// move to fragment.boats (`Shape::TwoSites`).
pub const BOATS: &str = "boats.localhost";
/// Where the next node puts the platform and the fragments, as a browser
/// tells sites apart: Chrome takes an unknown top-level domain's last label
/// as its suffix, so every `*.fragment.localhost` is one site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shape {
    /// The platform on 127.0.0.1, the fragments under `fragment.localhost`.
    Plain,
    /// As fragment.club is since the move (docs/fragment-boats.md): the
    /// platform at `fragment.localhost`, cross-site from the fragments
    /// (`<flat>.boats.localhost`, one site with each other), whose old
    /// hosts under `fragment.localhost` redirect to their new ones.
    TwoSites,
}
const ORG: &str = "fragment-e2e";
/// The poll backstop runs this often here (5 minutes in production).
pub const POLL_S: u32 = 2;
/// Blobs no branch names are kept this long here (7 days in production).
pub const BLOB_GRACE_S: u32 = 4;
/// The pending sign-ins this fleet keeps: small, so the signin lane fills
/// the table and proves the oldest goes first in a few hundred requests.
pub const SIGNINS_PENDING_MAX: u64 = 200;
/// A new person's plan here: a seat, so each starts the month with its
/// included credit (production's is `guest`).
pub const DEFAULT_PLAN: &str = "seat";
/// The WorkOS fake's environment.
const WORKOS_CLIENT: &str = "client_fragment_e2e";
const WORKOS_KEY: &str = "sk_test_fragment_e2e";
/// The OpenID Connect fake's client (`FRAGMENT_E2E_SIGNIN=oidc`).
pub const OIDC_CLIENT: &str = "fragment-e2e";
const OIDC_SECRET: &str = "oidc-secret-fragment-e2e";
/// The branch a rehearsal of the hosted lane shapes the local node as.
pub const REHEARSAL_BRANCH: &str = "rh";
/// What the fleet's computers may swap in (docs/computers.md): the
/// platform's own catalog, as the hosted e2e deploys it
/// (`deploy/e2e.jsonc`: Google, and the Perplexity, Google Places, xAI and
/// ElevenLabs operator keys, at their real hosts, which the swap sends to
/// the upstream fake), each key a test value; and an own key's provider
/// behind basic auth, the e2e's alone.
pub const SWAP_CONNECTION: &str = "google";
pub const SWAP_CONNECTION_HOST: &str = "www.googleapis.com";
pub const SWAP_CONNECTION_ENV: &str = "GOOGLE_OAUTH_ACCESS_TOKEN";
/// The operator keys, each its test value and its environment variable.
pub const SWAP_KEYS: [(&str, &str, &str); 4] = [
    ("perplexity", "pplx-e2e-7f3a9c", "PERPLEXITY_API_KEY"),
    ("google-places", "AIza-e2e-places-51b2", "GOOGLE_PLACES_API_KEY"),
    ("xai", "xai-e2e-0c4d22", "XAI_API_KEY"),
    ("elevenlabs", "sk_e2e_eleven_9e1f", "ELEVENLABS_API_KEY"),
];
pub const SWAP_OWN: &str = "e2e-mail";
pub const SWAP_OWN_HOST: &str = "api.mail.test";
pub const SWAP_OWN_ENV: &str = "E2E_MAIL_KEY";

/// The fleet's provider catalog: `deploy/e2e.jsonc`'s, its key files
/// dropped (the deploy's), and the own key's provider.
pub fn swap_providers() -> Result<Value> {
    let text = std::fs::read_to_string(devstack::repo_root().join("deploy/e2e.jsonc"))?;
    let config: Value = serde_json::from_str(&devstack::strip_comments(&text))?;
    let mut rows = config["providers"].as_array().cloned().context("deploy/e2e.jsonc names its providers")?;
    for r in &mut rows {
        r.as_object_mut().context("a provider is an object")?.remove("key_file");
    }
    rows.push(json!({ "name": SWAP_OWN, "kind": "own", "hosts": [SWAP_OWN_HOST], "placements": [{ "basic": "password" }], "env": [SWAP_OWN_ENV] }));
    fragment_core::catalog::Catalog::parse(&Value::Array(rows.clone()).to_string()).map_err(|e| anyhow!("the e2e's catalog: {e}"))?;
    Ok(Value::Array(rows))
}

/// A vendor fake, the lanes' on a local run only: a section that touches
/// one declares `Need::Fakes`, and the hosted lane skips it before it
/// could. A rehearsal's node still runs on the fakes (`node`), hidden from
/// the lanes as a preview's vendors are.
pub struct Fake<T> {
    what: &'static str,
    fake: Option<T>,
    /// The lanes may use it (a local run's).
    lanes: bool,
}

impl<T> Fake<T> {
    /// A local run's fake, or (`hidden`) a rehearsal's: the node's vendor,
    /// which no lane reaches.
    fn of(hidden: bool, what: &'static str, fake: T) -> Fake<T> {
        Fake { what, fake: Some(fake), lanes: !hidden }
    }

    fn absent(what: &'static str) -> Fake<T> {
        Fake { what, fake: None, lanes: false }
    }

    /// The fake as the node is configured with it.
    fn node(&self) -> &T {
        self.fake.as_ref().unwrap_or_else(|| panic!("a hosted run starts no node on the {} fake", self.what))
    }
}

impl<T> Deref for Fake<T> {
    type Target = T;

    fn deref(&self) -> &T {
        match (&self.fake, self.lanes) {
            (Some(fake), true) => fake,
            (_, false) if self.what == "code.storage" => {
                panic!("the code.storage fake is not the lanes' on this run (hosted, or an external code store): a section that uses it declares Need::Fakes, and a check that pulls one of its levers asks Suite::store_levers first")
            }
            _ => panic!("the {} fake on the hosted lane: a section that uses it declares Need::Fakes", self.what),
        }
    }
}

/// Where a repo's main stood, to count the commits it took after
/// (`Suite::commits_since`).
pub enum Mark {
    /// The commit packs the fake was sent for the repo.
    Packs(u32),
    /// main's head on an external store (`None`: no main yet).
    Head(Option<String>),
}

/// One section as a dry run plans it: what it needs, and why it would be
/// skipped (`None`: it would run).
pub struct Planned {
    pub section: String,
    pub needs: Vec<Need>,
    pub skip: Option<String>,
}

pub struct Suite {
    /// The sections to run (`None`: all of them), and those not to.
    only: Option<Vec<String>>,
    except: Vec<String>,
    /// The table's shard to run (`--shard k/n`; `None`: no split).
    shard: Option<Shard>,
    /// Where the run writes its summary as it ends (`--summary`).
    summary: Option<PathBuf>,
    /// Each section the run accounted for (ran, or skipped whole), with
    /// its checks: what the summary reports (lanes/mod.rs `run` counts them).
    accounted: Vec<summary::Section>,
    /// Checks made outside any section.
    outside: summary::Counts,
    /// Where the run is: under `wrangler dev` with the fakes, or a preview.
    rung: needs::Rung,
    /// The hosted lane's rules: on a preview, or its rehearsal on the local
    /// node (shaped as a branch deployment, its fakes hidden from the lanes).
    hosted_rules: bool,
    /// The preview a hosted run targets (`None`: the local node).
    preview: Option<api::Preview>,
    /// A dry run's plan (`Some`: nothing runs, each section is planned).
    plan: Option<Vec<Planned>>,
    /// What the run's APIs share: the levers' secret, and (hosted) its
    /// people and the paid calls it may lend.
    shared: Arc<api::Run>,
    /// Who made each fragment `create` made (hosted: commits and deploys
    /// go through the API as its owner, where locally a writer pushes to
    /// the code.storage fake).
    owners: RefCell<BTreeMap<String, Keys>>,
    /// Every section a lane asked for, run or not: a name given to
    /// `--only` or `--except` that is none of them fails the run.
    asked: Vec<String>,
    passed: usize,
    failed: Vec<String>,
    /// Checks this rung cannot make, each with why and where it is made.
    skipped: Vec<String>,
    /// The sections that ran, in order: the last is the one running.
    ran: Vec<String>,
    /// A lane stopped early: the next section to run gets the node back as
    /// the lanes expect it first.
    recovery_due: bool,
    /// Why the node could not be brought back: every later section is a FAIL.
    lost: Option<String>,
    /// The cron fragment, deployed sections before `triggers` checks its
    /// tick (lanes/mod.rs), or why it was not.
    cron: Option<Result<Option<lanes::jobs::Cron>, String>>,
    /// Chrome, started when a lane first asks for it and shared after.
    chrome: browser::Shared,
    /// The local node's tools (`None`: a hosted run, which starts no node).
    tools: Option<devstack::Tools>,
    node: Option<devstack::AnyNode>,
    /// celld's tools when the node runs on celld (`FRAGMENT_E2E_RUNTIME=celld`).
    celld: Option<devstack::celld::CelldTools>,
    port: u16,
    /// Distinguishes this run's fragment names from any earlier state.
    run: String,
    pub fake: Fake<CodeStorage>,
    /// The code store outside the run, when the node's git lives there
    /// (`None`: the fake, or a hosted run's own vendor).
    store: Option<store::External>,
    /// macrofiche, when the run started it: stopped at the run's end.
    macrofiche: Option<devstack::codestore::Macrofiche>,
    /// The code store's org: every token's `iss`.
    org: String,
    /// The model route's vendor boundary, text and images: Workers AI, scripted.
    pub ai: Fake<fragment_fakes::workers_ai::WorkersAi>,
    pub push: Fake<fragment_fakes::push::PushService>,
    org_key: String,
    host_secret: String,
    /// The node's test levers' secret (`FRAGMENT_TEST_SECRET`), made per run.
    test_secret: String,
    /// Sign-in's stand-in: people sign in through it (`Api::person`), and
    /// Pipes', which hands out connections' tokens.
    pub workos: Fake<fragment_fakes::workos::WorkOs>,
    /// An OpenID Connect provider: people sign in through it in WorkOS's
    /// place when the run says so (`oidc_signin`); WorkOS then serves Pipes.
    pub oidc: Fake<fragment_fakes::oidc::Oidc>,
    /// The provider APIs a computer's swap sends to.
    pub upstream: Fake<fragment_fakes::upstream::Upstream>,
    /// The fleet's operator (`FRAGMENT_OPERATORS`): a key a person approves
    /// when a lane needs it.
    pub operator: Keys,
    pub cli: PathBuf,
    pub scratch: PathBuf,
    /// The node's own copy of the cell project (never `cell/`, where `xtask dev` runs).
    project: PathBuf,
    /// The agents' script (phase 5), co-hosted on the node.
    agents_project: PathBuf,
    /// The next node's shape (`start_as_browsers_see_it`).
    shape: Shape,
}

impl Suite {
    /// The deployment's secrets (label, value): the platform Worker holds
    /// them, never an app. A local run's alone: a section that reads them
    /// declares `Need::Deployment`.
    pub fn deployment_secrets(&self) -> Vec<(&'static str, String)> {
        assert!(!self.hosted_rules, "the hosted lane holds none of the deployment's secrets: a section that reads them declares Need::Deployment");
        vec![
            ("host secret", self.host_secret.clone()),
            // a line of the PEM's body: found however the PEM was escaped
            ("code.storage key", self.org_key.lines().find(|l| !l.starts_with("-----") && !l.trim().is_empty()).unwrap_or_default().trim().to_string()),
            ("WorkOS API key", WORKOS_KEY.into()),
            ("OpenID Connect client secret", OIDC_SECRET.into()),
            ("test secret", self.test_secret.clone()),
        ]
    }

    /// Whether the run keeps the hosted lane's rules (on a preview, or its
    /// rehearsal): a section adapts a check, or skips it, where a real
    /// vendor or the deployment's own image answers.
    pub fn hosted(&self) -> bool {
        self.hosted_rules
    }

    /// Whether the node shoots preview cards: celld has no Browser
    /// Rendering, and a deployment without it takes no shots.
    pub fn shoots_cards(&self) -> bool {
        self.rung != needs::Rung::Celld
    }

    /// Whether people sign in through the OpenID Connect fake
    /// (`FRAGMENT_E2E_SIGNIN=oidc`: docs/self-host.md, seam 4), not WorkOS's.
    pub fn oidc_signin(&self) -> bool {
        std::env::var("FRAGMENT_E2E_SIGNIN").as_deref() == Ok("oidc")
    }

    /// Whether the text models go through an OpenAI-compatible server's
    /// route (`FRAGMENT_E2E_MODELS=openai`: docs/self-host.md, seam 3), not
    /// the binding's through the gateway.
    pub fn openai_models(&self) -> bool {
        std::env::var("FRAGMENT_E2E_MODELS").as_deref() == Ok("openai")
    }

    /// Whether a job's sleep outlives a crash of the node: celld's
    /// Workflows keep it (each instance a cell, its sleep an alarm); local
    /// workerd's hold it as a timer in the process.
    pub fn durable_workflows(&self) -> bool {
        self.rung == needs::Rung::Celld
    }

    /// A heavy section runs only when `--only` names it (the real-Hermes
    /// lane builds a 3.8 GB image); otherwise it is a skip that says how.
    pub fn section_by_name(&mut self, name: &str, needs: &[Need], why: &str) -> bool {
        if self.only.as_ref().is_some_and(|only| only.iter().any(|o| o == name)) {
            return self.section(name, needs);
        }
        self.asked.push(name.to_string());
        if self.selected(name) {
            // what it needs first: by name it would be skipped all the same
            let why = match needs::unmet(needs, self.rung) {
                Some((need, missing)) => format!("{missing} ({})", need.name()),
                None => format!("{why}: run it by name, cargo xtask e2e --only {name}"),
            };
            match &mut self.plan {
                Some(plan) => plan.push(Planned { section: name.into(), needs: needs.to_vec(), skip: Some(why) }),
                None => {
                    self.account(name, false);
                    self.skip(&format!("the {name} section"), &why);
                }
            }
        }
        false
    }

    /// Whether `--only`, `--except` and `--shard` select the section `name`.
    fn selected(&self, name: &str) -> bool {
        let named = self.only.as_ref().is_none_or(|only| only.iter().any(|o| o == name)) && !self.except.iter().any(|e| e == name);
        named && self.shard.is_none_or(|shard| lanes::shard_runs(shard.k, name))
    }

    /// A section accounted for: it runs, or (`ran` false) is skipped whole.
    /// Its checks are counted as its lane ends (lanes/mod.rs `run`).
    fn account(&mut self, name: &str, ran: bool) {
        let zero = summary::Counts::default();
        self.accounted.push(summary::Section { name: name.to_string(), ran, counts: zero, ms: 0 });
    }

    /// The checks counted so far.
    fn counts(&self) -> summary::Counts {
        summary::Counts { passed: self.passed as u64, failed: self.failed.len() as u64, skipped: self.skipped.len() as u64 }
    }

    /// Whether the section `name`, needing `needs`, runs in this suite:
    /// selected, on a rung that has what it needs, and not a dry run.
    pub fn runs(&self, name: &str, needs: &[Need]) -> bool {
        self.selected(name) && self.plan.is_none() && needs::unmet(needs, self.rung).is_none()
    }

    /// Whether the section `name` runs (it prints its header when it does).
    /// It needs `needs`: on a rung without one of them it is a skip that
    /// says which, and a dry run plans it instead.
    pub fn section(&mut self, name: &str, needs: &[Need]) -> bool {
        self.asked.push(name.to_string());
        if !self.selected(name) {
            return false;
        }
        let unmet = needs::unmet(needs, self.rung);
        if let Some(plan) = &mut self.plan {
            let skip = unmet.map(|(need, why)| format!("{why} ({})", need.name()));
            plan.push(Planned { section: name.into(), needs: needs.to_vec(), skip });
            return false;
        }
        if let Some((need, why)) = unmet {
            self.account(name, false);
            self.skip(&format!("the {name} section"), &format!("{why} ({})", need.name()));
            return false;
        }
        println!("\n# {name}");
        self.ran.push(name.to_string());
        self.account(name, true);
        if let Some(why) = self.lost.clone() {
            self.fail(&format!("{name} did not run: no node"), why);
            return false;
        }
        if std::mem::take(&mut self.recovery_due) {
            if let Err(e) = self.recover() {
                let why = format!("{e:#}");
                self.fail(&format!("{name} did not run: the node would not start again"), &why);
                self.lost = Some(why);
                return false;
            }
        }
        true
    }

    pub fn ok(&mut self, label: &str, cond: bool, detail: impl std::fmt::Display) {
        if cond {
            self.passed += 1;
            println!("ok    {label}");
        } else {
            self.fail(label, detail);
        }
    }

    /// A check local workerd cannot make (it is the hosted lane's): said,
    /// counted, and never a pass.
    pub fn skip(&mut self, label: &str, why: &str) {
        self.skipped.push(label.to_string());
        println!("skip  {label}: {why}");
    }

    pub fn fail(&mut self, label: &str, detail: impl std::fmt::Display) {
        self.failed.push(label.to_string());
        println!("FAIL  {label}: {detail}");
    }

    /// A lane that returned an error or panicked after its section began:
    /// one FAIL, and the next section gets the node back first.
    pub fn stopped_early(&mut self, why: &str) {
        let section = self.ran.last().cloned().unwrap_or_default();
        self.fail(&format!("{section} stopped early"), why);
        self.recovery_due = true;
    }

    /// The node as the lanes expect it, after a lane that stopped early:
    /// started again (so no test lever that lane pulled, the registry held
    /// down among them, outlives it), fragments on their own hosts, the
    /// platform on 127.0.0.1, no extra settings, and no model answers left
    /// scripted.
    fn recover(&mut self) -> Result<()> {
        if self.hosted() {
            // the deployment is not the run's to restart; a hosted lane pulls
            // levers on its own fragments and people alone
            return Ok(());
        }
        self.ai.clear_script();
        self.shape = Shape::Plain;
        if let Some(node) = self.node.take() {
            // one that does not stop in time is killed, and starts all the same
            if let Err(e) = node.stop() {
                println!("      {e:#}");
            }
        }
        self.start(false, true)?;
        Ok(())
    }

    /// The node's API as the lanes use it: fragments on their own hosts
    /// (hosted: the preview's).
    pub fn api(&self) -> Api {
        match (&self.preview, self.hosted_rules) {
            (Some(preview), _) => Api::hosted(preview, &self.shared),
            // a rehearsal's node is shaped as a branch deployment
            (None, true) => Api::new(self.port, Some(SUFFIX), &self.shared).branch(REHEARSAL_BRANCH),
            (None, false) => Api::new(self.port, Some(SUFFIX), &self.shared),
        }
    }

    /// The shared Chrome, in a context of the lane's own (`None`: no Chrome
    /// is installed). Its pages close when the lease is dropped.
    pub fn browser(&self) -> Result<Option<browser::Lease>> {
        self.chrome.lease()
    }

    /// A label for this run (a fragment's full name adds its owner's
    /// username). Hosted, it starts `e2e-`, so a sweep finds it.
    pub fn name(&self, base: &str) -> String {
        match self.hosted_rules {
            true => format!("{}{base}-{}", fragment_core::levers::E2E_LABEL_PREFIX, self.run),
            false => format!("{base}-{}", self.run),
        }
    }

    /// `base`'s full name for this run, under `owner`'s username.
    pub fn named(&self, api: &Api, owner: &Keys, base: &str) -> Result<String> {
        api.qualified(owner, &self.name(base))
    }

    /// Starts the node as fragment.club is shaped (`Shape::TwoSites`): a
    /// browser treats the platform and the fragments as two sites.
    pub fn start_as_browsers_see_it(&mut self) -> Result<Api> {
        self.start_shaped(Shape::TwoSites)
    }

    fn start_shaped(&mut self, shape: Shape) -> Result<Api> {
        self.shape = shape;
        let started = self.start(false, true);
        self.shape = Shape::Plain;
        let mut api = started?;
        api.base = format!("http://{SUFFIX}:{}", self.port);
        Ok(api)
    }

    /// Starts the node; `suffix` serves fragments from their own hosts.
    pub fn start(&mut self, clean: bool, suffix: bool) -> Result<Api> {
        anyhow::ensure!(self.preview.is_none(), "a hosted run starts no node: a section that does declares Need::Node");
        assert!(self.node.is_none(), "one node at a time");
        let tools = self.tools.as_ref().context("a local run locates its tools")?;
        devstack::Fleet {
            host_secret: self.host_secret.clone(),
            codestorage_org: self.org.clone(),
            codestorage_key_pem: self.org_key.clone(),
            codestorage_url: self.store_url(),
            host_suffix: suffix.then(|| self.suffix().to_string()),
            legacy_host_suffix: (suffix && self.shape == Shape::TwoSites).then(|| SUFFIX.to_string()),
            // a rehearsal's node is shaped as a branch deployment, whose
            // levers are scoped as a preview's are
            host_label_suffix: self.hosted_rules.then(|| format!("--{REHEARSAL_BRANCH}")),
            poll_interval_s: POLL_S,
            egress_local: true,
            job_retry_delay_s: 1,
            blob_grace_s: Some(BLOB_GRACE_S),
            // the lower rung: the model route's calls go to the Workers AI fake
            ai_url: Some(self.ai.node().url.clone()),
            ai_gateway: None,
            default_plan: Some(DEFAULT_PLAN.into()),
            delivery_retry_s: Some(1),
            workos: Some(devstack::WorkOsVars {
                client_id: self.workos.node().client_id.clone(),
                api_key: WORKOS_KEY.into(),
                api_url: Some(self.workos.node().url.clone()),
            }),
            oidc: self.oidc_signin().then(|| devstack::OidcVars {
                issuer: self.oidc.node().url.clone(),
                client_id: OIDC_CLIENT.into(),
                client_secret: Some(OIDC_SECRET.into()),
                scopes: None,
                claims: None,
                auth: None,
            }),
            platform_url: Some(match (self.shape, suffix) {
                (Shape::TwoSites, true) => format!("http://{SUFFIX}:{}", self.port),
                _ => format!("http://127.0.0.1:{}", self.port),
            }),
            operators: Some(fragment_core::npub::encode(self.operator.pubkey_hex())),
            signins_pending_max: Some(SIGNINS_PENDING_MAX),
            // the levers, as a preview has them: each request carries the secret
            test_secret: Some(self.test_secret.clone()),
            computer_image: Some("stub".into()),
            computer_snapshots: false,
            providers: Some(swap_providers()?.to_string()),
            operator_key_values: SWAP_KEYS.iter().map(|(name, value, _)| (name.to_string(), value.to_string())).collect(),
            swap_upstream: Some(self.upstream.node().url.clone()),
            // FRAGMENT_E2E_MODELS=openai: the text models through an
            // OpenAI-compatible server's route (docs/self-host.md, seam 3),
            // the same fake answering in OpenAI's shape; images go on to it
            // as the binding's input
            model_upstream: (std::env::var("FRAGMENT_E2E_MODELS").as_deref() == Ok("openai")).then(|| devstack::ModelUpstreamVars {
                url: format!("{}/v1", self.ai.node().url),
                models: json!({ fragment_core::models::CHEAP_MODEL: fragment_core::models::CHEAP_MODEL, fragment_core::models::MEDIUM_MODEL: fragment_core::models::MEDIUM_MODEL }).to_string(),
                key: None,
            }),
            node: None,
        }
        .configure(&self.project)?;
        // the agents' Worker runs beside it, as a deployment runs it: the
        // router hands it /api/agents and /api/a/*, its inboxes included
        devstack::AgentFleet {
            host_secret: self.host_secret.clone(),
            fragment_api: format!("http://127.0.0.1:{}", self.port),
            agent_url: format!("http://127.0.0.1:{}", self.port),
            test_hooks: true,
        }
        .configure(&self.agents_project)?;
        let opts = devstack::NodeOptions {
            project: self.project.clone(),
            port: self.port,
            clean,
            with: vec![self.agents_project.clone()],
            // each boot's log, in this run's scratch: a FAIL comes with the
            // node's side of it, the logs of a node a lane killed included
            log_dir: self.scratch.clone(),
            node_logs: true,
            containers: true,
        };
        let node = match &self.celld {
            None => devstack::AnyNode::Wrangler(devstack::Node::start(tools, &opts)?.0),
            Some(celld) => {
                let copts = devstack::celld::CelldOptions { project: opts.project, with: opts.with, port: opts.port, clean: opts.clean, log_dir: opts.log_dir };
                devstack::AnyNode::Celld(devstack::celld::CelldNode::start(celld, &copts)?.0)
            }
        };
        self.node = Some(node);
        Ok(Api::new(self.port, suffix.then_some(self.suffix()), &self.shared))
    }

    /// The fragments' suffix in the next node's shape.
    fn suffix(&self) -> &'static str {
        match self.shape {
            Shape::TwoSites => BOATS,
            Shape::Plain => SUFFIX,
        }
    }

    /// The agents' API: the node's own (the agents' script is co-hosted),
    /// started again after `crash`. Its state is the node's.
    pub fn agents(&mut self) -> Result<Api> {
        if let Some(preview) = &self.preview {
            // the platform's own: the router hands the agents' Worker /api/a/*
            return Ok(Api::hosted(preview, &self.shared));
        }
        if self.node.is_none() {
            self.start(false, true)?;
        }
        Ok(Api::new(self.port, None, &self.shared))
    }

    pub fn stop(&mut self) -> Result<()> {
        anyhow::ensure!(self.preview.is_none(), "a hosted run stops no node: a section that does declares Need::Node");
        self.node.take().expect("a running node").stop()
    }

    pub fn crash(&mut self) -> Result<()> {
        anyhow::ensure!(!self.hosted_rules, "the hosted lane crashes no node: a section that does declares Need::Node");
        self.node.take().expect("a running node").crash()
    }

    /// Records `owner` as the fragment's (a write the harness makes as
    /// another writer is followed by their `refresh` on an external store,
    /// and made as theirs hosted), and registers its push webhook with the
    /// fake (the dashboard registration the real service has), so git moves
    /// reach the cell. Hosted, nothing more: the run's commits and deploys
    /// go through the API (`commit`, `deploy`), which moves the pins itself.
    /// On an external store, nothing more either: none registers the cell's
    /// webhook (macrofiche's contract, question 2), so `refresh` and the
    /// poll move the pins, as on the hosted fleet.
    pub fn hook(&self, api: &Api, owner: &Keys, created: &Value) {
        let name = created["name"].as_str().expect("created.name");
        self.owners.borrow_mut().insert(name.to_string(), owner.clone());
        if self.hosted() || self.store.is_some() {
            return;
        }
        let repo = created["repo"].as_str().expect("created.repo");
        let secret = created["webhookSecret"].as_str().expect("created.webhookSecret");
        self.fake.register_webhook(repo, &format!("{}/api/f/{name}/webhook", api.base), secret);
    }

    /// Creates a fragment (signed by `keys`) with its webhook registered.
    pub fn create(&self, api: &Api, keys: &Keys, name: &str) -> Result<Value> {
        let r = api.create(keys, name)?;
        if r.status != 200 {
            bail!("create {name}: {r}");
        }
        r.body["name"].as_str().context("a create answers the fragment's name")?;
        self.hook(api, keys, &r.body);
        Ok(r.body)
    }

    /// A commit on main by another writer, announced by webhook. Hosted,
    /// its owner's commit through the files route (`POST /api/f/{name}/files`:
    /// one commit, main moved at once), since a preview's git is real. On
    /// an external store, a commit pack through its API, then the owner's
    /// `refresh`, as the CLI follows its own.
    pub fn commit(&self, created: &Value, changes: &[(&str, Option<&[u8]>)]) -> String {
        if !self.hosted() {
            let repo = created["repo"].as_str().expect("created.repo");
            let Some(store) = &self.store else {
                return self.fake.external_commit(repo, "main", changes, "e2e commit");
            };
            let (_, sha) = store.commit(repo, "main", changes, "e2e commit").unwrap_or_else(|e| panic!("a commit on {repo}: {e:#}"));
            self.as_owner(created, "refresh", &json!({}));
            return sha;
        }
        let files: Vec<Value> = changes
            .iter()
            .map(|(path, bytes)| match bytes.map(std::str::from_utf8) {
                None => json!({ "path": path, "delete": true }),
                Some(Ok(text)) => json!({ "path": path, "text": text }),
                Some(Err(_)) => json!({ "path": path, "base64": base64::engine::general_purpose::STANDARD.encode(bytes.unwrap_or_default()) }),
            })
            .collect();
        let r = self.as_owner(created, "files", &json!({ "files": files, "message": "e2e commit" }));
        r["commit"].as_str().unwrap_or_else(|| panic!("a commit answers its sha: {r}")).to_string()
    }

    /// A commit on main that nothing announces (a lost webhook): only the
    /// owner's `refresh` or the poll finds it. On an external store, any
    /// commit the harness makes and does not follow with a refresh.
    pub fn silent_commit(&self, created: &Value, changes: &[(&str, Option<&[u8]>)]) -> String {
        let repo = created["repo"].as_str().expect("created.repo");
        match &self.store {
            None => self.fake.silent_commit(repo, "main", changes, "silent"),
            Some(store) => store.commit(repo, "main", changes, "silent").unwrap_or_else(|e| panic!("a commit on {repo}: {e:#}")).1,
        }
    }

    /// Moves live to main's tip (what `fragment deploy` does), announced.
    /// Hosted, its owner's deploy (`POST /api/f/{name}/deploy`), which
    /// installs the app at once. On an external store, live created at
    /// main's tip or merged to it through its API, then the owner's
    /// `refresh`, as the CLI's deploy.
    pub fn deploy(&self, created: &Value) -> String {
        if !self.hosted() {
            let repo = created["repo"].as_str().expect("created.repo");
            let Some(store) = &self.store else {
                let tip = self.fake.branch(repo, "main").expect("main has a commit");
                self.fake.set_branch(repo, "live", &tip);
                return tip;
            };
            let (_, live) = store.go_live(repo, "deploy (e2e)").unwrap_or_else(|e| panic!("a deploy of {repo}: {e:#}"));
            self.as_owner(created, "refresh", &json!({}));
            return live;
        }
        let r = self.as_owner(created, "deploy", &json!({ "note": "e2e deploy" }));
        r["live"].as_str().unwrap_or_else(|| panic!("a deploy answers live: {r}")).to_string()
    }

    /// The code store outside the run, when the node's git lives in one.
    pub fn external_store(&self) -> Option<&store::External> {
        self.store.as_ref()
    }

    /// Whether the section `name`, which probes a code store outside the
    /// run, runs: only on one (`FRAGMENT_E2E_CODESTORE`). Elsewhere it is a
    /// skip that says so: the fake in the process is the contract's
    /// reference, and a preview's git is code.storage itself.
    pub fn store_section(&mut self, name: &str) -> bool {
        if self.store.is_some() {
            return self.section(name, &[]);
        }
        self.asked.push(name.to_string());
        if self.selected(name) {
            // what the rung lacks first (a hosted run without its secret runs nothing)
            let why = match needs::unmet(&[], self.rung) {
                Some((need, missing)) => format!("{missing} ({})", need.name()),
                None => format!("it probes a code store outside the run, and this run's git is {} ({}=external or macrofiche)", if self.hosted() { "the deployment's own" } else { "the fake, the contract's reference" }, store::CODESTORE_VAR),
            };
            match &mut self.plan {
                Some(plan) => plan.push(Planned { section: name.into(), needs: vec![], skip: Some(why) }),
                None => self.skip(&format!("the {name} section"), &why),
            }
        }
        false
    }

    /// Whether the code.storage fake's levers are the lanes': a local run
    /// whose git is the fake in this process. Its levers are an outage, a
    /// sabotaged commit, a count of the requests it answered, and its refs'
    /// flags; on an external store a check that pulls one is a skip that
    /// says which (`skip_lever`).
    pub fn store_levers(&self) -> bool {
        self.store.is_none()
    }

    /// A check that pulls the fake's `lever`, on an external store: a skip.
    pub fn skip_lever(&mut self, label: &str, lever: &str) {
        let store = self.store.as_ref().map_or_else(|| "the code store".to_string(), |s| s.label.clone());
        self.skip(label, &format!("it pulls the code.storage fake's {lever}, and the node's git is {store} ({}; docs/self-host.md, seam 5)", store::CODESTORE_VAR));
    }

    /// The code store's API base, as the node and the CLI reach it.
    pub fn store_url(&self) -> String {
        match &self.store {
            Some(store) => store.url.clone(),
            None => self.fake.node().url.clone(),
        }
    }

    /// A file's bytes on a branch of `repo` (its url form), as the store
    /// holds them.
    pub fn file_at(&self, repo: &str, branch: &str, path: &str) -> Option<Vec<u8>> {
        match &self.store {
            None => self.fake.file_at(repo, branch, path),
            Some(store) => store.file(repo, branch, path).unwrap_or_else(|e| panic!("{e:#}")),
        }
    }

    /// A branch's head in `repo`, as the store holds it.
    pub fn head(&self, repo: &str, branch: &str) -> Option<String> {
        match &self.store {
            None => self.fake.branch(repo, branch),
            Some(store) => store.head(repo, branch).unwrap_or_else(|e| panic!("{e:#}")),
        }
    }

    /// An ephemeral branch's head (a preview), as an external store's API
    /// reads one: with `ephemeral=true` (macrofiche's contract, 5.9).
    pub fn head_ephemeral(&self, repo: &str, branch: &str) -> Option<String> {
        let store = self.store.as_ref().expect("an external store: the fake's own refs say which are ephemeral");
        store.head_ephemeral(repo, branch).unwrap_or_else(|e| panic!("{e:#}"))
    }

    /// The url form of the repo named `name`, as the store lists it.
    pub fn repo_url(&self, name: &str) -> Option<String> {
        match &self.store {
            None => self.fake.repo_url(name),
            Some(store) => store.repo_url(name).unwrap_or_else(|e| panic!("{e:#}")),
        }
    }

    /// `n` empty repos made after every existing one (a busy org, so a
    /// lookup by name must page). On an external store, made as the cell
    /// makes one, named for the run.
    pub fn seed_filler(&self, n: usize) {
        let Some(store) = &self.store else { return self.fake.seed_filler(n) };
        for i in 0..n {
            store.create_repo(&format!("filler-{}-{i}", self.run)).unwrap_or_else(|e| panic!("{e:#}"));
        }
    }

    /// The requests to `repo` on one route (`"GET branch"`) the store has
    /// answered, from every caller: the fake counts them; an external
    /// store's are not counted here (`None`).
    pub fn requests(&self, repo: &str, route: &str) -> Option<u32> {
        self.store.is_none().then(|| self.fake.requests(repo, route))
    }

    /// Where `repo`'s main stands now, to count the commits after it.
    pub fn mark(&self, repo: &str) -> Mark {
        match &self.store {
            None => Mark::Packs(self.fake.requests(repo, "POST commit-pack")),
            Some(_) => Mark::Head(self.head(repo, "main")),
        }
    }

    /// The commits `repo` took since `mark`: the commit packs it was sent
    /// (the fake), or the commits on main past the marked head (an
    /// external store, which counts no requests).
    pub fn commits_since(&self, repo: &str, mark: &Mark) -> u64 {
        match (mark, &self.store) {
            (Mark::Packs(n), None) => u64::from(self.fake.requests(repo, "POST commit-pack") - n),
            (Mark::Head(marked), Some(store)) => {
                if self.head(repo, "main").is_none() {
                    return 0;
                }
                let history = store.history(repo, "main").unwrap_or_else(|e| panic!("{e:#}"));
                let at = match marked {
                    None => Some(history.len()),
                    Some(sha) => history.iter().position(|c| c == sha),
                };
                at.unwrap_or_else(|| panic!("{repo}'s main moved more than {} commits past {marked:?}", store::HISTORY_MAX)) as u64
            }
            _ => unreachable!("a mark is the store's that made it"),
        }
    }

    /// A hosted run's `POST /api/f/{name}/{route}`, signed by the owner
    /// `create` recorded: a git move as a local run makes it through the fake.
    fn as_owner(&self, created: &Value, route: &str, body: &Value) -> Value {
        let name = created["name"].as_str().expect("created.name");
        let owner = self.owners.borrow().get(name).cloned().unwrap_or_else(|| panic!("{name} was made by Suite::create, which records its owner"));
        let r = self.api().signed(&owner, "POST", &format!("/api/f/{name}/{route}"), Some(body)).unwrap_or_else(|e| panic!("{route} {name}: {e:#}"));
        assert!(r.status == 200, "{route} {name}: {r}");
        r.body
    }

    /// Polls `f` until it holds or `timeout` passes. One that runs out says
    /// so, with its place: a wait whose limit is its usual length (a check
    /// that something never happens aside) costs every run that limit.
    #[track_caller]
    pub fn eventually(&self, timeout: Duration, mut f: impl FnMut() -> bool) -> bool {
        let at = std::panic::Location::caller();
        let t0 = Instant::now();
        loop {
            if f() {
                return true;
            }
            if t0.elapsed() > timeout {
                println!("      (a wait ran out its {timeout:.0?} at {}:{})", at.file(), at.line());
                return false;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// Runs the CLI with its own HOME against the node.
    pub fn cli(&self, api: &Api, home: &Path, args: &[&str]) -> Output {
        self.cli_command(api, home, args).output().expect("run the fragment CLI")
    }

    /// `cli`, run in `cwd` (where `fragment init` makes its folder).
    pub fn cli_in(&self, api: &Api, home: &Path, cwd: &Path, args: &[&str]) -> Output {
        self.cli_command(api, home, args).current_dir(cwd).output().expect("run the fragment CLI")
    }

    /// The CLI as a lane spawns it itself (a watcher, a piped input): its
    /// config under the HOME the lane sets, on every system.
    pub fn bare_cli(&self) -> Command {
        let mut c = Command::new(&self.cli);
        // a Linux runner's XDG_CONFIG_HOME would put every lane's key in one place
        c.env_remove("XDG_CONFIG_HOME");
        c
    }

    fn cli_command(&self, api: &Api, home: &Path, args: &[&str]) -> Command {
        let mut c = self.bare_cli();
        c.args(args).env("HOME", home).env("FRAGMENT_HOST", &api.base).env_remove("FRAGMENT_OUTPUT");
        c
    }

    /// `fragment login` in `home`, with a person approving its key in a
    /// browser: the CLI's pending login, a sign-in through the WorkOS fake,
    /// the approval, then the CLI's login finishing, and a username taken.
    /// A login that fails is a FAIL of its own, so the lane's checks that
    /// fail after it have their cause printed first.
    pub fn login(&mut self, api: &Api, home: &Path) -> Output {
        let args = ["login", "--no-wait", "--json"];
        let pending = self.cli(api, home, &args);
        if let Err(e) = cli_data(&args, &pending).and_then(|p| approve_login(api, &p)) {
            // `fragment login` would wait ten minutes for an approval that is not coming
            self.fail("fragment login: a person approves its key in a browser", format!("{e:#}"));
            return pending;
        }
        let out = self.cli(api, home, &["login", "--no-browser"]);
        let done = match out.status.success() {
            true => self.take_username(api, home),
            false => Err(anyhow!("{}", String::from_utf8_lossy(&out.stderr))),
        };
        if let Err(e) = done {
            self.fail("fragment login", format!("{e:#}"));
        }
        out
    }

    /// A username through the CLI, as a person takes one once.
    fn take_username(&self, api: &Api, home: &Path) -> Result<()> {
        let me = self.cli_json(api, home, &["whoami", "--json"])?;
        if !me["identity"]["username"].is_null() {
            return Ok(());
        }
        let npub = me["npub"].as_str().context("whoami answers the key's npub")?;
        let username = format!("c{}", npub.get(5..15).context("an npub is longer than 15 characters")?);
        self.cli_json(api, home, &["username", &username, "--json"])?;
        Ok(())
    }

    /// The `data` of a `--json` CLI answer.
    pub fn cli_json(&self, api: &Api, home: &Path, args: &[&str]) -> Result<Value> {
        cli_data(args, &self.cli(api, home, args))
    }

    /// The key a CLI home logged in with (its config file, per platform).
    pub fn cli_keys(&self, home: &Path) -> Option<Keys> {
        ["Library/Application Support/fragment/config.json", ".config/fragment/config.json"]
            .iter()
            .find_map(|p| std::fs::read_to_string(home.join(p)).ok())
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            .and_then(|v| v["secret_key"].as_str().and_then(Keys::from_secret_hex))
    }

    /// A fresh directory in this run's scratch.
    pub fn dir(&self, name: &str) -> PathBuf {
        let d = self.scratch.join(name);
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("scratch dir");
        d
    }
}

/// The `data` of a `--json` CLI answer (`args`, its command, for errors).
fn cli_data(args: &[&str], out: &Output) -> Result<Value> {
    let v: Value = serde_json::from_slice(&out.stdout)
        .with_context(|| format!("fragment {args:?}: {}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)))?;
    if v["ok"] != true {
        bail!("fragment {args:?}: {v}");
    }
    Ok(v["data"].clone())
}

/// Whom `Suite::login` signs in as for a CLI key (its npub): the same
/// person again when a lane signs in with it.
pub fn cli_email(npub: &str) -> Result<String> {
    Ok(format!("cli-{}@e2e.test", npub.get(5..17).context("an npub is longer than 17 characters")?))
}

/// A person signs in through the WorkOS fake and approves the key a
/// pending `fragment login` names (a key already approved has nothing
/// pending).
fn approve_login(api: &Api, pending: &Value) -> Result<()> {
    if pending["pending"] != true {
        return Ok(());
    }
    let npub = pending["npub"].as_str().context("a pending login names its key")?;
    let link = pending["approve"].as_str().context("a pending login answers its approval link")?;
    let session = api.sign_in(&cli_email(npub)?)?;
    let r = api.approve_link(&session, link)?;
    anyhow::ensure!(r.status == 200, "the approval: {r}");
    Ok(())
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args = hosted::parse(&args)?;
    match args.hosted {
        None => local(args.only, args.except, LocalRun { rehearse: args.rehearse, shard: args.shard, summary: args.summary }),
        Some(hosted) => hosted::run(args.only, args.except, hosted),
    }
}

/// The CLI the lanes drive (`FRAGMENT_BIN`, else the release build).
fn cli_binary() -> Result<PathBuf> {
    let cli = std::env::var_os("FRAGMENT_BIN").map(PathBuf::from).unwrap_or_else(|| devstack::repo_root().join("target/release/fragment"));
    if !cli.is_file() {
        bail!("no CLI at {} (cargo build --release -p fragment-cli, or set FRAGMENT_BIN)", cli.display());
    }
    Ok(cli)
}

/// A run's name: distinguishes its fragments from any earlier state.
fn run_name() -> String {
    format!("{:x}", api::now_s() % 0xffffff)
}

/// A local run's settings beside its sections (hosted.rs `Args`).
struct LocalRun {
    /// A rehearsal of the hosted lane, lending at most this many paid calls.
    rehearse: Option<u64>,
    shard: Option<Shard>,
    summary: Option<PathBuf>,
}

/// The local run: a fresh `wrangler dev` node and the fakes. A rehearsal
/// (`rehearse`: the paid calls it may lend) keeps the hosted lane's rules
/// on it: the node shaped as a branch deployment (its levers scoped as a
/// preview's), people signed in through the levers, git moved through the
/// API, the fakes hidden from the lanes, and each section run or skipped by
/// what it needs, as on a preview; it ends with the sweep. A shard runs the
/// sections the table gives it, in the lanes' order.
fn local(only: Option<Vec<String>>, except: Vec<String>, settings: LocalRun) -> Result<()> {
    let LocalRun { rehearse, shard, summary } = settings;
    let root = devstack::repo_root();
    let cli = cli_binary()?;
    let tools = devstack::Tools::locate()?;
    let run = run_name();
    let scratch = root.join("target/e2e").join(&run);
    std::fs::create_dir_all(&scratch)?;
    let hidden = rehearse.is_some();
    let codestore = codestore(hidden, &scratch)?;
    println!("code store: {}", codestore.said);
    let project = devstack::stage_project(&scratch.join("cell"))?;
    if only.as_ref().is_some_and(|o| o.iter().any(|n| n == lanes::hermes::SECTION)) {
        lanes::hermes::stage_images(&project)?;
    }
    let agents_project = devstack::stage_agent(&scratch.join("agent"))?;
    let test_secret = devstack::random_hex(32);
    // a local run's people sign in through the WorkOS fake, and pay the fake
    // model; a rehearsal's, through the levers, lent paid calls as on a preview
    // the runtime: wrangler dev's workerd, or celld (docs/self-host.md)
    let celld = match std::env::var("FRAGMENT_E2E_RUNTIME").as_deref() {
        Err(_) | Ok("wrangler") => None,
        Ok("celld") => Some(devstack::celld::CelldTools::locate()?),
        Ok(other) => bail!("FRAGMENT_E2E_RUNTIME is wrangler or celld, not {other}"),
    };
    // who signs people in: the WorkOS fake, or the OpenID Connect fake (docs/self-host.md, seam 4)
    if let Some(other) = std::env::var("FRAGMENT_E2E_SIGNIN").ok().filter(|s| s != "workos" && s != "oidc") {
        bail!("FRAGMENT_E2E_SIGNIN is workos or oidc, not {other}");
    }
    let (rung, shared) = match rehearse {
        None if celld.is_some() => (needs::Rung::Celld, api::Run::new(test_secret.clone(), 0)),
        None => (needs::Rung::Local, api::Run::new(test_secret.clone(), 0)),
        Some(paid_calls) => {
            let offers = needs::Offers { levers: true, computers: true, models: paid_calls > 0, chrome: browser::chrome().is_some() };
            (needs::Rung::Hosted(offers), api::Run::signing_in_by_levers(test_secret.clone(), paid_calls))
        }
    };
    let mut s = Suite {
        only,
        except,
        shard,
        summary,
        accounted: vec![],
        outside: summary::Counts::default(),
        rung,
        hosted_rules: rehearse.is_some(),
        preview: None,
        plan: None,
        shared,
        owners: RefCell::new(BTreeMap::new()),
        asked: vec![],
        passed: 0,
        failed: vec![],
        skipped: vec![],
        ran: vec![],
        recovery_due: false,
        lost: None,
        cron: None,
        chrome: browser::Shared::new(&scratch),
        tools: Some(tools),
        node: None,
        celld,
        port: devstack::free_port()?,
        run,
        fake: codestore.fake,
        store: codestore.store,
        macrofiche: codestore.macrofiche,
        org: codestore.org,
        ai: Fake::of(hidden, "Workers AI", fragment_fakes::workers_ai::WorkersAi::start(0)?),
        push: Fake::of(hidden, "push service", fragment_fakes::push::PushService::start()?),
        org_key: codestore.org_key,
        host_secret: devstack::random_hex(32),
        test_secret,
        workos: Fake::of(hidden, "WorkOS", fragment_fakes::workos::WorkOs::start(WORKOS_CLIENT, WORKOS_KEY)?),
        oidc: Fake::of(hidden, "OpenID Connect", fragment_fakes::oidc::Oidc::start(OIDC_CLIENT, OIDC_SECRET)?),
        upstream: Fake::of(hidden, "upstream", fragment_fakes::upstream::Upstream::start()?),
        operator: Keys::generate(),
        cli,
        scratch,
        project,
        agents_project,
        shape: Shape::Plain,
    };
    if let Some(shard) = s.shard {
        println!("shard {shard}: {}", lanes::SHARDS[shard.k as usize - 1].join(", "));
    }
    let t0 = Instant::now();
    s.start(true, true)?;
    // wrangler builds the computer images as it boots: built ahead (xtask's
    // build), every step is a cache hit, and the boot takes seconds
    let log = s.node.as_ref().map(|n| std::fs::read_to_string(n.log()).unwrap_or_default()).unwrap_or_default();
    let cached = log.lines().filter(|l| l.starts_with('#') && l.ends_with(" CACHED")).count();
    println!("the node is ready in {:.1?} (its image builds: {cached} steps cached)", t0.elapsed());
    lanes::run(&mut s);
    if rehearse.is_some() {
        lanes::counted(&mut s, |s, _| {
            hosted::rehearse_sweep(s);
            Ok(())
        });
    }
    finish(&mut s)
}

/// A local run's code store, as `codestore` started or found it.
struct RunStore {
    fake: Fake<CodeStorage>,
    store: Option<store::External>,
    macrofiche: Option<devstack::codestore::Macrofiche>,
    org: String,
    org_key: String,
    /// What the run's first line says it is.
    said: String,
}

/// The node's code store (`FRAGMENT_E2E_CODESTORE`, store.rs): the fake in
/// this process (`hidden` from the lanes on a rehearsal), a store already
/// running, or macrofiche, started on the run's scratch with an org key
/// made for the run (its log beside the node's).
fn codestore(hidden: bool, scratch: &Path) -> Result<RunStore> {
    match store::Choice::from_env()? {
        store::Choice::Fake => {
            let org_key = fake::generate_org_key_pem();
            let fake = CodeStorage::start(fake::Options { org: ORG.into(), org_key_pem: Some(org_key.clone()), ..Default::default() })?;
            let said = format!("{} (the code.storage fake, in this process)", fake.url);
            Ok(RunStore { fake: Fake::of(hidden, "code.storage", fake), store: None, macrofiche: None, org: ORG.into(), org_key, said })
        }
        store::Choice::External => {
            let x = devstack::codestore::ExternalStore::from_env()?;
            let label = format!("the external store at {}", x.url);
            let store = store::External::new(&x.url, &x.org, &x.key_pem, label.clone())?;
            let said = format!("{label}, org {} ({}=external)", x.org, store::CODESTORE_VAR);
            Ok(RunStore { fake: Fake::absent("code.storage"), store: Some(store), macrofiche: None, org: x.org, org_key: x.key_pem, said })
        }
        store::Choice::Macrofiche => {
            let bin = devstack::codestore::macrofiche_bin()?;
            let org_key = fake::generate_org_key_pem();
            let opts = devstack::codestore::MacroficheOptions {
                dir: scratch.join("macrofiche"),
                port: devstack::free_port()?,
                org: ORG.into(),
                key_pem: org_key.clone(),
                log_dir: scratch.to_path_buf(),
            };
            let m = devstack::codestore::Macrofiche::start(&bin, &opts)?;
            let mut store = store::External::new(&m.url, ORG, &org_key, "macrofiche".into())?;
            store.service = true;
            let said = format!("{} (macrofiche, {}; its log {})", m.url, bin.display(), m.log.display());
            Ok(RunStore { fake: Fake::absent("code.storage"), store: Some(store), macrofiche: Some(m), org: ORG.into(), org_key, said })
        }
    }
}

/// A run's end: a name `--only` or `--except` gave that no section has
/// fails it; the node stops; the counts, each FAIL, and (hosted) what the
/// run spent are printed. Non-zero when any check failed, the scratch kept.
fn finish(s: &mut Suite) -> Result<()> {
    // the run's own checks, outside any section
    let before = s.counts();
    for (flag, name) in s.only.clone().unwrap_or_default().into_iter().map(|n| ("--only", n)).chain(s.except.clone().into_iter().map(|n| ("--except", n))) {
        if !s.asked.contains(&name) {
            s.fail(&format!("{flag} {name}"), "no section has that name");
        }
    }
    // what the run's people spent, from their ledgers, while they answer
    if s.hosted() {
        hosted::spent(s);
    }
    if s.node.is_some() {
        let t0 = Instant::now();
        if let Err(e) = s.stop() {
            s.fail("the node stops at the end of the run", format!("{e:#}"));
        }
        println!("      (the node stopped in {:.1?})", t0.elapsed());
    }
    // the code store the run started stops after the node that wrote to it
    if let Some(m) = s.macrofiche.take() {
        if let Err(e) = m.stop() {
            s.fail("macrofiche stops at the end of the run", format!("{e:#}"));
        }
    }
    let t0 = Instant::now();
    s.chrome.close();
    println!("      (Chrome closed in {:.1?})", t0.elapsed());
    s.outside = s.outside + s.counts().since(before);
    if let Some(path) = s.summary.clone() {
        // written before the counts are printed: a red run's summary too
        // (the aggregate names its FAILs), and a run that cannot write one fails
        if let Err(e) = write_summary(s, &path) {
            s.fail("the run writes its summary", format!("{}: {e:#}", path.display()));
        }
    }
    let skipped = match s.hosted() || s.store.is_some() {
        true => "skipped (each says why)",
        false => "skipped (the hosted lane's)",
    };
    println!("\n{} passed, {} failed, {} {skipped}", s.passed, s.failed.len(), s.skipped.len());
    if !s.failed.is_empty() {
        for f in &s.failed {
            println!("  FAIL {f}");
        }
        match s.hosted() {
            true => println!("kept for a look: {}", s.scratch.display()),
            false => println!("kept for a look: {} (each node boot's log is node-<port>-<boot>.log there)", s.scratch.display()),
        }
        std::process::exit(1);
    }
    if std::env::var_os("FRAGMENT_E2E_KEEP").is_some() {
        println!("kept: {}", s.scratch.display());
    } else if let Err(e) = std::fs::remove_dir_all(&s.scratch) {
        println!("could not remove {}: {e}", s.scratch.display());
    }
    Ok(())
}

/// The run's summary (devstack's `summary::Summary`) at `path`: the suite
/// as the lanes asked for it, the sections it accounted for, and its counts.
fn write_summary(s: &Suite, path: &Path) -> Result<()> {
    let mut suite: Vec<String> = Vec::with_capacity(s.asked.len());
    for name in &s.asked {
        if !suite.contains(name) {
            suite.push(name.clone());
        }
    }
    let summary = summary::Summary {
        shard: s.shard,
        suite,
        sections: s.accounted.clone(),
        outside: s.outside,
        totals: s.counts(),
        failures: s.failed.clone(),
    };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, serde_json::to_vec_pretty(&summary)?)?;
    // read back: what the aggregate will read is what was meant
    let back: summary::Summary = serde_json::from_slice(&std::fs::read(path)?)?;
    assert_eq!(back, summary, "a summary reads back as written");
    Ok(())
}
