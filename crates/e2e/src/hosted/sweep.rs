//! The hosted lane's sweep: what a run made on a preview, removed.
//!
//! A preview is shared: several sessions run the hosted lane on it at
//! once, so a sweep removes one run's fragments unless told otherwise. A
//! hosted run labels its fragments `e2e-<run>-<base>` (`label`): its id
//! (`RUN_HEX` hex digits, from the clock as it starts) right after the
//! prefix, so a label a lane extends (`<agent label>-chat`) still names its
//! run. A sweep walks every e2e person (the levers' list), signs each in
//! again (a session, no paid calls) and acts as them: it has no power of
//! its own.
//!
//! - **`Scope::Run`** (`--sweep <run>`; `--sweep`, the last run that
//!   finished here; a rehearsal's end) deletes that run's fragments,
//!   whatever their age, and no other. It puts a person's computer to sleep
//!   when it deleted one of theirs and spared none of the e2e's: e2e people
//!   are mostly made fresh by a run, but a few sign in by a fixed email in
//!   every run, and another run may be using their computer.
//! - **`Scope::All`** (`--sweep-all`, for when nothing else runs there)
//!   deletes every e2e fragment but those younger than `SPARED_FOR_MS` (a
//!   run started meanwhile may be using them) and those whose age does not
//!   answer. It puts a person's computer to sleep unless it spared one of
//!   their fragments.
//!
//! Either says what it kept, and why (`Swept::report`).

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Context, Result};
use fragment_core::levers::E2E_LABEL_PREFIX;
use fragment_devstack as devstack;
use serde_json::json;

use crate::api::{Api, Call, Preview, Reply};

/// A run's id is this many lowercase hex digits.
pub const RUN_HEX: usize = 6;
/// The ids a run's clock cycles through (16^6 seconds: about 194 days, so
/// a run's labels are new beside any earlier run's still on the preview).
const RUN_IDS: i64 = 1 << (4 * RUN_HEX);
/// `--sweep-all` spares a fragment younger than this: a run started after
/// whoever chose to sweep everything may be using it. A whole hosted run
/// takes well under an hour.
pub const SPARED_FOR_MS: i64 = 60 * 60 * 1000;
/// The pages of e2e people one sweep walks at most (a page is 100).
const PAGES_MAX: usize = 100;
/// The spared fragments a report names one by one; the rest it counts.
const NAMED_MAX: usize = 20;

const _: () = assert!(RUN_HEX * 4 < 63, "a run's ids fit an i64");
const _: () = assert!(SPARED_FOR_MS > 0);

/// The id of a run that starts at `now_s`.
pub fn run_id(now_s: i64) -> String {
    let id = format!("{:0width$x}", now_s.rem_euclid(RUN_IDS), width = RUN_HEX);
    assert!(is_run(&id), "a run's id is {RUN_HEX} hex digits: {id:?}");
    id
}

/// Whether `text` is a run's id: `RUN_HEX` lowercase hex digits.
pub fn is_run(text: &str) -> bool {
    text.len() == RUN_HEX && text.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The label a hosted run gives `base`: `e2e-<run>-<base>`.
pub fn label(run: &str, base: &str) -> String {
    assert!(is_run(run), "a run's id is {RUN_HEX} hex digits: {run:?}");
    assert!(!base.is_empty(), "a label names what it is after its run");
    format!("{E2E_LABEL_PREFIX}{run}-{base}")
}

/// The run a label names (`e2e-<run>-…`), if it names one. A label of an
/// older layout (`e2e-<base>-<run>`), or one made by hand, names none.
pub fn run_of(label: &str) -> Option<&str> {
    let rest = label.strip_prefix(E2E_LABEL_PREFIX)?;
    let (run, base) = rest.split_once('-')?;
    if is_run(run) && !base.is_empty() {
        Some(run)
    } else {
        None
    }
}

/// What a sweep removes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    /// One run's fragments, by its id.
    Run(String),
    /// Every e2e fragment at least this old (ms): `--sweep-all` spares
    /// `SPARED_FOR_MS`.
    All { spared_for_ms: i64 },
}

/// Why a sweep kept a fragment.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kept {
    /// Not the e2e's: its label does not start `e2e-`.
    NotE2e,
    /// Another run's, by its label (`None`: the label names no run).
    OtherRun(Option<String>),
    /// Younger than the sweep spares: its age, in ms.
    Young(i64),
    /// Its age did not answer, so the sweep cannot tell it is old enough.
    AgeUnknown,
}

/// What a sweep does with one of an e2e person's fragments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Choice {
    Delete,
    Keep(Kept),
}

/// The choice for the fragment labelled `label`. `created_ms` asks when it
/// was made, and only `Scope::All` asks (it costs a call).
pub fn choose(scope: &Scope, label: &str, created_ms: impl FnOnce() -> Option<i64>, now_ms: i64) -> Choice {
    if !label.starts_with(E2E_LABEL_PREFIX) {
        return Choice::Keep(Kept::NotE2e);
    }
    match scope {
        Scope::Run(run) => match run_of(label) {
            Some(of) if of == run => Choice::Delete,
            of => Choice::Keep(Kept::OtherRun(of.map(str::to_string))),
        },
        Scope::All { spared_for_ms } => match created_ms() {
            None => Choice::Keep(Kept::AgeUnknown),
            // a fragment the clocks put in the future is young: kept
            Some(at) if now_ms - at >= *spared_for_ms => Choice::Delete,
            Some(at) => Choice::Keep(Kept::Young(now_ms - at)),
        },
    }
}

/// What a sweep does with a person's computers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Computers {
    /// Not the sweep's to touch: the person is not the run's.
    Untouched,
    /// Left awake: the sweep spared one of the person's fragments, so a
    /// run may be using them.
    LeftAwake,
    Sleep,
}

/// The fate of a person's computers, once the sweep chose to delete
/// `chosen` of their fragments and spared `spared` of the e2e's.
pub fn computers(scope: &Scope, chosen: usize, spared: usize) -> Computers {
    let ours = match scope {
        Scope::Run(_) => chosen > 0,
        // every e2e person is the whole sweep's
        Scope::All { .. } => true,
    };
    match (ours, spared) {
        (false, _) => Computers::Untouched,
        (true, 0) => Computers::Sleep,
        (true, _) => Computers::LeftAwake,
    }
}

/// What a sweep did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Swept {
    pub people: usize,
    pub deleted: usize,
    pub slept: usize,
    /// The fragments it kept that are not the e2e's.
    pub not_e2e: usize,
    /// The e2e fragments it kept, by name, and why.
    pub spared: Vec<(String, Kept)>,
    /// The awake computers it left so: their people own a fragment it spared.
    pub awake: usize,
}

impl Swept {
    /// The fragments spared for `why`.
    pub fn spared_for(&self, why: impl Fn(&Kept) -> bool) -> Vec<&str> {
        self.spared.iter().filter(|(_, k)| why(k)).map(|(n, _)| n.as_str()).collect()
    }

    /// What it kept, and why: other runs' fragments counted by run, the
    /// young ones named with their age, and the computers it left awake.
    pub fn report(&self) -> String {
        let mut by_run: BTreeMap<&str, usize> = BTreeMap::new();
        let (mut young, mut unknown) = (vec![], 0usize);
        for (name, why) in &self.spared {
            match why {
                Kept::OtherRun(run) => *by_run.entry(run.as_deref().unwrap_or("no run id")).or_default() += 1,
                Kept::Young(age_ms) => young.push(format!("{name} ({} min old)", (*age_ms).max(0) / 60_000)),
                Kept::AgeUnknown => unknown += 1,
                Kept::NotE2e => unreachable!("a fragment not the e2e's is counted, not spared"),
            }
        }
        let mut out = format!("kept {}", count(self.spared.len(), "e2e- fragment"));
        if !by_run.is_empty() {
            let runs: Vec<String> = by_run.iter().map(|(run, n)| format!("{run} ({n})")).collect();
            out += &format!("\n  {} of other runs: {}", by_run.values().sum::<usize>(), runs.join(", "));
        }
        if !young.is_empty() {
            let more = young.len().saturating_sub(NAMED_MAX);
            young.truncate(NAMED_MAX);
            out += &format!("\n  {} younger than {} min (a run may be using them): {}", young.len() + more, SPARED_FOR_MS / 60_000, young.join(", "));
            if more > 0 {
                out += &format!(", and {more} more");
            }
        }
        if unknown > 0 {
            out += &format!("\n  {unknown} whose age did not answer");
        }
        out += &format!("\nkept {} not the e2e's; left {} awake (their people own a fragment kept)", count(self.not_e2e, "fragment"), count(self.awake, "computer"));
        out
    }
}

/// `n` of `noun`, as a person says it.
fn count(n: usize, noun: &str) -> String {
    match n {
        1 => format!("1 {noun}"),
        _ => format!("{n} {noun}s"),
    }
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

/// When `name` was made, in ms: its earliest member's `addedAt` (its owner
/// joined as it was made). `None` when that does not answer.
fn created_ms(api: &Api, session: &str, name: &str) -> Option<i64> {
    let r = shell(api, session, "GET", &format!("/api/f/{name}/members")).ok()?;
    if r.status != 200 {
        return None;
    }
    r.body["members"].as_array()?.iter().filter_map(|m| m["addedAt"].as_i64()).min()
}

/// The sweep of `scope`, on the deployment `api` reaches (a preview, or a
/// rehearsal's node), as of `now_ms`.
pub fn sweep_on(api: &Api, scope: &Scope, now_ms: i64) -> Result<Swept> {
    assert!(api.signs_in_by_levers(), "a sweep signs the e2e people in through the levers");
    match scope {
        Scope::Run(run) => assert!(is_run(run), "a run's sweep names a run: {run:?}"),
        Scope::All { spared_for_ms } => assert!(*spared_for_ms >= 0, "a sweep spares fragments younger than an age"),
    }
    let mut swept = Swept::default();
    // one that did not go is named at the end; the rest still go, and a
    // sweep again finishes it. A delete answers within one round of its
    // members' lists (the cell's ended.rs), well inside the client's 60 s
    let mut left: Vec<String> = Vec::new();
    let mut after: Option<String> = None;
    for page in 0..=PAGES_MAX {
        anyhow::ensure!(page < PAGES_MAX, "more than {PAGES_MAX} pages of e2e people: sweep again");
        let r = api.unsigned("POST", "/api/test/people", Some(&json!({ "after": after })))?;
        anyhow::ensure!(r.status == 200, "the e2e people: {r}");
        for person in r.body["people"].as_array().into_iter().flatten() {
            let email = person["email"].as_str().context("an e2e person has an email")?;
            swept.people += 1;
            let (session, _) = api.e2e_sign_in(email, 0)?;
            let listed = shell(api, &session, "GET", "/api/fragments")?;
            anyhow::ensure!(listed.status == 200, "{email}'s fragments: {listed}");
            let (mut chosen, mut spared) = (0usize, 0usize);
            for f in listed.body["fragments"].as_array().into_iter().flatten().filter(|f| f["role"] == "owner") {
                let name = f["name"].as_str().unwrap_or("");
                let label = name.split_once('.').map_or(name, |(label, _)| label);
                match choose(scope, label, || created_ms(api, &session, name), now_ms) {
                    Choice::Keep(Kept::NotE2e) => swept.not_e2e += 1,
                    Choice::Keep(why) => {
                        spared += 1;
                        swept.spared.push((name.to_string(), why));
                    }
                    Choice::Delete => {
                        chosen += 1;
                        match shell(api, &session, "DELETE", &format!("/api/f/{name}")) {
                            Ok(r) if r.status == 200 || r.status == 404 => swept.deleted += 1,
                            Ok(r) => left.push(format!("deleting {name}: {r}")),
                            Err(e) => left.push(format!("deleting {name}: {e:#}")),
                        }
                    }
                }
            }
            let fate = computers(scope, chosen, spared);
            if fate == Computers::Untouched {
                continue;
            }
            let listed = shell(api, &session, "GET", "/api/computers")?;
            anyhow::ensure!(listed.status == 200, "{email}'s computers: {listed}");
            for c in listed.body["computers"].as_array().into_iter().flatten().filter(|c| c["phase"] != "asleep") {
                if fate == Computers::LeftAwake {
                    swept.awake += 1;
                    continue;
                }
                let id = c["computer"].as_str().unwrap_or("");
                match shell(api, &session, "POST", &format!("/api/computers/{id}/sleep")) {
                    Ok(r) if r.status == 200 => swept.slept += 1,
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
    if let Scope::Run(run) = scope {
        assert!(swept.spared.iter().all(|(n, _)| run_of(n.split('.').next().unwrap_or("")) != Some(run.as_str())), "a run's sweep spares none of its run's");
    }
    Ok(swept)
}

/// Where a hosted run that finished here notes its id, for `--sweep`
/// without one: one file a preview, in this checkout's target directory
/// (another checkout's runs are its own).
pub fn last_run_file(preview: &Preview) -> PathBuf {
    devstack::repo_root().join("target/e2e").join(format!("hosted-{}.{}.last-run", preview.branch, preview.zone))
}

/// Notes `run` as the last run that finished here on `preview`.
pub fn note_last_run(preview: &Preview, run: &str) -> Result<()> {
    assert!(is_run(run), "a run's id is {RUN_HEX} hex digits: {run:?}");
    let file = last_run_file(preview);
    std::fs::create_dir_all(file.parent().context("the file is in a directory")?)?;
    std::fs::write(&file, format!("{run}\n")).with_context(|| format!("write {}", file.display()))
}

/// The last run that finished here on `preview`, as `note_last_run` noted it.
pub fn last_run(preview: &Preview) -> Result<String> {
    let file = last_run_file(preview);
    let text = std::fs::read_to_string(&file).with_context(|| {
        format!(
            "no hosted run of {} finished in this checkout ({}): name one (`--sweep <run>`, the {RUN_HEX} hex digits after `e2e-` in its labels), or `--sweep-all` when nothing else runs there",
            preview.platform(),
            file.display()
        )
    })?;
    let run = text.trim();
    anyhow::ensure!(is_run(run), "{} holds {run:?}, not a run's id: name one (`--sweep <run>`)", file.display());
    Ok(run.to_string())
}
