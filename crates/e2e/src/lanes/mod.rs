//! The sections, in the order they run.

mod addon;
mod agents;
mod app;
mod appfiles;
mod blobs;
mod brain;
mod chat;
mod author;
mod computers;
mod control;
mod credentials;
mod delegation;
mod deliver;
mod frames;
pub mod hermes;
mod identities;
mod isolation;
pub(crate) mod jobs;
mod keys;
mod levers;
mod ledger;
mod limits;
mod members;
mod notes;
mod plane;
mod posts;
mod restart;
mod share;
mod shell;
mod signin;
mod site;
mod sync;
mod templates;

use std::panic::{self, AssertUnwindSafe};
use std::time::Instant;

use anyhow::Result;

use crate::api::Api;
use crate::Suite;

/// A lane: it asks `s.section` whether its section runs, then checks.
type Lane = fn(&mut Suite, &Api) -> Result<()>;

/// The lanes, in the order they run.
const LANES: &[Lane] = &[
    control::auth,
    control::create,
    control::lockdown,
    keys::keys,
    members::members,
    identities::identities,
    signin::signin,
    levers::levers,
    members::secrets,
    delegation::delegation,
    plane::files,
    plane::deploy,
    templates::templates,
    share::share,
    isolation::isolation,
    frames::frames,
    app::ops,
    app::public,
    app::effects,
    // after the last node restart before the triggers section: the cron
    // fragment's first minute passes while the sections between run (not
    // a section: its outcome waits for triggers, which reports it)
    |s, api| {
        s.cron = Some(jobs::cron(s, api).map_err(|e| format!("{e:#}")));
        Ok(())
    },
    limits::facet_cap,
    limits::lockdown,
    site::site,
    site::watch,
    author::schemas,
    author::channels,
    author::live,
    author::routes,
    author::cli,
    author::browser,
    jobs::jobs,
    |s, api| {
        let cron = s.cron.take();
        jobs::triggers(s, api, cron)
    },
    appfiles::appfiles,
    blobs::blobs,
    notes::notes,
    brain::brain,
    deliver::push,
    deliver::ai,
    ledger::ledger_lane,
    agents::agents,
    addon::addon,
    shell::shell_platform,
    computers::computers,
    chat::chat,
    shell::shell_ui,
    hermes::hermes,
    sync::folder_sync,
    restart::restart,
    restart::pathmode,
];

/// The suite split for CI (`--shard k/n`): each shard runs on a runner of
/// its own, with its own build and node, the sections it lists in the
/// lanes' order. Every section is in exactly one shard (a test below), so
/// the shards together run what one whole run does; CI's `e2e` job checks
/// that from their summaries (`cargo xtask e2e-summary`). `hermes` runs
/// only by name, so its shard reports it as the whole run does: one skip.
///
/// Balanced by measured time (each summary's `ms`, CI's runners): a shard
/// is about a quarter of the suite's ~13 minutes. Shard 3 holds `triggers`
/// and the sections from the cron fragment's deploy (after `effects`) up to
/// it, so its first cron minute passes while they run, as in a whole run.
pub const SHARDS: [&[&str]; 4] = [
    &["shell", "computers", "chat", "hermes", "restart", "pathmode"],
    &["agents", "addon", "shell-ui", "sync"],
    &["facet-cap", "app-lockdown", "site", "watch", "schemas", "channels", "live", "routes", "cli", "browser", "jobs", "triggers", "push"],
    &[
        "auth", "create", "lockdown", "keys", "members", "identities", "signin", "levers", "secrets", "delegation", "files", "deploy", "templates", "share", "isolation", "frames",
        "ops", "public", "effects", "appfiles", "blobs", "notes", "brain", "ai", "ledger",
    ],
];

/// Whether shard `k` (from 1) of the table runs the section `name`.
pub fn shard_runs(k: u32, name: &str) -> bool {
    assert!((1..=SHARDS.len() as u32).contains(&k), "a shard parsed is one of the table's: {k}");
    SHARDS[k as usize - 1].contains(&name)
}

/// Runs every lane, each with an API of its own. A lane that returns an
/// error or panics is one FAIL, and the lanes after it still run: a red run
/// reports every failure at once.
pub fn run(s: &mut Suite) {
    for lane in LANES {
        counted(s, *lane);
    }
}

/// Runs one lane, its checks counted to the section it ran (or skipped
/// whole), or outside any section when it asked for none that ran.
pub fn counted(s: &mut Suite, lane: impl FnOnce(&mut Suite, &Api) -> Result<()>) {
    let api = s.api();
    let before = s.ran.len();
    let (accounted, counts) = (s.accounted.len(), s.counts());
    let t0 = Instant::now();
    let stopped = match panic::catch_unwind(AssertUnwindSafe(|| lane(s, &api))) {
        Ok(Ok(())) => None,
        Ok(Err(e)) => Some(format!("{e:#}")),
        Err(panic) => Some(format!("it panicked: {}", panic_message(panic.as_ref()))),
    };
    // every lane asks for its section before anything else
    assert!(s.ran.len() <= before + 1, "a lane runs one section");
    if let Some(why) = stopped {
        assert!(s.ran.len() == before + 1, "a lane stops early only once its section runs: {why}");
        s.stopped_early(&why);
    }
    if s.ran.len() == before + 1 {
        println!("      ({} in {:.1?})", s.ran[before], t0.elapsed());
    }
    let made = s.counts().since(counts);
    match s.accounted.len() - accounted {
        0 => s.outside = s.outside + made,
        1 => {
            let section = s.accounted.last_mut().expect("a section was accounted for");
            section.counts = made;
            section.ms = u64::try_from(t0.elapsed().as_millis()).unwrap_or(u64::MAX);
        }
        more => panic!("a lane accounts for one section at most, not {more}"),
    }
}

fn panic_message(panic: &(dyn std::any::Any + Send)) -> &str {
    match (panic.downcast_ref::<&str>(), panic.downcast_ref::<String>()) {
        (Some(s), _) => s,
        (_, Some(s)) => s,
        _ => "(no message)",
    }
}

#[cfg(test)]
mod tests {
    //! The shard table against the lanes themselves: every section a lane
    //! asks for (a dry run's plan, which asks each lane without a node) is
    //! in exactly one shard, and each shard runs exactly its own.
    use super::*;

    /// Every section the lanes ask for, in their order.
    fn sections() -> Vec<String> {
        let args = crate::hosted::parse(&["--hosted", "--zone", "finite.place", "--branch", "p5", "--dry-run"].map(String::from)).unwrap();
        let (plan, unknown) = crate::hosted::plan(None, vec![], &args.hosted.expect("a hosted dry run"));
        assert!(unknown.is_empty());
        plan.into_iter().map(|p| p.section).collect()
    }

    #[test]
    fn every_section_is_in_exactly_one_shard() {
        let sections = sections();
        assert!(sections.len() >= 45, "the plan asks every lane: {} sections", sections.len());
        for name in &sections {
            let holders: Vec<usize> = (1..=SHARDS.len()).filter(|k| SHARDS[k - 1].contains(&name.as_str())).collect();
            assert_eq!(holders.len(), 1, "{name} is in shards {holders:?}: each section is in exactly one");
        }
        for (i, shard) in SHARDS.iter().enumerate() {
            assert!(!shard.is_empty(), "shard {} runs something", i + 1);
            for name in *shard {
                assert!(sections.iter().any(|s| s == name), "shard {} names {name}, which no lane asks for", i + 1);
            }
        }
        let listed: usize = SHARDS.iter().map(|s| s.len()).sum();
        assert_eq!(listed, sections.len(), "the table names each section once");
    }

    /// `shard_runs`, which a sharded run's selection asks, follows the
    /// table: shard k runs its own sections and no other's.
    #[test]
    fn a_shard_runs_its_own_sections_alone() {
        for name in sections() {
            let runs: Vec<u32> = (1..=SHARDS.len() as u32).filter(|k| shard_runs(*k, &name)).collect();
            assert_eq!(runs.len(), 1, "{name} runs in shards {runs:?}");
        }
        assert!(!(1..=SHARDS.len() as u32).any(|k| shard_runs(k, "nonesuch")));
    }

    /// The cron fragment is deployed (after `effects`) in the shard that
    /// runs `triggers`, and every section from there to `triggers` is in
    /// that shard: its first minute passes while they run, as in a whole run.
    #[test]
    fn the_triggers_shard_holds_the_sections_from_the_cron_deploy() {
        let sections = sections();
        let at = |name: &str| sections.iter().position(|s| s == name).unwrap_or_else(|| panic!("{name} is a section"));
        let (from, to) = (at("effects") + 1, at("triggers"));
        let shard = (1..=SHARDS.len() as u32).find(|k| shard_runs(*k, "triggers")).expect("a shard runs triggers");
        for name in &sections[from..=to] {
            assert!(shard_runs(shard, name), "{name} runs between the cron deploy and triggers, so in shard {shard}");
        }
    }
}
