//! The computer's agents while it is awake (docs/computers.md: a computer's
//! agents may change while it runs), as pure functions of what the boot
//! runs and what `GET /api/computer` says now. A change is carried out with
//! no restart of anything (main.rs, `follow_agents`): Hermes v0.21.5's
//! multiplexed gateway serves a profile written after it started (its
//! `rescan-profiles` control verb, and every relayed turn resolving its
//! profile's directory as it arrives), and the bridge runs an agent once
//! the ready file names it (the bridge's `ready.rs`). So no turn of
//! another agent is ever cut by a change, and none waits for one.

use std::collections::BTreeSet;

use fragment_bridge::runtime::relay::wire;
use fragment_bridge::runtime::Agent;
use serde_json::{json, Value};

/// What changed between the agents the boot runs and the agents the
/// platform lists now, each by its fragment (its profile's name).
#[derive(Debug, Default, PartialEq)]
pub struct Change {
    /// New to this computer: each needs its profile before the bridge runs it.
    pub added: Vec<Agent>,
    /// No longer on this computer: the bridge stops running it at once; its
    /// profile is retired at the computer's next start, as a boot retires
    /// one (moving it while Hermes winds down a turn of its could leave a
    /// half profile behind).
    pub removed: Vec<Agent>,
    /// The agent whose desktop the screen shows (the first) is another.
    pub screen: bool,
}

impl Change {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty() && !self.screen
    }
}

/// The change from `running` to `read`. The same fragment is the same
/// agent: a profile is named by its fragment, and what else the platform
/// says of it (its identity, its name) the bridge reads for itself.
pub fn diff(running: &[Agent], read: &[Agent]) -> Change {
    let had: BTreeSet<&str> = running.iter().map(|a| a.fragment.as_str()).collect();
    let has: BTreeSet<&str> = read.iter().map(|a| a.fragment.as_str()).collect();
    assert_eq!(has.len(), read.len(), "the platform lists an agent once");
    Change {
        added: read.iter().filter(|a| !had.contains(a.fragment.as_str())).cloned().collect(),
        removed: running.iter().filter(|a| !has.contains(a.fragment.as_str())).cloned().collect(),
        screen: running.first().map(|a| &a.fragment) != read.first().map(|a| &a.fragment),
    }
}

/// The bridge's ready file (its `ready.rs`): every agent whose profile is
/// written, in the platform's order.
pub fn ready_file(agents: &[Agent]) -> String {
    json!({ "agents": agents.iter().map(|a| a.fragment.as_str()).collect::<Vec<_>>() }).to_string()
}

/// The profiles of `agents` the gateway's answer to `rescan-profiles` does
/// not list as served (`served_profiles`). An answer that is pending (the
/// gateway still starting, or a rescan that outlasted its five seconds),
/// or has no list, names none: every relayed turn finds its profile's
/// directory as it arrives anyway, and Hermes rescans every 30 s.
pub fn unserved(agents: &[Agent], answer: &Value) -> Vec<String> {
    if answer["pending"] == true {
        return vec![];
    }
    let Some(served) = answer["served_profiles"].as_array() else { return vec![] };
    agents.iter().map(|a| wire::profile(&a.fragment)).filter(|p| !served.iter().any(|s| s == p.as_str())).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(label: &str) -> Agent {
        Agent { fragment: format!("{label}.paul"), identity: format!("id:{label}"), name: label.into(), owner: "id:paul".into() }
    }

    fn names(agents: &[Agent]) -> Vec<&str> {
        agents.iter().map(|a| a.fragment.as_str()).collect()
    }

    /// Valid: an agent assigned while it runs is added, one unassigned is
    /// removed, and a new first agent moves the screen.
    #[test]
    fn a_change_is_what_was_added_and_removed() {
        let c = diff(&[agent("juniper")], &[agent("juniper"), agent("maple")]);
        assert_eq!((names(&c.added), names(&c.removed), c.screen), (vec!["maple.paul"], vec![], false));
        let c = diff(&[agent("juniper"), agent("maple")], &[agent("maple")]);
        assert_eq!((names(&c.added), names(&c.removed), c.screen), (vec![], vec!["juniper.paul"], true));
        // the first run: a computer started before its first agent was made
        let c = diff(&[], &[agent("juniper")]);
        assert_eq!((names(&c.added), c.screen), (vec!["juniper.paul"], true));
        let c = diff(&[agent("juniper")], &[]);
        assert_eq!((names(&c.removed), c.screen), (vec!["juniper.paul"], true));
    }

    /// Replay: the same set read again is no change, nor is an agent the
    /// platform describes anew (its fragment is its profile); the order
    /// matters only for the screen's agent.
    #[test]
    fn the_same_agents_again_are_no_change() {
        let set = [agent("juniper"), agent("maple")];
        assert!(diff(&set, &set).is_empty());
        assert!(diff(&[], &[]).is_empty());
        let mut renamed = agent("juniper");
        renamed.name = "Juniper".into();
        assert!(diff(&[agent("juniper")], &[renamed]).is_empty());
        let c = diff(&[agent("juniper"), agent("maple"), agent("oak")], &[agent("juniper"), agent("oak"), agent("maple")]);
        assert!(c.is_empty(), "{c:?}");
    }

    #[test]
    #[should_panic(expected = "the platform lists an agent once")]
    fn an_agent_listed_twice_is_a_platform_bug() {
        diff(&[], &[agent("juniper"), agent("juniper")]);
    }

    #[test]
    fn the_ready_file_names_every_agent_in_order() {
        assert_eq!(ready_file(&[agent("maple"), agent("juniper")]), r#"{"agents":["maple.paul","juniper.paul"]}"#);
        assert_eq!(ready_file(&[]), r#"{"agents":[]}"#);
    }

    /// The gateway's answer says whether it serves the new profiles; an
    /// answer without the list (pending, or not multiplexed) says nothing.
    #[test]
    fn a_rescan_answer_names_what_it_does_not_serve() {
        let answer = json!({ "multiplex": true, "added": ["maple-paul"], "removed": [], "served_profiles": ["default", "juniper-paul", "maple-paul"] });
        assert!(unserved(&[agent("juniper"), agent("maple")], &answer).is_empty());
        let answer = json!({ "multiplex": true, "served_profiles": ["default", "juniper-paul"] });
        assert_eq!(unserved(&[agent("juniper"), agent("maple")], &answer), vec!["maple-paul"]);
        assert!(unserved(&[agent("maple")], &json!({ "multiplex": true, "pending": true, "served_profiles": ["default"] })).is_empty());
        assert!(unserved(&[agent("maple")], &json!({ "multiplex": true })).is_empty());
    }
}
