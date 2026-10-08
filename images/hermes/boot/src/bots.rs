//! Hermes' Bot Mode on this image's agents (docs/computers.md, "Bot
//! Mode"): each agent is a bot, so each has Hermes' teammate roster and its
//! `message_agent` tool in its Bot Chat. Hermes turns both on for a session
//! titled exactly `Bot Chat`, on an install where a profile's `profile.yaml`
//! carries `ui_meta: {hermes-bots: …}` (v0.21.6: `tools/bot_mode_probe.py`,
//! `agent/system_prompt.py`'s `_bot_mode_parts`, `tools/bot_mode_dm.py`'s
//! gate). The boot writes each profile's `profile.yaml` (`profile_yaml`),
//! and links the image's gateway hook into its `hooks/` (`link_hook`): as
//! each of the agent's turns starts, before its prompt is built, the hook
//! (`images/hermes/hooks/fragment-bot-chat`) titles its own chat's session
//! `Bot Chat`, reading which chat that is from the bots file
//! (`bots_file`).

use std::path::Path;

use fragment_bridge::runtime::relay::wire;
use fragment_bridge::runtime::Agent;
use serde_json::json;

/// Under the boot's run directory: what the Bot Chat hook reads.
pub const BOTS_FILE: &str = "bots.json";

/// The image's gateway hook that titles an agent's own chat's session, under
/// its own files (`/opt/fragment`); a profile's `hooks/` holds a link to it
/// by its name.
pub const HOOK: &str = "hooks/fragment-bot-chat";

/// A bot's description, in characters at most: Hermes cuts each line of its
/// roster at 160 (`_profile_role`), the bot's title and `@handle` among them.
pub const DESCRIPTION_MAX_CHARS: usize = 120;

fn q(s: &str) -> String {
    serde_json::to_string(s).expect("a string serializes")
}

/// An agent's Bot Chat: its own chat with its owner, the chat fragment the
/// shell makes with it (cell/shell/shell.js, `makeAgent`: `<label>-chat`
/// beside the agent's `<label>`), which `fragment ask` finds as a person's
/// direct chat with it too (cli/src/ask.rs, `direct_label`). The image
/// names it here alone. An agent made some other way, with no such chat,
/// has no Bot Chat: Hermes then makes one of its own for a teammate's
/// message (Hermes' `chat -c "Bot Chat" --create-if-missing`).
pub fn bot_chat(agent_fragment: &str) -> String {
    match agent_fragment.split_once('.') {
        Some((label, owner)) => format!("{label}-chat.{owner}"),
        None => format!("{agent_fragment}-chat"),
    }
}

/// What a bot does, from its job (its `SOUL.md`): the first line that says
/// anything, a heading's or a list's marks taken off, its spaces collapsed,
/// at most `DESCRIPTION_MAX_CHARS`.
pub fn description(soul: &str) -> String {
    let line = soul.lines().map(|l| l.trim().trim_start_matches(['#', '>', '-', '*']).trim()).find(|l| !l.is_empty()).unwrap_or("");
    let words = line.split_whitespace().collect::<Vec<_>>().join(" ");
    if words.chars().count() <= DESCRIPTION_MAX_CHARS {
        return words;
    }
    let cut: String = words.chars().take(DESCRIPTION_MAX_CHARS - 1).collect();
    format!("{}…", cut.trim_end())
}

/// An agent's `profile.yaml`: Hermes' profile metadata, which its roster
/// reads (each teammate's name and role, `_profile_role`) and which marks
/// the install as Bot-Mode-managed (`ui_meta.hermes-bots`). Its name is its
/// display name and its bot's title; its description is its job's first
/// line (`description`). Written whole at every boot, and for an agent
/// assigned while the computer runs.
pub fn profile_yaml(agent: &Agent, soul: Option<&str>) -> String {
    let name = agent.name.trim();
    let name = if name.is_empty() { agent.fragment.split('.').next().unwrap_or(&agent.fragment) } else { name };
    let about = soul.map(description).unwrap_or_default();
    let mut y = format!("# Written by hermes-boot for {} at every boot: its Bot Mode identity (images/hermes/boot/src/bots.rs).\n", agent.fragment);
    y.push_str(&format!("display_name: {}\n", q(name)));
    if !about.is_empty() {
        y.push_str(&format!("description: {}\n", q(&about)));
    }
    y.push_str(&format!("ui_meta:\n  hermes-bots:\n    title: {}\n", q(name)));
    if !about.is_empty() {
        y.push_str(&format!("    description: {}\n", q(&about)));
    }
    y
}

/// The gateway's own profile's `profile.yaml` (Hermes' `default`, at its
/// home's root). Hermes lists it in every bot's roster as `@hermes`, always
/// (`_roster`); it runs no agent's turns (hermes.rs, `default_config`), so
/// its role says so.
pub const GATEWAY_PROFILE_YAML: &str = "# Written by hermes-boot: the gateway's own profile, which Hermes' Bot Mode roster lists as @hermes.\ndescription: \"this computer's gateway, not an agent: never message it\"\n";

/// Links the image's hook (`hook`, a directory) into `profile`'s `hooks/`
/// by its name, made if missing: Hermes' gateway loads each profile's hooks
/// in that profile's scope, as its first turn there starts. A link already
/// there is kept; anything else by that name is replaced.
pub fn link_hook(profile: &Path, hook: &Path) -> std::io::Result<()> {
    let name = hook.file_name().ok_or_else(|| std::io::Error::other(format!("a hook's directory has a name: {}", hook.display())))?;
    let dir = profile.join("hooks");
    std::fs::create_dir_all(&dir)?;
    let link = dir.join(name);
    match std::fs::symlink_metadata(&link) {
        Ok(m) if m.file_type().is_symlink() && std::fs::read_link(&link)? == hook => return Ok(()),
        Ok(m) if m.is_dir() => std::fs::remove_dir_all(&link)?,
        Ok(_) => std::fs::remove_file(&link)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    std::os::unix::fs::symlink(hook, &link)
}

/// The bots file: for each agent, its profile's home, its Bot Chat
/// (`bot_chat`), and the session key Hermes keeps that chat's session
/// under (`wire::session_key`), whose session the hook titles `Bot Chat`.
pub fn bots_file(agents: &[Agent], home: &Path) -> String {
    let bots: Vec<serde_json::Value> = agents
        .iter()
        .map(|a| {
            let profile = wire::profile(&a.fragment);
            let chat = bot_chat(&a.fragment);
            json!({
                "agent": a.fragment,
                "owner": a.owner,
                "profile": profile,
                "home": crate::hermes::profile_dir(home, &a.fragment).display().to_string(),
                "chat": chat,
                "sessionKey": wire::session_key(&profile, &wire::chat_id(&chat, &a.fragment)),
            })
        })
        .collect();
    json!({ "bots": bots }).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(label: &str, name: &str) -> Agent {
        Agent { fragment: format!("{label}.paul"), identity: format!("id:{label}"), name: name.into(), owner: "id:paul".into(), credentials: vec![] }
    }

    /// The shell's own chat with an agent, as makeAgent names it.
    #[test]
    fn a_bot_chat_is_the_agents_own_chat() {
        assert_eq!(bot_chat("juniper.paul"), "juniper-chat.paul");
        assert_eq!(bot_chat("maple-2.skyler"), "maple-2-chat.skyler");
        assert_eq!(bot_chat("loner"), "loner-chat");
    }

    /// Valid: the job's first line that says anything; invalid: none, or
    /// only marks; a long one cut, whitespace collapsed.
    #[test]
    fn a_description_is_the_jobs_first_line() {
        assert_eq!(description("You research plants.\nAnd more."), "You research plants.");
        assert_eq!(description("\n\n# Juniper\n\nGardener."), "Juniper");
        assert_eq!(description("  - keeps   the   books  "), "keeps the books");
        assert_eq!(description(""), "");
        assert_eq!(description("#\n> \n"), "");
        let long = "word ".repeat(100);
        let d = description(&long);
        assert_eq!(d.chars().count(), DESCRIPTION_MAX_CHARS);
        assert!(d.ends_with('…') && !d.contains("  "), "{d}");
        let wide = "é".repeat(500);
        assert_eq!(description(&wide).chars().count(), DESCRIPTION_MAX_CHARS, "cut on a character, never inside one");
    }

    /// Valid: name, title and description, each a YAML double-quoted
    /// scalar (a JSON string) whatever it holds; with no job, no
    /// description; with no name, the label. Replay: the same agent writes
    /// the same bytes.
    #[test]
    fn a_profile_says_who_the_bot_is() {
        let y = profile_yaml(&agent("juniper", "Juniper"), Some("You tend the garden: \"weeds\" first.\n"));
        assert_eq!(
            y,
            "# Written by hermes-boot for juniper.paul at every boot: its Bot Mode identity (images/hermes/boot/src/bots.rs).\n\
             display_name: \"Juniper\"\n\
             description: \"You tend the garden: \\\"weeds\\\" first.\"\n\
             ui_meta:\n  hermes-bots:\n    title: \"Juniper\"\n    description: \"You tend the garden: \\\"weeds\\\" first.\"\n"
        );
        assert_eq!(y, profile_yaml(&agent("juniper", "Juniper"), Some("You tend the garden: \"weeds\" first.\n")));
        let bare = profile_yaml(&agent("fred", " "), None);
        assert!(bare.contains("display_name: \"fred\"\n") && bare.contains("    title: \"fred\"\n") && !bare.contains("description"), "{bare}");
        let odd = profile_yaml(&agent("x", "a: b\n# c"), Some("line: one"));
        assert!(odd.contains("display_name: \"a: b\\n# c\"\n"), "a name is one scalar, never more YAML: {odd}");
        assert!(GATEWAY_PROFILE_YAML.contains("description: ") && !GATEWAY_PROFILE_YAML.contains("hermes-bots"), "the gateway is no bot");
    }

    /// Valid: a fresh profile gets `hooks/` and the link; replay: the same
    /// link is kept; a link elsewhere, a file or a directory by its name is
    /// replaced.
    #[test]
    fn a_profile_links_the_hook() {
        let root = std::env::temp_dir().join(format!("bots-hook-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (profile, hook) = (root.join("profiles/juniper-paul"), root.join("opt/hooks/fragment-bot-chat"));
        std::fs::create_dir_all(&profile).unwrap();
        std::fs::create_dir_all(&hook).unwrap();
        let link = profile.join("hooks/fragment-bot-chat");
        link_hook(&profile, &hook).unwrap();
        assert_eq!(std::fs::read_link(&link).unwrap(), hook);
        link_hook(&profile, &hook).unwrap();
        assert_eq!(std::fs::read_link(&link).unwrap(), hook, "kept");
        for stale in ["link", "file", "dir"] {
            std::fs::remove_file(&link).unwrap();
            match stale {
                "link" => std::os::unix::fs::symlink(root.join("elsewhere"), &link).unwrap(),
                "file" => std::fs::write(&link, "x").unwrap(),
                _ => std::fs::create_dir_all(link.join("inside")).unwrap(),
            }
            link_hook(&profile, &hook).unwrap();
            assert_eq!(std::fs::read_link(&link).unwrap(), hook, "a {stale} by its name replaced");
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The hook's file: each agent's home, Bot Chat, and the key Hermes'
    /// gateway keeps that chat's session under (the bridge's own).
    #[test]
    fn the_bots_file_names_each_bot_chats_session() {
        let f: serde_json::Value = serde_json::from_str(&bots_file(&[agent("juniper", "Juniper"), agent("fred", "Fred")], Path::new("/data/hermes"))).unwrap();
        assert_eq!(
            f["bots"][0],
            json!({
                "agent": "juniper.paul",
                "owner": "id:paul",
                "profile": "juniper-paul",
                "home": "/data/hermes/profiles/juniper-paul",
                "chat": "juniper-chat.paul",
                "sessionKey": "agent:juniper-paul:relay:group:juniper-chat.paul/juniper.paul",
            })
        );
        assert_eq!(f["bots"][1]["chat"], "fred-chat.paul");
        assert_eq!(bots_file(&[], Path::new("/data/hermes")), r#"{"bots":[]}"#);
    }
}
