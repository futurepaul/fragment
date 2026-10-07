//! The hosted lane (docs/cloudflare-v1.md, phase 7): the suite against a
//! branch deployment (a preview, `https://<branch>.<zone>`) on real vendors:
//! WorkOS staging, code.storage, Workers AI through the gateway, and the
//! deployment's own computer image.
//!
//! `cargo xtask e2e --hosted --config <deploy config> --branch <b>` reads
//! the config and runs this binary with:
//!
//!   --hosted --zone <zone> --branch <b> [--secret-file <file>]
//!       [--operator-key-file <file> --operators <npub,...>]
//!       [--offers computers,models] [--max-paid-calls <n>]
//!       [--only <section>[,...] | --except <section>[,...]]
//!       [--dry-run | --sweep [<run>] | --sweep-all]
//!
//! - **People.** Each signs in through the preview's levers
//!   (`POST /api/test/signin`, the deployment's test secret in its header):
//!   `<name>@e2e.test`, a seat under the e2e issuer, which no real sign-in
//!   reaches. No real account, and no credential typed anywhere.
//! - **The operator.** A section that wipes a person (`Need::Operator`)
//!   signs as the deployment's operator with the key in the file
//!   `--operator-key-file` names (read when the run starts, never
//!   printed), whose npub the config's `operators` lists; it wipes only
//!   people it signed in.
//! - **Sections.** Each says what it needs (needs.rs); one that needs what a
//!   preview lacks is a skip that says why, counted. Its fragments are
//!   labelled `e2e-<run>-…`: the run's id, 6 hex digits, printed as it
//!   starts.
//! - **Money.** A person makes no paid call (a model call, an AI step)
//!   unless their section lends them some of the run's budget
//!   (`--max-paid-calls`, default 60), and their ledger refuses the one past
//!   it: a loop spends what it was lent, never more. The run ends saying
//!   what its people spent, from their ledgers.
//! - **`--dry-run`** prints the plan: the base URL, the sections it would
//!   run, and those it would skip and why. It calls nothing and reads no
//!   secret.
//! - **`--sweep [<run>]`** deletes one run's fragments on the preview (by
//!   default the last run that finished in this checkout), whatever their
//!   age, and no other; and puts its people's computers to sleep. A preview
//!   is shared: another session's run is never this sweep's.
//!   **`--sweep-all`**, for when nothing else runs there, deletes every
//!   e2e fragment at least an hour old and puts their people's computers
//!   to sleep. Either says what it kept, and why (sweep.rs).

pub mod sweep;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use fragment_core::levers;
use fragment_devstack as devstack;
use fragment_nip98::Keys;
use serde_json::{json, Value};

use crate::api::{self, Api, Preview};
use crate::needs::{Need, Offers, Rung};
use crate::{browser, Fake, Planned, Shape, Suite};

/// A hosted run's paid calls unless `--max-paid-calls` says otherwise,
/// and the most it may say: a hosted run spends test cents.
pub const MAX_PAID_CALLS_DEFAULT: u64 = 60;
pub const MAX_PAID_CALLS_MAX: u64 = 400;

const USAGE: &str = "usage: fragment-e2e [--only <section>[,...] | --except <section>[,...] | --shard <k>/<n>] [--rehearse [--max-paid-calls <n>]]
       fragment-e2e --hosted --zone <zone> --branch <branch> [--secret-file <file>] [--operator-key-file <file> --operators <npub,...>] [--offers computers,models]
                    [--max-paid-calls <n>] [--only <section>[,...] | --except <section>[,...]] [--dry-run | --sweep [<run>] | --sweep-all]";

/// The suite's command line.
#[derive(Debug, PartialEq, Eq)]
pub struct Args {
    pub only: Option<Vec<String>>,
    pub except: Vec<String>,
    /// `None`: the local run.
    pub hosted: Option<Hosted>,
    /// A rehearsal of the hosted lane on the local node, lending at most
    /// this many paid calls (`--rehearse`).
    pub rehearse: Option<u64>,
    /// A local run of one shard's sections, `k` of `--shard k/n` (the
    /// table's split: lanes/mod.rs `SHARDS`), as CI runs the suite.
    pub shard: Option<u32>,
}

/// A hosted run's settings.
#[derive(Debug, PartialEq, Eq)]
pub struct Hosted {
    pub preview: Preview,
    /// The file holding the deployment's test secret: read when the run
    /// starts, never printed, never on a command line.
    pub secret_file: Option<PathBuf>,
    /// The file holding an operator key the deployment lists
    /// (`--operator-key-file`): read when the run starts, never printed.
    pub operator_key_file: Option<PathBuf>,
    /// The deployment's operators (its config's `operators`, public).
    pub operators: Vec<String>,
    /// The deployment makes computers (its config's `computers`).
    pub computers: bool,
    /// The deployment calls models (its config's `ai_gateway`).
    pub models: bool,
    /// The paid calls the run may lend its people, in all.
    pub max_paid_calls: u64,
    pub action: Action,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Run,
    DryRun,
    Sweep(SweepOf),
}

/// What a sweep removes, as its command line names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SweepOf {
    /// `--sweep`: the last run that finished in this checkout.
    LastRun,
    /// `--sweep <run>`: that run's.
    Run(String),
    /// `--sweep-all`: every e2e fragment at least `sweep::SPARED_FOR_MS` old.
    All,
}

/// Reads the suite's arguments: the local run's `--only`/`--except`, or a
/// hosted run's (`--hosted` and its own).
pub fn parse(args: &[String]) -> Result<Args> {
    let usage = || anyhow::anyhow!("{USAGE}");
    let list = |names: &str| names.split(',').filter(|n| !n.is_empty()).map(str::to_string).collect::<Vec<_>>();
    let (mut only, mut except) = (None, vec![]);
    let (mut hosted, mut zone, mut branch, mut secret_file, mut offers, mut max_paid_calls) = (false, None, None, None, None, None);
    let (mut operator_key_file, mut operators) = (None, vec![]);
    let (mut dry_run, mut sweep, mut rehearse) = (false, None, false);
    let mut shard = None;
    let mut it = args.iter().peekable();
    // bounded: each pass takes one argument at least
    while let Some(arg) = it.next() {
        let mut value = || it.next().cloned().ok_or_else(usage);
        match arg.as_str() {
            "--only" if only.is_none() && except.is_empty() && shard.is_none() => only = Some(list(&value()?)),
            "--except" if only.is_none() && except.is_empty() && shard.is_none() => except = list(&value()?),
            "--shard" if only.is_none() && except.is_empty() && shard.is_none() => shard = Some(parse_shard(&value()?)?),
            "--hosted" => hosted = true,
            "--zone" => zone = Some(value()?),
            "--branch" => branch = Some(value()?),
            "--secret-file" => secret_file = Some(PathBuf::from(value()?)),
            "--operator-key-file" => operator_key_file = Some(PathBuf::from(value()?)),
            "--operators" => operators = list(&value()?),
            "--offers" => offers = Some(value()?),
            "--max-paid-calls" => max_paid_calls = Some(value()?.parse::<u64>().map_err(|_| anyhow::anyhow!("--max-paid-calls is a whole number"))?),
            "--dry-run" => dry_run = true,
            // its run is the next argument, when that is not another flag
            "--sweep" if sweep.is_none() => {
                sweep = Some(match it.next_if(|next| !next.starts_with("--")) {
                    Some(run) => SweepOf::Run(sweep_run(run)?),
                    None => SweepOf::LastRun,
                })
            }
            "--sweep-all" if sweep.is_none() => sweep = Some(SweepOf::All),
            "--rehearse" => rehearse = true,
            _ => return Err(usage()),
        }
    }
    if only.as_ref().is_some_and(Vec::is_empty) {
        bail!("--only names at least one section");
    }
    let max_paid_calls_or_default = || -> Result<u64> {
        let n = max_paid_calls.unwrap_or(MAX_PAID_CALLS_DEFAULT);
        anyhow::ensure!(n <= MAX_PAID_CALLS_MAX, "--max-paid-calls is at most {MAX_PAID_CALLS_MAX}: a hosted run spends test cents");
        Ok(n)
    };
    if !hosted {
        let hosted_only = zone.is_some() || branch.is_some() || secret_file.is_some() || offers.is_some() || dry_run || sweep.is_some() || operator_key_file.is_some() || !operators.is_empty();
        if hosted_only || (max_paid_calls.is_some() && !rehearse) {
            bail!("--zone, --branch, --secret-file, --operator-key-file, --operators, --offers, --dry-run, --sweep and --sweep-all are a hosted run's (--hosted), and --max-paid-calls a hosted run's or a rehearsal's\n{USAGE}");
        }
        if rehearse && shard.is_some() {
            // a rehearsal ends with its sweep, which belongs to no shard
            bail!("--shard splits the local run as CI runs it: not with --rehearse");
        }
        let rehearse = match rehearse {
            true => Some(max_paid_calls_or_default()?),
            false => None,
        };
        return Ok(Args { only, except, hosted: None, rehearse, shard });
    }
    if rehearse {
        bail!("--rehearse is the hosted lane on the local node: not with --hosted");
    }
    if shard.is_some() {
        bail!("--shard is a local run's, as CI runs it: not with --hosted");
    }
    let (Some(zone), Some(branch)) = (zone, branch) else { bail!("a hosted run names its preview: --zone and --branch\n{USAGE}") };
    if !devstack::valid_branch(&branch) {
        bail!("a branch is 1-16 of a-z, 0-9 and single dashes inside, not {branch:?}");
    }
    if zone.is_empty() || !zone.contains('.') || !zone.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'-') {
        bail!("--zone is a domain (finite.place), not {zone:?}");
    }
    let action = match (dry_run, sweep) {
        (true, Some(_)) => bail!("--dry-run and a sweep are two runs"),
        (true, None) => Action::DryRun,
        (false, Some(of)) => Action::Sweep(of),
        (false, None) => Action::Run,
    };
    if matches!(action, Action::Sweep(_)) && (only.is_some() || !except.is_empty()) {
        bail!("a sweep runs no sections: no --only or --except");
    }
    if action != Action::DryRun && secret_file.is_none() {
        bail!("a hosted run signs its people in through the preview's levers: --secret-file names the deployment's test secret (its config's test_secret_file)");
    }
    let (mut computers, mut models) = (false, false);
    for offer in offers.as_deref().map(list).unwrap_or_default() {
        match offer.as_str() {
            "computers" => computers = true,
            "models" => models = true,
            other => bail!("--offers is computers and models, not {other:?}"),
        }
    }
    if operator_key_file.is_some() && operators.is_empty() {
        bail!("--operator-key-file needs --operators: the deployment's (its config's `operators`), among which its key is");
    }
    let max_paid_calls = max_paid_calls_or_default()?;
    let preview = Preview::new(&zone, &branch);
    let hosted = Hosted { preview, secret_file, operator_key_file, operators, computers, models, max_paid_calls, action };
    Ok(Args { only, except, hosted: Some(hosted), rehearse: None, shard: None })
}

/// `--sweep <run>`'s run: the id a run prints as it starts, the 6 hex
/// digits after `e2e-` in its labels.
fn sweep_run(text: &str) -> Result<String> {
    if sweep::is_run(text) {
        Ok(text.to_string())
    } else {
        bail!("--sweep <run> names a run by its id, {} hex digits (the c58b2a of e2e-c58b2a-todo), not {text:?}", sweep::RUN_HEX)
    }
}

/// `--shard k/n`: shard `k` (from 1) of the table's split, whose size `n`
/// must be (so CI's matrix and the table cannot drift apart unnoticed).
fn parse_shard(text: &str) -> Result<u32> {
    let n = crate::lanes::SHARDS.len();
    let parsed = text.split_once('/').and_then(|(k, of)| Some((k.parse::<u32>().ok()?, of.parse::<usize>().ok()?)));
    match parsed {
        Some((k, of)) if of == n && (1..=n as u32).contains(&k) => Ok(k),
        _ => bail!("--shard is k/{n}, 1 <= k <= {n} (the e2e's table, lanes/mod.rs SHARDS, splits the suite in {n}), not {text:?}"),
    }
}

/// A hosted run: its plan, its sweep, or its sections.
pub fn run(only: Option<Vec<String>>, except: Vec<String>, hosted: Hosted) -> Result<()> {
    match hosted.action {
        Action::DryRun => {
            let (planned, unknown) = plan(only, except, &hosted);
            print!("{}", render(&hosted, &planned, &unknown));
            anyhow::ensure!(unknown.is_empty(), "no section is named {}", unknown.join(", "));
            Ok(())
        }
        Action::Sweep(ref of) => sweep(&hosted, of),
        Action::Run => sections(only, except, hosted),
    }
}

/// What a hosted run would have: levers when it has a secret, computers
/// and models when its deployment offers them (models only with paid calls
/// to lend), a real agent when it has both (its computers run the
/// deployment's own image on its real model), and Chrome when it is
/// installed.
fn offers(hosted: &Hosted) -> Offers {
    let models = hosted.models && hosted.max_paid_calls > 0;
    let operator = hosted.operator_key_file.is_some();
    Offers { levers: hosted.secret_file.is_some(), computers: hosted.computers, models, chrome: browser::chrome().is_some(), real_agent: hosted.computers && models, operator }
}

/// A hosted suite: no node and no fakes, the preview's API, its levers'
/// secret in `shared`.
fn suite(only: Option<Vec<String>>, except: Vec<String>, hosted: &Hosted, shared: Arc<api::Run>, cli: PathBuf, scratch: PathBuf) -> Suite {
    Suite {
        only,
        except,
        shard: None,
        rung: Rung::Hosted(offers(hosted)),
        hosted_rules: true,
        preview: Some(hosted.preview.clone()),
        plan: None,
        shared,
        owners: Default::default(),
        asked: vec![],
        passed: 0,
        failed: vec![],
        skipped: vec![],
        ran: vec![],
        recovery_due: false,
        lost: None,
        cron: None,
        chrome: browser::Shared::new(&scratch),
        tools: None,
        node: None,
        port: 0,
        run: crate::run_name(),
        fake: Fake::absent("code.storage"),
        ai: Fake::absent("Workers AI"),
        push: Fake::absent("push service"),
        org_key: String::new(),
        host_secret: String::new(),
        test_secret: String::new(),
        workos: Fake::absent("WorkOS"),
        upstream: Fake::absent("upstream"),
        // no operator the deployment names: a section that needs one declares Need::Deployment
        operator: Keys::generate(),
        // read as the run starts (`sections`): a dry run reads no secret
        wiper: None,
        cli,
        scratch,
        project: PathBuf::new(),
        shape: Shape::Plain,
        containers_removed: false,
    }
}

/// The plan of a hosted run: each section the lanes ask for, in order,
/// with what it needs and why it would be skipped; and the names `--only`
/// or `--except` gave that no section has. Nothing is called.
pub fn plan(only: Option<Vec<String>>, except: Vec<String>, hosted: &Hosted) -> (Vec<Planned>, Vec<String>) {
    // never sent: a dry run makes no request
    let shared = api::Run::signing_in_by_levers("0".repeat(levers::SECRET_BYTES_MIN), 0);
    let mut s = suite(only, except, hosted, shared, PathBuf::new(), devstack::repo_root().join("target/e2e/plan"));
    s.plan = Some(vec![]);
    crate::lanes::run(&mut s);
    let named = s.only.clone().unwrap_or_default().into_iter().chain(s.except.clone());
    let unknown = named.filter(|n| !s.asked.contains(n)).collect();
    (s.plan.take().unwrap_or_default(), unknown)
}

/// The plan as the dry run prints it.
pub fn render(hosted: &Hosted, planned: &[Planned], unknown: &[String]) -> String {
    let o = offers(hosted);
    let offered: Vec<&str> =
        [(o.levers, "levers"), (o.computers, "computers"), (o.models, "models"), (o.real_agent, "real-agent"), (o.chrome, "chrome"), (o.operator, "operator")]
            .iter()
            .filter(|(on, _)| *on)
            .map(|(_, n)| *n)
            .collect();
    let needs = |n: &[Need]| match n.is_empty() {
        true => "nothing local".to_string(),
        false => n.iter().map(|n| n.name()).collect::<Vec<_>>().join(", "),
    };
    let width = planned.iter().map(|p| p.section.len()).max().unwrap_or(0);
    let mut out = format!("the hosted plan for {} (a dry run: nothing is called, no secret is read)\n", hosted.preview.branch);
    out += &format!("  platform     {}\n", hosted.preview.platform());
    out += &format!("  fragments    https://<label>--<username>--{}.{}/ (labels e2e-<run>-…, the run's id printed as it starts)\n", hosted.preview.branch, hosted.preview.zone);
    out += &match &hosted.secret_file {
        Some(file) => format!("  sign-in      e2e people (<name>@e2e.test) through the levers, the test secret read from {} when the run starts\n", file.display()),
        None => "  sign-in      none: no --secret-file, so no one can sign in and nothing needing the levers runs\n".to_string(),
    };
    if let Some(file) = &hosted.operator_key_file {
        out += &format!("  operator     the key in {} (read when the run starts), among the config's operators\n", file.display());
    }
    out += &format!("  offers       {}\n", if offered.is_empty() { "nothing".to_string() } else { offered.join(", ") });
    out += &format!("  paid calls   at most {} in all, lent to a person as they sign in (--max-paid-calls); each refused past its lending\n", hosted.max_paid_calls);
    let (runs, skips): (Vec<&Planned>, Vec<&Planned>) = planned.iter().partition(|p| p.skip.is_none());
    out += &format!("\nwould run ({}):\n", runs.len());
    for p in &runs {
        out += &format!("  {:width$}  needs {}\n", p.section, needs(&p.needs));
    }
    out += &format!("\nwould skip ({}):\n", skips.len());
    for p in &skips {
        out += &format!("  {:width$}  {}\n", p.section, p.skip.as_deref().unwrap_or(""));
    }
    for name in unknown {
        out += &format!("\nno section is named {name}\n");
    }
    out
}

/// The deployment's test secret, from its file: checked as the cell checks
/// it, and never printed.
fn read_secret(file: &Path) -> Result<String> {
    let text = std::fs::read_to_string(file).with_context(|| format!("read the test secret file {}", file.display()))?;
    let secret = text.trim().to_string();
    levers::check(&secret).map_err(|why| anyhow::anyhow!("the test secret in {}: {}", file.display(), why.message()))?;
    Ok(secret)
}

/// The deployment's operator key, from its file: a key whose npub the
/// deployment's operators list, read once and never printed.
fn read_operator(file: &Path, operators: &[String]) -> Result<Keys> {
    let text = std::fs::read_to_string(file).with_context(|| format!("read the operator key file {}", file.display()))?;
    let keys = Keys::from_secret_hex(text.trim()).with_context(|| format!("{} holds no key (64 hex: `fragment operator key` makes one)", file.display()))?;
    let listed = operators.iter().filter_map(|o| fragment_core::npub::parse(o)).any(|k| k == keys.pubkey_hex());
    anyhow::ensure!(listed, "the key in {} is none the deployment's operators list ({}): add its npub, {}, and deploy", file.display(), operators.join(", "), fragment_core::npub::encode(keys.pubkey_hex()));
    Ok(keys)
}

/// The preview answers, and its levers take the secret: answers its deploy.
fn preflight(api: &Api) -> Result<String> {
    // a route just deployed answers 52x at Cloudflare's edge for a moment:
    // bounded, one look every 5 s for a minute
    let mut health = api.unsigned("GET", "/healthz", None)?;
    for _ in 0..12 {
        if health.status == 200 || !(520..=530).contains(&health.status) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_secs(5));
        health = api.unsigned("GET", "/healthz", None)?;
    }
    anyhow::ensure!(health.status == 200, "{}/healthz: {health}", api.base);
    let deploy = health.header("x-fragment-deploy");
    let r = api.unsigned("POST", "/api/test/people", Some(&json!({})))?;
    anyhow::ensure!(
        r.status == 200,
        "the preview's levers did not take the secret ({}): deploy it with the config's test_secret_file, and run with the same file",
        r.status
    );
    Ok(deploy)
}

/// The sections, on the preview.
fn sections(only: Option<Vec<String>>, except: Vec<String>, hosted: Hosted) -> Result<()> {
    let secret = read_secret(hosted.secret_file.as_deref().context("a hosted run names its secret file")?)?;
    let shared = api::Run::signing_in_by_levers(secret, hosted.max_paid_calls);
    let deploy = preflight(&Api::hosted(&hosted.preview, &shared))?;
    let cli = crate::cli_binary()?;
    let mut s = suite(only, except, &hosted, shared, cli, PathBuf::new());
    s.wiper = hosted.operator_key_file.as_deref().map(|file| read_operator(file, &hosted.operators)).transpose()?;
    let run = s.run.clone();
    println!("hosted: {} (deploy {deploy}), run {run} (its fragments e2e-{run}-…); at most {} paid calls", hosted.preview.platform(), hosted.max_paid_calls);
    s.scratch = devstack::repo_root().join("target/e2e").join(format!("hosted-{run}"));
    std::fs::create_dir_all(&s.scratch)?;
    s.chrome = browser::Shared::new(&s.scratch);
    crate::lanes::run(&mut s);
    // noted as it finishes, so `--sweep` never takes a run still going here
    let noted = sweep::note_last_run(&hosted.preview, &run);
    let finished = crate::finish(&mut s);
    match noted {
        Ok(()) => println!("run {run}'s fragments stay for a look: `--sweep` deletes them (this run's alone; `--sweep {run}` names it)"),
        Err(e) => println!("run {run}'s fragments stay; `--sweep {run}` deletes them (it could not be noted for `--sweep`: {e:#})"),
    }
    finished
}

/// What the run's people spent, from their ledgers: each one's model
/// calls, AI steps, and everything charged (awake time and storage too).
/// Their paid calls are never more than the run lent them: a check.
pub fn spent(s: &mut Suite) {
    let api = s.api();
    let people = s.shared.people();
    // a person a lane wiped has no ledger left to read (it refuses: wiped)
    let wiped = s.shared.wiped();
    let (mut models, mut steps, mut charged, mut unread) = (0usize, 0usize, 0i64, 0usize);
    // bounded: the people this run signed in
    for identity in people.iter().filter(|p| !wiped.contains(p)) {
        let ask = |body: Value| api.unsigned("POST", "/api/test/ledger", Some(&body)).ok().filter(|r| r.status == 200).map(|r| r.body);
        let counted = |prefix: &str| ask(json!({ "identity": identity, "op": "entries", "prefix": prefix })).and_then(|b| b["entries"].as_array().map(Vec::len));
        match (ask(json!({ "identity": identity, "op": "totals" })), counted("aig:"), counted("step:")) {
            (Some(totals), Some(m), Some(st)) => {
                charged += totals["charged"].as_i64().unwrap_or(0);
                models += m;
                steps += st;
            }
            _ => unread += 1,
        }
    }
    let lent = s.shared.paid_calls_lent();
    println!(
        "      (spent by the run's {} people, from their ledgers: {models} model calls, {steps} AI steps, ${:.4} charged in all; {lent} paid calls lent of {}{}{})",
        people.len(),
        charged as f64 / fragment_core::price::USD as f64,
        lent + s.shared.paid_calls_left(),
        if wiped.is_empty() { String::new() } else { format!("; {} wiped, lent nothing, their ledgers gone with them", wiped.len()) },
        if unread > 0 { format!("; {unread} ledgers did not answer") } else { String::new() },
    );
    let paid = (models + steps) as u64;
    s.ok(
        "the run's people made no more paid calls than it lent them, as their ledgers keep them",
        unread == 0 && paid <= lent,
        format!("{paid} paid calls, {lent} lent; {unread} ledgers did not answer"),
    );
}

/// A sweep of the preview (sweep.rs): one run's fragments, or
/// (`--sweep-all`) every e2e fragment old enough; it says what it kept.
fn sweep(hosted: &Hosted, of: &SweepOf) -> Result<()> {
    let scope = match of {
        SweepOf::LastRun => sweep::Scope::Run(sweep::last_run(&hosted.preview)?),
        SweepOf::Run(run) => sweep::Scope::Run(run.clone()),
        SweepOf::All => sweep::Scope::All { spared_for_ms: sweep::SPARED_FOR_MS },
    };
    let secret = read_secret(hosted.secret_file.as_deref().context("a sweep names the secret file")?)?;
    let shared = api::Run::signing_in_by_levers(secret, 0);
    let api = Api::hosted(&hosted.preview, &shared);
    preflight(&api)?;
    let platform = hosted.preview.platform();
    match &scope {
        sweep::Scope::Run(run) => println!("sweeping run {run}'s fragments (e2e-{run}-…) on {platform}, and no other"),
        sweep::Scope::All { spared_for_ms } => println!("sweeping every e2e fragment on {platform} but those younger than {} min", spared_for_ms / 60_000),
    }
    let swept = sweep::sweep_on(&api, &scope, api::now_ms())?;
    println!("swept {platform}: {} e2e people, {} fragments deleted, {} computers put to sleep", swept.people, swept.deleted, swept.slept);
    println!("{}", swept.report());
    Ok(())
}

/// A rehearsal's last section, `sweep`, on its node: the run's sweep
/// deletes the run's fragments and keeps another run's, made just now;
/// a second finds nothing of the run's left; the whole sweep spares the
/// other run's for their age, and says so; then a sweep naming the other
/// run, and a whole sweep sparing nothing, delete the rest. A hosted run
/// sweeps by `--sweep`.
pub fn rehearse_sweep(s: &mut Suite) {
    if !s.section("sweep", &[Need::Levers]) {
        return;
    }
    if let Err(e) = rehearse_sweeps(s) {
        s.fail("the sweeps", format!("{e:#}"));
    }
}

fn rehearse_sweeps(s: &mut Suite) -> Result<()> {
    use sweep::{Kept, Scope};
    let api = s.api();
    // another run's fragment, and one of an older layout, whose label names
    // no run: a person of their own makes them now
    let other = if s.run == "abcdef" { "fedcba" } else { "abcdef" };
    let stranger = api.person_paying(0)?;
    let theirs = api.qualified(&stranger, &sweep::label(other, "decoy"))?;
    let older = api.qualified(&stranger, "e2e-decoy-older")?;
    for label in [sweep::label(other, "decoy"), "e2e-decoy-older".to_string()] {
        let r = api.create(&stranger, &label)?;
        anyhow::ensure!(r.status == 200, "making {label}: {r}");
    }
    let there = |name: &str| api.status(&stranger, name).map(|r| r.status);
    let decoys = |swept: &sweep::Swept| {
        let mut spared = swept.spared.clone();
        spared.sort();
        spared
    };
    let kept_older = (older.clone(), Kept::OtherRun(None));
    let mut other_runs = vec![kept_older.clone(), (theirs.clone(), Kept::OtherRun(Some(other.to_string())))];
    other_runs.sort();

    let ours = Scope::Run(s.run.clone());
    let first = sweep::sweep_on(&api, &ours, api::now_ms())?;
    s.ok(
        "the run's sweep signs each e2e person in again and deletes the run's fragments, and keeps another run's, by their labels",
        first.people > 0 && first.deleted > 0 && decoys(&first) == other_runs,
        format!("{first:?}"),
    );
    let again = sweep::sweep_on(&api, &ours, api::now_ms())?;
    s.ok(
        "a second finds none of the run's left, nor an awake computer of its people",
        again.deleted == 0 && again.slept == 0 && again.people >= first.people && decoys(&again) == other_runs && there(&theirs)? == 200,
        format!("{again:?}"),
    );
    let all = sweep::sweep_on(&api, &Scope::All { spared_for_ms: sweep::SPARED_FOR_MS }, api::now_ms())?;
    let young = all.spared_for(|k| matches!(k, Kept::Young(age) if (0..sweep::SPARED_FOR_MS).contains(age)));
    s.ok(
        "the whole sweep spares another run's fragments made within the hour, and says so",
        all.deleted == 0 && young.len() == 2 && all.spared.len() == 2 && all.report().contains("2 younger than 60 min") && there(&older)? == 200,
        format!("{all:?}\n{}", all.report()),
    );
    let named = sweep::sweep_on(&api, &Scope::Run(other.to_string()), api::now_ms())?;
    s.ok(
        "a sweep naming the other run deletes its fragment alone",
        named.deleted == 1 && decoys(&named) == [kept_older] && there(&theirs)? == 404 && there(&older)? == 200,
        format!("{named:?}"),
    );
    let rest = sweep::sweep_on(&api, &Scope::All { spared_for_ms: 0 }, api::now_ms())?;
    s.ok("and a whole sweep sparing nothing deletes the rest", rest.deleted == 1 && rest.spared.is_empty() && there(&older)? == 404, format!("{rest:?}"));
    Ok(())
}

#[cfg(test)]
mod tests;
