//! A fragment's own Hermes on the fleet's sandcastle node
//! (docs/hermes-chat.md): what its `Hermes` cell decides, apart from the
//! calls it makes. Its names on the node and in the registry, the spec of
//! its computer, the grant its key is given, how a step's save keeps what
//! a deploy asked meanwhile, the page origins it names, and the session
//! read from Hermes' login.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::Digest;

/// Hermes' login's user (the password is what is secret).
pub const USERNAME: &str = "owner";
/// A native session handed out lives at most this long (Hermes' own TTL,
/// which the spec sets); one within `SESSION_MARGIN_S` of it is renewed first.
pub const SESSION_TTL_S: i64 = 3600;
pub const SESSION_MARGIN_S: i64 = 60;
/// Page origins a Hermes names at once: the latest few a fragment was
/// served from (a host move leaves the old one a while). Within
/// sandcastle's own bound (`CORS_ORIGINS_MAX`, 8).
pub const ORIGINS_MAX: usize = 4;

/// Where a fragment's Hermes is.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// Its cell makes it, step by step, until it serves.
    Make,
    Ready,
    /// Its cell removes its computer.
    Remove,
    Gone,
}

/// A step's phase over the row as a deploy's ask left it while the step
/// ran (`declared`, `stored`): a removal asked for stands over a making,
/// and a declaration asked for stands over a removal that finished.
pub fn merged(declared: bool, stored: Phase, stepped: Phase) -> (bool, Phase) {
    let phase = match (stored, stepped) {
        (Phase::Remove | Phase::Gone, p) if p != Phase::Gone => Phase::Remove,
        (Phase::Make, Phase::Gone) => Phase::Make,
        (_, p) => p,
    };
    (declared, phase)
}

/// The phase a deploy's declaration asks for, from the one it finds.
pub fn declared(declared: bool, now: Phase) -> Phase {
    match (declared, now) {
        (true, Phase::Gone | Phase::Remove) => Phase::Make,
        (false, Phase::Make | Phase::Ready) => Phase::Remove,
        (_, p) => p,
    }
}

/// The name a fragment's Hermes has on the node: `h` and 24 hex of the
/// platform and the fragment (a node may serve several fleets).
pub fn computer_name(platform: &str, fragment: &str) -> String {
    let digest = sha2::Sha256::digest(format!("{platform}\n{fragment}").as_bytes());
    format!("h{}", &hex::encode(digest)[..24])
}

/// The computer identity's name in the registry: `<label>-hermes.<username>`,
/// or, past the label's limit, a digest's (a fragment's Sprite holds
/// `<label>.<username>`).
pub fn identity_name(fragment: &str) -> String {
    let (label, username) = fragment_proto::split_fragment_name(fragment).expect("a fragment's name");
    let named = format!("{label}-hermes.{username}");
    if fragment_proto::valid_fragment_name(&named) {
        return named;
    }
    let digest = hex::encode(sha2::Sha256::digest(label.as_bytes()));
    format!("hermes-{}.{username}", &digest[..16])
}

/// The fragment whose Hermes a computer identity named `name` is, when
/// `identity_name` gave it (`<label>-hermes.<username>`); a digest's name
/// (a label past the limit) names none, and its Hermes goes with its
/// fragment instead.
pub fn fragment_of_identity(name: &str) -> Option<String> {
    let (label, username) = fragment_proto::split_fragment_name(name)?;
    let fragment = fragment_proto::fragment_name(label.strip_suffix("-hermes")?, username);
    (fragment_proto::valid_fragment_name(&fragment) && identity_name(&fragment) == name).then_some(fragment)
}

/// What its computer is made of, apart from its secrets.
pub struct Spec<'a> {
    pub image: &'a str,
    pub model: &'a str,
    /// The platform's URL: its model route and credential source.
    pub platform: &'a str,
    pub origins: &'a [String],
}

impl Spec<'_> {
    /// The computer's spec: Hermes under its own init (s6: its dashboard
    /// and gateway), its login and model settings in `env`, its model's
    /// key from the platform's credential source, public, with the page's
    /// origins named, and awake while its gateway works.
    pub fn json(&self, password: &str, secret: &str) -> Value {
        let platform = self.platform;
        json!({
            "image": self.image,
            "vcpus": 2,
            "memory_mib": 4096,
            "storage": "data",
            "data_gib": 10,
            "data_path": "/opt/data",
            "service": {
                "argv": [],
                "init": { "argv": ["/init", "/opt/hermes/docker/main-wrapper.sh", "gateway", "run"], "stop": ["/run/s6/basedir/bin/halt"] },
                "port": 9119,
                "health_path": "/api/auth/providers",
                "env": {
                    "HERMES_DASHBOARD": "1",
                    "HERMES_DASHBOARD_PORT": "9119",
                    "HERMES_DASHBOARD_BASIC_AUTH_USERNAME": USERNAME,
                    "HERMES_DASHBOARD_BASIC_AUTH_PASSWORD": password,
                    "HERMES_DASHBOARD_BASIC_AUTH_SECRET": secret,
                    "HERMES_DASHBOARD_BASIC_AUTH_TTL_SECONDS": SESSION_TTL_S.to_string(),
                    "HERMES_INFERENCE_PROVIDER": "custom",
                    "HERMES_INFERENCE_MODEL": self.model,
                    "OPENAI_BASE_URL": format!("{platform}/api/model"),
                    "CUSTOM_BASE_URL": format!("{platform}/api/model"),
                },
                "busy": { "path": "/api/status", "field": "active_agents" },
            },
            "url_auth": "public",
            "cors_origins": self.origins,
            "credentials_url": format!("{platform}/api/sandcastle/credentials"),
        })
    }
}

/// The grant the platform gives each Hermes' key: one computer of the
/// spec's size.
pub fn grant() -> Value {
    json!({ "computers_max": 1, "vcpus_max": 2, "memory_mib_max": 4096, "data_gib_max": 10 })
}

/// `origins` with `origin` named, the oldest dropped past `ORIGINS_MAX`;
/// None when it is named already.
pub fn with_origin(origins: &[String], origin: &str) -> Option<Vec<String>> {
    if origins.iter().any(|o| o == origin) {
        return None;
    }
    let mut named = origins.to_vec();
    named.push(origin.to_string());
    let past = named.len().saturating_sub(ORIGINS_MAX);
    named.drain(..past);
    Some(named)
}

/// A session's end: what Hermes says (`api/auth/me`), never past the TTL
/// from `now_s`.
pub fn session_end(said: Option<i64>, now_s: i64) -> i64 {
    said.unwrap_or(now_s + SESSION_TTL_S).min(now_s + SESSION_TTL_S)
}

/// Whether a session ending at `expires_at` is handed out again at `now_s`.
pub fn session_fresh(expires_at: i64, now_s: i64) -> bool {
    expires_at > now_s + SESSION_MARGIN_S
}

/// The native session in Hermes' login answer: the value of its
/// `hermes_session_at` cookie, whatever prefix its scheme gave it.
pub fn session_cookie(set_cookie: &str) -> Option<String> {
    // Set-Cookie headers come joined (", "); the token has neither ';' nor ','.
    let at = set_cookie.find("hermes_session_at=")? + "hermes_session_at=".len();
    let token: String = set_cookie[at..].chars().take_while(|c| *c != ';' && *c != ',' && !c.is_whitespace()).collect();
    (!token.is_empty()).then_some(token)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_stable_valid_and_the_fleets_own() {
        let a = computer_name("https://fragment.club", "chat.alice");
        assert_eq!(a, computer_name("https://fragment.club", "chat.alice"));
        assert_ne!(a, computer_name("http://127.0.0.1:8790", "chat.alice"), "a node may serve several fleets");
        assert_eq!(a.len(), 25);
        assert!(a.starts_with('h') && a.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()));
        assert_eq!(identity_name("chat.alice"), "chat-hermes.alice");
        let long = format!("{}.alice", "a".repeat(fragment_proto::limits::NAME_MAX_BYTES));
        let named = identity_name(&long);
        assert!(fragment_proto::valid_fragment_name(&named) && named.starts_with("hermes-"), "{named}");
    }

    #[test]
    fn an_identity_names_its_hermes_fragment() {
        assert_eq!(fragment_of_identity("chat-hermes.alice").as_deref(), Some("chat.alice"));
        assert_eq!(fragment_of_identity("my-chat-hermes.alice").as_deref(), Some("my-chat.alice"));
        for name in ["builder.alice", "hermes.alice", "-hermes.alice", "chat-hermes", "chat-hermes.", ""] {
            assert_eq!(fragment_of_identity(name), None, "{name}");
        }
        let long = format!("{}.alice", "a".repeat(fragment_proto::limits::NAME_MAX_BYTES));
        assert_eq!(fragment_of_identity(&identity_name(&long)), None, "a digest's name names none");
    }

    /// Goal: a deploy's ask that lands while a step awaits is never lost
    /// to that step's save. Method: each (stored, stepped) pair a step can
    /// meet, with the phase the row must keep.
    #[test]
    fn asks_made_during_a_step_stand() {
        use Phase::*;
        // no ask meanwhile: the step's own progress
        assert_eq!(merged(true, Make, Make), (true, Make));
        assert_eq!(merged(true, Make, Ready), (true, Ready));
        assert_eq!(merged(false, Remove, Gone), (false, Gone));
        // the block dropped while it was being made: removed, not ready
        assert_eq!(merged(false, Remove, Make), (false, Remove));
        assert_eq!(merged(false, Remove, Ready), (false, Remove));
        // a grant's save of a ready row after a removal finished
        assert_eq!(merged(false, Gone, Ready), (false, Remove));
        // declared again while it was being removed: made again
        assert_eq!(merged(true, Make, Gone), (true, Make));
        // replayed: the same answer
        assert_eq!(merged(false, Remove, Remove), (false, Remove));
    }

    #[test]
    fn a_declaration_makes_or_removes() {
        use Phase::*;
        assert_eq!(declared(true, Gone), Make);
        assert_eq!(declared(true, Remove), Make, "declared again mid-removal");
        assert_eq!(declared(true, Ready), Ready, "a redeploy keeps it");
        assert_eq!(declared(false, Make), Remove);
        assert_eq!(declared(false, Ready), Remove);
        assert_eq!(declared(false, Gone), Gone, "a deploy without it, again, changes nothing");
    }

    #[test]
    fn origins_are_the_latest_few() {
        let o = |n: usize| (0..n).map(|i| format!("https://f{i}.example")).collect::<Vec<_>>();
        assert_eq!(with_origin(&o(2), "https://f1.example"), None, "named already");
        assert_eq!(with_origin(&[], "https://a.example"), Some(vec!["https://a.example".to_string()]));
        let full = with_origin(&o(ORIGINS_MAX), "https://new.example").unwrap();
        assert_eq!(full.len(), ORIGINS_MAX);
        assert_eq!(full.first().map(String::as_str), Some("https://f1.example"), "the oldest goes");
        assert_eq!(full.last().map(String::as_str), Some("https://new.example"));
    }

    #[test]
    fn a_session_lasts_what_hermes_says_within_the_ttl() {
        assert_eq!(session_end(Some(1_000 + 600), 1_000), 1_600);
        assert_eq!(session_end(Some(1_000 + 99_999), 1_000), 1_000 + SESSION_TTL_S, "never past the TTL");
        assert_eq!(session_end(None, 1_000), 1_000 + SESSION_TTL_S);
        assert!(session_fresh(1_000 + SESSION_MARGIN_S + 1, 1_000));
        assert!(!session_fresh(1_000 + SESSION_MARGIN_S, 1_000), "renewed within the margin");
    }

    #[test]
    fn the_spec_names_its_secrets_and_the_platform() {
        let origins = vec!["https://chat--alice.fragment.boats".to_string()];
        let spec = Spec { image: "img", model: "m", platform: "https://fragment.club", origins: &origins }.json("pw", "sec");
        assert_eq!(spec["service"]["env"]["HERMES_DASHBOARD_BASIC_AUTH_PASSWORD"], "pw");
        assert_eq!(spec["service"]["env"]["HERMES_DASHBOARD_BASIC_AUTH_SECRET"], "sec");
        assert_eq!(spec["service"]["env"]["OPENAI_BASE_URL"], "https://fragment.club/api/model");
        assert_eq!(spec["credentials_url"], "https://fragment.club/api/sandcastle/credentials");
        assert_eq!(spec["url_auth"], "public");
        assert_eq!(spec["cors_origins"], json!(origins));
        assert_eq!(grant()["computers_max"], 1);
    }

    #[test]
    fn the_session_is_read_from_the_login_cookie() {
        assert_eq!(session_cookie("hermes_session_at=abc_DEF-1; Path=/; HttpOnly; SameSite=Lax").as_deref(), Some("abc_DEF-1"));
        assert_eq!(session_cookie("__Host-hermes_session_rt=r; Path=/, __Host-hermes_session_at=tok; Path=/; Secure").as_deref(), Some("tok"));
        assert_eq!(session_cookie("other=1"), None);
        assert_eq!(session_cookie("hermes_session_at=; Path=/"), None);
    }
}
