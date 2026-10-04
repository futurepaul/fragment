//! The hosted lane (docs/cloudflare-v1.md, phase 7): the suite against a
//! branch deployment (a preview, `https://<branch>.<zone>`) on real vendors:
//! WorkOS staging, code.storage, Workers AI through the gateway, and the
//! deployment's own computer image.
//!
//! `cargo xtask e2e --hosted --config <deploy config> --branch <b>` reads
//! the config and runs this binary with:
//!
//!   --hosted --zone <zone> --branch <b> [--secret-file <file>]
//!       [--offers computers,models] [--max-paid-calls <n>]
//!       [--only <section>[,...] | --except <section>[,...]] [--dry-run | --sweep]
//!
//! - **People.** Each signs in through the preview's levers
//!   (`POST /api/test/signin`, the deployment's test secret in its header):
//!   `<name>@e2e.test`, a seat under the e2e issuer, which no real sign-in
//!   reaches. No real account, and no credential typed anywhere.
//! - **Sections.** Each says what it needs (needs.rs); one that needs what a
//!   preview lacks is a skip that says why, counted. Its fragments are
//!   named `e2e-…`.
//! - **Money.** A person makes no paid call (a model call, an AI step)
//!   unless their section lends them some of the run's budget
//!   (`--max-paid-calls`, default 60), and their ledger refuses the one past
//!   it: a loop spends what it was lent, never more. The run ends saying
//!   what its people spent, from their ledgers.
//! - **`--dry-run`** prints the plan: the base URL, the sections it would
//!   run, and those it would skip and why. It calls nothing and reads no
//!   secret.
//! - **`--sweep`** deletes the e2e people's `e2e-…` fragments on the
//!   preview, and puts their computers to sleep.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use fragment_core::levers;
use fragment_devstack as devstack;
use fragment_nip98::Keys;
use serde_json::{json, Value};

use crate::api::{self, Api, Call, Preview, Reply};
use devstack::summary::Shard;
use crate::needs::{Need, Offers, Rung};
use crate::{browser, Fake, Planned, Shape, Suite};

/// A hosted run's paid calls unless `--max-paid-calls` says otherwise,
/// and the most it may say: a hosted run spends test cents.
pub const MAX_PAID_CALLS_DEFAULT: u64 = 60;
pub const MAX_PAID_CALLS_MAX: u64 = 400;
/// The pages of e2e people one sweep walks at most (a page is 100).
const SWEEP_PAGES_MAX: usize = 100;

const USAGE: &str = "usage: fragment-e2e [--only <section>[,...] | --except <section>[,...] | --shard <k>/<n>] [--rehearse [--max-paid-calls <n>]]
                    [--summary <file>]
       fragment-e2e --hosted --zone <zone> --branch <branch> [--secret-file <file>] [--offers computers,models]
                    [--max-paid-calls <n>] [--only <section>[,...] | --except <section>[,...]] [--dry-run | --sweep]";

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
    /// A local run of one shard's sections (`--shard k/n`, the table's
    /// split: lanes/mod.rs `SHARDS`), as CI runs the suite.
    pub shard: Option<Shard>,
    /// Where a local run writes its summary as it ends (`--summary`),
    /// which `cargo xtask e2e-summary` combines with its other shards'.
    pub summary: Option<PathBuf>,
}

/// A hosted run's settings.
#[derive(Debug, PartialEq, Eq)]
pub struct Hosted {
    pub preview: Preview,
    /// The file holding the deployment's test secret: read when the run
    /// starts, never printed, never on a command line.
    pub secret_file: Option<PathBuf>,
    /// The deployment makes computers (its config's `computers`).
    pub computers: bool,
    /// The deployment calls models (its config's `ai_gateway`).
    pub models: bool,
    /// The paid calls the run may lend its people, in all.
    pub max_paid_calls: u64,
    pub action: Action,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Run,
    DryRun,
    Sweep,
}

/// Reads the suite's arguments: the local run's `--only`/`--except`, or a
/// hosted run's (`--hosted` and its own).
pub fn parse(args: &[String]) -> Result<Args> {
    let usage = || anyhow::anyhow!("{USAGE}");
    let list = |names: &str| names.split(',').filter(|n| !n.is_empty()).map(str::to_string).collect::<Vec<_>>();
    let (mut only, mut except) = (None, vec![]);
    let (mut hosted, mut zone, mut branch, mut secret_file, mut offers, mut max_paid_calls) = (false, None, None, None, None, None);
    let (mut dry_run, mut sweep, mut rehearse) = (false, false, false);
    let (mut shard, mut summary) = (None, None);
    let mut it = args.iter();
    // bounded: each pass takes one argument at least
    while let Some(arg) = it.next() {
        let mut value = || it.next().cloned().ok_or_else(usage);
        match arg.as_str() {
            "--only" if only.is_none() && except.is_empty() && shard.is_none() => only = Some(list(&value()?)),
            "--except" if only.is_none() && except.is_empty() && shard.is_none() => except = list(&value()?),
            "--shard" if only.is_none() && except.is_empty() && shard.is_none() => shard = Some(parse_shard(&value()?)?),
            "--summary" if summary.is_none() => summary = Some(PathBuf::from(value()?)),
            "--hosted" => hosted = true,
            "--zone" => zone = Some(value()?),
            "--branch" => branch = Some(value()?),
            "--secret-file" => secret_file = Some(PathBuf::from(value()?)),
            "--offers" => offers = Some(value()?),
            "--max-paid-calls" => max_paid_calls = Some(value()?.parse::<u64>().map_err(|_| anyhow::anyhow!("--max-paid-calls is a whole number"))?),
            "--dry-run" => dry_run = true,
            "--sweep" => sweep = true,
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
        let hosted_only = zone.is_some() || branch.is_some() || secret_file.is_some() || offers.is_some() || dry_run || sweep;
        if hosted_only || (max_paid_calls.is_some() && !rehearse) {
            bail!("--zone, --branch, --secret-file, --offers, --dry-run and --sweep are a hosted run's (--hosted), and --max-paid-calls a hosted run's or a rehearsal's\n{USAGE}");
        }
        if rehearse && shard.is_some() {
            // a rehearsal ends with its sweep, which belongs to no shard
            bail!("--shard splits the local run as CI runs it: not with --rehearse");
        }
        let rehearse = match rehearse {
            true => Some(max_paid_calls_or_default()?),
            false => None,
        };
        return Ok(Args { only, except, hosted: None, rehearse, shard, summary });
    }
    if rehearse {
        bail!("--rehearse is the hosted lane on the local node: not with --hosted");
    }
    if shard.is_some() || summary.is_some() {
        bail!("--shard and --summary are a local run's, as CI runs it: not with --hosted");
    }
    let (Some(zone), Some(branch)) = (zone, branch) else { bail!("a hosted run names its preview: --zone and --branch\n{USAGE}") };
    if !devstack::valid_branch(&branch) {
        bail!("a branch is 1-16 of a-z, 0-9 and single dashes inside, not {branch:?}");
    }
    if zone.is_empty() || !zone.contains('.') || !zone.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'-') {
        bail!("--zone is a domain (finite.place), not {zone:?}");
    }
    let action = match (dry_run, sweep) {
        (true, true) => bail!("--dry-run and --sweep are two runs"),
        (true, false) => Action::DryRun,
        (false, true) => Action::Sweep,
        (false, false) => Action::Run,
    };
    if action == Action::Sweep && (only.is_some() || !except.is_empty()) {
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
    let max_paid_calls = max_paid_calls_or_default()?;
    let preview = Preview::new(&zone, &branch);
    Ok(Args { only, except, hosted: Some(Hosted { preview, secret_file, computers, models, max_paid_calls, action }), rehearse: None, shard: None, summary: None })
}

/// `--shard k/n`: shard `k` of the table's split, whose size `n` must be
/// (so CI's matrix and the table cannot drift apart unnoticed).
fn parse_shard(text: &str) -> Result<Shard> {
    let shard = Shard::parse(text).ok_or_else(|| anyhow::anyhow!("--shard is k/n, 1 <= k <= n, not {text:?}"))?;
    let n = crate::lanes::SHARDS.len();
    anyhow::ensure!(shard.n as usize == n, "--shard {shard}: the e2e's table (lanes/mod.rs SHARDS) splits the suite in {n}, not {}", shard.n);
    Ok(shard)
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
        Action::Sweep => sweep(&hosted),
        Action::Run => sections(only, except, hosted),
    }
}

/// What a hosted run would have: levers when it has a secret, computers
/// and models when its deployment offers them (models only with paid calls
/// to lend), and Chrome when it is installed.
fn offers(hosted: &Hosted) -> Offers {
    Offers {
        levers: hosted.secret_file.is_some(),
        computers: hosted.computers,
        models: hosted.models && hosted.max_paid_calls > 0,
        chrome: browser::chrome().is_some(),
    }
}

/// A hosted suite: no node and no fakes, the preview's API, its levers'
/// secret in `shared`.
fn suite(only: Option<Vec<String>>, except: Vec<String>, hosted: &Hosted, shared: Arc<api::Run>, cli: PathBuf, scratch: PathBuf) -> Suite {
    Suite {
        only,
        except,
        shard: None,
        summary: None,
        accounted: vec![],
        outside: Default::default(),
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
        celld: None,
        renderer: None,
        nodes: vec![],
        node_images: serde_json::Value::Null,
        image_tags: vec![],
        port: 0,
        run: crate::run_name(),
        fake: Fake::absent("code.storage"),
        store: None,
        macrofiche: None,
        org: String::new(),
        ai: Fake::absent("Workers AI"),
        push: Fake::absent("push service"),
        org_key: String::new(),
        host_secret: String::new(),
        test_secret: String::new(),
        workos: Fake::absent("WorkOS"),
        oidc: Fake::absent("OpenID Connect"),
        upstream: Fake::absent("upstream"),
        // no operator the deployment names: a section that needs one declares Need::Deployment
        operator: Keys::generate(),
        cli,
        scratch,
        project: PathBuf::new(),
        agents_project: PathBuf::new(),
        shape: Shape::Plain,
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
    let offered: Vec<&str> = [(o.levers, "levers"), (o.computers, "computers"), (o.models, "models"), (o.chrome, "chrome")].iter().filter(|(on, _)| *on).map(|(_, n)| *n).collect();
    let needs = |n: &[Need]| match n.is_empty() {
        true => "nothing local".to_string(),
        false => n.iter().map(|n| n.name()).collect::<Vec<_>>().join(", "),
    };
    let width = planned.iter().map(|p| p.section.len()).max().unwrap_or(0);
    let mut out = format!("the hosted plan for {} (a dry run: nothing is called, no secret is read)\n", hosted.preview.branch);
    out += &format!("  platform     {}\n", hosted.preview.platform());
    out += &format!("  fragments    https://<label>--<username>--{}.{}/ (labels e2e-…)\n", hosted.preview.branch, hosted.preview.zone);
    out += &match &hosted.secret_file {
        Some(file) => format!("  sign-in      e2e people (<name>@e2e.test) through the levers, the test secret read from {} when the run starts\n", file.display()),
        None => "  sign-in      none: no --secret-file, so no one can sign in and nothing needing the levers runs\n".to_string(),
    };
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
    println!("hosted: {} (deploy {deploy}); at most {} paid calls", hosted.preview.platform(), hosted.max_paid_calls);
    let cli = crate::cli_binary()?;
    let mut s = suite(only, except, &hosted, shared, cli, PathBuf::new());
    s.scratch = devstack::repo_root().join("target/e2e").join(format!("hosted-{}", s.run));
    std::fs::create_dir_all(&s.scratch)?;
    s.chrome = browser::Shared::new(&s.scratch);
    crate::lanes::run(&mut s);
    crate::finish(&mut s)
}

/// What the run's people spent, from their ledgers: each one's model
/// calls, AI steps, and everything charged (awake time and storage too).
/// Their paid calls are never more than the run lent them: a check.
pub fn spent(s: &mut Suite) {
    let api = s.api();
    let people = s.shared.people();
    let (mut models, mut steps, mut charged, mut unread) = (0usize, 0usize, 0i64, 0usize);
    // bounded: the people this run signed in
    for identity in &people {
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
        "      (spent by the run's {} people, from their ledgers: {models} model calls, {steps} AI steps, ${:.4} charged in all; {lent} paid calls lent of {}{})",
        people.len(),
        charged as f64 / fragment_core::price::USD as f64,
        lent + s.shared.paid_calls_left(),
        if unread > 0 { format!("; {unread} ledgers did not answer") } else { String::new() },
    );
    let paid = (models + steps) as u64;
    s.ok(
        "the run's people made no more paid calls than it lent them, as their ledgers keep them",
        unread == 0 && paid <= lent,
        format!("{paid} paid calls, {lent} lent; {unread} ledgers did not answer"),
    );
}

/// A call as the shell makes it, with a platform session (the sweep's
/// people have no key).
fn shell(api: &Api, session: &str, method: &str, path: &str) -> Result<Reply> {
    api.call(Call {
        method,
        url: format!("{}{path}", api.base),
        cookie: Some(format!("fragment_session={session}")),
        extra: vec![("x-fragment-shell", "1".into()), ("sec-fetch-site", "same-origin".into()), ("origin", api.base.clone())],
        ..Call::default()
    })
}

/// Deletes every e2e person's `e2e-…` fragments on the preview, and puts
/// their computers to sleep. Each person signs in again (a session, no
/// paid calls) and acts as themself: the sweep has no power of its own.
fn sweep(hosted: &Hosted) -> Result<()> {
    let secret = read_secret(hosted.secret_file.as_deref().context("a sweep names the secret file")?)?;
    let shared = api::Run::signing_in_by_levers(secret, 0);
    let api = Api::hosted(&hosted.preview, &shared);
    preflight(&api)?;
    let swept = sweep_on(&api)?;
    println!(
        "swept {}: {} e2e people, {} e2e- fragments deleted, {} others kept, {} computers put to sleep",
        hosted.preview.platform(),
        swept.people,
        swept.deleted,
        swept.kept,
        swept.slept
    );
    Ok(())
}

/// A rehearsal's last section, `sweep`: on its node, the sweep deletes the
/// run's e2e fragments (each e2e person's, by the label alone) and leaves
/// none, and a second finds nothing to do. A hosted run sweeps by `--sweep`.
pub fn rehearse_sweep(s: &mut Suite) {
    if !s.section("sweep", &[Need::Levers]) {
        return;
    }
    let api = s.api();
    let swept = sweep_on(&api).and_then(|first| Ok((first, sweep_on(&api)?)));
    match swept {
        Ok((first, again)) => {
            s.ok(
                "the sweep signs each e2e person in again and deletes their e2e- fragments",
                first.people > 0 && first.deleted > 0 && first.kept == 0,
                format!("{first:?}"),
            );
            s.ok("and puts their computers to sleep, and a second sweep finds nothing left", again.deleted == 0 && again.slept == 0 && again.people >= first.people, format!("{again:?}"));
        }
        Err(e) => s.fail("the sweep", format!("{e:#}")),
    }
}

/// What a sweep did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Swept {
    pub people: usize,
    pub deleted: usize,
    pub kept: usize,
    pub slept: usize,
}

/// The sweep, on the deployment `api` reaches (a preview, or a rehearsal's node).
pub fn sweep_on(api: &Api) -> Result<Swept> {
    assert!(api.signs_in_by_levers(), "a sweep signs the e2e people in through the levers");
    let (mut people, mut deleted, mut kept, mut slept) = (0usize, 0usize, 0usize, 0usize);
    // one that did not go (a delete past the client's timeout) is named at
    // the end; the rest still go, and a sweep again finishes it
    let mut left: Vec<String> = Vec::new();
    let mut after: Option<String> = None;
    for page in 0..=SWEEP_PAGES_MAX {
        anyhow::ensure!(page < SWEEP_PAGES_MAX, "more than {SWEEP_PAGES_MAX} pages of e2e people: sweep again");
        let r = api.unsigned("POST", "/api/test/people", Some(&json!({ "after": after })))?;
        anyhow::ensure!(r.status == 200, "the e2e people: {r}");
        for person in r.body["people"].as_array().into_iter().flatten() {
            let email = person["email"].as_str().context("an e2e person has an email")?;
            people += 1;
            let (session, _) = api.e2e_sign_in(email, 0)?;
            let listed = shell(api, &session, "GET", "/api/fragments")?;
            anyhow::ensure!(listed.status == 200, "{email}'s fragments: {listed}");
            for f in listed.body["fragments"].as_array().into_iter().flatten().filter(|f| f["role"] == "owner") {
                let name = f["name"].as_str().unwrap_or("");
                if !name.starts_with(levers::E2E_LABEL_PREFIX) {
                    // not a hosted run's: an e2e person's fragment is the run's only by its label
                    kept += 1;
                    continue;
                }
                match shell(api, &session, "DELETE", &format!("/api/f/{name}")) {
                    Ok(r) if r.status == 200 || r.status == 404 => deleted += 1,
                    Ok(r) => left.push(format!("deleting {name}: {r}")),
                    Err(e) => left.push(format!("deleting {name}: {e:#}")),
                }
            }
            let computers = shell(api, &session, "GET", "/api/computers")?;
            for c in computers.body["computers"].as_array().into_iter().flatten().filter(|c| c["phase"] != "asleep") {
                let id = c["computer"].as_str().unwrap_or("");
                match shell(api, &session, "POST", &format!("/api/computers/{id}/sleep")) {
                    Ok(r) if r.status == 200 => slept += 1,
                    Ok(r) => left.push(format!("putting {id} to sleep: {r}")),
                    Err(e) => left.push(format!("putting {id} to sleep: {e:#}")),
                }
            }
        }
        after = r.body["next"].as_str().map(str::to_string);
        if after.is_none() {
            break;
        }
    }
    anyhow::ensure!(left.is_empty(), "the sweep left {} (sweep again):\n  {}", left.len(), left.join("\n  "));
    Ok(Swept { people, deleted, kept, slept })
}

#[cfg(test)]
mod tests;
