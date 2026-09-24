//! The fragment end-to-end suite: a real `celld dev` node serving the real
//! cell (from a staged copy under `target/e2e/cell`, so a running `xtask
//! dev` is never touched), the code.storage fake from `crates/fakes`
//! (webhooks included), the real CLI, and signed HTTP the way the CLI and
//! a browser send it.
//!
//! `cargo xtask e2e [--only <section>]`. Each section makes its own
//! fragments, so any one can run alone. Every check prints `ok` or `FAIL`;
//! the process exits non-zero when any check fails.

mod api;
mod browser;
mod hosted;
mod lanes;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use fragment_devstack as devstack;
use fragment_fakes::codestorage::{self as fake, CodeStorage};
use fragment_nip98::Keys;
use serde_json::Value;

use api::Api;

pub const SUFFIX: &str = "fragment.localhost";
const ORG: &str = "fragment-e2e";
/// The poll backstop runs this often here (5 minutes in production).
pub const POLL_S: u32 = 2;
/// The key the OpenRouter fake takes (a fragment's OPENROUTER_API_KEY secret here).
pub const OPENROUTER_KEY: &str = "sk-or-e2e-7c1d";
/// Blobs no branch names are kept this long here (7 days in production).
pub const BLOB_GRACE_S: u32 = 4;
/// The OpenRouter fake's management key, and each person's monthly budget
/// here (small, so a few steps use it up).
pub const OPENROUTER_MANAGEMENT: &str = "sk-or-v1-management-e2e";
pub const BUDGET_USD: &str = "0.1";
/// The WorkOS fake's environment.
const WORKOS_CLIENT: &str = "client_fragment_e2e";
const WORKOS_KEY: &str = "sk_test_fragment_e2e";

pub struct Suite {
    only: Option<String>,
    passed: usize,
    failed: Vec<String>,
    tools: devstack::Tools,
    node: Option<devstack::Node>,
    port: u16,
    /// Distinguishes this run's fragment names from any earlier state.
    run: String,
    pub fake: CodeStorage,
    pub openrouter: fragment_fakes::openrouter::OpenRouter,
    pub push: fragment_fakes::push::PushService,
    org_key: String,
    host_secret: String,
    /// Sign-in's stand-in: people sign in through it (`Api::person`).
    pub workos: fragment_fakes::workos::WorkOs,
    /// The fleet's operator (`FRAGMENT_OPERATORS`): a key a person approves
    /// when a lane needs it.
    pub operator: Keys,
    pub cli: PathBuf,
    pub scratch: PathBuf,
    /// The node's own copy of the cell project (never `cell/`, where `xtask dev` runs).
    project: PathBuf,
    /// The agents' script (phase 5), co-hosted on the node.
    agents_project: PathBuf,
    /// More environment for the next node started (celld settings a lane tries).
    pub node_env_extra: Vec<(String, String)>,
    /// The next node's platform is on the fragments' domain (`start_as_browsers_see_it`).
    platform_on_suffix: bool,
}

impl Suite {
    /// The fleet's secrets (label, value): the node holds them, never a cell.
    pub fn fleet_secrets(&self) -> Vec<(&'static str, String)> {
        vec![
            ("host secret", self.host_secret.clone()),
            // a line of the PEM's body: found however the PEM was escaped
            ("code.storage key", self.org_key.lines().find(|l| !l.starts_with("-----") && !l.trim().is_empty()).unwrap_or_default().trim().to_string()),
            ("WorkOS API key", WORKOS_KEY.into()),
            ("OpenRouter management key", OPENROUTER_MANAGEMENT.into()),
        ]
    }

    pub fn section(&self, name: &str) -> bool {
        if self.only.as_deref().is_some_and(|o| o != name) {
            return false;
        }
        println!("\n# {name}");
        true
    }

    pub fn ok(&mut self, label: &str, cond: bool, detail: impl std::fmt::Display) {
        if cond {
            self.passed += 1;
            println!("ok    {label}");
        } else {
            self.failed.push(label.to_string());
            println!("FAIL  {label}: {detail}");
        }
    }

    /// A label for this run (a fragment's full name adds its owner's username).
    pub fn name(&self, base: &str) -> String {
        format!("{base}-{}", self.run)
    }

    /// `base`'s full name for this run, under `owner`'s username.
    pub fn named(&self, api: &Api, owner: &Keys, base: &str) -> Result<String> {
        api.qualified(owner, &self.name(base))
    }

    /// Starts the node as fragment.club is shaped: the platform on the
    /// fragments' own domain (`fragment.localhost`), so a browser treats
    /// the platform and every fragment as one site. A fragment framed in
    /// another's page signs in through the platform only then (the desktop).
    pub fn start_as_browsers_see_it(&mut self) -> Result<Api> {
        self.platform_on_suffix = true;
        let started = self.start(false, true);
        self.platform_on_suffix = false;
        let mut api = started?;
        api.base = format!("http://{SUFFIX}:{}", self.port);
        Ok(api)
    }

    /// Starts the node; `suffix` serves fragments from their own hosts.
    pub fn start(&mut self, clean: bool, suffix: bool) -> Result<Api> {
        assert!(self.node.is_none(), "one node at a time");
        let env = devstack::Fleet {
            host_secret: self.host_secret.clone(),
            codestorage_org: ORG.into(),
            codestorage_key_pem: self.org_key.clone(),
            codestorage_url: self.fake.url.clone(),
            host_suffix: suffix.then(|| SUFFIX.to_string()),
            poll_interval_s: POLL_S,
            egress_local: true,
            job_retry_delay_s: 1,
            blob_grace_s: Some(BLOB_GRACE_S),
            openrouter_url: Some(self.openrouter.url.clone()),
            delivery_retry_s: Some(1),
            workos: Some(devstack::WorkOsVars {
                client_id: self.workos.client_id.clone(),
                api_key: WORKOS_KEY.into(),
                api_url: Some(self.workos.url.clone()),
            }),
            platform_url: Some(match self.platform_on_suffix && suffix {
                true => format!("http://{SUFFIX}:{}", self.port),
                false => format!("http://127.0.0.1:{}", self.port),
            }),
            openrouter_management: Some(OPENROUTER_MANAGEMENT.into()),
            budget_usd: Some(BUDGET_USD.into()),
            operators: Some(fragment_core::npub::encode(self.operator.pubkey_hex())),
            test_hooks: true,
        }
        .configure(&self.project)?;
        // the agents' script is co-hosted, as the fleet runs it: the
        // router hands it /api/agents and /api/a/*, its inboxes included
        devstack::AgentFleet {
            host_secret: self.host_secret.clone(),
            fragment_api: format!("http://127.0.0.1:{}", self.port),
            agent_url: format!("http://127.0.0.1:{}", self.port),
            openrouter_url: Some(self.openrouter.url.clone()),
            test_hooks: true,
            egress_local: true,
        }
        .configure(&self.agents_project)?;
        let env = env.into_iter().chain(self.node_env_extra.iter().cloned()).collect();
        let opts = devstack::NodeOptions { project: self.project.clone(), port: self.port, clean, watch: false, env, with: vec![self.agents_project.clone()] };
        let (node, _) = devstack::Node::start(&self.tools, &opts)?;
        self.node = Some(node);
        Ok(Api::new(self.port, suffix.then_some(SUFFIX)))
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
        Command::new(&self.cli)
            .args(args)
            .env("HOME", home)
            .env("FRAGMENT_HOST", &api.base)
            .env_remove("FRAGMENT_OUTPUT")
            .output()
            .expect("run the fragment CLI")
    }

    /// `fragment login` in `home`, with a person approving its key in a
    /// browser: the CLI's pending login, a sign-in through the WorkOS fake,
    /// the approval, then the CLI's login finishing.
    pub fn login(&self, api: &Api, home: &Path) -> Output {
        let pending = self.cli_json(api, home, &["login", "--no-wait", "--json"]);
        if let Ok(p) = &pending {
            if p["pending"] == true {
                let npub = p["npub"].as_str().unwrap_or("").to_string();
                let link = p["approve"].as_str().unwrap_or("").to_string();
                let email = format!("cli-{}@e2e.test", &npub[5..17]);
                let approved = api.sign_in(&email).and_then(|session| api.approve_link(&session, &link));
                if let Err(e) = approved {
                    println!("      the browser's approval failed: {e}");
                }
            }
        }
        let out = self.cli(api, home, &["login", "--no-browser"]);
        // and takes a username through the CLI, as a person does once
        if let Ok(me) = self.cli_json(api, home, &["whoami", "--json"]) {
            if me["identity"]["username"].is_null() {
                let npub = me["npub"].as_str().unwrap_or("npub1xxxxxxxxxxxxxxx");
                let _ = self.cli_json(api, home, &["username", &format!("c{}", &npub[5..15]), "--json"]);
            }
        }
        out
    }

    /// The `data` of a `--json` CLI answer.
    pub fn cli_json(&self, api: &Api, home: &Path, args: &[&str]) -> Result<Value> {
        let out = self.cli(api, home, args);
        let v: Value = serde_json::from_slice(&out.stdout)
            .with_context(|| format!("fragment {args:?}: {}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)))?;
        if v["ok"] != true {
            bail!("fragment {args:?}: {v}");
        }
        Ok(v["data"].clone())
    }

    /// The key a CLI home logged in with (its config file, per platform).
    pub fn cli_keys(&self, home: &Path) -> Option<Keys> {
        ["Library/Application Support/fragment/config.json", ".config/fragment/config.json"]
            .iter()
            .find_map(|p| std::fs::read_to_string(home.join(p)).ok())
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            .and_then(|v| v["secret_key"].as_str().and_then(Keys::from_secret_hex))
    }

    /// A fresh directory for this run.
    pub fn dir(&self, name: &str) -> PathBuf {
        let d = self.scratch.join(format!("{name}-{}", self.run));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("scratch dir");
        d
    }
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (hosted, rest) = match args.split_first() {
        Some((flag, rest)) if flag == "--hosted" => (true, rest),
        _ => (false, args.as_slice()),
    };
    let only = match rest {
        [] => None,
        [flag, section] if flag == "--only" => Some(section.clone()),
        _ => bail!("usage: fragment-e2e [--hosted] [--only <section>]"),
    };
    let root = devstack::repo_root();
    let cli = std::env::var_os("FRAGMENT_BIN").map(PathBuf::from).unwrap_or_else(|| root.join("target/release/fragment"));
    if !cli.is_file() {
        bail!("no CLI at {} (cargo build --release -p fragment-cli, or set FRAGMENT_BIN)", cli.display());
    }
    if hosted {
        let scratch = root.join("target/e2e-hosted");
        std::fs::create_dir_all(&scratch)?;
        return hosted::run(cli, scratch, only);
    }
    let tools = devstack::Tools::locate()?;
    let org_key = fake::generate_org_key_pem();
    let fake = CodeStorage::start(fake::Options { org: ORG.into(), org_key_pem: Some(org_key.clone()), ..Default::default() })?;
    let scratch = root.join("target/e2e");
    std::fs::create_dir_all(&scratch)?;
    let project = devstack::stage_project(&scratch.join("cell"))?;
    let agents_project = devstack::stage_agent(&scratch.join("agent"))?;
    let run = format!("{:x}", api::now_s() % 0xffffff);
    let mut s = Suite {
        only,
        passed: 0,
        failed: vec![],
        tools,
        node: None,
        port: devstack::free_port()?,
        run,
        fake,
        openrouter: fragment_fakes::openrouter::OpenRouter::start(OPENROUTER_KEY, OPENROUTER_MANAGEMENT)?,
        push: fragment_fakes::push::PushService::start()?,
        org_key,
        host_secret: devstack::random_hex(32),
        workos: fragment_fakes::workos::WorkOs::start(WORKOS_CLIENT, WORKOS_KEY)?,
        operator: Keys::generate(),
        cli,
        scratch,
        project,
        agents_project,
        node_env_extra: vec![],
        platform_on_suffix: false,
    };
    let api = s.start(true, true)?;
    lanes::run(&mut s, api)?;
    if s.node.is_some() {
        s.stop()?;
    }
    println!("\n{} passed, {} failed", s.passed, s.failed.len());
    if !s.failed.is_empty() {
        for f in &s.failed {
            println!("  FAIL {f}");
        }
        std::process::exit(1);
    }
    Ok(())
}
