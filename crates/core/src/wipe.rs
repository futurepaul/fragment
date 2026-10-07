//! Wiping a person (docs/api.md, Operators): the pure parts of the
//! operator's wipe, which deletes everything that is a person's or their
//! agents' and frees their username and sign-in, so their next sign-in is a
//! new person. The router runs it (cell/src/wipe.rs), the Registry keeps
//! its progress and locks the person while it runs (cell/src/registry/
//! wipe.rs); this file decides what those two would otherwise each decide:
//!
//! - **whom** an operator names (`named`): a username or an identity;
//! - **the steps**, in their order (`STEPS`), and how far a wipe has got
//!   (`Progress`): each step is idempotent, a step is done only after the
//!   one before it, and a wipe run again picks up at its first step not
//!   done, so a wipe cut anywhere (a crash, a timeout, a vendor's error)
//!   finishes when it is run again;
//! - **the confirmation** a wipe takes (`confirmed`): the identity the dry
//!   run named, so a username that changed hands between the two is never
//!   the wrong person wiped;
//! - **whose** a fragment on the person's lists is (`whose`): theirs (under
//!   their username: every fragment there is theirs), or someone else's,
//!   where only their membership goes;
//! - **what a report shows** (`listed`): counts, and a bounded list of names.

use fragment_proto::wipe::Listed;
use serde::{Deserialize, Serialize};

/// The names of fragments a report lists (its counts are whole).
pub const NAMES_SHOWN_MAX: usize = 50;

/// How long one wipe call works before it answers, the step it was in left
/// for the next call: a step's every unit (a fragment, a page of saves) is
/// bounded, so a call ends within this and one unit. The CLI calls again
/// until the wipe is done.
pub const CALL_BUDGET_MS: i64 = 20_000;

/// Fragments one call ends or leaves at most (the rest are the next call's).
pub const FRAGMENTS_PER_CALL: usize = 64;

/// Whom an operator names: a person's username, or an identity (a wipe of
/// one whose username is gone, or one finished, is named by its identity).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Named {
    Username(String),
    Identity(String),
}

/// Why a wipe is refused before anything changes: each is the operator's
/// to correct.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// Neither a username nor an identity.
    Malformed(String),
    /// An agent is wiped with its owner, never alone.
    NotAPerson,
    /// A wipe names the identity its dry run named.
    Unconfirmed,
    /// The person named is not the one the confirmation names.
    Mismatch { named: String, confirmed: String },
    /// An operator known by their identity wipes it: the wipe would
    /// delete the keys and sessions it is asked with, so it could never
    /// be run again (a dedicated operator key, held by no one, wipes
    /// anyone, its holder included).
    Themselves,
}

impl Refusal {
    pub fn message(&self) -> String {
        match self {
            Refusal::Malformed(what) => format!("{what:?} is neither a username nor an identity (id:…)"),
            Refusal::NotAPerson => "that is an agent: an agent is wiped with its owner, so name the person".into(),
            Refusal::Unconfirmed => "a wipe names, in `confirm`, the identity its dry run answered".into(),
            Refusal::Mismatch { named, confirmed } => {
                format!("the person named is {named}, not {confirmed}, whom the confirmation names: run the dry run again")
            }
            Refusal::Themselves => {
                "an operator known by their identity does not wipe it (the wipe ends the keys it is asked with): sign with an operator key no one holds".into()
            }
        }
    }
}

/// Whom `text` names: `id:<32 hex>` an identity, else a username.
pub fn named(text: &str) -> Result<Named, Refusal> {
    if crate::npub::is_identity(text) {
        return Ok(Named::Identity(text.to_string()));
    }
    if fragment_proto::valid_username(text) {
        return Ok(Named::Username(text.to_string()));
    }
    Err(Refusal::Malformed(text.chars().take(80).collect()))
}

/// A wipe's confirmation: the identity the dry run answered, exactly.
pub fn confirmed(identity: &str, confirm: Option<&str>) -> Result<(), Refusal> {
    assert!(crate::npub::is_identity(identity), "a wipe confirms an identity the registry named");
    match confirm {
        None | Some("") => Err(Refusal::Unconfirmed),
        Some(c) if c == identity => Ok(()),
        Some(c) => Err(Refusal::Mismatch { named: identity.to_string(), confirmed: c.chars().take(80).collect() }),
    }
}

/// Whether an operator may wipe `person`: one known only by a key always
/// may; one known by their identity may not wipe that identity
/// (`Refusal::Themselves`).
pub fn may_wipe(operator_identity: Option<&str>, person: &str) -> Result<(), Refusal> {
    match operator_identity {
        Some(me) if me == person => Err(Refusal::Themselves),
        _ => Ok(()),
    }
}

/// The steps of a wipe after it began (the Registry's `begin` locked the
/// person: their sessions and keys ended, no sign-in or agent made for
/// them, their username held), in the order they run. Each is idempotent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Step {
    /// Their computer: its container destroyed, its saves deleted from R2,
    /// its storage emptied (its snapshot's id with it).
    Computer,
    /// Every fragment they own ended, through the delete's end of life,
    /// each with its repo to delete.
    Fragments,
    /// Their and their agents' memberships in other people's fragments.
    Memberships,
    /// The ended fragments' cleanup done: their members' lists told, their
    /// apps' databases, blobs and repos deleted.
    Cleanup,
    /// Their ledger.
    Ledger,
    /// Their and their agents' lists of fragments (`Principal`s).
    Lists,
    /// Their picture's bytes (where no one else's is the same picture).
    Pictures,
    /// The Registry's rows: identities, keys, sign-ins, username, sessions.
    Registry,
}

/// Every step, in order.
pub const STEPS: [Step; 8] = [Step::Computer, Step::Fragments, Step::Memberships, Step::Cleanup, Step::Ledger, Step::Lists, Step::Pictures, Step::Registry];

const _: () = assert!(STEPS.len() == 8 && STEPS.len() <= u8::MAX as usize, "the steps fit a small count");

impl Step {
    pub fn name(self) -> &'static str {
        match self {
            Step::Computer => "computer",
            Step::Fragments => "fragments",
            Step::Memberships => "memberships",
            Step::Cleanup => "cleanup",
            Step::Ledger => "ledger",
            Step::Lists => "lists",
            Step::Pictures => "pictures",
            Step::Registry => "registry",
        }
    }

    /// Its place in `STEPS`.
    pub fn index(self) -> u32 {
        let i = STEPS.iter().position(|s| *s == self).expect("every step is in STEPS");
        u32::try_from(i).expect("the steps are few")
    }
}

/// How far a wipe has got: how many of `STEPS` are done. The Registry keeps
/// it (`wipes.done`); the router asks it which step is next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Progress {
    done: u32,
}

/// A step reported done.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Completed {
    /// It was the next step: the wipe moved on.
    Advanced,
    /// It was done already (a wipe run twice at once, or a report lost and
    /// sent again): nothing changes.
    Replayed,
}

/// A step reported done before the steps ahead of it: a bug in the caller,
/// never a state to keep.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutOfOrder {
    pub step: Step,
    pub next: Option<Step>,
}

/// A stored count no wipe makes: the row is corrupt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Corrupt(pub i64);

impl Default for Progress {
    fn default() -> Progress {
        Progress::new()
    }
}

impl Progress {
    /// A wipe just begun: no step done.
    pub const fn new() -> Progress {
        Progress { done: 0 }
    }

    /// As the Registry stored it.
    pub fn stored(done: i64) -> Result<Progress, Corrupt> {
        match u32::try_from(done) {
            Ok(n) if (n as usize) <= STEPS.len() => Ok(Progress { done: n }),
            _ => Err(Corrupt(done)),
        }
    }

    /// The count to store.
    pub fn done(self) -> u32 {
        assert!(self.done as usize <= STEPS.len(), "progress counts at most every step");
        self.done
    }

    /// The step to run next, or `None` once every step is done.
    pub fn next(self) -> Option<Step> {
        STEPS.get(self.done as usize).copied()
    }

    /// Every step is done: the person is wiped.
    pub fn finished(self) -> bool {
        self.next().is_none()
    }

    /// Whether `step` is done.
    pub fn has_done(self, step: Step) -> bool {
        step.index() < self.done
    }

    /// `step` is done: the wipe moves past it, or (done already) stays.
    pub fn complete(&mut self, step: Step) -> Result<Completed, OutOfOrder> {
        let before = self.done;
        let at = step.index();
        let completed = match at.cmp(&self.done) {
            std::cmp::Ordering::Less => Completed::Replayed,
            std::cmp::Ordering::Equal => {
                self.done += 1;
                Completed::Advanced
            }
            std::cmp::Ordering::Greater => return Err(OutOfOrder { step, next: self.next() }),
        };
        assert!(self.done >= before && self.done <= before + 1, "a step moves a wipe one step at most, never back");
        assert!(self.has_done(step), "a completed step is done");
        Ok(completed)
    }
}

/// Whose a fragment on the wiped person's lists (or their agents') is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Whose {
    /// Under their username: theirs (only its holder makes fragments
    /// under a username, an agent for its owner among them), ended with
    /// everything in it.
    Theirs,
    /// Someone else's: only the membership goes.
    Elsewhere,
}

/// Whose `fragment` is, for a person whose username is `username` (`None`:
/// they never chose one, so nothing is under it).
pub fn whose(fragment: &str, username: Option<&str>) -> Whose {
    match (fragment_proto::split_fragment_name(fragment), username) {
        (Some((_, under)), Some(theirs)) if under == theirs => Whose::Theirs,
        _ => Whose::Elsewhere,
    }
}

/// `names` as a report lists them (`fragment_proto::wipe::Listed`): sorted,
/// each once, all counted, the first `NAMES_SHOWN_MAX` shown.
pub fn listed(names: impl IntoIterator<Item = String>) -> Listed {
    let mut all: Vec<String> = names.into_iter().collect();
    all.sort();
    all.dedup();
    let count = all.len() as u64;
    let more = all.len() > NAMES_SHOWN_MAX;
    all.truncate(NAMES_SHOWN_MAX);
    assert!(all.len() <= NAMES_SHOWN_MAX && all.len() as u64 <= count, "a report's names are bounded");
    Listed { count, names: all, more }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(c: char) -> String {
        format!("id:{}", c.to_string().repeat(32))
    }

    /// Goal: an operator names a person by username or identity, and
    /// anything else is refused before anything is read. Method: each form.
    #[test]
    fn whom_a_wipe_names() {
        assert_eq!(named(&id('a')), Ok(Named::Identity(id('a'))));
        assert_eq!(named("paul"), Ok(Named::Username("paul".into())));
        for bad in ["", "Paul", "id:short", "me", "a--b", &"x".repeat(200)] {
            assert!(matches!(named(bad), Err(Refusal::Malformed(_))), "{bad:?}");
        }
        // the message quotes at most a bounded piece of what it was given
        assert!(Refusal::Malformed("x".repeat(80)).message().len() < 200);
    }

    /// Goal: a wipe changes nothing unless it names the identity its dry
    /// run answered; a username that changed hands between the two is
    /// refused, not followed. Method: valid, missing, empty and another.
    #[test]
    fn a_wipe_is_confirmed_by_identity() {
        assert_eq!(confirmed(&id('a'), Some(&id('a'))), Ok(()));
        assert_eq!(confirmed(&id('a'), None), Err(Refusal::Unconfirmed));
        assert_eq!(confirmed(&id('a'), Some("")), Err(Refusal::Unconfirmed));
        assert_eq!(confirmed(&id('a'), Some(&id('b'))), Err(Refusal::Mismatch { named: id('a'), confirmed: id('b') }));
        assert!(Refusal::Mismatch { named: id('a'), confirmed: id('b') }.message().contains("dry run"));
    }

    /// Goal: an operator key no one holds wipes anyone; an operator known by
    /// their identity wipes anyone but themselves. Method: both kinds.
    #[test]
    fn an_operator_by_identity_never_wipes_themselves() {
        assert_eq!(may_wipe(None, &id('a')), Ok(()));
        assert_eq!(may_wipe(Some(&id('b')), &id('a')), Ok(()));
        assert_eq!(may_wipe(Some(&id('a')), &id('a')), Err(Refusal::Themselves));
    }

    /// Goal: a whole wipe runs every step once, in order, and is then
    /// finished. Method: complete each step as `next` names it.
    #[test]
    fn a_wipe_runs_every_step_in_order() {
        let mut p = Progress::new();
        let mut ran = vec![];
        // bounded: one step a pass, STEPS of them
        while let Some(step) = p.next() {
            assert!(!p.has_done(step));
            assert_eq!(p.complete(step), Ok(Completed::Advanced));
            ran.push(step);
            assert!(ran.len() <= STEPS.len());
        }
        assert_eq!(ran, STEPS);
        assert!(p.finished());
        assert_eq!(p.done(), STEPS.len() as u32);
        assert!(STEPS.iter().all(|s| p.has_done(*s)));
        let names: Vec<&str> = STEPS.iter().map(|s| s.name()).collect();
        assert_eq!(names, ["computer", "fragments", "memberships", "cleanup", "ledger", "lists", "pictures", "registry"]);
        assert!(STEPS.iter().enumerate().all(|(i, s)| s.index() as usize == i));
    }

    /// Goal: a step reported twice (two wipes at once, a report sent again)
    /// changes nothing; one reported before the steps ahead of it is
    /// refused and changes nothing. Method: replay each step done; report
    /// a later one early.
    #[test]
    fn a_replayed_step_changes_nothing_and_a_skipped_one_is_refused() {
        let mut p = Progress::new();
        assert_eq!(p.complete(Step::Fragments), Err(OutOfOrder { step: Step::Fragments, next: Some(Step::Computer) }));
        assert_eq!(p, Progress::new(), "a refused step changes nothing");
        p.complete(Step::Computer).unwrap();
        p.complete(Step::Fragments).unwrap();
        for step in [Step::Computer, Step::Fragments] {
            assert_eq!(p.complete(step), Ok(Completed::Replayed));
        }
        assert_eq!(p.next(), Some(Step::Memberships));
        assert_eq!(p.complete(Step::Registry), Err(OutOfOrder { step: Step::Registry, next: Some(Step::Memberships) }));
        assert_eq!(p.next(), Some(Step::Memberships));
        // finished: every report is a replay
        let mut done = Progress::stored(STEPS.len() as i64).unwrap();
        for step in STEPS {
            assert_eq!(done.complete(step), Ok(Completed::Replayed));
        }
        assert!(done.finished());
    }

    /// Goal: a wipe cut at any step picks up there once its progress is read
    /// back (a node's restart between two calls), and a count no wipe makes
    /// is refused as corrupt. Method: store and read back at every count.
    #[test]
    fn a_wipe_cut_anywhere_picks_up_where_it_stopped() {
        let mut p = Progress::new();
        for (i, step) in STEPS.iter().enumerate() {
            assert_eq!(p.done() as usize, i, "one step done a pass");
            let back = Progress::stored(i64::from(p.done())).unwrap();
            assert_eq!(back, p, "read back as it was stored");
            assert_eq!(back.next(), Some(*step), "it goes on at the step it stopped before");
            p.complete(*step).unwrap();
        }
        assert!(Progress::stored(i64::from(p.done())).unwrap().finished());
        for bad in [-1, STEPS.len() as i64 + 1, i64::MAX] {
            assert_eq!(Progress::stored(bad), Err(Corrupt(bad)));
        }
    }

    /// Goal: a fragment under the person's username is theirs, any other
    /// someone else's (only their membership goes). Method: names under
    /// theirs, another's, a look-alike, and a person with no username.
    #[test]
    fn whose_a_listed_fragment_is() {
        assert_eq!(whose("todo.paul", Some("paul")), Whose::Theirs);
        assert_eq!(whose("juniper-chat.paul", Some("paul")), Whose::Theirs);
        assert_eq!(whose("todo.bob", Some("paul")), Whose::Elsewhere);
        assert_eq!(whose("todo.paula", Some("paul")), Whose::Elsewhere);
        assert_eq!(whose("todo.paul", None), Whose::Elsewhere);
        assert_eq!(whose("not a name", Some("paul")), Whose::Elsewhere);
    }

    /// Goal: a report counts every name and lists a bounded, sorted few.
    /// Method: fewer than the bound, duplicates, and more than it.
    #[test]
    fn a_report_lists_a_bounded_few() {
        let few = listed(["b.x".to_string(), "a.x".into(), "b.x".into()]);
        assert_eq!(few, Listed { count: 2, names: vec!["a.x".into(), "b.x".into()], more: false });
        let many = listed((0..NAMES_SHOWN_MAX + 5).map(|i| format!("f{i:03}.x")));
        assert_eq!((many.count, many.names.len(), many.more), ((NAMES_SHOWN_MAX + 5) as u64, NAMES_SHOWN_MAX, true));
        assert_eq!(listed(Vec::<String>::new()), Listed::default());
    }
}
