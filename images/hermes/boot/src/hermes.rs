//! What the Hermes image writes for Hermes v0.21.5 at each boot (and for an
//! agent assigned while it runs), as pure functions of the computer's
//! agents: the managed overlay (`/etc/hermes/config.yaml`, merged over every
//! profile's config), each agent's profile config, the gateway's Relay
//! environment; and the wire of the
//! gateway's control socket. YAML is written by hand: every string is a
//! JSON string, which is a YAML double-quoted scalar.

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

/// The model route's name for the deployment's vision model
/// (`fragment_core::models::VISION`): Hermes' auxiliary vision names it.
pub const VISION_MODEL: &str = "vision";

fn q(s: &str) -> String {
    serde_json::to_string(s).expect("a string serializes")
}

/// How long Hermes waits on an approval: the bridge's prompt lifetime, so a
/// card and its command expire together.
pub const APPROVAL_TIMEOUT_S: u64 = fragment_bridge::limits::PROMPT_TTL_MS_DEFAULT / 1000;

/// The approval timeout a boot uses, in seconds: `setting`
/// (`HERMES_BOOT_APPROVAL_TIMEOUT_S`, a test's) within the bounds the
/// bridge holds a prompt's life to, else `APPROVAL_TIMEOUT_S`. The bridge
/// is given the same (`BRIDGE_PROMPT_TTL_MS`), so a card and its command
/// still expire together.
pub fn approval_timeout_s(setting: Option<&str>) -> u64 {
    use fragment_bridge::limits::{PROMPT_TTL_MS_MAX, PROMPT_TTL_MS_MIN};
    setting.and_then(|v| v.trim().parse::<u64>().ok()).map_or(APPROVAL_TIMEOUT_S, |s| s.clamp(PROMPT_TTL_MS_MIN / 1000, PROMPT_TTL_MS_MAX / 1000))
}

/// The managed overlay: how every profile streams, shows progress, asks for
/// approvals (waiting `approval_timeout_s` on each), and what it never runs.
pub fn managed_config(disabled_plugins: &[String], approval_timeout_s: u64) -> String {
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
    // message, each line a step. No interim messages: the model's text beside a tool
    // call is no message of its own (Hermes' default for a platform it has no tier
    // for, as `relay`, is to send each as one). Hermes reads `display` for a turn
    // from the profile's config with this overlay merged over it (its
    // `_load_gateway_config`, under the profile's scope).
    y.push_str("display:\n  busy_input_mode: \"queue\"\n  tool_progress: \"all\"\n  tool_progress_grouping: \"accumulate\"\n  long_running_notifications: false\n  interim_assistant_messages: false\n");
    y.push_str("platforms:\n  relay:\n    gateway_restart_notification: false\n");
    // Approvals default to Hermes' `smart` mode (decision 16); a card waits as long as
    // the bridge's prompt does. Slash confirmations stay off: a person's leading `/`
    // never reaches Hermes as a command.
    y.push_str(&format!("approvals:\n  mode: \"smart\"\n  timeout: {approval_timeout_s}\n  destructive_slash_confirm: false\n"));
    // Hermes' own cron is off: an agent's routines are its fragment's cron (decision 38).
    y.push_str("agent:\n  disabled_toolsets: [\"cronjob\"]\n");
    // The first agent's desktop starts for the screen's first viewer (the
    // bridge's `screen-start`), and any agent's at its first computer_use or
    // browser call, never at boot (measured: about 300 MiB more once it
    // runs). Its browser, on that desktop, is each profile's own config's
    // (`profile_config`).
    y.push_str("bot_desktop:\n  auto_start: true\n");
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
/// auxiliary ones'), and its vision model (the route's `vision`); and what
/// its terminal is given: who it is
/// (`PROFILE_ENV`) and every credential's environment variable the
/// deployment may give it (`credential_env`, the guest view's: Hermes reads
/// the list once per gateway, so it names them all, held now or not, and
/// each value is the profile's `.env`'s, read again at every turn). Hermes
/// never passes a name it keeps for its own providers' keys
/// (`PERPLEXITY_API_KEY`, `XAI_API_KEY`, `ELEVENLABS_API_KEY`, …: its
/// `_HERMES_PROVIDER_ENV_BLOCKLIST`), so the terminal also sources the
/// profile's credentials file (`credentials_sh`) as its shell starts.
pub fn profile_config(agent: &Agent, tier: Tier, model_base: &str, credential_env: &[String], credentials_file: &Path) -> String {
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
    // The managed skills, then the platform skill (skills.rs), after the
    // profile's own `skills/`: Hermes takes the first skill of a name, so an
    // agent's own wins, and a managed one over the platform's.
    let dirs: Vec<String> = crate::skills::EXTERNAL_DIRS.iter().map(|d| q(d)).collect();
    y.push_str(&format!("skills:\n  external_dirs: [{}]\n", dirs.join(", ")));
    // Its eyes: Hermes' auxiliary vision (each computer_use screenshot, and
    // an image a person attaches, described in words for the main model) on
    // the route's `vision`, the deployment's vision model, whatever the
    // agent's tier (the medium tier's GLM-5.3 reads no images). Named
    // outright, Hermes routes every capture through it (its
    // `tools/computer_use/vision_routing.py`, step 1) and sends it, as every
    // call to a custom endpoint, with `model.default_headers`: the agent's
    // `x-fragment-agent`, so the intercept meters it to the agent's owner.
    // Always OpenAI's shape, the high tier's agents' too.
    y.push_str(&format!(
        "auxiliary:\n  vision:\n    provider: \"custom\"\n    base_url: {}\n    model: {}\n    api_key: \"fragment-model\"\n",
        q(&format!("{base}/v1")),
        q(VISION_MODEL)
    ));
    // Its browser: Hermes' built-in browser tools (browser_navigate, …),
    // driving the image's own Chromium, headed, on the agent's desktop, so
    // the screen shows it. Left unset, Hermes picks Browser Use mode (one
    // browser_exec tool) whenever uvx is on PATH, which fetches its CLI,
    // unpinned, into /data (about 490 MB) at the first call. Hermes reads
    // `browser` from the profile's own config file alone, never from the
    // managed overlay.
    y.push_str("browser:\n  headed: true\n  backend: \"off\"\n");
    // Its terminal acts as the agent: the fragment CLI and the skills' helpers
    // read these from the profile's `.env` (`profile_env`), which Hermes passes
    // only to the commands of this profile's turns. Its shell's start files are
    // Hermes' own three, then the agent's credentials.
    let mut passed: Vec<String> = PROFILE_ENV.iter().map(|s| s.to_string()).collect();
    for e in credential_env.iter().filter(|e| env_name_ok(e)) {
        if !passed.contains(e) {
            passed.push(e.clone());
        }
    }
    y.push_str(&format!("terminal:\n  env_passthrough: [{}]\n", passed.iter().map(|k| q(k)).collect::<Vec<_>>().join(", ")));
    let init = ["~/.profile", "~/.bash_profile", "~/.bashrc"].iter().map(|f| q(f)).chain([q(&credentials_file.display().to_string())]);
    y.push_str(&format!("  shell_init_files: [{}]\n", init.collect::<Vec<_>>().join(", ")));
    // Its commands run in the agent's work directory, which the computer
    // saves on its own (the seam: step 2 of docs/durable-computers.md), never
    // in Hermes' home (left unset, the gateway's own home, /data/hermes)
    y.push_str(&format!("  cwd: {}\n", q(&work_dir(&agent.fragment).display().to_string())));
    y
}

/// The flags Chromium needs in a container: Hermes' own
/// (`CHROMIUM_SANDBOX_BYPASS_ARGS`, its `tools/browser_tool_session.py`),
/// which it gives its browser and its desktop's Browser icon only as root
/// or where it sees Docker's marker (`/.dockerenv`). Cloudflare Containers
/// has neither that marker nor a `/dev/shm`, and Chromium without these
/// dies as it starts: no usable sandbox (no user namespaces), or no shared
/// memory (p5, 2026-10-05).
pub const CHROMIUM_FLAGS: [&str; 2] = ["--no-sandbox", "--disable-dev-shm-usage"];

/// The image's Chromium: a script that starts Playwright's with
/// `CHROMIUM_FLAGS` whatever the runtime. Hermes is pointed at it
/// (`AGENT_BROWSER_EXECUTABLE_PATH`), so its browser and its desktop's
/// Browser icon (Hermes' `bot_desktop.browser.executable()`) both run it.
pub const CHROMIUM: &str = "/opt/fragment/bin/chromium";

/// The script at `CHROMIUM`, written at image build: on a desktop
/// (`DISPLAY` set) the full Chromium, `full`; with none the headless
/// shell, `shell`, as Hermes picks between them itself (its boot pins the
/// shell, and a running desktop swaps in the full one).
pub fn chromium_script(full: &Path, shell: &Path) -> String {
    let quoted = |p: &Path| {
        let s = p.display().to_string();
        assert!(p.is_absolute() && !s.contains('\''), "a browser's path, absolute, quotable: {s}");
        format!("'{s}'")
    };
    let flags = CHROMIUM_FLAGS.join(" ");
    format!(
        "#!/bin/sh\n# The image's Chromium (hermes-boot build-info: images/hermes/boot/src/hermes.rs):\n# Playwright's, always with the flags a container needs.\n[ -n \"$DISPLAY\" ] && exec {} {flags} \"$@\"\nexec {} {flags} \"$@\"\n",
        quoted(full),
        quoted(shell)
    )
}

/// What Hermes would otherwise decide by guessing whether it runs in a
/// container, pinned in the boot's environment (and so the gateway's, its
/// agents' terminals' and the desktop's) the same on every runtime. Its
/// guess (`hermes_platform/host/runtime.py: is_container()`) is Docker's
/// marker `/.dockerenv`, Podman's, Kubernetes', or a runtime's name in
/// `/proc/1/cgroup` or the root mount: Docker gives every container the
/// marker, Cloudflare Containers likely none of them, so there Hermes would
/// act as on a host. Each pin is what Hermes does in a container, as the
/// Docker rung has always run it (docs/computers.md, "Hermes' container
/// guesses", has every guess and what decides it):
///
/// - `TERMINAL_HOME_MODE=profile`: each agent's terminal, `execute_code`
///   and file tools' `~` have the agent's own `HOME`, its profile's `home`
///   (one of `PROFILE_DIRS`). Taken for a host, every agent's would be the
///   gateway's own, `/data/hermes`: one `~` for all the computer's agents,
///   among Hermes' own files (its `.env`, `config.yaml`).
/// - `HERMES_SKIP_CHMOD=1`: Hermes leaves the modes of its home's
///   directories and files as the image and the boot make them. Taken for
///   a host, it makes them owner-only (0700, 0600) at each start; the
///   computer has one user and root, and holds no secret, so that guards
///   nothing here.
///
/// The third guess the image's paths reach, Chromium's flags, is pinned by
/// the image's Chromium (`CHROMIUM`).
pub const RUNTIME_ENV: [(&str, &str); 2] = [("TERMINAL_HOME_MODE", "profile"), ("HERMES_SKIP_CHMOD", "1")];

/// What the computer keeps as its guest's tools' work, saved as a record
/// of its own (docs/computers.md, "Data and the restore gate").
pub const WORK: &str = "/data/work";

/// An agent's work directory: its terminal's cwd, and its browser's
/// profile (`BROWSER_PROFILE`).
pub fn work_dir(agent_fragment: &str) -> PathBuf {
    Path::new(WORK).join(wire::profile(agent_fragment))
}

/// Where Hermes keeps a profile's desktop browser's profile (its
/// `tools/bot_desktop/browser.py`: `<profile>/bot-desktop/browser-profile`),
/// relative to the profile: a link into the agent's work directory, so the
/// cookies and history its tools make are its work, saved with it.
pub const BROWSER_PROFILE: &str = "bot-desktop/browser-profile";

/// What a profile's terminal knows of the agent it runs (cli/GUIDE.md, "As
/// an agent"): the agent fragment its computer signs as, and the person it
/// acts for, its owner. Hermes passes these from the profile's `.env` to its
/// terminal's commands (`terminal.env_passthrough`, scoped per profile under
/// one gateway), so each agent's commands act as that agent alone.
pub const PROFILE_ENV: [&str; 2] = ["FRAGMENT_AS_AGENT", "FRAGMENT_FOR"];

/// The profile's credentials file, which its terminal's shell sources.
pub const CREDENTIALS_FILE: &str = "credentials.sh";

/// An environment variable's name a credential may be given in: upper
/// case, and not one of the profile's own.
fn env_name_ok(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && name.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
        && !name.as_bytes()[0].is_ascii_digit()
        && !name.starts_with("FRAGMENT_")
        && !name.starts_with("HERMES_")
}

/// A placeholder, as the platform makes one: `fcx_`/`fck_`, a provider and
/// a tag, nothing a line or a shell would read as more.
fn placeholder_ok(p: &str) -> bool {
    (p.starts_with("fcx_") || p.starts_with("fck_")) && p.len() <= 128 && p.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

/// The agent's credentials as `(name, placeholder)`, each in every
/// environment variable it names, in the platform's order; one the
/// platform sent out of shape is left out (and the name first given wins).
pub fn credential_vars(agent: &Agent) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = vec![];
    for c in &agent.credentials {
        if !placeholder_ok(&c.placeholder) {
            continue;
        }
        for e in c.env.iter().filter(|e| env_name_ok(e)) {
            if !out.iter().any(|(n, _)| n == e) {
                out.push((e.clone(), c.placeholder.clone()));
            }
        }
    }
    out
}

/// A profile's `.env`: who the agent is, and its credentials, each a
/// placeholder (the computer holds no credential: the swap fills them on
/// the way to their providers).
pub fn profile_env(agent: &Agent) -> String {
    let line_safe = |v: &str| !v.is_empty() && v.bytes().all(|b| b.is_ascii_graphic() && b != b'"' && b != b'\'' && b != b'\\' && b != b'#');
    assert!(line_safe(&agent.fragment) && line_safe(&agent.owner), "an agent's fragment and owner are names: {agent:?}");
    let mut e = format!(
        "# The profile of {}; the computer holds no credential, only placeholders.\n{}={}\n{}={}\n",
        agent.fragment, PROFILE_ENV[0], agent.fragment, PROFILE_ENV[1], agent.owner
    );
    for (name, placeholder) in credential_vars(agent) {
        e.push_str(&format!("{name}={placeholder}\n"));
    }
    e
}

/// The profile's credentials for its terminal's shell (`CREDENTIALS_FILE`,
/// sourced as a session's shell starts): each exported under its name.
pub fn credentials_sh(agent: &Agent) -> String {
    let mut s = format!("# Written by hermes-boot: {}'s credentials, placeholders the computer's swap fills.\n", agent.fragment);
    for (name, placeholder) in credential_vars(agent) {
        s.push_str(&format!("export {name}='{placeholder}'\n"));
    }
    s
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
    // bridge ends: docs/chat-records.md; the boot closes them in their
    // sessions, CLOSE_CUT_TURNS, and the bridge tells the next turn what was
    // cut instead, from the journal).
    format!("GATEWAY_RELAY_URL=http://{listen}\nGATEWAY_RELAY_ID={gateway_id}\nGATEWAY_RELAY_SECRET={secret}\nHERMES_GATEWAY_BUSY_INPUT_MODE=queue\nHERMES_GATEWAY_NO_SUPERVISE=1\nGATEWAY_MULTIPLEX_PROFILES=true\nRELAY_HOME_CHANNEL=none\nHERMES_AUTO_CONTINUE_FRESHNESS=1\n")
}

/// A profile's directory, under the Hermes home.
pub fn profile_dir(home: &Path, agent_fragment: &str) -> PathBuf {
    home.join("profiles").join(wire::profile(agent_fragment))
}

/// Closes each turn a restart cut, in Hermes' own sessions, before the
/// gateway starts (docs/durable-computers.md, P5), run by Hermes' Python as
/// the hermes user with each `state.db` to look in as its arguments.
///
/// Why: Hermes persists a turn's message as the turn starts and the rest as
/// it goes, so a turn the container's end cut leaves its session's tail open
/// (its message, or its tool calls, with no answer). v0.21.5 then joins the
/// next message to the open request (two user messages in a row are one:
/// `agent/agent_runtime_helpers.py`, `_merge_consecutive_users`; and
/// `get_messages_as_conversation(repair_alternation=True)` as the gateway
/// loads a transcript), and its model redoes the cut request (F10: seen on
/// the real image, `[paul] do the risky thing\n\n[paul] good morning`). The
/// bridge has already ended that turn as lost (P1), and tells the next turn
/// what was cut, from the journal.
///
/// How, through Hermes' own code and nothing else: the row Hermes itself
/// writes when a turn ends without an answer, its failed-turn boundary
/// (`agent/turn_failure_copy.py`: an assistant row, `display_kind`
/// `failed_turn`, its copy chosen by `failed_turn_notice` from the turn's
/// rows: "Some actions may already have run" when a tool call is among
/// them). Hermes writes it for a turn that fails in its process
/// (`agent/conversation_loop.py`, `_close_durable_failed_turn`;
/// `gateway/run_turn.py`, `_hmwa_close_failed_turn`, keyed on the durable
/// tail); a process that was killed never does, and neither its clean-exit
/// path (it only discards turn markers) nor its unclean one (it resumes the
/// turn under its old message id, the auto-continue kept off above) closes
/// it. No Relay frame does either: an inbound's fields (text, media,
/// context, reply, prompt response) never change how the session's tail is
/// read. So the boot writes the same row for the turns a restart cut:
/// every gateway session of the relay platform (`SessionDB.
/// list_gateway_sessions`, the newest session per chat) whose last message
/// is a user row, a tool result or a tool call, appended with
/// `SessionDB.append_message`. At boot every such tail is a cut turn: the
/// gateway that ran it is gone, and the bridge ended it. Idempotent: a
/// closed tail is an assistant row. Prints `{"sessions": n, "closed": n}`.
pub const CLOSE_CUT_TURNS: &str = r#"import json, sys
from pathlib import Path
from hermes_state import SessionDB
from agent.turn_failure_copy import FAILED_TURN_DISPLAY_KIND, failed_turn_notice
sessions = closed = 0
for p in sys.argv[1:]:
    db = SessionDB(Path(p))
    try:
        for s in db.list_gateway_sessions(platform="relay"):
            sessions += 1
            rows = [m for m in db.get_messages_as_conversation(s["id"]) if m.get("role") in ("user", "assistant", "tool")]
            if not rows or (rows[-1]["role"] == "assistant" and not rows[-1].get("tool_calls")):
                continue
            asked = max((i for i, m in enumerate(rows) if m["role"] == "user"), default=0)
            db.append_message(s["id"], "assistant", failed_turn_notice(rows[asked:]), display_kind=FAILED_TURN_DISPLAY_KIND)
            closed += 1
    finally:
        db.close()
print(json.dumps({"sessions": sessions, "closed": closed}))
"#;

/// What `CLOSE_CUT_TURNS` printed: the sessions it looked at and those it
/// closed (its last line), or why that is not its answer.
pub fn cut_turns_closed(out: &str) -> Result<(u64, u64), String> {
    let line = out.lines().rev().find(|l| !l.trim().is_empty()).ok_or("it printed nothing")?;
    let v: serde_json::Value = serde_json::from_str(line).map_err(|e| format!("{line:?}: {e}"))?;
    match (v["sessions"].as_u64(), v["closed"].as_u64()) {
        (Some(sessions), Some(closed)) if closed <= sessions => Ok((sessions, closed)),
        _ => Err(format!("not its answer: {line}")),
    }
}

/// The gateway's control socket (Hermes' `gateway/control_socket.py`):
/// `$HERMES_HOME/gateway.sock`, or the path its pointer file names when the
/// home's is too long for a socket. One JSON line in, one out, per
/// connection.
pub const CONTROL_SOCKET: &str = "gateway.sock";
pub const CONTROL_POINTER: &str = "gateway.sock.path";
/// The control protocol's version (`CONTROL_PROTOCOL_VERSION`).
pub const CONTROL_PROTOCOL: u64 = 1;
/// An answer is at most this many bytes (its `_MAX_RESPONSE_BYTES`).
pub const CONTROL_ANSWER_MAX_BYTES: usize = 512 * 1024;

/// One control request: `verb`, with no arguments.
pub fn control_request(verb: &str) -> String {
    assert!(!verb.is_empty() && verb.bytes().all(|b| b.is_ascii_lowercase() || b == b'-'), "a control verb: {verb}");
    format!("{}\n", serde_json::json!({ "verb": verb, "id": 1, "protocol": CONTROL_PROTOCOL }))
}

/// A control answer's `result`, or why there is none (`ok: false` names
/// its error).
pub fn control_answer(line: &[u8]) -> Result<serde_json::Value, String> {
    if line.len() > CONTROL_ANSWER_MAX_BYTES {
        return Err(format!("an answer of {} bytes", line.len()));
    }
    let v: serde_json::Value = serde_json::from_slice(line).map_err(|e| format!("an answer that is not JSON: {e}"))?;
    if v["ok"] != true {
        return Err(v["error"].as_str().unwrap_or("refused, saying nothing").to_string());
    }
    match &v["result"] {
        r @ serde_json::Value::Object(_) => Ok(r.clone()),
        _ => Err("an answer with no result".into()),
    }
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

    /// The closer's answer is its last line, and nothing else is: a line
    /// Hermes logged before it is passed over, a missing or impossible count
    /// is no answer. (Run against the real Hermes in the bridge's
    /// tests/docker.rs.)
    #[test]
    fn the_closers_answer() {
        assert_eq!(cut_turns_closed("{\"sessions\": 2, \"closed\": 1}\n"), Ok((2, 1)));
        assert_eq!(cut_turns_closed("a warning Hermes logged\n{\"sessions\": 0, \"closed\": 0}\n\n"), Ok((0, 0)));
        for bad in ["", "\n", "Traceback (most recent call last):", "{\"sessions\": 1}", "{\"sessions\": 1, \"closed\": 2}", "{\"sessions\": -1, \"closed\": 0}"] {
            assert!(cut_turns_closed(bad).is_err(), "{bad:?}");
        }
        // only Hermes' own session code writes: its boundary row, through its API
        for uses in ["from hermes_state import SessionDB", "list_gateway_sessions(platform=\"relay\")", "failed_turn_notice(", "display_kind=FAILED_TURN_DISPLAY_KIND", "db.append_message("] {
            assert!(CLOSE_CUT_TURNS.contains(uses), "{uses}");
        }
        for never in ["sqlite3", "execute(", "DELETE", "UPDATE", "INSERT"] {
            assert!(!CLOSE_CUT_TURNS.contains(never), "no SQL of ours: {never}");
        }
    }

    fn agent() -> Agent {
        Agent { fragment: "juniper.paul".into(), identity: "id:j".into(), name: "Juniper".into(), owner: "id:paul".into(), credentials: vec![] }
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
        let m = managed_config(&["platforms/discord".into(), "dashboard_auth/basic".into()], APPROVAL_TIMEOUT_S);
        for want in [
            "transport: \"draft\"",
            "busy_input_mode: \"queue\"",
            "group_sessions_per_user: false",
            "disabled_toolsets: [\"cronjob\"]",
            "mode: \"smart\"",
            "    - \"platforms/discord\"",
            "bot_desktop:\n  auto_start: true\n",
        ] {
            assert!(m.contains(want), "managed config has {want}:\n{m}");
        }
        assert!(m.contains(&format!("timeout: {APPROVAL_TIMEOUT_S}")));
        // `display`'s own lines: the model's text beside a tool call is no
        // message of its own, for every platform (none names `relay`)
        let display: Vec<&str> = m.lines().skip_while(|l| *l != "display:").skip(1).take_while(|l| l.starts_with("  ")).collect();
        assert!(display.contains(&"  interim_assistant_messages: false"), "no interim messages, under display: {m}");
        assert!(!m.contains("\n  platforms:"), "no platform's display setting overrides it: {m}");
        // a test's shorter approval, held within the bridge's bounds
        assert_eq!(approval_timeout_s(None), APPROVAL_TIMEOUT_S);
        assert_eq!(approval_timeout_s(Some("20")), 20);
        assert_eq!(approval_timeout_s(Some("1")), 10, "no shorter than the bridge's shortest prompt");
        assert_eq!(approval_timeout_s(Some("not a number")), APPROVAL_TIMEOUT_S);
        assert!(managed_config(&[], 20).contains("timeout: 20\n"));
        let creds = Path::new("/data/hermes/profiles/juniper-paul/credentials.sh");
        let p = profile_config(&agent(), Tier::Medium, "http://model.fragment.internal/", &[], creds);
        assert!(p.contains("base_url: \"http://model.fragment.internal/v1\""), "{p}");
        assert!(p.contains("default: \"medium\""));
        assert!(p.contains("x-fragment-agent: \"juniper.paul\""), "every model call names its agent");
        let h = profile_config(&agent(), Tier::High, "http://model.fragment.internal", &[], creds);
        assert!(h.contains("provider: \"anthropic\"") && h.contains("/anthropic\""), "{h}");
        // its eyes: the route's vision model, OpenAI's shape, whatever its tier
        let vision = "auxiliary:\n  vision:\n    provider: \"custom\"\n    base_url: \"http://model.fragment.internal/v1\"\n    model: \"vision\"\n    api_key: \"fragment-model\"\n";
        for (tier, config) in [("medium", &p), ("high", &h), ("cheap", &profile_config(&agent(), Tier::Cheap, "http://model.fragment.internal", &[], creds))] {
            assert!(config.contains(vision), "the {tier} tier's screenshots go to the route's vision model: {config}");
        }
        assert!(!m.contains("auxiliary:"), "each profile's own, beside the headers that name its agent: {m}");
        assert!(p.contains("skills:\n  external_dirs: [\"/data/hermes/managed-skills\", \"/opt/fragment/skills\"]\n"), "the managed skills, then the platform skill, after its own: {p}");
        assert!(p.contains("browser:\n  headed: true\n  backend: \"off\"\n"), "Hermes' built-in browser, headed, in the profile's own config: {p}");
        assert!(!m.contains("browser:"), "Hermes never reads `browser` from the managed overlay: {m}");
        assert!(p.contains("terminal:\n  env_passthrough: [\"FRAGMENT_AS_AGENT\", \"FRAGMENT_FOR\"]\n"), "its terminal acts as the agent: {p}");
        assert!(p.contains("\n  cwd: \"/data/work/juniper-paul\"\n"), "its terminal works in its work directory: {p}");
        assert_eq!(work_dir("juniper.paul"), PathBuf::from("/data/work/juniper-paul"));
        assert!(
            p.contains("  shell_init_files: [\"~/.profile\", \"~/.bash_profile\", \"~/.bashrc\", \"/data/hermes/profiles/juniper-paul/credentials.sh\"]\n"),
            "its shell starts as Hermes' does, then reads its credentials: {p}"
        );
        let e = profile_env(&agent());
        assert!(e.contains("\nFRAGMENT_AS_AGENT=juniper.paul\n") && e.contains("\nFRAGMENT_FOR=id:paul\n"), "{e}");
        assert!(!e.contains("KEY") && !e.contains("TOKEN"), "no credential, and none held: {e}");
        let env = gateway_env("127.0.0.1:8650", "computer", &"s".repeat(32));
        assert!(env.contains("GATEWAY_RELAY_URL=http://127.0.0.1:8650\n"));
        assert!(env.contains("HERMES_GATEWAY_BUSY_INPUT_MODE=queue"));
        assert!(env.contains("HERMES_AUTO_CONTINUE_FRESHNESS=1\n"), "a turn a restart cut short is never auto-continued");
        assert_eq!(profile_dir(Path::new("/data/hermes"), "juniper.paul"), PathBuf::from("/data/hermes/profiles/juniper-paul"));
    }

    /// The image's Chromium starts Playwright's with the container's flags,
    /// headed on a desktop and the headless shell with none: run here by
    /// `sh`, as Hermes runs it.
    #[test]
    fn the_images_chromium_carries_the_containers_flags() {
        let s = chromium_script(Path::new("/opt/p/chrome-linux64/chrome"), Path::new("/opt/p/shell/chrome-headless-shell"));
        assert!(s.starts_with("#!/bin/sh\n"), "{s}");
        assert_eq!(CHROMIUM_FLAGS, ["--no-sandbox", "--disable-dev-shm-usage"], "Hermes' CHROMIUM_SANDBOX_BYPASS_ARGS");
        let dir = std::env::temp_dir().join(format!("hermes-chromium-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // the two browsers, each saying what it was started with
        let echo = |name: &str| {
            let p = dir.join(name);
            std::fs::write(&p, format!("#!/bin/sh\necho {name} \"$@\"\n")).unwrap();
            std::fs::set_permissions(&p, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
            p
        };
        let script = dir.join("chromium");
        std::fs::write(&script, chromium_script(&echo("full"), &echo("shell"))).unwrap();
        let run = |display: Option<&str>| {
            let mut c = std::process::Command::new("sh");
            c.arg(&script).args(["--user-data-dir=/p", "https://example.com"]).env_remove("DISPLAY");
            if let Some(d) = display {
                c.env("DISPLAY", d);
            }
            String::from_utf8(c.output().unwrap().stdout).unwrap()
        };
        assert_eq!(run(Some(":20")), "full --no-sandbox --disable-dev-shm-usage --user-data-dir=/p https://example.com\n", "on a desktop, the full Chromium");
        assert_eq!(run(None), "shell --no-sandbox --disable-dev-shm-usage --user-data-dir=/p https://example.com\n", "with none, the headless shell");
        assert_eq!(run(Some("")), "shell --no-sandbox --disable-dev-shm-usage --user-data-dir=/p https://example.com\n", "an empty DISPLAY is none");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Hermes' container guesses, pinned: each named once, as Hermes reads
    /// it, and nothing the boot writes names one again (Hermes' config
    /// overrides its environment); `profile` needs each profile's `home`,
    /// without which Hermes keeps the gateway's own `HOME`.
    #[test]
    fn hermes_container_guesses_are_pinned() {
        let pins: std::collections::BTreeMap<&str, &str> = RUNTIME_ENV.into_iter().collect();
        assert_eq!(pins.len(), RUNTIME_ENV.len(), "each named once: {RUNTIME_ENV:?}");
        assert_eq!(pins.get("TERMINAL_HOME_MODE"), Some(&"profile"), "one of Hermes' auto, real, profile");
        assert_eq!(pins.get("HERMES_SKIP_CHMOD"), Some(&"1"));
        assert!(PROFILE_DIRS.contains(&"home"), "every profile has its home: {PROFILE_DIRS:?}");
        let creds = Path::new("/data/hermes/profiles/juniper-paul/credentials.sh");
        let written = [managed_config(&["platforms/discord".into()], APPROVAL_TIMEOUT_S), default_config("http://m"), profile_config(&agent(), Tier::Medium, "http://m", &[], creds), gateway_env("127.0.0.1:1", "c", &"s".repeat(32))];
        for (name, _) in RUNTIME_ENV {
            let key = name.strip_prefix("TERMINAL_").unwrap_or(name).to_ascii_lowercase();
            assert!(written.iter().all(|w| !w.contains(name) && !w.contains(&format!("{key}:"))), "{name} is the boot's environment's alone: {written:#?}");
        }
    }

    #[test]
    #[should_panic(expected = "a browser's path, absolute, quotable")]
    fn a_browser_path_a_shell_would_misread_is_a_bug() {
        chromium_script(Path::new("/opt/it's/chrome"), Path::new("/opt/shell"));
    }

    fn credential(provider: &str, env: &[&str], placeholder: &str) -> fragment_bridge::runtime::Credential {
        fragment_bridge::runtime::Credential {
            provider: provider.into(),
            kind: "operator".into(),
            env: env.iter().map(|e| e.to_string()).collect(),
            placeholder: placeholder.into(),
            hosts: vec!["api.perplexity.ai".into()],
        }
    }

    /// Valid: each credential's placeholder is in its environment
    /// variables, in the profile's `.env` (for Hermes and its tools) and in
    /// the terminal's credentials file; the terminal is passed every name
    /// the deployment may give, held now or not.
    #[test]
    fn a_profile_holds_its_credentials_placeholders() {
        let mut a = agent();
        let tag = "0123456789abcdef0123456789abcdef";
        a.credentials = vec![
            credential("perplexity", &["PERPLEXITY_API_KEY"], &format!("fck_perplexity_{tag}")),
            credential("google", &["GOOGLE_OAUTH_ACCESS_TOKEN"], &format!("fcx_google_{tag}")),
        ];
        let e = profile_env(&a);
        assert!(e.contains(&format!("\nPERPLEXITY_API_KEY=fck_perplexity_{tag}\n")) && e.contains(&format!("\nGOOGLE_OAUTH_ACCESS_TOKEN=fcx_google_{tag}\n")), "{e}");
        let s = credentials_sh(&a);
        assert!(s.contains(&format!("\nexport PERPLEXITY_API_KEY='fck_perplexity_{tag}'\n")), "{s}");
        let env = ["GOOGLE_OAUTH_ACCESS_TOKEN".to_string(), "PERPLEXITY_API_KEY".to_string(), "XAI_API_KEY".to_string()];
        let p = profile_config(&a, Tier::Medium, "http://model.fragment.internal", &env, Path::new("/c.sh"));
        assert!(p.contains("env_passthrough: [\"FRAGMENT_AS_AGENT\", \"FRAGMENT_FOR\", \"GOOGLE_OAUTH_ACCESS_TOKEN\", \"PERPLEXITY_API_KEY\", \"XAI_API_KEY\"]"), "{p}");
    }

    /// Invalid: a credential the platform sent out of shape (a name an image
    /// relies on, a value a line or a shell would read as more) is left out;
    /// a name given twice keeps the first.
    #[test]
    fn a_credential_out_of_shape_is_left_out() {
        let mut a = agent();
        let tag = "0123456789abcdef0123456789abcdef";
        a.credentials = vec![
            credential("a", &["FRAGMENT_AS_AGENT", "A_KEY"], &format!("fck_a_{tag}")),
            credential("b", &["B_KEY"], "fck_b_x'\nrm -rf /"),
            credential("c", &["lower"], &format!("fck_c_{tag}")),
            credential("d", &["A_KEY"], &format!("fck_d_{tag}")),
        ];
        assert_eq!(credential_vars(&a), vec![("A_KEY".to_string(), format!("fck_a_{tag}"))]);
        let p = profile_config(&a, Tier::Medium, "http://m", &["FRAGMENT_FOR".into(), "HERMES_HOME".into(), "A_KEY".into()], Path::new("/c.sh"));
        assert!(p.contains("env_passthrough: [\"FRAGMENT_AS_AGENT\", \"FRAGMENT_FOR\", \"A_KEY\"]"), "{p}");
        assert!(!credentials_sh(&a).contains("rm -rf"));
    }

    /// The gateway's control wire as `gateway/control_socket.py` speaks it:
    /// a request is one line; an answer's result, or its refusal.
    #[test]
    fn the_control_socket_wire() {
        let r: serde_json::Value = serde_json::from_str(control_request("rescan-profiles").trim_end()).unwrap();
        assert_eq!(r, serde_json::json!({ "verb": "rescan-profiles", "id": 1, "protocol": 1 }));
        assert!(control_request("status").ends_with('\n'), "one line");
        let ok = br#"{"ok": true, "protocol": 1, "result": {"multiplex": true, "added": ["maple-paul"], "served_profiles": ["default", "maple-paul"]}, "id": 1}"#;
        assert_eq!(control_answer(ok).unwrap()["added"][0], "maple-paul");
        assert_eq!(control_answer(br#"{"ok": false, "error": "unknown verb: 'x'", "protocol": 1}"#).unwrap_err(), "unknown verb: 'x'");
        assert!(control_answer(b"not json").is_err());
        assert!(control_answer(br#"{"ok": true, "result": null}"#).is_err());
        assert!(control_answer(&vec![b' '; CONTROL_ANSWER_MAX_BYTES + 1]).is_err());
    }

    #[test]
    #[should_panic(expected = "a control verb")]
    fn a_verb_out_of_shape_is_a_bug() {
        control_request("rescan profiles\n{");
    }

    #[test]
    #[should_panic(expected = "an agent's fragment and owner are names")]
    fn a_profile_env_of_no_names_is_a_bug() {
        let mut a = agent();
        a.owner = "id:paul\nOPENAI_API_KEY=x".into();
        profile_env(&a);
    }

    #[test]
    #[should_panic(expected = "a Relay secret of 32 characters or more")]
    fn a_short_secret_is_refused() {
        gateway_env("127.0.0.1:1", "c", "short");
    }
}
