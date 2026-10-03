//! Computers' wire types (docs/computers.md): what the owner reads and
//! sends (`/api/computers…`), and what the guest reads about itself
//! (`GET /api/computer`). The lifecycle's own state is
//! `fragment_core::computer`.

use serde::{Deserialize, Serialize};

/// A computer's id: `computer:` and 24 hex.
pub fn valid_computer_id(id: &str) -> bool {
    id.strip_prefix("computer:").is_some_and(|h| h.len() == 24 && h.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
}

/// A computer's host label under the fragments' suffix: its own origin,
/// cross-site from the platform, for its ports (`<24 hex>--computer`;
/// `computer` is a reserved username, so no fragment's host is one).
pub fn computer_label(id: &str) -> Option<String> {
    valid_computer_id(id).then(|| format!("{}--computer", &id["computer:".len()..]))
}

/// The computer a host label names (`computer_label`'s inverse).
pub fn computer_of_label(label: &str) -> Option<String> {
    let id = format!("computer:{}", label.strip_suffix("--computer")?);
    valid_computer_id(&id).then_some(id)
}

/// Where a computer is in its life (`fragment_core::computer::Phase`, for
/// people).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComputerPhase {
    Asleep,
    Starting,
    Awake,
    Sleeping,
    /// Its starts kept failing: its owner can wake it to try again.
    WontWake,
}

/// An agent fragment a computer runs, as the guest and the owner see it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComputerAgent {
    /// The agent fragment's name (`<label>.<username>`).
    pub fragment: String,
    /// The agent's identity (`id:…`): it acts as this.
    pub identity: String,
    /// What it is called: its fragment's label (`juniper` of
    /// `juniper.paul`), how people @mention it.
    pub name: String,
    /// Its owner, a person.
    pub owner: String,
    /// The connections (WorkOS Pipes providers) its owner lets it use
    /// through the computer's swap (decision 22); none by default.
    #[serde(default)]
    pub connections: Vec<String>,
}

/// `PUT /api/computers/{id}/agents/{fragment}/connections`: the
/// connections an agent may use, all of them named at once.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentConnections {
    pub connections: Vec<String>,
}

/// `GET /api/computers/{id}` (its owner), `POST /api/computers` (made, or
/// the one the person has), and the guest's `GET /api/computer`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComputerView {
    pub computer: String,
    pub owner: String,
    /// The pinned image's name (the deployment's `containers` config).
    pub image: String,
    pub phase: ComputerPhase,
    /// Why it won't wake, or why a wake was refused (the owner's credit).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub why: Option<String>,
    pub agents: Vec<ComputerAgent>,
    /// Its own origin, where its ports are served.
    pub origin: String,
}

/// `POST /api/computers/{id}/ports/{port}/ticket` → a one-time link that
/// signs a browser in to the computer's origin for that port.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PortTicket {
    pub url: String,
    pub expires_at: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn computer_ids_and_their_labels() {
        let id = "computer:0123456789abcdef01234567";
        assert!(valid_computer_id(id));
        assert_eq!(computer_label(id).as_deref(), Some("0123456789abcdef01234567--computer"));
        assert_eq!(computer_of_label("0123456789abcdef01234567--computer").as_deref(), Some(id));
        for bad in ["computer:", "computer:0123456789ABCDEF01234567", "computer:0123", "id:0123456789abcdef01234567"] {
            assert!(!valid_computer_id(bad), "{bad}");
        }
        assert_eq!(computer_of_label("todo--paul"), None);
        // a computer's label is never a fragment's host: `computer` is reserved
        assert_eq!(crate::from_flat_name("0123456789abcdef01234567--computer"), None);
    }
}
