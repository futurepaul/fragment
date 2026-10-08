//! Orgs and seats' public wire types (docs/billing.md; docs/cloudflare-v1.md,
//! decisions 51 to 53): what the shell and the CLI read of a person's seat
//! and their org, and what operators send to comp a seat. The rules are
//! `fragment_core::org`; the registry keeps the rows.

use serde::{Deserialize, Serialize};

use crate::ledger::Plan;

/// A seat's kind: the plan it gives its holder (decision 25).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SeatKind {
    /// $100 a month: a computer that sleeps when idle.
    Seat,
    /// $200 a month: a computer that stays awake, unless its owner lets it sleep.
    SeatAlwaysOn,
}

impl SeatKind {
    pub fn plan(self) -> Plan {
        match self {
            SeatKind::Seat => Plan::Seat,
            SeatKind::SeatAlwaysOn => Plan::SeatAlwaysOn,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            SeatKind::Seat => "seat",
            SeatKind::SeatAlwaysOn => "seat_always_on",
        }
    }

    pub fn parse(s: &str) -> Option<SeatKind> {
        match s {
            "seat" => Some(SeatKind::Seat),
            "seat_always_on" => Some(SeatKind::SeatAlwaysOn),
            _ => None,
        }
    }
}

/// An org's name and id, as a seat's holder sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrgRef {
    pub id: String,
    pub name: String,
}

/// A person's own seat (`GET /api/seat`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SeatView {
    /// The seat's id in its org.
    pub id: String,
    pub org: OrgRef,
    pub kind: SeatKind,
    /// An operator's, outside Stripe.
    pub comped: bool,
    /// Whether the seat is in good standing (a lapsed one stops agents).
    pub good: bool,
    /// Whether its holder lets their `seat_always_on` computer sleep.
    pub sleeps: bool,
    pub admin: bool,
    /// While its org's subscription is a trial: when it ends (seconds).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trial_ends: Option<i64>,
}

/// A seat waiting for a person: its org named it to their email, and they
/// hold another seat (or belong to another org) already.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OfferedSeat {
    pub id: String,
    pub org: OrgRef,
    pub kind: SeatKind,
}

/// `GET /api/seat`'s answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MySeat {
    pub seat: Option<SeatView>,
    /// The org they are in without a seat (an admin who holds none).
    pub org: Option<OrgRef>,
    pub admin: bool,
    pub offered: Vec<OfferedSeat>,
}

/// `PUT /api/seat`: a `seat_always_on` holder lets their computer sleep, or not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SetSleeps {
    pub sleeps: bool,
}

/// One of an org's people: an admin, a seat's holder, or both, held by a
/// person (`person`) or pending on an email.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrgMember {
    /// The row's id: a seat is changed or removed by it.
    pub id: String,
    /// `None`: pending on `email` until someone signs in with it verified.
    pub person: Option<String>,
    /// The email it was offered to; a held one shows its holder's latest.
    pub email: String,
    pub admin: bool,
    pub seat: Option<SeatKind>,
    pub comped: bool,
    pub added_at: i64,
}

/// `GET /api/org` (an admin's view; an operator's of any org).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrgView {
    pub id: String,
    pub name: String,
    pub created_at: i64,
    pub members: Vec<OrgMember>,
}

/// `POST /api/admin/seats`: an operator comps a seat of `kind` for
/// `email`, in `org` (an existing one), or else in the org its person is
/// in, or else in a new org of one named by the email.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CompSeat {
    pub email: String,
    pub kind: SeatKind,
    #[serde(default)]
    pub org: Option<String>,
}

/// `PATCH /api/admin/seats/<id>`: a comped seat's new kind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SetSeatKind {
    pub kind: SeatKind,
}

/// An operator's comp, done: the seat, and the org it is in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Comped {
    pub seat: OrgMember,
    pub org: OrgRef,
    /// Whether this call made the seat (a replay answers the same seat, `false`).
    pub created: bool,
    /// Whether the platform mailed the email its seat (a new comp, on a
    /// deployment that sends mail; a mail that failed is `false`, the seat made).
    #[serde(default)]
    pub mailed: bool,
}

/// A trial code (decision 56), as operators see it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrialCode {
    pub id: String,
    /// As shown: `XXXX-XXXX-XXXX-XXXX`.
    pub code: String,
    pub name: String,
    pub kind: SeatKind,
    pub days: u32,
    pub capacity: u64,
    /// After this (ms), it is refused.
    pub expires_at: Option<i64>,
    pub active: bool,
    /// Each change raises it: a change names the one it saw.
    pub revision: u64,
    pub created_at: i64,
    pub created_by: String,
    /// Its uses that bought a subscription, and its Checkouts still open.
    pub subscribed: u64,
    pub open: u64,
    pub uses: Vec<TrialUse>,
}

/// Someone's use of a trial code.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrialUse {
    pub person: String,
    pub email: Option<String>,
    pub at: i64,
    /// Its subscription's status, once its Checkout completed.
    pub status: Option<String>,
}

/// `POST /api/admin/trials`: a new code; `code` none, one is made.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NewTrialCode {
    pub name: String,
    pub kind: SeatKind,
    pub days: u32,
    pub capacity: u64,
    #[serde(default)]
    pub expires_at: Option<i64>,
    #[serde(default)]
    pub code: Option<String>,
}

/// `PATCH /api/admin/trials/<id>`: what changes, if `revision` is still
/// the code's (else 409). Capacity only grows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TrialCodeChange {
    pub revision: u64,
    #[serde(default)]
    pub capacity: Option<u64>,
    #[serde(default)]
    pub active: Option<bool>,
    #[serde(default)]
    pub code: Option<String>,
    #[serde(default)]
    pub expires_at: Option<i64>,
}

/// A person as the operators' list shows them (`GET /api/admin/people`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminPerson {
    pub npub: String,
    pub email: Option<String>,
    pub joined_at: i64,
    pub last_sign_in_at: Option<i64>,
    pub org: Option<OrgRef>,
    pub admin: bool,
    pub seat: Option<AdminSeat>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminSeat {
    pub id: String,
    pub kind: SeatKind,
    pub comped: bool,
    pub good: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminPeople {
    pub people: Vec<AdminPerson>,
    /// Ask after this for the next page (`None`: the last).
    pub next: Option<String>,
}

/// An org as the operators' list shows it (`GET /api/admin/orgs`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminOrg {
    pub id: String,
    pub name: String,
    pub created_at: i64,
    /// Its subscription's status (`None`: it never paid).
    pub status: Option<String>,
    pub customer: Option<String>,
    pub subscription: Option<String>,
    pub period_end: Option<i64>,
    pub cancel_at_end: bool,
    pub admins: u64,
    pub paid_seats: u64,
    pub comped_seats: u64,
    pub pending: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminOrgs {
    pub orgs: Vec<AdminOrg>,
    pub next: Option<String>,
}

/// One push waiting in a queue, tried and failed at least once.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Failing {
    /// `plan` (a person's ledger and computer) or `quantity` (an org's
    /// subscription in Stripe).
    pub queue: String,
    pub target: String,
    pub tries: u64,
    pub due: i64,
}

/// Billing's health (`GET /api/admin/health`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminHealth {
    pub orgs_paying: u64,
    pub orgs_lapsed: u64,
    pub seats_paid: u64,
    pub seats_comped: u64,
    pub plan_pushes_queued: u64,
    pub quantity_pushes_queued: u64,
    pub failing: Vec<Failing>,
    /// The newest subscription event applied (Stripe's seconds).
    pub last_event_at: Option<i64>,
    /// The oldest copy of a subscription (ms): the reconcile keeps it
    /// within a day.
    pub oldest_copy_at: Option<i64>,
}

/// What an operator did (`GET /api/admin/log`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminLogEntry {
    pub n: u64,
    pub at: i64,
    pub operator: String,
    pub action: String,
    pub target: String,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminLog {
    pub entries: Vec<AdminLogEntry>,
    /// Ask before this for the next page (`None`: the last).
    pub next: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_round_trip_and_name_their_plans() {
        for k in [SeatKind::Seat, SeatKind::SeatAlwaysOn] {
            assert_eq!(SeatKind::parse(k.as_str()), Some(k));
            assert_eq!(serde_json::to_value(k).unwrap(), serde_json::json!(k.as_str()));
            assert_eq!(serde_json::to_value(k.plan()).unwrap(), serde_json::json!(k.as_str()));
        }
        assert_eq!(SeatKind::parse("guest"), None);
    }

    #[test]
    fn a_comp_refuses_fields_it_does_not_take() {
        let ok: CompSeat = serde_json::from_value(serde_json::json!({ "email": "a@b.c", "kind": "seat" })).unwrap();
        assert_eq!(ok.org, None);
        assert!(serde_json::from_value::<CompSeat>(serde_json::json!({ "email": "a@b.c", "kind": "seat", "paid": true })).is_err());
        assert!(serde_json::from_value::<CompSeat>(serde_json::json!({ "email": "a@b.c", "kind": "guest" })).is_err());
    }
}
