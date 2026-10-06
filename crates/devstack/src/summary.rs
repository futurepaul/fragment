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
//!
//! A re-run (GitHub's "re-run failed jobs") leaves the first attempt's
//! summaries beside the new ones, so each summary says which attempt wrote
//! it, and of one shard's only the newest counts: a re-run that passes
//! turns the check green, and one that fails keeps it red. The choice is
//! made here, not by download-artifact: that keeps one artifact per name,
//! the highest id, and a run's ids do not follow time (run 37394854864:
//! attempt 2's `e2e-summary-2` had the lower id, so attempt 1's red
//! summary was read, and the re-run could never turn the check green).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Deserialize, Serialize};

/// The most shards a run is split into: a bound for the check's loops.
pub const SHARDS_MAX: u32 = 64;
/// The most attempts of one run: GitHub re-runs a run at most 50 times,
/// so its attempts are 1 to 51.
pub const ATTEMPTS_MAX: u32 = 64;
/// The most summaries one check reads: each shard's, from each attempt.
pub const SUMMARIES_MAX: usize = SHARDS_MAX as usize * ATTEMPTS_MAX as usize;
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
    /// The CI run's attempt that wrote it (GitHub's `run_attempt`, from 1;
    /// a run outside CI is its first): of one shard's, the newest counts.
    pub attempt: u32,
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
    /// A summary's attempt outside 1..=ATTEMPTS_MAX.
    AttemptInvalid { shard: Option<Shard>, attempt: u32 },
    /// Two summaries of one shard (or of the whole suite) from one attempt.
    ShardTwice { shard: Option<Shard>, attempt: u32 },
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
            SummaryError::AttemptInvalid { shard, attempt } => write!(f, "{} names attempt {attempt}: a run's attempts are 1 to {ATTEMPTS_MAX}", shown(shard)),
            SummaryError::ShardTwice { shard, attempt } => write!(f, "{} reported twice by attempt {attempt}", shown(shard)),
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
    /// The summaries that count, each shard's newest attempt's, in the
    /// order given.
    pub counted: Vec<Summary>,
    /// The older attempts a shard's newer one set aside, as (shard,
    /// attempt), in the order given.
    pub set_aside: Vec<(Option<Shard>, u32)>,
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

/// Each shard's newest summary, and the older attempts it set aside.
struct Newest<'a> {
    counted: Vec<&'a Summary>,
    set_aside: Vec<(Option<Shard>, u32)>,
}

/// Of a run's summaries, each shard's (and the whole suite's) newest
/// attempt's, in the order given, and the older attempts it set aside:
/// a re-run's summary replaces its shard's from the attempt before, whether
/// either passed. Two of one shard from one attempt are an error.
fn newest(summaries: &[Summary]) -> Result<Newest<'_>, SummaryError> {
    let mut seen = BTreeSet::new();
    let mut newest: BTreeMap<Option<Shard>, usize> = BTreeMap::new();
    // bounded: at most SUMMARIES_MAX summaries (combine asserts it)
    for (i, s) in summaries.iter().enumerate() {
        if !(1..=ATTEMPTS_MAX).contains(&s.attempt) {
            return Err(SummaryError::AttemptInvalid { shard: s.shard, attempt: s.attempt });
        }
        if !seen.insert((s.shard, s.attempt)) {
            return Err(SummaryError::ShardTwice { shard: s.shard, attempt: s.attempt });
        }
        if newest.get(&s.shard).is_none_or(|&j| summaries[j].attempt < s.attempt) {
            newest.insert(s.shard, i);
        }
    }
    let kept: BTreeSet<usize> = newest.into_values().collect();
    let (mut counted, mut set_aside) = (Vec::with_capacity(kept.len()), Vec::new());
    for (i, s) in summaries.iter().enumerate() {
        match kept.contains(&i) {
            true => counted.push(s),
            false => set_aside.push((s.shard, s.attempt)),
        }
    }
    assert_eq!(counted.len() + set_aside.len(), summaries.len(), "each summary counts or is set aside");
    Ok(Newest { counted, set_aside })
}

/// The shards present, checked: one whole suite, or shards 1..=n each once.
fn split(summaries: &[&Summary]) -> Result<u32, SummaryError> {
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
        assert!(!*slot, "newest leaves each shard once: {shard}");
        *slot = true;
    }
    match seen.iter().position(|present| !present) {
        Some(i) => Err(SummaryError::ShardMissing(Shard { k: i as u32 + 1, n })),
        None => Ok(n),
    }
}

/// The summaries of one run, from any of its attempts: each shard's, or
/// the whole suite's. Each shard's newest attempt counts, and an error
/// unless the summaries that count make the suite exactly once.
pub fn combine(summaries: &[Summary]) -> Result<Combined, SummaryError> {
    assert!(summaries.len() <= SUMMARIES_MAX, "the caller reads at most {SUMMARIES_MAX} summaries, not {}", summaries.len());
    let Newest { counted, set_aside } = newest(summaries)?;
    let shards = split(&counted)?;
    let suite = &counted[0].suite;
    let mut unique = std::collections::BTreeSet::new();
    if !suite.iter().all(|name| unique.insert(name.as_str())) {
        return Err(SummaryError::SuiteMalformed(counted[0].shard));
    }
    let mut by_section: BTreeMap<&str, (Section, Option<Shard>)> = BTreeMap::new();
    let (mut totals, mut failures) = (Counts::default(), Vec::new());
    // bounded: at most SHARDS_MAX summaries count, each of at most SECTIONS_MAX sections
    for s in &counted {
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
    let outside = counted.iter().fold(Counts::default(), |sum, s| sum + s.outside);
    let sum = sections.iter().fold(outside, |sum, (s, _)| sum + s.counts);
    assert_eq!(sum, totals, "the sections and the checks outside them make the totals");
    let counted = counted.into_iter().cloned().collect();
    Ok(Combined { shards, counted, set_aside, sections, totals, failures })
}

#[cfg(test)]
mod tests {
    //! The check against summaries made here: two shards that together
    //! ran a suite of three sections pass; a re-run's newest attempt
    //! counts for its shard; each way a set of summaries can fail to be
    //! that run is its own error.
    use super::*;

    const SKIPPED_WHOLE: Counts = Counts { passed: 0, failed: 0, skipped: 1 };

    fn section(name: &str, passed: u64) -> Section {
        Section { name: name.into(), ran: true, counts: Counts { passed, failed: 0, skipped: 0 }, ms: 10 }
    }

    fn summary(shard: Option<(u32, u32)>, sections: Vec<Section>) -> Summary {
        let totals = sections.iter().fold(Counts::default(), |sum, s| sum + s.counts);
        Summary {
            shard: shard.map(|(k, n)| Shard { k, n }),
            attempt: 1,
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

    /// Shard 1's first attempt, red: auth's one check failed.
    fn red_first_attempt() -> Summary {
        let mut red = summary(Some((1, 2)), vec![Section { name: "auth".into(), ran: true, counts: Counts { passed: 8, failed: 1, skipped: 0 }, ms: 10 }]);
        red.failures = vec!["auth: a flake".into()];
        red
    }

    fn attempt(mut s: Summary, attempt: u32) -> Summary {
        s.attempt = attempt;
        s
    }

    /// CI's re-run of a failed shard: its first attempt's red summary stays
    /// beside the re-run's green one, in either order, and the re-run's
    /// counts; the other shard's first attempt counts as it is.
    #[test]
    fn a_rerun_shards_newest_attempt_counts() {
        let rerun = attempt(two_shards().remove(1), 2);
        for summaries in [vec![red_first_attempt(), two_shards().remove(0), rerun.clone()], vec![rerun.clone(), two_shards().remove(0), red_first_attempt()]] {
            let combined = combine(&summaries).unwrap();
            assert_eq!(combined.totals, Counts { passed: 29, failed: 0, skipped: 1 });
            assert!(combined.failures.is_empty(), "{:?}", combined.failures);
            assert_eq!(combined.set_aside, [(Some(Shard { k: 1, n: 2 }), 1)]);
            let mut counted: Vec<_> = combined.counted.iter().map(|s| (s.shard.unwrap().k, s.attempt)).collect();
            counted.sort();
            assert_eq!(counted, [(1, 2), (2, 1)]);
        }
        // the newest counts whether it passed: a re-run that fails keeps the check red
        let green_then_red = [attempt(two_shards().remove(1), 1), two_shards().remove(0), attempt(red_first_attempt(), 2)];
        let combined = combine(&green_then_red).unwrap();
        assert_eq!((combined.totals.failed, combined.failures), (1, vec![("auth: a flake".to_string(), Some(Shard { k: 1, n: 2 }))]));
        // three attempts: the third counts, the two before are set aside
        let thrice = [red_first_attempt(), attempt(red_first_attempt(), 3), two_shards().remove(0), attempt(red_first_attempt(), 2)];
        let combined = combine(&thrice).unwrap();
        assert_eq!((combined.totals.failed, combined.set_aside.len()), (1, 2));
        // a whole run re-run is the same choice
        let whole = summary(None, vec![section("auth", 9), section("agents", 20), hermes()]);
        let combined = combine(&[attempt(whole.clone(), 2), whole]).unwrap();
        assert_eq!((combined.shards, combined.counted[0].attempt, combined.set_aside), (1, 2, vec![(None, 1)]));
    }

    /// Attempts are no substitute for a shard: one that no attempt reported
    /// still fails the check, however many attempts the others made.
    #[test]
    fn a_missing_shard_still_fails_beside_attempts() {
        let shard_2 = two_shards().remove(0);
        let only_shard_2 = [shard_2.clone(), attempt(shard_2.clone(), 2), attempt(shard_2, 3)];
        assert_eq!(combine(&only_shard_2), Err(SummaryError::ShardMissing(Shard { k: 1, n: 2 })));
        // and a set-aside attempt never fills a section the newest left out
        let mut emptied = attempt(two_shards().remove(1), 2);
        emptied.sections.clear();
        emptied.totals = Counts::default();
        assert_eq!(combine(&[two_shards().remove(1), two_shards().remove(0), emptied]), Err(SummaryError::SectionMissing("auth".into())));
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
        cases.push((twice, SummaryError::ShardTwice { shard: Some(shard(2, 2)), attempt: 1 }));
        let mut whole_twice = vec![summary(None, vec![section("auth", 9), section("agents", 20), hermes()])];
        whole_twice.push(whole_twice[0].clone());
        cases.push((whole_twice, SummaryError::ShardTwice { shard: None, attempt: 1 }));
        for bad in [0, ATTEMPTS_MAX + 1] {
            let mut attempts = two_shards();
            attempts[1].attempt = bad;
            cases.push((attempts, SummaryError::AttemptInvalid { shard: Some(shard(1, 2)), attempt: bad }));
        }
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
