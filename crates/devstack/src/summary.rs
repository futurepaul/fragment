//! What an e2e run reports as it ends (`fragment-e2e --summary <file>`),
//! and the check that a sharded run's shards, together, ran the suite
//! once (`cargo xtask e2e-summary <dir>`: CI's `e2e` job, which needs
//! every shard).
//!
//! A shard runs on a runner of its own, with its own build and node, the
//! sections the e2e's table gives it (crates/e2e/src/lanes/mod.rs,
//! `SHARDS`). Its summary names the whole suite as its lanes asked for it,
//! and the sections it accounted for: each ran, or was skipped whole (one
//! skip). Combined, every section of the suite is accounted for exactly
//! once, by exactly one shard, and each shard's totals are the sum of its
//! sections' checks and those outside any section. A shard that never
//! wrote its summary (it crashed, or was cancelled) fails the check.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

/// The most shards a run is split into: a bound for the check's loops.
pub const SHARDS_MAX: u32 = 64;
/// The most sections a suite has: a bound, far above the e2e's ~50.
pub const SECTIONS_MAX: usize = 1024;

/// Checks counted: passed, failed, and skipped (the hosted lane's, each
/// saying why).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Counts {
    pub passed: u64,
    pub failed: u64,
    pub skipped: u64,
}

impl std::ops::Add for Counts {
    type Output = Counts;

    fn add(self, other: Counts) -> Counts {
        Counts { passed: self.passed + other.passed, failed: self.failed + other.failed, skipped: self.skipped + other.skipped }
    }
}

impl Counts {
    /// What was counted since `before` (counts only grow).
    pub fn since(self, before: Counts) -> Counts {
        assert!(self.passed >= before.passed && self.failed >= before.failed && self.skipped >= before.skipped, "counts only grow: {before:?} then {self:?}");
        Counts { passed: self.passed - before.passed, failed: self.failed - before.failed, skipped: self.skipped - before.skipped }
    }
}

impl fmt::Display for Counts {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} passed, {} failed, {} skipped", self.passed, self.failed, self.skipped)
    }
}

/// Shard `k` of `n`, from 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Shard {
    pub k: u32,
    pub n: u32,
}

impl Shard {
    /// `k/n`, as `--shard` takes it: 1 <= k <= n <= SHARDS_MAX.
    pub fn parse(text: &str) -> Option<Shard> {
        let (k, n) = text.split_once('/')?;
        let shard = Shard { k: k.parse().ok()?, n: n.parse().ok()? };
        let valid = (1..=SHARDS_MAX).contains(&shard.n) && (1..=shard.n).contains(&shard.k);
        valid.then_some(shard)
    }
}

impl fmt::Display for Shard {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.k, self.n)
    }
}

/// One section a run accounted for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Section {
    pub name: String,
    /// It ran; otherwise it was skipped whole (its needs unmet, or it runs
    /// only by name), which is one skip in `counts`.
    pub ran: bool,
    /// The checks it made (and a lane that stopped early, one FAIL).
    pub counts: Counts,
    /// How long it took, in milliseconds: what a rebalance reads.
    pub ms: u64,
}

/// A run's summary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Summary {
    /// The shard this run was (`None`: the whole suite).
    pub shard: Option<Shard>,
    /// Every section the lanes asked for, in their order: the suite as
    /// this commit has it, whatever the run selected.
    pub suite: Vec<String>,
    /// The sections the run accounted for, in the order they ran.
    pub sections: Vec<Section>,
    /// Checks outside any section: a FAIL of the run's own (an unknown
    /// `--only` name, a node that would not stop).
    pub outside: Counts,
    /// The run's totals, as it counted them.
    pub totals: Counts,
    /// Each FAIL's label, for the check to print.
    pub failures: Vec<String>,
}

/// Why a set of summaries is not one whole run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SummaryError {
    /// No summary at all.
    None,
    /// The whole suite's summary beside others: one run is either whole,
    /// or shards.
    Mixed,
    /// A shard of a split other than the rest's (`n`), or two of one.
    ShardTwice(Shard),
    /// Shards disagree on how many there are.
    ShardCounts { first: u32, other: u32 },
    /// A shard of the split wrote no summary.
    ShardMissing(Shard),
    /// A shard's suite is not the first's (two commits' summaries).
    SuitesDiffer(Option<Shard>),
    /// A suite naming more sections than `SECTIONS_MAX`, or one twice.
    SuiteMalformed(Option<Shard>),
    /// A section accounted for that the suite does not name.
    SectionUnknown { section: String, shard: Option<Shard> },
    /// A section accounted for twice.
    SectionTwice { section: String, shards: [Option<Shard>; 2] },
    /// A section of the suite that no shard accounted for.
    SectionMissing(String),
    /// A section skipped whole whose counts are not that one skip.
    SkippedWholeCounts { section: String, counts: Counts },
    /// A run's totals are not the sum of its sections' and those outside them.
    CountsDisagree { shard: Option<Shard>, totals: Counts, sum: Counts },
    /// A run's failures are not as many as its failed checks.
    FailuresDisagree { shard: Option<Shard>, failed: u64, labels: usize },
}

fn shown(shard: &Option<Shard>) -> String {
    match shard {
        Some(s) => format!("shard {s}"),
        None => "the whole suite".to_string(),
    }
}

impl fmt::Display for SummaryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SummaryError::None => write!(f, "no summary: no shard finished"),
            SummaryError::Mixed => write!(f, "a whole suite's summary beside others: a run is whole, or shards"),
            SummaryError::ShardTwice(s) => write!(f, "shard {s} reported twice"),
            SummaryError::ShardCounts { first, other } => write!(f, "shards of {first} and of {other} in one run"),
            SummaryError::ShardMissing(s) => write!(f, "shard {s} wrote no summary (it failed before its end, or was cancelled)"),
            SummaryError::SuitesDiffer(s) => write!(f, "{}'s suite is not the others' (summaries of two commits?)", shown(s)),
            SummaryError::SuiteMalformed(s) => write!(f, "{}'s suite names a section twice, or more than {SECTIONS_MAX}", shown(s)),
            SummaryError::SectionUnknown { section, shard } => write!(f, "{} ran {section}, which its suite does not name", shown(shard)),
            SummaryError::SectionTwice { section, shards } => write!(f, "{section} ran twice: in {} and {}", shown(&shards[0]), shown(&shards[1])),
            SummaryError::SectionMissing(section) => write!(f, "no shard ran {section}"),
            SummaryError::SkippedWholeCounts { section, counts } => write!(f, "{section} was skipped whole, but counts {counts}"),
            SummaryError::CountsDisagree { shard, totals, sum } => write!(f, "{} counted {totals} in all, but its sections and the rest add up to {sum}", shown(shard)),
            SummaryError::FailuresDisagree { shard, failed, labels } => write!(f, "{} counted {failed} failed, but named {labels}", shown(shard)),
        }
    }
}

impl std::error::Error for SummaryError {}

/// The run the summaries make together: each section of the suite once,
/// with the shard that ran it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Combined {
    /// How many shards (1: the whole suite in one run).
    pub shards: u32,
    /// The suite's sections in its order, each with its shard.
    pub sections: Vec<(Section, Option<Shard>)>,
    pub totals: Counts,
    /// Every FAIL's label, with its shard.
    pub failures: Vec<(String, Option<Shard>)>,
}

/// One summary's own sums: its totals are its sections' and the rest.
fn consistent(s: &Summary) -> Result<(), SummaryError> {
    if s.suite.len() > SECTIONS_MAX || s.sections.len() > SECTIONS_MAX {
        return Err(SummaryError::SuiteMalformed(s.shard));
    }
    let mut sum = s.outside;
    for section in &s.sections {
        if !section.ran && section.counts != (Counts { passed: 0, failed: 0, skipped: 1 }) {
            return Err(SummaryError::SkippedWholeCounts { section: section.name.clone(), counts: section.counts });
        }
        sum = sum + section.counts;
    }
    if sum != s.totals {
        return Err(SummaryError::CountsDisagree { shard: s.shard, totals: s.totals, sum });
    }
    if s.failures.len() as u64 != s.totals.failed {
        return Err(SummaryError::FailuresDisagree { shard: s.shard, failed: s.totals.failed, labels: s.failures.len() });
    }
    Ok(())
}

/// The shards present, checked: one whole suite, or shards 1..=n each once.
fn split(summaries: &[Summary]) -> Result<u32, SummaryError> {
    let first = summaries.first().ok_or(SummaryError::None)?;
    let Some(first_shard) = first.shard else {
        return match summaries.len() {
            1 => Ok(1),
            _ => Err(SummaryError::Mixed),
        };
    };
    let n = first_shard.n;
    let mut seen = vec![false; n as usize];
    for s in summaries {
        let shard = s.shard.ok_or(SummaryError::Mixed)?;
        if shard.n != n {
            return Err(SummaryError::ShardCounts { first: n, other: shard.n });
        }
        assert!((1..=n).contains(&shard.k), "a shard parsed is within its split: {shard}");
        let slot = &mut seen[(shard.k - 1) as usize];
        if *slot {
            return Err(SummaryError::ShardTwice(shard));
        }
        *slot = true;
    }
    match seen.iter().position(|present| !present) {
        Some(i) => Err(SummaryError::ShardMissing(Shard { k: i as u32 + 1, n })),
        None => Ok(n),
    }
}

/// The summaries of one run's shards (or the one summary of a whole run),
/// combined: an error unless they make the suite exactly once.
pub fn combine(summaries: &[Summary]) -> Result<Combined, SummaryError> {
    let shards = split(summaries)?;
    let suite = &summaries[0].suite;
    let mut unique = std::collections::BTreeSet::new();
    if !suite.iter().all(|name| unique.insert(name.as_str())) {
        return Err(SummaryError::SuiteMalformed(summaries[0].shard));
    }
    let mut by_section: BTreeMap<&str, (Section, Option<Shard>)> = BTreeMap::new();
    let (mut totals, mut failures) = (Counts::default(), Vec::new());
    // bounded: at most SHARDS_MAX summaries, each of at most SECTIONS_MAX sections
    for s in summaries {
        consistent(s)?;
        if &s.suite != suite {
            return Err(SummaryError::SuitesDiffer(s.shard));
        }
        for section in &s.sections {
            if !unique.contains(section.name.as_str()) {
                return Err(SummaryError::SectionUnknown { section: section.name.clone(), shard: s.shard });
            }
            if let Some((_, earlier)) = by_section.get(section.name.as_str()) {
                return Err(SummaryError::SectionTwice { section: section.name.clone(), shards: [*earlier, s.shard] });
            }
            by_section.insert(&section.name, (section.clone(), s.shard));
        }
        totals = totals + s.totals;
        failures.extend(s.failures.iter().map(|f| (f.clone(), s.shard)));
    }
    let mut sections = Vec::with_capacity(suite.len());
    for name in suite {
        match by_section.remove(name.as_str()) {
            Some(found) => sections.push(found),
            None => return Err(SummaryError::SectionMissing(name.clone())),
        }
    }
    assert!(by_section.is_empty(), "every section accounted for is one of the suite's");
    let outside = summaries.iter().fold(Counts::default(), |sum, s| sum + s.outside);
    let counted = sections.iter().fold(outside, |sum, (s, _)| sum + s.counts);
    assert_eq!(counted, totals, "the sections and the checks outside them make the totals");
    Ok(Combined { shards, sections, totals, failures })
}

#[cfg(test)]
mod tests {
    //! The check against summaries made here: two shards that together
    //! ran a suite of three sections pass; each way a set of summaries can
    //! fail to be that run is its own error.
    use super::*;

    const SKIPPED_WHOLE: Counts = Counts { passed: 0, failed: 0, skipped: 1 };

    fn section(name: &str, passed: u64) -> Section {
        Section { name: name.into(), ran: true, counts: Counts { passed, failed: 0, skipped: 0 }, ms: 10 }
    }

    fn summary(shard: Option<(u32, u32)>, sections: Vec<Section>) -> Summary {
        let totals = sections.iter().fold(Counts::default(), |sum, s| sum + s.counts);
        Summary {
            shard: shard.map(|(k, n)| Shard { k, n }),
            suite: ["auth", "agents", "hermes"].map(String::from).to_vec(),
            sections,
            outside: Counts::default(),
            totals,
            failures: vec![],
        }
    }

    fn hermes() -> Section {
        Section { name: "hermes".into(), ran: false, counts: SKIPPED_WHOLE, ms: 0 }
    }

    fn two_shards() -> Vec<Summary> {
        vec![summary(Some((2, 2)), vec![section("agents", 20), hermes()]), summary(Some((1, 2)), vec![section("auth", 9)])]
    }

    #[test]
    fn shards_that_ran_the_suite_once_combine_in_its_order() {
        let combined = combine(&two_shards()).unwrap();
        assert_eq!(combined.shards, 2);
        assert_eq!(combined.totals, Counts { passed: 29, failed: 0, skipped: 1 });
        assert_eq!(combined.sections.iter().map(|(s, k)| (s.name.as_str(), k.map(|k| k.k))).collect::<Vec<_>>(), [("auth", Some(1)), ("agents", Some(2)), ("hermes", Some(2))]);
        // the whole suite in one run is the same check
        let whole = summary(None, vec![section("auth", 9), section("agents", 20), hermes()]);
        assert_eq!(combine(&[whole]).unwrap().totals, combined.totals);
    }

    #[test]
    fn a_failed_check_is_counted_and_named() {
        let mut shards = two_shards();
        shards[1].totals.failed = 1;
        shards[1].sections[0].counts.failed = 1;
        shards[1].failures = vec!["auth: a check".into()];
        let combined = combine(&shards).unwrap();
        assert_eq!((combined.totals.failed, combined.failures), (1, vec![("auth: a check".to_string(), Some(Shard { k: 1, n: 2 }))]));
    }

    #[test]
    fn each_way_summaries_fail_to_make_one_run_is_its_own_error() {
        let shard = |k, n| Shard { k, n };
        let mut cases: Vec<(Vec<Summary>, SummaryError)> = vec![(vec![], SummaryError::None)];
        let mut missing = two_shards();
        missing.remove(1);
        cases.push((missing, SummaryError::ShardMissing(shard(1, 2))));
        let mut twice = two_shards();
        twice.push(twice[0].clone());
        cases.push((twice, SummaryError::ShardTwice(shard(2, 2))));
        let mut counts = two_shards();
        counts[1].shard = Some(shard(1, 3));
        cases.push((counts, SummaryError::ShardCounts { first: 2, other: 3 }));
        let mut mixed = two_shards();
        mixed[1].shard = None;
        cases.push((mixed, SummaryError::Mixed));
        let mut differ = two_shards();
        differ[1].suite.push("sync".into());
        cases.push((differ, SummaryError::SuitesDiffer(Some(shard(1, 2)))));
        let mut unknown = two_shards();
        unknown[1].sections.push(section("sync", 1));
        unknown[1].totals.passed += 1;
        cases.push((unknown, SummaryError::SectionUnknown { section: "sync".into(), shard: Some(shard(1, 2)) }));
        let mut ran_twice = two_shards();
        ran_twice[1].sections.push(section("agents", 20));
        ran_twice[1].totals.passed += 20;
        cases.push((ran_twice, SummaryError::SectionTwice { section: "agents".into(), shards: [Some(shard(2, 2)), Some(shard(1, 2))] }));
        let mut not_run = two_shards();
        not_run[1].sections.clear();
        not_run[1].totals = Counts::default();
        cases.push((not_run, SummaryError::SectionMissing("auth".into())));
        let mut skipped = two_shards();
        skipped[0].sections[1].counts.passed = 3;
        skipped[0].totals.passed += 3;
        cases.push((skipped, SummaryError::SkippedWholeCounts { section: "hermes".into(), counts: Counts { passed: 3, failed: 0, skipped: 1 } }));
        let mut sums = two_shards();
        sums[0].totals.passed += 1;
        cases.push((sums, SummaryError::CountsDisagree { shard: Some(shard(2, 2)), totals: Counts { passed: 21, failed: 0, skipped: 1 }, sum: Counts { passed: 20, failed: 0, skipped: 1 } }));
        let mut labels = two_shards();
        labels[0].failures.push("a FAIL with no count".into());
        cases.push((labels, SummaryError::FailuresDisagree { shard: Some(shard(2, 2)), failed: 0, labels: 1 }));
        let mut suite_twice = two_shards();
        for s in &mut suite_twice {
            s.suite.push("auth".into());
        }
        cases.push((suite_twice, SummaryError::SuiteMalformed(Some(shard(2, 2)))));
        for (summaries, error) in cases {
            assert_eq!(combine(&summaries), Err(error.clone()), "{error}");
        }
    }

    #[test]
    fn a_shard_is_k_of_n_from_one() {
        assert_eq!(Shard::parse("2/4"), Some(Shard { k: 2, n: 4 }));
        assert_eq!(Shard::parse("1/1"), Some(Shard { k: 1, n: 1 }));
        for bad in ["0/4", "5/4", "4", "/4", "a/b", "1/0", "1/65", "-1/4", " 1/4"] {
            assert_eq!(Shard::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn a_summary_reads_back_as_written() {
        let s = two_shards().remove(0);
        let text = serde_json::to_string(&s).unwrap();
        assert_eq!(serde_json::from_str::<Summary>(&text).unwrap(), s);
    }
}
