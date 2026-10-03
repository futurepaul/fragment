//! The usage ledger's public wire types (docs/ledger.md): what the shell
//! and the CLI read (`LedgerStatus`) and what operators, Stripe's hooks
//! and fragment owners send (credit, plans, seats, overdrafts, caps).
//! Money is integer micro-dollars. The ledger's state machine is
//! `fragment_core::ledger`; the platform's own inner routes (reserve,
//! settle, release, meter) are its types, not these.

use serde::{Deserialize, Serialize};

/// A person's plan (docs/cloudflare-v1.md, decision 25). A guest signs in
/// free and uses what is shared with them: no agents, no AI, no computer,
/// and nothing billed to them. A seat includes credit each month; the
/// $200 seat's computer is always on, its awake time not metered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Plan {
    Guest,
    /// $100 a month, $50 of credit included, a computer that sleeps.
    Seat,
    /// $200 a month, $100 of credit included, an always-on computer.
    SeatAlwaysOn,
}

/// A seat's state, as its payment provider reports it (Stripe, later).
/// It means nothing on a guest's ledger.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SeatState {
    Active,
    /// A payment failed and is being retried: the seat keeps this month's
    /// credit, and next month's waits until it is active again.
    PastDue,
    /// The seat ended: agents stop; fragments keep serving on credit.
    Canceled,
}

/// What a person's money still lets happen (decision 27).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "standing", rename_all = "snake_case")]
pub enum Standing {
    /// Agents run, AI steps run, fragments take writes.
    Ok,
    /// No agent turns, no wakes, no AI steps; fragments still take writes.
    AgentsStopped { why: Why },
    /// The person's fragments serve but take no writes, and agents stop.
    ReadOnly { why: Why },
}

/// Why a standing is short of `Ok`, for the shell to say.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Why {
    /// A guest has no agents.
    Guest,
    /// The seat was canceled.
    SeatCanceled,
    /// The balance is zero or less.
    NoCredit,
    /// The balance reached the overdraft, and has not been above zero since.
    Overdrawn,
}

/// One fragment's spend this month on its owner's ledger, and its cap.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FragmentSpend {
    pub fragment: String,
    pub spent_micros: i64,
    pub cap_micros: i64,
}

/// A person's ledger now (`GET /api/ledger`). The balance is the included
/// credit left this month plus the purchased credit left (negative while
/// the person owes); `available` is what a new reservation may take.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LedgerStatus {
    pub plan: Plan,
    pub seat: SeatState,
    /// The UTC month, `YYYY-MM`.
    pub month: String,
    pub balance_micros: i64,
    pub included_micros: i64,
    /// This month's included credit, as granted (it expires at the month's end).
    pub included_granted_micros: i64,
    pub purchased_micros: i64,
    pub reserved_micros: i64,
    pub available_micros: i64,
    pub overdraft_micros: i64,
    pub standing: Standing,
    /// The price book's version this ledger charges with.
    pub price_book: u32,
    /// This month's spend by fragment, the largest first (a bounded list).
    pub fragments: Vec<FragmentSpend>,
}

/// Credit granted: bought (Stripe's hook), or given by an operator. Once
/// per `id` (Stripe's payment id, or the operator's command id).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GrantCredit {
    pub id: String,
    pub micros: i64,
    /// Who granted it: an operator's identity, or `stripe`.
    pub by: String,
    /// Why, for people reading the ledger later.
    pub why: String,
}

/// A person's plan, set by an operator (and by Stripe's hook, later).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SetPlan {
    pub id: String,
    pub plan: Plan,
}

/// A seat's state, from its payment provider's hook. Hooks arrive out of
/// order, so each names its place in the seat's own order (`seq`, from 1:
/// Stripe's event time in ms, later), and a change older than the last
/// one applied changes nothing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SetSeat {
    pub id: String,
    pub seat: SeatState,
    pub seq: u64,
}

/// How far below zero a person's fragments keep taking writes (an
/// operator's; $2 until set).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SetOverdraft {
    pub id: String,
    pub micros: i64,
}

/// A fragment's monthly cap, set by its owner on the owner's ledger
/// (decision 26). `None` goes back to the default ($5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SetFragmentCap {
    pub id: String,
    pub fragment: String,
    pub micros: Option<i64>,
}

/// `PUT /api/f/<name>/cap` (the fragment's owner): its monthly cap, in
/// micro-dollars, or `None` for the default. The path names the fragment;
/// `id` makes the change once (the same id again changes nothing).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PutCap {
    pub id: String,
    pub micros: Option<i64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Goal: the shell reads a standing by its tag and its reason; the
    /// names are the documented ones. Method: each against literal JSON.
    #[test]
    fn standings_and_plans_are_the_documented_shape() {
        assert_eq!(serde_json::to_value(Standing::Ok).unwrap(), json!({ "standing": "ok" }));
        assert_eq!(
            serde_json::to_value(Standing::AgentsStopped { why: Why::NoCredit }).unwrap(),
            json!({ "standing": "agents_stopped", "why": "no_credit" })
        );
        assert_eq!(serde_json::to_value(Standing::ReadOnly { why: Why::Overdrawn }).unwrap(), json!({ "standing": "read_only", "why": "overdrawn" }));
        assert_eq!(serde_json::to_value(Plan::SeatAlwaysOn).unwrap(), json!("seat_always_on"));
        assert_eq!(serde_json::to_value(SeatState::PastDue).unwrap(), json!("past_due"));
        for why in [Why::Guest, Why::SeatCanceled, Why::NoCredit, Why::Overdrawn] {
            let s = Standing::AgentsStopped { why };
            assert_eq!(serde_json::from_value::<Standing>(serde_json::to_value(s).unwrap()).unwrap(), s);
        }
    }

    /// Requests are camelCase and refuse fields they do not name, so a
    /// misspelt `micros` is an error, not a grant of nothing.
    #[test]
    fn requests_refuse_unknown_fields() {
        let g: GrantCredit = serde_json::from_value(json!({ "id": "g1", "micros": 5, "by": "stripe", "why": "top-up" })).unwrap();
        assert_eq!(g.micros, 5);
        assert!(serde_json::from_value::<GrantCredit>(json!({ "id": "g1", "micro": 5, "by": "stripe", "why": "" })).is_err());
        let cap: SetFragmentCap = serde_json::from_value(json!({ "id": "c1", "fragment": "todo.ann", "micros": null })).unwrap();
        assert_eq!(cap.micros, None);
    }
}
