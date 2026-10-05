//! A person's nodes (bring your own computer, experimental: docs/self-host.md,
//! seam 2): pairing one (`fragment_core::pairing`), the nodes they may run
//! computers on, and where their new computers run. sandcastle's
//! `sandcastle-node pair` speaks the pairing half (its crates/node
//! `pair.rs` mirrors these shapes).

use serde::{Deserialize, Serialize};

/// `POST /api/nodes/pair` (unsigned: the node holds nothing yet): a node
/// asks to be paired, naming itself and its architecture.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PairStart {
    pub name: String,
    pub arch: String,
}

/// Its answer: the code a person compares, the link they approve it at,
/// and the code only the node holds, which it polls with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PairStarted {
    pub user_code: String,
    pub device_code: String,
    pub verify_url: String,
    pub expires_in_s: u32,
    pub interval_s: u32,
}

/// `POST /api/nodes/pair/poll`: the node asks whether it was approved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PairPoll {
    pub device_code: String,
}

/// Its answer. `Approved` is had once: the node's id and its secret, which
/// it writes to its secret's file and never shows. A poll replayed after it
/// finds no pairing (404).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum PairPolled {
    Pending { interval_s: u32 },
    SlowDown { interval_s: u32 },
    Expired,
    Approved { node: String, secret: String },
}

/// Whose a node is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    /// The deployment's (`FRAGMENT_NODES`): everyone's.
    Deployment,
    /// The person's own, paired: theirs alone.
    Own,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeState {
    Up,
    Down,
    Revoked,
}

/// One node a person may run computers on, or did (a revoked node of
/// theirs stays listed, with its computers).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeView {
    pub id: String,
    /// Its own name (a deployment node's id; a person's node's machine name).
    pub name: String,
    pub kind: NodeKind,
    pub arch: String,
    pub state: NodeState,
    /// Why it is down, as its object last found it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub why: Option<String>,
    /// The person's computers placed on it.
    pub computers: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paired_at: Option<i64>,
}

/// `GET /api/nodes`: whether this platform pairs people's nodes, the
/// node the person chose for new computers (`None`: the deployment's rule),
/// and every node they may use, the deployment's first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NodesView {
    pub byoc: bool,
    pub prefer: Option<String>,
    pub nodes: Vec<NodeView>,
}

/// `PUT /api/nodes/prefer`: where new computers run: a node, or `null` for
/// the deployment's rule. The field is named even when it is `null` (the
/// route refuses a body without it: a forgotten field is no reset).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PreferNode {
    pub node: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    // Goal: a poll's answer reads as sandcastle reads it: a state, and its
    // fields in camelCase.
    #[test]
    fn a_poll_answers_its_state() {
        let j = |p: &PairPolled| serde_json::to_value(p).unwrap();
        assert_eq!(j(&PairPolled::Pending { interval_s: 5 }), serde_json::json!({ "state": "pending", "intervalS": 5 }));
        assert_eq!(j(&PairPolled::SlowDown { interval_s: 10 }), serde_json::json!({ "state": "slow_down", "intervalS": 10 }));
        assert_eq!(j(&PairPolled::Expired), serde_json::json!({ "state": "expired" }));
        assert_eq!(j(&PairPolled::Approved { node: "paired-0".into(), secret: "s".into() }), serde_json::json!({ "state": "approved", "node": "paired-0", "secret": "s" }));
        assert!(serde_json::from_str::<PairStart>(r#"{"name":"mac","arch":"aarch64","id":"box"}"#).is_err(), "a node names no id of its own");
        assert_eq!(serde_json::from_str::<PreferNode>(r#"{"node":null}"#).unwrap(), PreferNode { node: None });
    }
}
