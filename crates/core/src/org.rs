//! Orgs and seats (docs/billing.md; docs/cloudflare-v1.md, decisions 51 to
//! 53): the rules the registry keeps them by, host-tested. An org is who
//! pays; its people are admins, seats' holders, or both, each a person or
//! an email a seat waits on. A seat is comped (an operator's, outside
//! Stripe) or paid (counted on the org's subscription, whose status is
//! Stripe's). The registry is the one writer of a seat holder's plan: it
//! tells their ledger and their computer what `desired` says.

use fragment_proto::ledger::{Plan, SeatState};
use fragment_proto::org::SeatKind;

/// Seats one org holds (pending ones included).
pub const SEATS_PER_ORG_MAX: u64 = 200;
/// Admins one org has (pending ones included); it keeps at least one.
pub const ADMINS_PER_ORG_MAX: u64 = 10;
/// Orgs that may offer one email a seat at once (it takes one).
pub const OFFERS_PER_EMAIL_MAX: u64 = 10;
/// An org's name: an org of one is named by its email, so as long as one.
pub const ORG_NAME_MAX_BYTES: usize = 320;
/// People whose plans one run of the registry's alarm pushes.
pub const SYNC_BATCH: u32 = 20;
/// How soon a push that failed is tried again.
pub const SYNC_RETRY_MS: i64 = 10_000;

/// A subscription's status, as Stripe names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Trialing,
    Active,
    PastDue,
    Canceled,
    Unpaid,
    Incomplete,
    IncompleteExpired,
    Paused,
}

impl Status {
    pub fn parse(s: &str) -> Option<Status> {
        Some(match s {
            "trialing" => Status::Trialing,
            "active" => Status::Active,
            "past_due" => Status::PastDue,
            "canceled" => Status::Canceled,
            "unpaid" => Status::Unpaid,
            "incomplete" => Status::Incomplete,
            "incomplete_expired" => Status::IncompleteExpired,
            "paused" => Status::Paused,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Status::Trialing => "trialing",
            Status::Active => "active",
            Status::PastDue => "past_due",
            Status::Canceled => "canceled",
            Status::Unpaid => "unpaid",
            Status::Incomplete => "incomplete",
            Status::IncompleteExpired => "incomplete_expired",
            Status::Paused => "paused",
        }
    }

    /// Decision 53: trialing, active and past due are good (Stripe's
    /// retries are the grace); anything else lapses the org's paid seats,
    /// whichever last step (cancel, or mark unpaid) the account's retry
    /// settings take.
    pub fn is_good(self) -> bool {
        matches!(self, Status::Trialing | Status::Active | Status::PastDue)
    }
}

/// A seat its holder holds, as the registry reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Held {
    pub kind: SeatKind,
    pub comped: bool,
    /// The org's subscription's status (`None`: it has none).
    pub status: Option<Status>,
    /// Their choice to let a `seat_always_on` computer sleep.
    pub sleeps: bool,
}

impl Held {
    /// A comped seat is always good; a paid one, while its org's
    /// subscription is.
    pub fn good(&self) -> bool {
        self.comped || self.status.is_some_and(Status::is_good)
    }
}

/// What a person's ledger and computer are told.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Desired {
    /// Their plan, while they hold a seat (`None`: left as it is).
    pub plan: Option<Plan>,
    pub seat: SeatState,
    pub always_on: bool,
}

/// What holding `held` (or no seat) means for a person: their seat's plan,
/// active while it is good and canceled when it lapses or goes (decision
/// 53: today's `canceled`, the plan kept); a `seat_always_on` computer
/// awake while it is good, unless they let it sleep (decision 57).
pub fn desired(held: Option<Held>) -> Desired {
    match held {
        None => Desired { plan: None, seat: SeatState::Canceled, always_on: false },
        Some(h) => {
            let good = h.good();
            Desired {
                plan: Some(h.kind.plan()),
                seat: if good { SeatState::Active } else { SeatState::Canceled },
                always_on: good && h.kind == SeatKind::SeatAlwaysOn && !h.sleeps,
            }
        }
    }
}

/// The next push's place in a ledger's seat order (`SetSeat::seq`): later
/// than the last, and never behind the clock in ms (the order any other
/// writer's `seq` was given in).
pub fn next_seq(now_ms: i64, last: u64) -> u64 {
    let now = u64::try_from(now_ms.max(0)).expect("a non-negative i64 fits a u64");
    now.max(last.saturating_add(1))
}

/// An org's name as kept: trimmed, 1 to `ORG_NAME_MAX_BYTES` bytes, no
/// control characters.
pub fn org_name(raw: &str) -> Option<String> {
    let name = raw.trim();
    (!name.is_empty() && name.len() <= ORG_NAME_MAX_BYTES && !name.chars().any(char::is_control)).then(|| name.to_string())
}

/// An org's id: `org-` and 16 lowercase hex.
pub fn valid_org_id(id: &str) -> bool {
    id.strip_prefix("org-").is_some_and(hex16)
}

/// A member row's id (a seat, an admin, or both): `mem-` and 16 lowercase hex.
pub fn valid_member_id(id: &str) -> bool {
    id.strip_prefix("mem-").is_some_and(hex16)
}

fn hex16(s: &str) -> bool {
    s.len() == 16 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// A search as a SQL LIKE prefix (`ESCAPE '\'`): trimmed, lower case,
/// its wildcards escaped.
pub fn like_prefix(q: &str) -> String {
    let mut p = String::with_capacity(q.len() + 1);
    for c in q.trim().to_ascii_lowercase().chars() {
        if matches!(c, '%' | '_' | '\\') {
            p.push('\\');
        }
        p.push(c);
    }
    p.push('%');
    p
}

// ------------------------------------------------------------- trials

/// A trial code's alphabet: base32 without O, I, 0 and 1, so a code read
/// aloud or typed from paper is never ambiguous (finite-mono's).
pub const TRIAL_ALPHABET: &[u8; 32] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
/// A trial's days, and a code's places (decision 56).
pub const TRIAL_DAYS_MAX: u32 = 30;
pub const TRIAL_CAPACITY_MAX: u64 = 10_000;
/// A code's name, as operators read it.
pub const TRIAL_NAME_MAX_BYTES: usize = 120;
/// Codes a fleet keeps (each with its uses).
pub const TRIAL_CODES_MAX: u64 = 1_000;

/// A code as typed: its letters and digits, upper case, spaces and dashes
/// dropped; 8 to 64 of them, all of the alphabet's.
pub fn trial_code(raw: &str) -> Option<String> {
    let code: String = raw.chars().filter(|c| !c.is_whitespace() && *c != '-').map(|c| c.to_ascii_uppercase()).collect();
    (8..=64).contains(&code.len()).then_some(())?;
    code.bytes().all(|b| TRIAL_ALPHABET.contains(&b)).then_some(code)
}

/// A new code from 10 random bytes: 16 of the alphabet's characters (80
/// bits), as `XXXX-XXXX-XXXX-XXXX`.
pub fn new_trial_code(random: [u8; 10]) -> String {
    let mut bits: u128 = 0;
    for b in random {
        bits = (bits << 8) | u128::from(b);
    }
    let chars: Vec<char> = (0..16).map(|i| TRIAL_ALPHABET[((bits >> (5 * (15 - i))) & 31) as usize] as char).collect();
    chars.chunks(4).map(|c| c.iter().collect::<String>()).collect::<Vec<_>>().join("-")
}

/// A code as it is shown: in groups of four.
pub fn shown_trial_code(code: &str) -> String {
    code.as_bytes().chunks(4).map(|c| String::from_utf8_lossy(c).into_owned()).collect::<Vec<_>>().join("-")
}

/// Whether a code has a place: its uses that bought a subscription, and
/// its Checkouts still open, are fewer than its capacity. A Checkout never
/// completed frees its place when it expires, with nothing to clean up.
pub fn trial_has_place(capacity: u64, subscribed: u64, open: u64) -> bool {
    subscribed.saturating_add(open) < capacity
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [Status; 8] =
        [Status::Trialing, Status::Active, Status::PastDue, Status::Canceled, Status::Unpaid, Status::Incomplete, Status::IncompleteExpired, Status::Paused];

    #[test]
    fn statuses_round_trip_and_three_are_good() {
        for s in ALL {
            assert_eq!(Status::parse(s.as_str()), Some(s));
        }
        let good: Vec<_> = ALL.iter().filter(|s| s.is_good()).collect();
        assert_eq!(good, [&Status::Trialing, &Status::Active, &Status::PastDue]);
        assert_eq!(Status::parse("Active"), None);
    }

    fn held(kind: SeatKind, comped: bool, status: Option<Status>, sleeps: bool) -> Option<Held> {
        Some(Held { kind, comped, status, sleeps })
    }

    #[test]
    fn a_comped_seat_is_good_whatever_its_org_pays() {
        for status in [None, Some(Status::Canceled), Some(Status::Unpaid)] {
            let d = desired(held(SeatKind::Seat, true, status, false));
            assert_eq!(d, Desired { plan: Some(Plan::Seat), seat: SeatState::Active, always_on: false });
        }
    }

    #[test]
    fn a_paid_seat_follows_its_orgs_status() {
        for s in ALL {
            let d = desired(held(SeatKind::Seat, false, Some(s), false));
            assert_eq!(d.plan, Some(Plan::Seat), "the plan is kept when it lapses");
            assert_eq!(d.seat == SeatState::Active, s.is_good(), "{s:?}");
        }
        assert_eq!(desired(held(SeatKind::Seat, false, None, false)).seat, SeatState::Canceled, "a paid seat with no subscription is not good");
    }

    #[test]
    fn an_always_on_seat_keeps_its_computer_awake_while_good_unless_let_sleep() {
        assert!(desired(held(SeatKind::SeatAlwaysOn, true, None, false)).always_on);
        assert!(!desired(held(SeatKind::SeatAlwaysOn, true, None, true)).always_on, "its holder let it sleep");
        assert!(!desired(held(SeatKind::SeatAlwaysOn, false, Some(Status::Canceled), false)).always_on, "lapsed");
        assert!(desired(held(SeatKind::SeatAlwaysOn, false, Some(Status::PastDue), false)).always_on, "past due is good");
        assert!(!desired(held(SeatKind::Seat, true, None, false)).always_on, "a $100 seat's computer sleeps");
        assert_eq!(desired(held(SeatKind::SeatAlwaysOn, true, None, false)).plan, Some(Plan::SeatAlwaysOn));
    }

    #[test]
    fn no_seat_cancels_and_leaves_the_plan() {
        assert_eq!(desired(None), Desired { plan: None, seat: SeatState::Canceled, always_on: false });
    }

    #[test]
    fn the_seq_never_goes_back_and_keeps_up_with_the_clock() {
        assert_eq!(next_seq(1_000, 0), 1_000);
        assert_eq!(next_seq(1_000, 1_000), 1_001, "the same ms twice");
        assert_eq!(next_seq(500, 1_000), 1_001, "a clock behind the last");
        assert_eq!(next_seq(-5, 0), 1, "never 0: a ledger refuses seq 0");
        assert_eq!(next_seq(0, u64::MAX), u64::MAX, "saturates");
    }

    #[test]
    fn names_and_ids() {
        assert_eq!(org_name("  Acme  ").as_deref(), Some("Acme"));
        assert_eq!(org_name("   "), None);
        assert_eq!(org_name("a\nb"), None);
        assert!(org_name(&"x".repeat(ORG_NAME_MAX_BYTES)).is_some());
        assert!(org_name(&"x".repeat(ORG_NAME_MAX_BYTES + 1)).is_none());
        assert!(valid_org_id("org-0123456789abcdef"));
        assert!(!valid_org_id("org-0123456789ABCDEF"));
        assert!(!valid_org_id("mem-0123456789abcdef"));
        assert!(!valid_org_id("org-0123"));
        assert!(valid_member_id("mem-0123456789abcdef"));
        assert!(!valid_member_id("mem-0123456789abcdeg"));
    }

    #[test]
    fn trial_codes_read_as_typed_and_are_made_unambiguous() {
        assert_eq!(trial_code("abcd-efgh jkmn-pqrs").as_deref(), Some("ABCDEFGHJKMNPQRS"));
        assert_eq!(trial_code("ABCD-EFG"), None, "fewer than 8");
        assert_eq!(trial_code("ABCD-EFGO"), None, "O is not the alphabet's");
        assert_eq!(trial_code("ABCD-EFG1"), None, "nor 1");
        assert_eq!(trial_code(&"A".repeat(65)), None);
        let made = new_trial_code([0xff; 10]);
        assert_eq!(made, "9999-9999-9999-9999");
        let made = new_trial_code([1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);
        assert_eq!(made.len(), 19);
        assert_eq!(trial_code(&made).map(|c| shown_trial_code(&c)), Some(made.clone()), "a made code reads back as itself");
        assert_ne!(new_trial_code([0; 10]), new_trial_code([0, 0, 0, 0, 0, 0, 0, 0, 0, 1]));
    }

    #[test]
    fn a_trial_place_is_taken_by_a_subscription_or_an_open_checkout() {
        assert!(trial_has_place(2, 0, 1));
        assert!(!trial_has_place(2, 1, 1), "the last place is held by an open Checkout");
        assert!(!trial_has_place(2, 2, 0));
        assert!(!trial_has_place(0, 0, 0));
    }

    #[test]
    fn a_search_is_a_prefix_with_its_wildcards_escaped() {
        assert_eq!(like_prefix(" Ann@"), "ann@%");
        assert_eq!(like_prefix("a_b%c\\"), "a\\_b\\%c\\\\%");
    }
}
