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
    /// The providers of the deployment's catalog (its connections, the
    /// operator's keys and its owner's own) it may have swapped in through
    /// the computer's swap (decisions 22 and 37): `None` (`null`), every
    /// one its owner has, by default, since a person's agents are not
    /// fenced from each other (decision 44); a list narrows it to those, a
    /// role's specialization rather than a wall.
    #[serde(default)]
    pub connections: Option<Vec<String>>,
    /// The guest's view only (`GET /api/computer`): each credential the
    /// agent may use now, its placeholder in its environment variables.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub credentials: Vec<AgentCredential>,
}

/// What a provider's credential is (docs/computers.md, "Connections and
/// operator keys"; `fragment_core::catalog`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    /// The person's account at the provider, through WorkOS Pipes.
    Connection,
    /// The operator's key, each call metered to the agent's owner.
    Operator,
    /// The person's own key, never metered.
    Own,
}

impl ProviderKind {
    pub fn name(self) -> &'static str {
        match self {
            ProviderKind::Connection => "connection",
            ProviderKind::Operator => "operator",
            ProviderKind::Own => "own",
        }
    }
}

/// One credential an agent may use, as its guest learns it: the
/// placeholder to send, the environment variables to put it in, and the
/// only hosts it is swapped for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentCredential {
    pub provider: String,
    pub kind: ProviderKind,
    pub env: Vec<String>,
    pub placeholder: String,
    pub hosts: Vec<String>,
}

/// A provider as its person sees it (`GET /api/connections`): the state
/// of their credential there.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderState {
    /// A connection whose account is connected.
    Connected,
    /// A connection whose account must be authorized again.
    NeedsReauthorization,
    /// A connection with no account connected.
    NotConnected,
    /// An operator key the deployment lends.
    Offered,
    /// An own key the person has given.
    Set,
    /// An own key the person has not given.
    NotSet,
}

/// An operator key's price at list (micro-dollars per `per` calls).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderPrice {
    pub micros: i64,
    pub per: u64,
}

/// `GET /api/connections`'s row: one provider the deployment offers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderView {
    pub provider: String,
    pub kind: ProviderKind,
    pub state: ProviderState,
    pub hosts: Vec<String>,
    pub env: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub price: Option<ProviderPrice>,
}

/// `GET /api/connections`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Providers {
    pub providers: Vec<ProviderView>,
}

/// One agent's use of one provider in a month: its calls the provider
/// answered, and what they were charged (an operator key's; zero for a
/// connection or an own key, which are counted, never charged).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderUse {
    pub provider: String,
    /// The agent fragment.
    pub agent: String,
    pub calls: u64,
    pub micros: i64,
}

/// `GET /api/computers/{id}/uses?month=YYYY-MM`: a month's uses of the
/// deployment's providers through the computer's swap.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComputerUses {
    pub computer: String,
    /// `YYYY-MM`, UTC.
    pub month: String,
    pub uses: Vec<ProviderUse>,
}

/// `PUT /api/computers/{id}/agents/{fragment}/connections`: the providers
/// an agent may use, all of them named at once, or `null` for every one
/// its owner has (the default again). The field is named even when it is
/// `null`: the route refuses a body without it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentConnections {
    pub connections: Option<Vec<String>>,
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
    /// The guest's view only: every environment variable a credential of
    /// the deployment's catalog may be in, whether its agents hold it now
    /// or not (so an image can pass them all through, once).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub credential_env: Vec<String>,
    /// What its last start that came up restored (docs/computers.md, "What
    /// a wake restored"); none before its first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restored: Option<ComputerRestore>,
    /// Its starts that went back in time: each woke with a `/data` older
    /// than the end of the life before it (a crash, or a sleep whose save
    /// failed).
    #[serde(default)]
    pub rollbacks: u64,
}

/// Where a start's `/data` came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RestoreSource {
    /// A container snapshot, a cache of the save it names, for one image.
    Snapshot,
    /// The save itself (`DirectoryBackup`), into the image.
    Backup,
    /// Nothing: a computer never saved (its first start, or one whose saves
    /// all failed).
    Nothing,
}

/// How one of a computer's lives (a start that came up) ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LifeEnd {
    /// Its sleep (the Computer DO's sequence, which saves first).
    Sleep,
    /// It stopped on its own: a crash, or the runtime's idle stop.
    Exit,
}

/// What one start restored, as the Computer DO recorded it as it came up.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComputerRestore {
    /// The start (the Computer DO's generation).
    pub generation: u64,
    pub from: RestoreSource,
    /// The save's id (its backup record's), when it restored one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub save: Option<String>,
    /// When that save was taken, and how old it was at this start (ms);
    /// none for nothing, or a save made before the platform kept its time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub saved_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub age_ms: Option<i64>,
    /// How the life before it ended; none for its first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<LifeEnd>,
    /// Whether it went back in time: the life before it ended other than
    /// by a sleep that saved, so what it did since its own start was lost.
    pub rollback: bool,
    /// When it came up (ms).
    pub at: i64,
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

    /// Goal: an agent's connections are every one its owner has unless a
    /// list narrows them (decision 44), and `null` says so on the wire.
    #[test]
    fn an_agents_connections_are_all_unless_narrowed() {
        let all: AgentConnections = serde_json::from_str(r#"{"connections":null}"#).unwrap();
        assert_eq!(all.connections, None);
        let narrowed: AgentConnections = serde_json::from_str(r#"{"connections":["github"]}"#).unwrap();
        assert_eq!(narrowed.connections, Some(vec!["github".to_string()]));
        let agent = ComputerAgent { fragment: "juniper.paul".into(), identity: "id:a".into(), name: "juniper".into(), owner: "id:p".into(), connections: None, credentials: vec![] };
        let v = serde_json::to_value(&agent).unwrap();
        assert_eq!(v["connections"], serde_json::Value::Null, "the default is named, as null");
        assert!(v.get("credentials").is_none(), "the owner's view names no placeholder");
        let older: ComputerAgent = serde_json::from_str(r#"{"fragment":"juniper.paul","identity":"id:a","name":"juniper","owner":"id:p"}"#).unwrap();
        assert_eq!(older.connections, None);
        assert!(older.credentials.is_empty());
    }

    /// The guest's credentials on the wire, as an image reads them.
    #[test]
    fn a_guests_credential() {
        let c = AgentCredential { provider: "google".into(), kind: ProviderKind::Connection, env: vec!["GOOGLE_OAUTH_ACCESS_TOKEN".into()], placeholder: "fcx_google_00".into(), hosts: vec!["www.googleapis.com".into()] };
        let v = serde_json::to_value(&c).unwrap();
        assert_eq!(v, serde_json::json!({ "provider": "google", "kind": "connection", "env": ["GOOGLE_OAUTH_ACCESS_TOKEN"], "placeholder": "fcx_google_00", "hosts": ["www.googleapis.com"] }));
        assert_eq!(serde_json::to_value(ProviderState::NeedsReauthorization).unwrap(), "needs_reauthorization");
    }
}
