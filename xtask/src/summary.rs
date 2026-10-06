//! `cargo xtask e2e-summary <dir>`: CI's `e2e` check over the summaries
//! its shards wrote (`fragment-e2e --shard k/n --summary <file> --attempt
//! <a>`), from every attempt of the run, or over the one a whole run wrote.
//! It fails unless each shard's newest summary, together, makes the suite
//! exactly once (devstack's `summary::combine`) and every check passed, and
//! prints the run as one: each section with its shard, its checks and its
//! time, each shard's with the attempt that counted, the attempts a re-run
//! set aside, and the totals in the suite's own words.

use std::path::Path;

use anyhow::{bail, Context, Result};
use fragment_devstack::summary::{self, Shard, Summary, SUMMARIES_MAX};

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
    let mut shards: Vec<&Summary> = combined.counted.iter().collect();
    shards.sort_by_key(|s| s.shard);
    for s in &shards {
        let ms: u64 = s.sections.iter().map(|x| x.ms).sum();
        println!("{} (attempt {}): {} sections in {:.1}s, {}", shown(s.shard), s.attempt, s.sections.len(), ms as f64 / 1000.0, s.totals);
    }
    for (shard, attempt) in &combined.set_aside {
        println!("{}'s attempt {attempt} set aside: a newer attempt ran it again", shown(*shard));
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

fn shown(shard: Option<Shard>) -> String {
    shard.map_or("the whole suite".to_string(), |k| format!("shard {k}"))
}

/// Every `*.json` in `dir` (CI downloads each shard's there, one file per
/// shard and attempt), parsed.
fn read_all(dir: &Path) -> Result<Vec<Summary>> {
    let mut paths: Vec<_> = std::fs::read_dir(dir)
        .with_context(|| format!("read {}", dir.display()))?
        .map(|e| e.map(|e| e.path()))
        .collect::<std::io::Result<Vec<_>>>()?
        .into_iter()
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    paths.sort();
    if paths.len() > SUMMARIES_MAX {
        bail!("{} summaries in {}: a run has at most {SUMMARIES_MAX} (each shard's, from each attempt)", paths.len(), dir.display());
    }
    paths
        .iter()
        .map(|p| {
            let bytes = std::fs::read(p).with_context(|| format!("read {}", p.display()))?;
            serde_json::from_slice(&bytes).with_context(|| format!("{} is not an e2e summary", p.display()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    //! The check as CI's `e2e` job runs it: over one directory of the
    //! shards' summaries, merged flat as download-artifact leaves them,
    //! each named for its shard and attempt as ci.yml names them.
    use super::*;
    use fragment_devstack as devstack;
    use fragment_devstack::summary::{Counts, Section};

    /// Shard `k` of 2 (shard 1 runs auth, shard 2 agents), from `attempt`,
    /// written as ci.yml's `--summary` names it.
    fn write(dir: &Path, k: u32, attempt: u32, failed: u64) -> std::path::PathBuf {
        let name = ["auth", "agents"][k as usize - 1];
        let counts = Counts { passed: 10 - failed, failed, skipped: 0 };
        let summary = Summary {
            shard: Some(Shard { k, n: 2 }),
            attempt,
            suite: vec!["auth".into(), "agents".into()],
            sections: vec![Section { name: name.into(), ran: true, counts, ms: 1000 }],
            outside: Counts::default(),
            totals: counts,
            failures: (0..failed).map(|i| format!("{name}: check {i}")).collect(),
        };
        let path = dir.join(format!("shard-{k}-attempt-{attempt}.json"));
        std::fs::write(&path, serde_json::to_vec_pretty(&summary).unwrap()).unwrap();
        path
    }

    /// Run 37394854864's shape: shard 2's first attempt failed and its
    /// re-run passed, both summaries in the directory. Method: the check
    /// before the re-run is red, after it green; without shard 1's summary
    /// it is red again, naming the shard, however many attempts shard 2 made.
    #[test]
    fn a_rerun_turns_the_check_green_and_a_missing_shard_keeps_it_red() {
        let dir = std::env::temp_dir().join(format!("xtask-e2e-summary-{}", devstack::random_hex(6)));
        std::fs::create_dir_all(&dir).unwrap();
        let args = [dir.to_string_lossy().into_owned()];
        let shard_1 = write(&dir, 1, 1, 0);
        write(&dir, 2, 1, 1);
        let red = e2e_summary(&args).unwrap_err();
        assert_eq!(format!("{red:#}"), "1 checks failed");
        write(&dir, 2, 2, 0);
        e2e_summary(&args).expect("the re-run's summary counts for shard 2");
        std::fs::remove_file(shard_1).unwrap();
        let missing = format!("{:#}", e2e_summary(&args).unwrap_err());
        assert!(missing.contains("shard 1/2 wrote no summary"), "{missing}");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
