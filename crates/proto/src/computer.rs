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
/// no fragment's name has `--`, so no fragment's host is one).
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
    /// The agent fragment's name (`<label>--<suffix>`).
    pub fragment: String,
    /// The agent's identity (its fragment's npub): it acts as this.
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
    /// An own key's provider that serves models (its catalog row's
    /// `models`): its chat-completions base URL, on one of `hosts`, where
    /// an agent whose `agent.json` names a model of the provider sends its
    /// calls with the placeholder as its key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_base: Option<String>,
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
    /// An own key the person connects through the provider's sign-in
    /// (`POST …/authorize`, then the provider's page), never pasted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sign_in: Option<ProviderSignIn>,
    /// The models an own key's provider offers for the person's agents.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub models: Option<Vec<ProviderModel>>,
}

/// An own key's sign-in, as its person sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderSignIn {
    /// The provider's page where they see and revoke their keys.
    pub manage: String,
}

/// A model a person may pick for an agent (`agent.json`'s `model:
/// {provider, id}`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderModel {
    pub id: String,
    pub name: String,
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
    /// Its newest start (the Computer DO's generation, 0 before its first):
    /// what a restart names, so the same restart asked twice is made once.
    #[serde(default)]
    pub generation: u64,
    /// Why it won't wake, or why a wake was refused (the owner's credit).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub why: Option<String>,
    /// What its owner is told of it now (docs/computers.md, "What its
    /// owner is told"), the most pressing first: it won't start, its saves
    /// are failing, or a start went back to an older save. Its owner's view
    /// only (the guest's is empty).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notices: Vec<ComputerNotice>,
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
    /// than the end of the life before it (a crash, or a sleep that slept
    /// unsaved).
    #[serde(default)]
    pub rollbacks: u64,
    /// The saves of its `/data` it keeps, newest first (docs/computers.md,
    /// "Saves and what a wake restores"): a wake restores the newest one
    /// that restores.
    #[serde(default)]
    pub saves: Vec<ComputerSave>,
    /// It stays awake whatever is open: its owner's `seat_always_on` seat,
    /// unless they let it sleep (docs/billing.md).
    #[serde(default)]
    pub always_on: bool,
}

/// One save of a computer's `/data`, as its view shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComputerSave {
    /// 1 for its first save, one more for each after it.
    pub number: u64,
    /// Its id (its first record's), as a wake's `restored.save` names it.
    pub id: String,
    /// When it was taken (ms).
    pub at: i64,
    /// The start it is of.
    pub generation: u64,
    /// Whether its guest answered the hold before it was taken.
    pub held: bool,
    /// Its restore failed as a whole save, or the image's check of it did:
    /// no wake restores it again.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub unusable: bool,
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

/// Why a start went back to an older save: how the life whose work is lost
/// ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LossCause {
    /// It stopped on its own (a crash, a host restart, the runtime's idle
    /// stop), or died while its sleep was saving it.
    Crash,
    /// Its saves kept failing for the deployment's bound
    /// (`computers.unsaved_max_ms`), and it was put to sleep unsaved.
    Unsaved,
    /// Its owner restarted it, and the restart's save failed.
    Restart,
    /// Its sleep saved it, but that save would not restore (its archive
    /// gone or altered, or the image's check of it failed): the start fell
    /// back to the save before it.
    Unusable,
}

/// One thing a computer's owner is told of it (`ComputerView::notices`;
/// docs/computers.md, "What its owner is told"). Times are ms; the page
/// says them in its person's own clock. Each is derived from the Computer
/// DO's own state, so it is the same whoever reads it, and goes when what
/// it says is no longer so (or, for `went_back`, once its owner says they
/// saw it).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ComputerNotice {
    /// Its starts kept failing: it starts again only when its owner asks
    /// (a restart). `why` is the last failure, as the platform said it.
    WontWake { why: String },
    /// Its saves are failing: what it did since `since` is in no save yet,
    /// and a stop now would go back to save `save` (none: an empty `/data`).
    /// `stops_at`, when a sleep's save has failed and nothing keeps it in
    /// use: when it will be put to sleep unsaved if no save works first
    /// (none: not while something uses it, or never, for an always-on
    /// computer). It keeps trying until then.
    #[serde(rename_all = "camelCase")]
    Unsaved {
        since: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        save: Option<u64>,
        /// The first failure of this run of them, how many, and the last's why.
        failing_since: i64,
        failures: u32,
        why: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stops_at: Option<i64>,
    },
    /// A start went back to an older save (a rollback), or will as it next
    /// starts (`pending`: it is asleep after the loss). `life` is the start
    /// whose work since `saved_at` is in no save: what its owner says they
    /// saw (`POST …/notices/seen {life}`), after which it is told no more.
    #[serde(rename_all = "camelCase")]
    WentBack {
        life: u64,
        cause: LossCause,
        /// When that life ended, when the platform knows.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ended_at: Option<i64>,
        pending: bool,
        /// The save it went back to (none: nothing, an empty `/data`), and
        /// when that save was taken.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        save: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        saved_at: Option<i64>,
        /// When the start that went back came up (none while `pending`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        at: Option<i64>,
    },
}

/// `POST /api/computers/{id}/restart`: the start its owner restarts, as
/// their view named it (`generation`); none, or 0, restarts whatever runs.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RestartAsk {
    #[serde(default)]
    pub generation: Option<u64>,
}

/// `POST /api/computers/{id}/notices/seen`: its owner saw the notice of
/// the lost life `life` (a `went_back` notice's).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NoticeSeen {
    pub life: u64,
}

/// `POST /api/computers/{id}/ports/{port}/ticket` → a one-time link that
/// signs a browser in to the computer's origin for that port.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PortTicket {
    pub url: String,
    pub expires_at: i64,
}

/// What a port ticket asks for: where on the port its browser lands, the
/// port's root by default. A path and a query an image reads, such as an
/// agent's screen's `/?agent=<agent fragment>` (docs/computers.md, Ports):
/// the platform carries it and reads nothing in it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PortTicketAsk {
    #[serde(default)]
    pub path: Option<String>,
}

/// A ticket's landing path is at most this many bytes.
pub const PORT_PATH_MAX_BYTES: usize = 512;

/// Whether `path` may be where a ticket lands on its port: `/`, then
/// visible ASCII (no fragment `#`, no `\`), no `//` (which a redirect
/// could read as another host), and no `.` or `..` segment in its path
/// (which would leave the port).
pub fn valid_port_path(path: &str) -> bool {
    let visible = path.bytes().all(|b| (0x21..=0x7e).contains(&b) && b != b'#' && b != b'\\');
    let segments = path.split('?').next().unwrap_or("").split('/').all(|s| s != "." && s != "..");
    path.starts_with('/') && path.len() <= PORT_PATH_MAX_BYTES && visible && !path.contains("//") && segments
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Valid: the port's root, or a path and query an image reads. Invalid:
    /// anything that is not a path on the port.
    #[test]
    fn a_tickets_landing_is_a_path_on_its_port() {
        for ok in ["/", "/?agent=juniper.paul", "/novnc/core/rfb.js", "/a/b?c=d&e=f%20g", "/?q=a/../b"] {
            assert!(valid_port_path(ok), "{ok}");
        }
        for bad in ["", "?agent=juniper.paul", "agent", "//evil.example/", "/a//b", "/../6081/", "/a/./b", "/a/..", "/a b", "/a#b", "/a\\b", "/\u{e9}", "/\n"] {
            assert!(!valid_port_path(bad), "{bad:?}");
        }
        assert!(valid_port_path(&format!("/{}", "a".repeat(PORT_PATH_MAX_BYTES - 1))));
        assert!(!valid_port_path(&format!("/{}", "a".repeat(PORT_PATH_MAX_BYTES))));
        let ask: PortTicketAsk = serde_json::from_str("{}").unwrap();
        assert_eq!(ask.path, None, "no path: the port's root");
        assert!(serde_json::from_str::<PortTicketAsk>(r#"{"next":"/"}"#).is_err(), "a field it does not know is refused");
    }

    #[test]
    fn computer_ids_and_their_labels() {
        let id = "computer:0123456789abcdef01234567";
        assert!(valid_computer_id(id));
        assert_eq!(computer_label(id).as_deref(), Some("0123456789abcdef01234567--computer"));
        assert_eq!(computer_of_label("0123456789abcdef01234567--computer").as_deref(), Some(id));
        for bad in ["computer:", "computer:0123456789ABCDEF01234567", "computer:0123", "id:0123456789abcdef01234567"] {
            assert!(!valid_computer_id(bad), "{bad}");
        }
        assert_eq!(computer_of_label("todo--k3x9"), None);
        // a computer's label is never a fragment's name: a name has no `--`
        assert!(!crate::valid_fragment_name("0123456789abcdef01234567--computer"));
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

    /// What its owner is told, on the wire as the shell reads it: a kind,
    /// camelCase fields, what is unknown left out.
    #[test]
    fn a_computers_notices_on_the_wire() {
        let unsaved = ComputerNotice::Unsaved { since: 10, save: Some(3), failing_since: 20, failures: 2, why: "R2".into(), stops_at: None };
        assert_eq!(serde_json::to_value(&unsaved).unwrap(), serde_json::json!({ "kind": "unsaved", "since": 10, "save": 3, "failingSince": 20, "failures": 2, "why": "R2" }));
        let back = ComputerNotice::WentBack { life: 4, cause: LossCause::Unsaved, ended_at: Some(30), pending: true, save: None, saved_at: None, at: None };
        assert_eq!(serde_json::to_value(&back).unwrap(), serde_json::json!({ "kind": "went_back", "life": 4, "cause": "unsaved", "endedAt": 30, "pending": true }));
        assert_eq!(serde_json::to_value(ComputerNotice::WontWake { why: "x".into() }).unwrap(), serde_json::json!({ "kind": "wont_wake", "why": "x" }));
        for n in [unsaved, back] {
            assert_eq!(serde_json::from_value::<ComputerNotice>(serde_json::to_value(&n).unwrap()).unwrap(), n);
        }
        // a view from before notices reads as none, at no start
        let v: ComputerView = serde_json::from_str(r#"{"computer":"computer:0123456789abcdef01234567","owner":"id:p","image":"stub","phase":"asleep","agents":[],"origin":"https://x"}"#).unwrap();
        assert!(v.notices.is_empty() && v.generation == 0);
        let ask: RestartAsk = serde_json::from_str("{}").unwrap();
        assert_eq!(ask.generation, None);
        assert!(serde_json::from_str::<RestartAsk>(r#"{"gen":1}"#).is_err() && serde_json::from_str::<NoticeSeen>(r#"{"life":1,"x":2}"#).is_err(), "a field it does not know is refused");
    }

    /// The guest's credentials on the wire, as an image reads them.
    #[test]
    fn a_guests_credential() {
        let c = AgentCredential { provider: "google".into(), kind: ProviderKind::Connection, env: vec!["GOOGLE_OAUTH_ACCESS_TOKEN".into()], placeholder: "fcx_google_00".into(), hosts: vec!["www.googleapis.com".into()], model_base: None };
        let v = serde_json::to_value(&c).unwrap();
        assert_eq!(v, serde_json::json!({ "provider": "google", "kind": "connection", "env": ["GOOGLE_OAUTH_ACCESS_TOKEN"], "placeholder": "fcx_google_00", "hosts": ["www.googleapis.com"] }));
        // an own key's provider that serves models names their base
        let own = AgentCredential { provider: "openrouter".into(), kind: ProviderKind::Own, env: vec!["OPENROUTER_API_KEY".into()], placeholder: "fck_openrouter_00".into(), hosts: vec!["openrouter.ai".into()], model_base: Some("https://openrouter.ai/api/v1".into()) };
        assert_eq!(serde_json::to_value(&own).unwrap()["modelBase"], "https://openrouter.ai/api/v1");
        assert_eq!(serde_json::to_value(ProviderState::NeedsReauthorization).unwrap(), "needs_reauthorization");
    }
}
