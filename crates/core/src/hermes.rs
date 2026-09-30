//! A fragment's own Hermes on the fleet's sandcastle node
//! (docs/hermes-chat.md, docs/runtime-seam.md): what its `Hermes` cell
//! decides, apart from the calls it makes. Its names on the node and in the
//! registry, the spec of its computer, the grant its key is given, how a
//! step's save keeps what a deploy asked meanwhile, and the admissions it
//! hands a page.
//!
//! Hermes runs in its loopback mode: its dashboard on the guest's
//! loopback, where its own login is off, behind a bridge from the port the
//! node publishes. A page reaches it by its computer's key, admitted by the
//! platform, which signs as the computer's owner; Hermes' session token is
//! its second check. No password exists.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::Digest;

/// How long an admission the platform signs lasts: a page asks for another
/// before it ends (a node takes at most ten minutes).
pub const ADMISSION_S: i64 = 300;
/// The Host a page's requests carry: Hermes in loopback mode takes only a
/// loopback name (its DNS-rebinding guard).
pub const HOST: &str = "127.0.0.1";
/// Where Hermes' dashboard listens in the guest, and the port the node
/// publishes (the bridge's).
const DASHBOARD_PORT: u16 = 9120;
const PUBLISHED_PORT: u16 = 9119;

/// In the guest, a bridge from the published port to Hermes' dashboard on
/// the guest's loopback: msb reaches only the guest's external interface,
/// from its gateway's address, and Hermes in loopback mode takes only
/// loopback peers. Started before the image's own command, as root; a
/// derived image with an s6 service for it replaces this
/// (docs/runtime-seam.md).
const BRIDGE: &str = r#"/opt/hermes/.venv/bin/python3 -c '
import asyncio
async def pipe(r, w):
    try:
        while True:
            d = await r.read(65536)
            if not d:
                break
            w.write(d)
            await w.drain()
    finally:
        w.close()
async def conn(cr, cw):
    try:
        ur, uw = await asyncio.open_connection("127.0.0.1", 9120)
    except OSError:
        cw.close()
        return
    await asyncio.gather(pipe(cr, uw), pipe(ur, cw), return_exceptions=True)
async def main():
    s = await asyncio.start_server(conn, "0.0.0.0", 9119)
    async with s:
        await s.serve_forever()
asyncio.run(main())
'"#;

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

/// What its computer is made of, apart from its session token.
pub struct Spec<'a> {
    pub image: &'a str,
    pub model: &'a str,
    /// The platform's URL: its model route and credential source.
    pub platform: &'a str,
}

impl Spec<'_> {
    /// The computer's spec: Hermes under its own init (s6: its dashboard
    /// and gateway), its dashboard in loopback mode behind the bridge, its
    /// session token pinned, its model's key from the platform's credential
    /// source, its URL its owner's alone, and awake while its gateway
    /// works.
    pub fn json(&self, token: &str) -> Value {
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
                "init": {
                    "argv": ["/init", "/bin/sh", "-c", format!("{BRIDGE} & exec /opt/hermes/docker/main-wrapper.sh gateway run")],
                    "stop": ["/run/s6/basedir/bin/halt"],
                },
                "port": PUBLISHED_PORT,
                "health_path": "/api/status",
                "env": {
                    "HERMES_DASHBOARD": "1",
                    "HERMES_DASHBOARD_HOST": "127.0.0.1",
                    "HERMES_DASHBOARD_PORT": DASHBOARD_PORT.to_string(),
                    "HERMES_DASHBOARD_SESSION_TOKEN": token,
                    "HERMES_INFERENCE_PROVIDER": "custom",
                    "HERMES_INFERENCE_MODEL": self.model,
                    "OPENAI_BASE_URL": format!("{platform}/api/model"),
                    "CUSTOM_BASE_URL": format!("{platform}/api/model"),
                },
                "busy": { "path": "/api/status", "field": "active_agents" },
            },
            "url_auth": "owner",
            "credentials_url": format!("{platform}/api/sandcastle/credentials"),
        })
    }
}

/// The grant the platform gives each Hermes' key: one computer of the
/// spec's size.
pub fn grant() -> Value {
    json!({ "computers_max": 1, "vcpus_max": 2, "memory_mib_max": 4096, "data_gib_max": 10 })
}

/// Whether `peer` is an iroh key a page may be admitted as: 64 lowercase hex.
pub fn valid_peer(peer: &str) -> bool {
    peer.len() == 64 && peer.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
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

    /// Goal: the spec puts Hermes in loopback mode behind the bridge, its
    /// token its only secret, its model on the platform's route, its URL
    /// its owner's. Method: one spec, each part checked.
    #[test]
    fn the_spec_is_loopback_hermes_behind_its_bridge() {
        let spec = Spec { image: "img", model: "m", platform: "https://fragment.club" }.json("tok");
        let env = &spec["service"]["env"];
        assert_eq!(env["HERMES_DASHBOARD_HOST"], "127.0.0.1", "loopback mode: Hermes' login is off");
        assert_eq!(env["HERMES_DASHBOARD_PORT"], "9120");
        assert_eq!(env["HERMES_DASHBOARD_SESSION_TOKEN"], "tok");
        assert!(env.as_object().unwrap().keys().all(|k| !k.contains("BASIC_AUTH")), "no password anywhere");
        assert_eq!(spec["service"]["port"], 9119, "the node publishes the bridge's port");
        let init = spec["service"]["init"]["argv"].as_array().unwrap();
        assert_eq!((init[0].as_str(), init[1].as_str(), init[2].as_str()), (Some("/init"), Some("/bin/sh"), Some("-c")));
        let script = init[3].as_str().unwrap();
        assert!(script.contains("start_server(conn, \"0.0.0.0\", 9119)") && script.contains("open_connection(\"127.0.0.1\", 9120)"));
        assert!(script.ends_with("& exec /opt/hermes/docker/main-wrapper.sh gateway run"));
        assert!(script.len() < 8 * 1024, "within a node's argv bound");
        assert_eq!(env["OPENAI_BASE_URL"], "https://fragment.club/api/model");
        assert_eq!(spec["credentials_url"], "https://fragment.club/api/sandcastle/credentials");
        assert_eq!(spec["url_auth"], "owner");
        assert!(spec.get("cors_origins").is_none());
        assert_eq!(grant()["computers_max"], 1);
    }

    #[test]
    fn a_peer_is_an_iroh_key() {
        assert!(valid_peer(&"a1".repeat(32)));
        for bad in ["", "abc", &"A1".repeat(32), &"g1".repeat(32), &"a".repeat(65)] {
            assert!(!valid_peer(bad), "{bad}");
        }
    }
}
