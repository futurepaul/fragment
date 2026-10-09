//! The commands the relay runtime takes from an agent's owner (a chat's
//! `{kind: "command"}` record, docs/chat-records.md), and how the bridge
//! carries each (`How`). Read from Hermes v0.21.6's own command manifest
//! (`gateway/relay/command_manifest.py`, the set its connectors register:
//! 28 commands), its names and descriptions; a description says what this
//! platform does where that differs. The rest of the manifest is left out,
//! so the bridge never says them to Hermes as commands:
//!
//! | Left out | Why |
//! |---|---|
//! | `update`, `restart` | they change the install, or restart the gateway every agent of the computer runs on |
//! | `sethome` | a home channel is the gateway's configuration (the image sets none) |
//! | `reload-mcp`, `reload-skills` | the install's MCP servers and skills, which the image keeps (its boot syncs the skills) |
//! | `personality` | it writes the profile's `config.yaml`, which the image owns: an agent's character is its `SOUL.md` |
//! | `title` | a session title is no chat's name (the shell names chats), and in an agent's own chat it is Bot Mode's key (`Bot Chat`: docs/computers.md), which the image's hook takes back at the next turn |
//! | `resume` | it loads another session into this chat: one chat, one session |
//! | `reset` | the same as `new` |
//! | `help` | it lists every command of Hermes', most of them left out here: the page's picker is the help |
//! | `approve`, `deny` | a card answers an approval, and its record says how it closed |
//! | `thread` | threads are off (the descriptor's `supports_threads`) |
//! | `bg` | a background session the bridge cannot see, stop, or keep its computer awake for |
//!
//! `queue` is the bridge's own queue (its text a message, which waits its
//! turn), never Hermes': a turn Hermes queued itself would run where the
//! bridge cannot see it. `stop` is the bridge's Stop (`interrupt_inbound`),
//! never `/stop` said to Hermes. `model` and `reasoning` change this chat's
//! session alone: `--global` (and `reasoning`'s `show` and `hide`, which
//! write the gateway's display settings for every agent) are refused.

use crate::runtime::{How, MenuItem};

/// Why a `--global` is refused.
const GLOBAL: &str = "a command here changes this chat's session alone; an agent's settings are its own (settings, its agent.json)";
/// Why `reasoning`'s `show` and `hide` are refused.
const DISPLAY: &str = "showing reasoning is the gateway's setting for every agent on the computer; change this chat's effort alone";

pub const MENU: &[MenuItem] = &[
    MenuItem { name: "new", description: "Start a new conversation in this chat (stops what it is doing)", args: None, how: How::Restart, refuse: &[] },
    MenuItem { name: "stop", description: "Stop the running Hermes agent (and what waits for it here)", args: None, how: How::Stop, refuse: &[] },
    MenuItem { name: "steer", description: "Inject a message after the next tool call (no interrupt)", args: Some("What to tell the agent"), how: How::Steer, refuse: &[] },
    MenuItem { name: "queue", description: "Queue a prompt for the next turn (doesn't interrupt)", args: Some("The prompt to queue"), how: How::Message, refuse: &[] },
    MenuItem { name: "btw", description: "Ask a side question about the current conversation", args: Some("The question to answer"), how: How::Aside, refuse: &[] },
    MenuItem { name: "retry", description: "Retry your last message", args: None, how: How::Turn, refuse: &[] },
    MenuItem { name: "undo", description: "Remove the last exchange", args: None, how: How::Turn, refuse: &[] },
    MenuItem { name: "compress", description: "Compress conversation context", args: None, how: How::Turn, refuse: &[] },
    MenuItem { name: "model", description: "Show or change the model", args: Some("Model name. Leave empty to see current."), how: How::Turn, refuse: &[("--global", GLOBAL)] },
    MenuItem {
        name: "reasoning",
        description: "Show or change reasoning effort",
        args: Some("Level, or reset. Leave empty to see current."),
        how: How::Turn,
        refuse: &[("--global", GLOBAL), ("show", DISPLAY), ("hide", DISPLAY)],
    },
    MenuItem { name: "voice", description: "Toggle voice reply mode", args: Some("on, off, tts, or status"), how: How::Turn, refuse: &[] },
    MenuItem { name: "usage", description: "Show token usage for this session", args: None, how: How::Turn, refuse: &[] },
    MenuItem { name: "status", description: "Show Hermes session status", args: None, how: How::Turn, refuse: &[] },
    MenuItem { name: "insights", description: "Show usage insights and analytics", args: None, how: How::Turn, refuse: &[] },
];

#[cfg(test)]
mod tests {
    use super::*;

    /// The manifest of Hermes v0.21.6 (`build_relay_command_manifest`), by
    /// name: every command it offers is either on the menu or left out
    /// (the module's table), never both, and nothing else is on it.
    const MANIFEST: [&str; 28] = [
        "new", "reset", "model", "reasoning", "personality", "retry", "undo", "status", "sethome", "stop", "steer", "compress", "title", "resume", "usage", "help", "insights", "reload-mcp", "reload-skills", "voice", "update", "restart", "approve", "deny", "thread", "queue", "bg", "btw",
    ];
    const LEFT_OUT: [&str; 14] = ["update", "restart", "sethome", "reload-mcp", "reload-skills", "personality", "title", "resume", "reset", "help", "approve", "deny", "thread", "bg"];

    #[test]
    fn the_menu_is_the_manifest_less_what_is_left_out() {
        for m in MENU {
            assert!(MANIFEST.contains(&m.name), "{} is Hermes' own", m.name);
            assert!(!LEFT_OUT.contains(&m.name), "{} is left out", m.name);
            assert!(crate::records::valid_command(m.name) && !m.description.is_empty());
        }
        for name in MANIFEST {
            assert!(MENU.iter().any(|m| m.name == name) != LEFT_OUT.contains(&name), "{name} is on the menu or left out");
        }
        for never in ["update", "restart", "sethome"] {
            assert!(!MENU.iter().any(|m| m.name == never), "{never} never reaches Hermes");
        }
        let names: std::collections::BTreeSet<&str> = MENU.iter().map(|m| m.name).collect();
        assert_eq!(names.len(), MENU.len(), "each once");
    }
}
