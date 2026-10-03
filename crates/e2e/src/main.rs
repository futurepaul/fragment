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

mod api;
mod browser;
mod lanes;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use fragment_devstack as devstack;
use fragment_fakes::codestorage::{self as fake, CodeStorage};
use fragment_nip98::Keys;
use serde_json::{json, Value};

use api::Api;

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
/// What the fleet's computers may swap in (docs/computers.md): one
/// connection and one operator key, each with a host of its own, and the
/// key's (test) value.
pub const SWAP_CONNECTION: &str = "github";
pub const SWAP_CONNECTION_HOST: &str = "api.github.test";
pub const SWAP_KEY: &str = "search";
pub const SWAP_KEY_HOST: &str = "api.search.test";
pub const SWAP_KEY_VALUE: &str = "sk-e2e-search-7f3a9c";
/// What one call of the key costs at list price: half a cent.
pub const SWAP_KEY_MICROS: i64 = 5_000;

pub struct Suite {
    /// The sections to run (`None`: all of them), and those not to.
    only: Option<Vec<String>>,
    except: Vec<String>,
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
    tools: devstack::Tools,
    node: Option<devstack::Node>,
    port: u16,
    /// Distinguishes this run's fragment names from any earlier state.
    run: String,
    pub fake: CodeStorage,
    /// The model route's vendor boundary, text and images: Workers AI, scripted.
    pub ai: fragment_fakes::workers_ai::WorkersAi,
    pub push: fragment_fakes::push::PushService,
    org_key: String,
    host_secret: String,
    /// Sign-in's stand-in: people sign in through it (`Api::person`), and
    /// Pipes', which hands out connections' tokens.
    pub workos: fragment_fakes::workos::WorkOs,
    /// The provider APIs a computer's swap sends to.
    pub upstream: fragment_fakes::upstream::Upstream,
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
    /// them, never an app.
    pub fn deployment_secrets(&self) -> Vec<(&'static str, String)> {
        vec![
            ("host secret", self.host_secret.clone()),
            // a line of the PEM's body: found however the PEM was escaped
            ("code.storage key", self.org_key.lines().find(|l| !l.starts_with("-----") && !l.trim().is_empty()).unwrap_or_default().trim().to_string()),
            ("WorkOS API key", WORKOS_KEY.into()),
        ]
    }

    /// A heavy section runs only when `--only` names it (the real-Hermes
    /// lane builds a 3.8 GB image); otherwise it is a skip that says how.
    pub fn section_by_name(&mut self, name: &str, why: &str) -> bool {
        if self.only.as_ref().is_some_and(|only| only.iter().any(|o| o == name)) {
            return self.section(name);
        }
        self.asked.push(name.to_string());
        if self.runs(name) {
            self.skip(&format!("the {name} section"), &format!("{why}: run it by name, cargo xtask e2e --only {name}"));
        }
        false
    }

    /// Whether the section `name` runs in this suite (`--only` names it,
    /// `--except` does not).
    pub fn runs(&self, name: &str) -> bool {
        self.only.as_ref().is_none_or(|only| only.iter().any(|o| o == name)) && !self.except.iter().any(|e| e == name)
    }

    /// Whether the section `name` runs (it prints its header when it does).
    pub fn section(&mut self, name: &str) -> bool {
        self.asked.push(name.to_string());
        if !self.runs(name) {
            return false;
        }
        println!("\n# {name}");
        self.ran.push(name.to_string());
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

    /// The node's API as the lanes use it: fragments on their own hosts.
    pub fn api(&self) -> Api {
        Api::new(self.port, Some(SUFFIX))
    }

    /// The shared Chrome, in a context of the lane's own (`None`: no Chrome
    /// is installed). Its pages close when the lease is dropped.
    pub fn browser(&self) -> Result<Option<browser::Lease>> {
        self.chrome.lease()
    }

    /// A label for this run (a fragment's full name adds its owner's username).
    pub fn name(&self, base: &str) -> String {
        format!("{base}-{}", self.run)
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
        assert!(self.node.is_none(), "one node at a time");
        devstack::Fleet {
            host_secret: self.host_secret.clone(),
            codestorage_org: ORG.into(),
            codestorage_key_pem: self.org_key.clone(),
            codestorage_url: self.fake.url.clone(),
            host_suffix: suffix.then(|| self.suffix().to_string()),
            legacy_host_suffix: (suffix && self.shape == Shape::TwoSites).then(|| SUFFIX.to_string()),
            poll_interval_s: POLL_S,
            egress_local: true,
            job_retry_delay_s: 1,
            blob_grace_s: Some(BLOB_GRACE_S),
            // the lower rung: the model route's calls go to the Workers AI fake
            ai_url: Some(self.ai.url.clone()),
            ai_gateway: None,
            default_plan: Some(DEFAULT_PLAN.into()),
            delivery_retry_s: Some(1),
            workos: Some(devstack::WorkOsVars {
                client_id: self.workos.client_id.clone(),
                api_key: WORKOS_KEY.into(),
                api_url: Some(self.workos.url.clone()),
            }),
            platform_url: Some(match (self.shape, suffix) {
                (Shape::TwoSites, true) => format!("http://{SUFFIX}:{}", self.port),
                _ => format!("http://127.0.0.1:{}", self.port),
            }),
            operators: Some(fragment_core::npub::encode(self.operator.pubkey_hex())),
            signins_pending_max: Some(SIGNINS_PENDING_MAX),
            test_hooks: true,
            computer_image: Some("stub".into()),
            computer_snapshots: false,
            connections: Some(json!({ SWAP_CONNECTION: [SWAP_CONNECTION_HOST] }).to_string()),
            operator_keys: Some(json!({ SWAP_KEY: [SWAP_KEY_HOST] }).to_string()),
            operator_key_values: vec![(SWAP_KEY.into(), SWAP_KEY_VALUE.into())],
            key_prices: Some(json!([{ "key": SWAP_KEY, "micros": SWAP_KEY_MICROS, "per": 1 }]).to_string()),
            swap_upstream: Some(self.upstream.url.clone()),
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
        };
        let (node, _) = devstack::Node::start(&self.tools, &opts)?;
        self.node = Some(node);
        Ok(Api::new(self.port, suffix.then_some(self.suffix())))
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
        if self.node.is_none() {
            self.start(false, true)?;
        }
        Ok(Api::new(self.port, None))
    }

    pub fn stop(&mut self) -> Result<()> {
        self.node.take().expect("a running node").stop()
    }

    pub fn crash(&mut self) -> Result<()> {
        self.node.take().expect("a running node").crash()
    }

    /// Registers the fragment's push webhook with the fake (the dashboard
    /// registration the real service has), so git moves reach the cell.
    pub fn hook(&self, api: &Api, created: &Value) {
        let name = created["name"].as_str().expect("created.name");
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
        self.hook(api, &r.body);
        Ok(r.body)
    }

    /// A commit on main by another writer, announced by webhook.
    pub fn commit(&self, created: &Value, changes: &[(&str, Option<&[u8]>)]) -> String {
        self.fake.external_commit(created["repo"].as_str().expect("created.repo"), "main", changes, "e2e commit")
    }

    /// Moves live to main's tip (what `fragment deploy` does), announced.
    pub fn deploy(&self, created: &Value) -> String {
        let repo = created["repo"].as_str().expect("created.repo");
        let tip = self.fake.branch(repo, "main").expect("main has a commit");
        self.fake.set_branch(repo, "live", &tip);
        tip
    }

    /// Polls `f` until it holds or `timeout` passes.
    pub fn eventually(&self, timeout: Duration, mut f: impl FnMut() -> bool) -> bool {
        let t0 = Instant::now();
        loop {
            if f() {
                return true;
            }
            if t0.elapsed() > timeout {
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
    let list = |names: &str| names.split(',').map(str::to_string).collect::<Vec<_>>();
    let (only, except) = match args.as_slice() {
        [] => (None, vec![]),
        [flag, sections] if flag == "--only" => (Some(list(sections)), vec![]),
        [flag, sections] if flag == "--except" => (None, list(sections)),
        _ => bail!("usage: fragment-e2e [--only <section>[,<section>...] | --except <section>[,<section>...]]"),
    };
    let root = devstack::repo_root();
    let cli = std::env::var_os("FRAGMENT_BIN").map(PathBuf::from).unwrap_or_else(|| root.join("target/release/fragment"));
    if !cli.is_file() {
        bail!("no CLI at {} (cargo build --release -p fragment-cli, or set FRAGMENT_BIN)", cli.display());
    }
    let tools = devstack::Tools::locate()?;
    let org_key = fake::generate_org_key_pem();
    let fake = CodeStorage::start(fake::Options { org: ORG.into(), org_key_pem: Some(org_key.clone()), ..Default::default() })?;
    let run = format!("{:x}", api::now_s() % 0xffffff);
    let scratch = root.join("target/e2e").join(&run);
    std::fs::create_dir_all(&scratch)?;
    let project = devstack::stage_project(&scratch.join("cell"))?;
    if only.as_ref().is_some_and(|o| o.iter().any(|n| n == lanes::hermes::SECTION)) {
        lanes::hermes::stage_images(&project)?;
    }
    let agents_project = devstack::stage_agent(&scratch.join("agent"))?;
    let mut s = Suite {
        only,
        except,
        asked: vec![],
        passed: 0,
        failed: vec![],
        skipped: vec![],
        ran: vec![],
        recovery_due: false,
        lost: None,
        cron: None,
        chrome: browser::Shared::new(&scratch),
        tools,
        node: None,
        port: devstack::free_port()?,
        run,
        fake,
        ai: fragment_fakes::workers_ai::WorkersAi::start(0)?,

        push: fragment_fakes::push::PushService::start()?,
        org_key,
        host_secret: devstack::random_hex(32),
        workos: fragment_fakes::workos::WorkOs::start(WORKOS_CLIENT, WORKOS_KEY)?,
        upstream: fragment_fakes::upstream::Upstream::start()?,
        operator: Keys::generate(),
        cli,
        scratch,
        project,
        agents_project,
        shape: Shape::Plain,
    };
    s.start(true, true)?;
    lanes::run(&mut s);
    for (flag, name) in s.only.clone().unwrap_or_default().into_iter().map(|n| ("--only", n)).chain(s.except.clone().into_iter().map(|n| ("--except", n))) {
        if !s.asked.contains(&name) {
            s.fail(&format!("{flag} {name}"), "no section has that name");
        }
    }
    if s.node.is_some() {
        let t0 = Instant::now();
        if let Err(e) = s.stop() {
            s.fail("the node stops at the end of the run", format!("{e:#}"));
        }
        println!("      (the node stopped in {:.1?})", t0.elapsed());
    }
    let t0 = Instant::now();
    s.chrome.close();
    println!("      (Chrome closed in {:.1?})", t0.elapsed());
    println!("\n{} passed, {} failed, {} skipped (the hosted lane's)", s.passed, s.failed.len(), s.skipped.len());
    if !s.failed.is_empty() {
        for f in &s.failed {
            println!("  FAIL {f}");
        }
        println!("kept for a look: {} (each node boot's log is node-<port>-<boot>.log there)", s.scratch.display());
        std::process::exit(1);
    }
    if std::env::var_os("FRAGMENT_E2E_KEEP").is_some() {
        println!("kept: {}", s.scratch.display());
    } else if let Err(e) = std::fs::remove_dir_all(&s.scratch) {
        println!("could not remove {}: {e}", s.scratch.display());
    }
    Ok(())
}
