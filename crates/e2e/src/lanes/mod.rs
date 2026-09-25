//! The sections, in the order they run.

mod agents;
mod app;
mod appfiles;
mod blobs;
mod author;
mod budget;
mod computer;
mod control;
mod deliver;
mod desktop;
mod identities;
mod jobs;
mod keys;
mod limits;
mod members;
mod notes;
mod plane;
mod restart;
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
    members::secrets,
    plane::files,
    plane::deploy,
    templates::templates,
    desktop::desktop,
    app::ops,
    app::public,
    app::effects,
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
    jobs::triggers,
    appfiles::appfiles,
    blobs::blobs,
    notes::notes,
    deliver::push,
    deliver::ai,
    budget::budget,
    agents::agents,
    agents::chat,
    computer::computer,
    computer::screenshots,
    sync::folder_sync,
    restart::restart,
    restart::pathmode,
    // runs last: a node of its own
    |s, _| limits::node_full(s),
];

/// Runs every lane, each with an API of its own. A lane that returns an
/// error or panics is one FAIL, and the lanes after it still run: a red run
/// reports every failure at once.
pub fn run(s: &mut Suite) {
    for lane in LANES {
        let api = s.api();
        let before = s.ran.len();
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
    }
}

fn panic_message(panic: &(dyn std::any::Any + Send)) -> &str {
    match (panic.downcast_ref::<&str>(), panic.downcast_ref::<String>()) {
        (Some(s), _) => s,
        (_, Some(s)) => s,
        _ => "(no message)",
    }
}
