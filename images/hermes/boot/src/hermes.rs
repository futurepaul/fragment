//! What the Hermes image writes for Hermes v0.21.5 at each boot, as pure
//! functions of the computer's agents: the managed overlay
//! (`/etc/hermes/config.yaml`, merged over every profile's config), each
//! agent's profile config, the gateway's Relay environment, and Litestream's
//! configuration. YAML is written by hand: every string is a JSON string,
//! which is a YAML double-quoted scalar.

use std::path::{Path, PathBuf};

use fragment_bridge::runtime::relay::wire;
use fragment_bridge::runtime::Agent;

/// A model tier (docs/computers.md, Models): the model intercept maps it to
/// the tier's model; Hermes sends it as the model's name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    Cheap,
    Medium,
    High,
}

impl Tier {
    /// An agent fragment's `agent.json` names its tier (`{"tier": …}`);
    /// anything else is the medium tier. The high tier is off until
    /// Cloudflare raises its limit (decision 23), so it falls back to medium
    /// unless `high_on`.
    pub fn of(agent_json: Option<&[u8]>, high_on: bool) -> Tier {
        let tier = agent_json.and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok()).and_then(|v| v["tier"].as_str().map(str::to_string));
        match tier.as_deref() {
            Some("cheap") => Tier::Cheap,
            Some("high") if high_on => Tier::High,
            _ => Tier::Medium,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Tier::Cheap => "cheap",
            Tier::Medium => "medium",
            Tier::High => "high",
        }
    }
}

fn q(s: &str) -> String {
    serde_json::to_string(s).expect("a string serializes")
}

/// How long Hermes waits on an approval: the bridge's prompt lifetime, so a
/// card and its command expire together.
pub const APPROVAL_TIMEOUT_S: u64 = fragment_bridge::limits::PROMPT_TTL_MS_DEFAULT / 1000;

/// The managed overlay: how every profile streams, shows progress, asks for
/// approvals, and what it never runs.
pub fn managed_config(disabled_plugins: &[String]) -> String {
    let mut y = String::new();
    y.push_str("# Written by hermes-boot at every boot (images/hermes): Hermes' managed overlay,\n");
    y.push_str("# merged over each profile's own config. Hand edits are lost.\n");
    // One session per chat, shared by everyone in it; each message names its writer.
    y.push_str("group_sessions_per_user: false\n");
    // One gateway for every profile (one profile per agent), said outright.
    y.push_str("gateway:\n  multiplex_profiles: true\n");
    // Hermes' own first-contact profile-building flow is off: an agent's first
    // run is the shell's (decision 10).
    y.push_str("onboarding:\n  profile_build: \"off\"\n");
    // A reply streams as Relay `draft` frames (the chat's drafts); its final is one send.
    y.push_str("streaming:\n  enabled: true\n  transport: \"draft\"\n");
    // The bridge hands one message of a chat at a time; tool progress is one growing
    // message, each line a step.
    y.push_str("display:\n  busy_input_mode: \"queue\"\n  tool_progress: \"all\"\n  tool_progress_grouping: \"accumulate\"\n  long_running_notifications: false\n");
    y.push_str("platforms:\n  relay:\n    gateway_restart_notification: false\n");
    // Approvals default to Hermes' `smart` mode (decision 16); a card waits as long as
    // the bridge's prompt does. Slash confirmations stay off: a person's leading `/`
    // never reaches Hermes as a command.
    y.push_str(&format!("approvals:\n  mode: \"smart\"\n  timeout: {APPROVAL_TIMEOUT_S}\n  destructive_slash_confirm: false\n"));
    // Hermes' own cron is off: an agent's routines are its fragment's cron (decision 38).
    y.push_str("agent:\n  disabled_toolsets: [\"cronjob\"]\n");
    // The desktop starts at a computer-use tool's first call, never at boot.
    y.push_str("bot_desktop:\n  auto_start: true\nbrowser:\n  headed: true\n");
    if !disabled_plugins.is_empty() {
        // Messaging platforms this computer never serves (it is reached through its
        // bridge) and the dashboard's auth providers: the gateway imports every one
        // it is not told to skip (spike S3b: 1.4 s).
        y.push_str("plugins:\n  disabled:\n");
        for p in disabled_plugins {
            y.push_str(&format!("    - {}\n", q(p)));
        }
    }
    y
}

/// An agent's profile config: its model, through the model intercept, as
/// that agent (`x-fragment-agent` on every call, the main model's and the
/// auxiliary ones').
pub fn profile_config(agent: &Agent, tier: Tier, model_base: &str) -> String {
    let base = model_base.trim_end_matches('/');
    let (provider, url) = match tier {
        // The high tier is Anthropic's Messages shape, passed through.
        Tier::High => ("anthropic", format!("{base}/anthropic")),
        Tier::Cheap | Tier::Medium => ("custom", format!("{base}/v1")),
    };
    let mut y = String::new();
    y.push_str(&format!("# Written by hermes-boot for {} (tier {}) at every boot.\n", agent.fragment, tier.name()));
    y.push_str(&format!("model:\n  provider: {}\n  base_url: {}\n  default: {}\n", q(provider), q(&url), q(tier.name())));
    // The guest holds no credential: the intercept strips auth and adds its own.
    y.push_str("  api_key: \"fragment-model\"\n  context_length: 262144\n");
    y.push_str(&format!("  default_headers:\n    x-fragment-agent: {}\n", q(&agent.fragment)));
    y
}

/// The default profile (the gateway's own, no agent's): it runs no turns.
pub fn default_config(model_base: &str) -> String {
    let base = model_base.trim_end_matches('/');
    format!("# Written by hermes-boot: the gateway's own profile runs no agent's turns.\nmodel:\n  provider: \"custom\"\n  base_url: {}\n  default: \"cheap\"\n  api_key: \"fragment-model\"\n  context_length: 262144\n", q(&format!("{base}/v1")))
}

/// The gateway's Relay settings (Hermes' `gateway/relay/__init__.py`): with
/// the id and secret pinned it provisions nothing; the URL turns the relay
/// platform on.
pub fn gateway_env(listen: &str, gateway_id: &str, secret: &str) -> String {
    assert!(secret.len() >= 32, "a Relay secret of 32 characters or more");
    // RELAY_HOME_CHANNEL: Hermes asks every new chat to become its "home"
    // (where its own cron would deliver) until one is set. Its cron is off,
    // so the home is a chat id the bridge refuses.
    // GATEWAY_MULTIPLEX_PROFILES: one gateway serves every profile. Said
    // outright, because left to its default Hermes stays standalone in an s6
    // container with no per-profile gateway slots, which this image has none
    // of by design.
    // HERMES_AUTO_CONTINUE_FRESHNESS: one second, so no message after a
    // restart is wrapped in Hermes' recovery notes (the boot's clean-exit
    // receipt already discards the turns a restart cut short, which the
    // bridge ends: docs/chat-records.md).
    format!("GATEWAY_RELAY_URL=http://{listen}\nGATEWAY_RELAY_ID={gateway_id}\nGATEWAY_RELAY_SECRET={secret}\nHERMES_GATEWAY_BUSY_INPUT_MODE=queue\nHERMES_GATEWAY_NO_SUPERVISE=1\nGATEWAY_MULTIPLEX_PROFILES=true\nRELAY_HOME_CHANNEL=none\nHERMES_AUTO_CONTINUE_FRESHNESS=1\n")
}

/// Litestream for each profile's `state.db`, to the computer's storage
/// endpoint (disaster recovery only: off the wake path, decision 18). The
/// keys are placeholders; the intercept scopes the bucket.
pub fn litestream_config(dbs: &[(String, PathBuf)], storage: &str) -> String {
    let mut y = String::from("# Written by hermes-boot: each profile's state.db, streamed for disaster recovery.\ndbs:\n");
    for (name, path) in dbs {
        y.push_str(&format!("  - path: {}\n    replica:\n      type: s3\n      bucket: computer\n      path: {}\n      endpoint: {}\n      region: auto\n      force-path-style: true\n      access-key-id: fragment\n      secret-access-key: fragment\n      sync-interval: 10s\n", q(&path.display().to_string()), q(&format!("litestream/{name}")), q(storage)));
    }
    y
}

/// A profile's directory, under the Hermes home.
pub fn profile_dir(home: &Path, agent_fragment: &str) -> PathBuf {
    home.join("profiles").join(wire::profile(agent_fragment))
}

/// The directories a profile holds from its start (Hermes' `_PROFILE_DIRS`).
pub const PROFILE_DIRS: [&str; 9] = ["memories", "sessions", "skills", "skins", "logs", "plans", "workspace", "cron", "home"];

/// The plugins to disable: every bundled messaging platform and dashboard
/// auth provider (`<group>/<name>` under Hermes' `plugins/`).
pub fn lean_plugins(plugins_root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    for group in ["platforms", "dashboard_auth"] {
        let Ok(entries) = std::fs::read_dir(plugins_root.join(group)) else { continue };
        let mut names: Vec<String> = entries.filter_map(Result::ok).filter(|e| e.path().is_dir()).filter_map(|e| e.file_name().into_string().ok()).filter(|n| n != "__pycache__" && !n.starts_with('.')).collect();
        names.sort();
        out.extend(names.into_iter().map(|n| format!("{group}/{n}")));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent() -> Agent {
        Agent { fragment: "juniper.paul".into(), identity: "id:j".into(), name: "Juniper".into(), owner: "id:paul".into() }
    }

    #[test]
    fn tiers_read_and_fall_back() {
        assert_eq!(Tier::of(Some(br#"{"tier":"cheap"}"#), false), Tier::Cheap);
        assert_eq!(Tier::of(Some(br#"{"tier":"high"}"#), false), Tier::Medium, "the high tier is off until its limit is raised");
        assert_eq!(Tier::of(Some(br#"{"tier":"high"}"#), true), Tier::High);
        assert_eq!(Tier::of(Some(b"not json"), true), Tier::Medium);
        assert_eq!(Tier::of(None, true), Tier::Medium);
    }

    #[test]
    fn configs_say_what_hermes_needs() {
        let m = managed_config(&["platforms/discord".into(), "dashboard_auth/basic".into()]);
        for want in ["transport: \"draft\"", "busy_input_mode: \"queue\"", "group_sessions_per_user: false", "disabled_toolsets: [\"cronjob\"]", "mode: \"smart\"", "    - \"platforms/discord\""] {
            assert!(m.contains(want), "managed config has {want}:\n{m}");
        }
        assert!(m.contains(&format!("timeout: {APPROVAL_TIMEOUT_S}")));
        let p = profile_config(&agent(), Tier::Medium, "http://model.fragment.internal/");
        assert!(p.contains("base_url: \"http://model.fragment.internal/v1\""), "{p}");
        assert!(p.contains("default: \"medium\""));
        assert!(p.contains("x-fragment-agent: \"juniper.paul\""), "every model call names its agent");
        let h = profile_config(&agent(), Tier::High, "http://model.fragment.internal");
        assert!(h.contains("provider: \"anthropic\"") && h.contains("/anthropic\""), "{h}");
        let env = gateway_env("127.0.0.1:8650", "computer", &"s".repeat(32));
        assert!(env.contains("GATEWAY_RELAY_URL=http://127.0.0.1:8650\n"));
        assert!(env.contains("HERMES_GATEWAY_BUSY_INPUT_MODE=queue"));
        assert!(env.contains("HERMES_AUTO_CONTINUE_FRESHNESS=1\n"), "a turn a restart cut short is never auto-continued");
        let l = litestream_config(&[("juniper-paul".into(), PathBuf::from("/data/hermes/profiles/juniper-paul/state.db"))], "http://storage.fragment.internal");
        assert!(l.contains("path: \"litestream/juniper-paul\"") && l.contains("endpoint: \"http://storage.fragment.internal\""), "{l}");
        assert_eq!(profile_dir(Path::new("/data/hermes"), "juniper.paul"), PathBuf::from("/data/hermes/profiles/juniper-paul"));
    }

    #[test]
    #[should_panic(expected = "a Relay secret of 32 characters or more")]
    fn a_short_secret_is_refused() {
        gateway_env("127.0.0.1:1", "c", "short");
    }
}
