//! `cargo xtask e2e-summary <dir>`: CI's `e2e` check over the summaries
//! its shards wrote (`fragment-e2e --shard k/n --summary <file>`), or over
//! the one a whole run wrote. It fails unless the summaries make the suite
//! exactly once (devstack's `summary::combine`) and every check passed,
//! and prints the run as one: each section with its shard, its checks and
//! its time, each shard's, and the totals in the suite's own words.

use std::path::Path;

use anyhow::{bail, Context, Result};
use fragment_devstack::summary::{self, Summary, SHARDS_MAX};

pub fn e2e_summary(args: &[String]) -> Result<()> {
    let [dir] = args else { bail!("usage: cargo xtask e2e-summary <dir of summaries>") };
    let summaries = read_all(Path::new(dir))?;
    let combined = summary::combine(&summaries).map_err(|e| anyhow::anyhow!("the e2e's summaries are not one whole run: {e}"))?;
    let width = combined.sections.iter().map(|(s, _)| s.name.len()).max().unwrap_or(0);
    for (section, shard) in &combined.sections {
        let shard = shard.map_or("whole".to_string(), |s| format!("shard {s}"));
        let ran = if section.ran { format!("{:>6.1}s", section.ms as f64 / 1000.0) } else { "skipped whole".to_string() };
        println!("  {:width$}  {shard:9}  {ran:>13}  {}", section.name, section.counts);
    }
    let mut shards: Vec<&Summary> = summaries.iter().collect();
    shards.sort_by_key(|s| s.shard);
    for s in &shards {
        let ms: u64 = s.sections.iter().map(|x| x.ms).sum();
        let what = s.shard.map_or("the whole suite".to_string(), |k| format!("shard {k}"));
        println!("{what}: {} sections in {:.1}s, {}", s.sections.len(), ms as f64 / 1000.0, s.totals);
    }
    let ran = combined.sections.iter().filter(|(s, _)| s.ran).count();
    println!("every section once: {} sections ({ran} ran, {} skipped whole) in {} run(s)", combined.sections.len(), combined.sections.len() - ran, combined.shards);
    let t = combined.totals;
    println!("\n{} passed, {} failed, {} skipped (the hosted lane's)", t.passed, t.failed, t.skipped);
    if t.failed > 0 {
        for (label, shard) in &combined.failures {
            println!("  FAIL {label} ({})", shard.map_or("whole".to_string(), |s| format!("shard {s}")));
        }
        bail!("{} checks failed", t.failed);
    }
    Ok(())
}

/// Every `*.json` in `dir` (CI downloads each shard's there), parsed.
fn read_all(dir: &Path) -> Result<Vec<Summary>> {
    let mut paths: Vec<_> = std::fs::read_dir(dir)
        .with_context(|| format!("read {}", dir.display()))?
        .map(|e| e.map(|e| e.path()))
        .collect::<std::io::Result<Vec<_>>>()?
        .into_iter()
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    paths.sort();
    if paths.len() > SHARDS_MAX as usize {
        bail!("{} summaries in {}: a run has at most {SHARDS_MAX}", paths.len(), dir.display());
    }
    paths
        .iter()
        .map(|p| {
            let bytes = std::fs::read(p).with_context(|| format!("read {}", p.display()))?;
            serde_json::from_slice(&bytes).with_context(|| format!("{} is not an e2e summary", p.display()))
        })
        .collect()
}
