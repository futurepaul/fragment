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
//! (`bots_file`). The image's Bot Mode keeper (`images/hermes/botmode.py`,
//! started by the boot) reads the same file: it holds each Bot Chat as
//! Hermes' live owner, and posts each teammate's message into the bot's own
//! chat as a hand-off, so its answer is a turn of the bridge's.

use std::collections::BTreeMap;
use std::path::Path;

use fragment_bridge::api::FragmentEntry;
use fragment_bridge::runtime::relay::wire;
use fragment_bridge::runtime::Agent;
use serde_json::json;

/// Under the boot's run directory: what the Bot Chat hook and the keeper
/// read.
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

/// A fragment name's label: `todo` of `todo--k3x9` (decision 47).
fn label(name: &str) -> &str {
    name.rsplit_once("--").map_or(name, |(label, _)| label)
}

/// An agent's Bot Chat: its own chat with its owner, the chat fragment the
/// shell makes with it (cell/shell/shell.js, `makeAgent`: labelled
/// `<label>-chat` beside the agent's `<label>`), which `fragment ask` finds
/// as a person's direct chat with it too (cli/src/ask.rs, `direct_label`).
/// A chat's name has a random suffix of its own, so it is found, never
/// derived: in the agent's list for its owner (`fragments`), the chat its
/// owner owns with that label. None until there is one: an agent made some
/// other way has no Bot Chat (Hermes then makes one of its own for a
/// teammate's message, `chat -c "Bot Chat" --create-if-missing`), and one
/// whose chat is made after it is assigned has it at a later look
/// (main.rs, `find_bot_chats`).
pub fn bot_chat(agent_fragment: &str, fragments: &[FragmentEntry]) -> Option<String> {
    let want = format!("{}-chat", label(agent_fragment));
    fragments.iter().find(|f| f.kind == "chat" && f.owned && label(&f.name) == want).map(|f| f.name.clone())
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
/// the install as Bot-Mode-managed (`ui_meta.hermes-bots`). Its name, its
/// display name and its bot's title, is its agent fragment's title (`title`:
/// "Juniper", as the shell titles it), else its name on the computer, else
/// its fragment's label: Hermes signs its messages to teammates with it
/// (`Message from 🤖 Juniper (@juniper-k3x9): …`). Its description is its
/// job's first line (`description`). Written whole at every boot, for an
/// agent assigned while the computer runs, and when a sync pulls its job.
pub fn profile_yaml(agent: &Agent, title: Option<&str>, soul: Option<&str>) -> String {
    let name = [title.unwrap_or(""), agent.name.as_str()].into_iter().map(str::trim).find(|n| !n.is_empty());
    let name = name.unwrap_or_else(|| label(&agent.fragment));
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

/// The bots file: for each agent whose Bot Chat is found (`chats`, by
/// `bot_chat`), its identity and owner, its profile's home, that chat, and
/// the session key Hermes keeps that chat's session under
/// (`wire::session_key`), whose session the hook titles `Bot Chat`. An
/// agent with none yet is left out: the hook and the keeper leave it alone.
pub fn bots_file(agents: &[Agent], home: &Path, chats: &BTreeMap<String, String>) -> String {
    let bots: Vec<serde_json::Value> = agents
        .iter()
        .filter_map(|a| {
            let profile = wire::profile(&a.fragment);
            let chat = chats.get(&a.fragment)?;
            Some(json!({
                "agent": a.fragment,
                "identity": a.identity,
                "owner": a.owner,
                "profile": profile,
                "home": crate::hermes::profile_dir(home, &a.fragment).display().to_string(),
                "chat": chat,
                "sessionKey": wire::session_key(&profile, &wire::chat_id(chat, &a.fragment)),
            }))
        })
        .collect();
    json!({ "bots": bots }).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(label: &str, name: &str) -> Agent {
        Agent { fragment: format!("{label}--k3x9"), identity: format!("npub1{label}"), name: name.into(), owner: "npub1paul".into(), credentials: vec![] }
    }

    fn listed(name: &str, kind: &str, owned: bool) -> FragmentEntry {
        FragmentEntry { name: name.into(), role: "editor".into(), kind: kind.into(), owned, title: String::new() }
    }

    /// Valid: the chat its owner owns labelled `<label>-chat`, whatever its
    /// suffix. Invalid: none, an app of that label, a chat of someone
    /// else's of it, another agent's chat, or a chat whose label only
    /// starts the same: no Bot Chat.
    #[test]
    fn a_bot_chat_is_the_agents_own_chat() {
        let list = [
            listed("juniper-chat--p2m4", "app", true),
            listed("juniper-chat--r7t5", "chat", false),
            listed("juniper-chats--b3c4", "chat", true),
            listed("maple-chat--z8w6", "chat", true),
            listed("juniper-chat--h6j7", "chat", true),
        ];
        assert_eq!(bot_chat("juniper--k3x9", &list).as_deref(), Some("juniper-chat--h6j7"));
        assert_eq!(bot_chat("maple--q4w5", &list).as_deref(), Some("maple-chat--z8w6"), "a chat's suffix is its own");
        assert_eq!(bot_chat("maple-2--q4w5", &list), None);
        assert_eq!(bot_chat("juniper--k3x9", &list[..4]), None, "only its owner's chat of that label");
        assert_eq!(bot_chat("loner--k3x9", &[]), None);
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

    /// Valid: name (its fragment's title), title and description, each a
    /// YAML double-quoted scalar (a JSON string) whatever it holds; with no
    /// job, no description; with no title, its name on the computer; with
    /// neither, the label. Replay: the same agent writes the same bytes.
    #[test]
    fn a_profile_says_who_the_bot_is() {
        let y = profile_yaml(&agent("juniper", "juniper"), Some("Juniper"), Some("You tend the garden: \"weeds\" first.\n"));
        assert_eq!(
            y,
            "# Written by hermes-boot for juniper--k3x9 at every boot: its Bot Mode identity (images/hermes/boot/src/bots.rs).\n\
             display_name: \"Juniper\"\n\
             description: \"You tend the garden: \\\"weeds\\\" first.\"\n\
             ui_meta:\n  hermes-bots:\n    title: \"Juniper\"\n    description: \"You tend the garden: \\\"weeds\\\" first.\"\n"
        );
        assert_eq!(y, profile_yaml(&agent("juniper", "juniper"), Some("Juniper"), Some("You tend the garden: \"weeds\" first.\n")));
        let untitled = profile_yaml(&agent("juniper", "juniper"), Some(" "), None);
        assert!(untitled.contains("display_name: \"juniper\"\n"), "no title: its name on the computer: {untitled}");
        let bare = profile_yaml(&agent("fred", " "), None, None);
        assert!(bare.contains("display_name: \"fred\"\n") && bare.contains("    title: \"fred\"\n") && !bare.contains("description"), "{bare}");
        let odd = profile_yaml(&agent("x", "x"), Some("a: b\n# c"), Some("line: one"));
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
        let (profile, hook) = (root.join("profiles/juniper--k3x9"), root.join("opt/hooks/fragment-bot-chat"));
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
    /// gateway keeps that chat's session under (the bridge's own); an agent
    /// whose Bot Chat is not found yet is left out.
    #[test]
    fn the_bots_file_names_each_bot_chats_session() {
        let chats: BTreeMap<String, String> = [("juniper--k3x9".to_string(), "juniper-chat--h6j7".to_string()), ("fred--k3x9".into(), "fred-chat--b3c4".into())].into();
        let agents = [agent("juniper", "Juniper"), agent("maple", "Maple"), agent("fred", "Fred")];
        let f: serde_json::Value = serde_json::from_str(&bots_file(&agents, Path::new("/data/hermes"), &chats)).unwrap();
        assert_eq!(
            f["bots"][0],
            json!({
                "agent": "juniper--k3x9",
                "identity": "npub1juniper",
                "owner": "npub1paul",
                "profile": "juniper--k3x9",
                "home": "/data/hermes/profiles/juniper--k3x9",
                "chat": "juniper-chat--h6j7",
                "sessionKey": "agent:juniper--k3x9:relay:group:juniper-chat--h6j7/juniper--k3x9",
            })
        );
        assert_eq!(f["bots"].as_array().map(Vec::len), Some(2), "maple, with no chat found, is left out");
        assert_eq!(f["bots"][1]["chat"], "fred-chat--b3c4");
        assert_eq!(bots_file(&[], Path::new("/data/hermes"), &chats), r#"{"bots":[]}"#);
    }
}
