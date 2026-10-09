//! What the Hermes image writes for its Hermes at each boot (and for an
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

/// New agents run on the cheap tier (GLM-5.3 Flash, DeepSeek V4 Flash its
/// fallback; Paul, 2026-10-09).
pub const DEFAULT_TIER: Tier = Tier::Cheap;

impl Tier {
    /// An agent fragment's `agent.json` names its tier (`{"tier": …}`);
    /// anything else is the default tier. The high tier is off until
    /// Cloudflare raises its limit (decision 23), so it falls back to the default
    /// unless `high_on`.
    pub fn of(agent_json: Option<&[u8]>, high_on: bool) -> Tier {
        let tier = agent_json.and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok()).and_then(|v| v["tier"].as_str().map(str::to_string));
        match tier.as_deref() {
            Some("cheap") => Tier::Cheap,
            Some("medium") => Tier::Medium,
            Some("high") if high_on => Tier::High,
            _ => DEFAULT_TIER,
        }
    }

    /// An agent's tier as the platform answered for its fragment's
    /// `agent.json`: the file's (`of`), or the default tier when it has none
    /// (403, 404); `None` when the platform did not answer for it.
    pub fn read(answer: &Result<bytes::Bytes, fragment_bridge::api::ApiError>, high_on: bool) -> Option<Tier> {
        match answer {
            Ok(b) => Some(Tier::of(Some(b), high_on)),
            Err(e) if e.gone() => Some(Tier::of(None, high_on)),
            Err(_) => None,
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

/// A model of a provider an agent's owner connected, which their account
/// there pays for: its `agent.json`'s `model: {provider, id}` beside its
/// `tier` (docs/computers.md, "An agent's own model").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnModel {
    pub provider: String,
    pub id: String,
}

/// A model's id, at most (`anthropic/claude-sonnet-5.5`; the platform's
/// catalog holds its offer to the same).
pub const MODEL_ID_MAX_BYTES: usize = 128;
/// A model base URL the platform names, at most.
pub const MODEL_BASE_MAX_BYTES: usize = 512;

impl OwnModel {
    /// The model `agent.json` names, if it names one well: a provider's
    /// name (`[a-z0-9-]`, 1 to 32) and an id of 1 to `MODEL_ID_MAX_BYTES`
    /// letters, digits and `.`, `_`, `-`, `:`, `/`, `@`.
    pub fn of(agent_json: &[u8]) -> Option<OwnModel> {
        let v: serde_json::Value = serde_json::from_slice(agent_json).ok()?;
        let (provider, id) = (v["model"]["provider"].as_str()?, v["model"]["id"].as_str()?);
        let provider_ok = (1..=32).contains(&provider.len()) && provider.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
        let id_ok = (1..=MODEL_ID_MAX_BYTES).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b':' | b'/' | b'@'));
        (provider_ok && id_ok).then(|| OwnModel { provider: provider.into(), id: id.into() })
    }
}

/// What an agent's main model is: a tier through the platform's model
/// route, or its own (`OwnModel`), sent to its provider's base URL with
/// the provider's placeholder as its key, which the computer's swap
/// replaces with its owner's key on the way out (the guest never holds it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MainModel {
    Tier(Tier),
    Own { provider: String, base: String, id: String, placeholder: String },
}

impl From<Tier> for MainModel {
    fn from(t: Tier) -> MainModel {
        MainModel::Tier(t)
    }
}

impl MainModel {
    /// The model `agent` runs on: `own` where its credentials now hold that
    /// provider's placeholder with a model base (its owner connected it and
    /// did not narrow the agent from it), else `tier`; and, when it names
    /// its own and that is not used, why.
    pub fn of(agent: &Agent, tier: Tier, own: Option<&OwnModel>) -> (MainModel, Option<String>) {
        let Some(own) = own else { return (MainModel::Tier(tier), None) };
        let Some(c) = agent.credentials.iter().find(|c| c.provider == own.provider) else {
            return (MainModel::Tier(tier), Some(format!("its owner has not connected {} (or narrowed the agent from it)", own.provider)));
        };
        let base = c.model_base.as_deref().map(|b| b.trim_end_matches('/')).unwrap_or_default();
        let base_ok = (base.starts_with("https://") || base.starts_with("http://")) && base.len() <= MODEL_BASE_MAX_BYTES && base.bytes().all(|b| (0x21..=0x7e).contains(&b) && b != b'"' && b != b'\\');
        if !base_ok {
            return (MainModel::Tier(tier), Some(format!("{} serves no models here", own.provider)));
        }
        (MainModel::Own { provider: own.provider.clone(), base: base.into(), id: own.id.clone(), placeholder: c.placeholder.clone() }, None)
    }

    /// How events name it: a tier, or `<provider>:<id>`.
    pub fn name(&self) -> String {
        match self {
            MainModel::Tier(t) => t.name().into(),
            MainModel::Own { provider, id, .. } => format!("{provider}:{id}"),
        }
    }
}

/// The model route's name for the deployment's vision model
/// (`fragment_core::models::VISION`): Hermes' auxiliary vision names it.
pub const VISION_MODEL: &str = "vision";
/// The model route's name for transcription
/// (`fragment_core::transcribe::WHISPER`): Hermes' speech-to-text names it.
pub const TRANSCRIBE_MODEL: &str = "whisper";
/// The model route's name for the deployment's fallback model
/// (`fragment_core::models::FALLBACK`): a tier agent's `fallback_providers`
/// names it.
pub const FALLBACK_MODEL: &str = "fallback";
/// How long a tier agent's call may stream nothing before Hermes kills it
/// (its `providers.<id>.models.<model>.stale_timeout_seconds`; Paul,
/// 2026-10-09): a stalled call is tried once more (`HERMES_STREAM_RETRIES`,
/// `gateway_env`), then the agent switches to `fallback_providers` (its
/// `agent.api_max_retries: 1`), about 40 s after the stall began. GLM-5.3
/// Flash's first bytes are 1 to 5 s; DeepSeek's were 11 to 61 s when
/// Workers AI was short of it.
pub const STALE_TIMEOUT_S: u32 = 20;
/// Hermes' in-place retries of a stream that dropped or went stale
/// (`HERMES_STREAM_RETRIES`, its default 2): one, so a stall reaches the
/// fallback after two stale timeouts.
pub const STREAM_RETRIES: u32 = 1;

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
/// approvals (waiting `approval_timeout_s` on each), when its desktop is
/// idle (`screen_idle_ms`), and what it never runs.
pub fn managed_config(disabled_plugins: &[String], approval_timeout_s: u64, screen_idle_ms: u64) -> String {
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
    // Approvals default to Hermes' `smart` mode (decision 16): a terminal command
    // Hermes flags, or an execute_code script, goes first to its guardian (the
    // agent's own model, through the route, as the agent), and only one it
    // escalates is a card. The agent's desktop is never asked about
    // (`DESKTOP_ACTIONS`, each profile's own config). A card waits as long as the
    // bridge's prompt does. Slash confirmations stay off: a person's leading `/`
    // never reaches Hermes as a command.
    y.push_str(&format!("approvals:\n  mode: \"smart\"\n  timeout: {approval_timeout_s}\n  destructive_slash_confirm: false\n"));
    // Hermes' own cron is off: an agent's routines are its fragment's cron (decision 38).
    y.push_str("agent:\n  disabled_toolsets: [\"cronjob\"]\n");
    // A voice memo's transcript is no message of its own: the agent hears it
    // and answers once (one reply a turn, #160). The gateway reads this from
    // its own home's config, with this overlay merged over it; each profile
    // transcribes through the route (`profile_config`, `stt`).
    y.push_str("stt:\n  echo_transcripts: false\n");
    // Hermes' remote model catalogs are off: every profile's model is the
    // platform's route, and they serve only its `/model` picker (which a
    // person never reaches: the bridge keeps a leading `/` from reading as a
    // command) and OpenRouter's and Nous' routes. On, the gateway fetched
    // them from four hosts 30 s after it started and every 20 minutes, and
    // rewrote four caches in its home while the computer was held
    // (docs/durable-computers.md, "What changes under the hold").
    y.push_str("model_catalog:\n  enabled: false\n");
    // Each agent's desktop starts for its screen's first viewer (the
    // bridge's `screen-start`), or at the agent's first computer_use or
    // browser call, never at boot (measured: about 300 MiB more once it
    // runs). Its browser, on that desktop, is each profile's own config's
    // (`profile_config`). Its idle stop is the boot's (desktop.rs: Hermes
    // stops an idle desktop only from its TUI's gateway), said here too so
    // Hermes' own watcher, were it to run, would agree.
    let minutes = screen_idle_ms as f64 / 60_000.0;
    y.push_str(&format!("bot_desktop:\n  auto_start: true\n  idle_stop_minutes: {minutes}\n"));
    // Lazy installs are off (upstream's image turns them on): they fetch
    // Hermes' own optional backends (providers, platforms, speech) into its
    // home, which a computer configures none of (its model is the platform's
    // route, its chat the relay). What our agents use is in the image (Edge's
    // speech for text_to_speech: images/hermes/Dockerfile). Software the
    // agent wants is its terminal's to install.
    y.push_str("security:\n  allow_lazy_installs: false\n");
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

/// Every `computer_use` action Hermes v0.21.6 asks a person about, and its
/// `bring_to_front` scope (`tools/computer_use/tool.py`: each `_ACTIONS`
/// entry marked `destructive`, which is every one but `capture`, `wait`,
/// `list_apps` and `list_windows`; `handle_computer_use` asks for each scope
/// in turn). Its gate (`_request_approval`, through `tools/approval.py`'s
/// `_run_approval_gate`) never consults the smart guardian: only `/yolo`,
/// `approvals.mode: off`, or a standing grant pass it, so in `smart` mode
/// every scroll and every focus change was a card (Paul on p5, 2026-10-09:
/// "asking me for permission for silly things like changing focus in
/// computer use and scrolling").
///
/// The desktop these drive is the agent's own (an Xvnc of its own in the
/// computer; Take over is the person's way onto it), so each profile holds
/// them all granted (`desktop_grants`), in both delivery modes: what the
/// agent does there is what its browser tools do in the same Chromium, which
/// Hermes never asks about. What still asks: Hermes' dangerous commands in
/// its terminal and its execute_code scripts, after the smart guardian
/// (`managed_config`), and the writes Hermes always asks about (an SSH
/// config; a project's `AGENTS.md`, `SOUL.md`, `CLAUDE.md`, `.cursorrules`).
/// Hermes' hard blocks on the desktop (a log-out key, `curl … | sh` typed)
/// stand: they come before the gate.
pub const DESKTOP_ACTIONS: [&str; 11] = ["click", "double_click", "right_click", "middle_click", "drag", "scroll", "type", "key", "set_value", "focus_app", "bring_to_front"];

/// `computer_use`'s delivery modes: a grant is per action and mode
/// (`cua:<action>:<mode>`), foreground (it raises the window) apart.
pub const DELIVERY_MODES: [&str; 2] = ["background", "foreground"];

/// The standing grants each profile holds: one `command_allowlist` key
/// (`cua:<action>:<mode>`, Hermes' own, as its "always" answer would store
/// it) for each of `DESKTOP_ACTIONS` in each of `DELIVERY_MODES`. Hermes
/// reads a profile's list as its gate's first answer (`is_approved`). No
/// key is a terminal command's pattern, and none a glob, so the terminal's
/// commands are flagged and reviewed as before.
pub fn desktop_grants() -> Vec<String> {
    DESKTOP_ACTIONS.iter().flat_map(|a| DELIVERY_MODES.iter().map(move |m| format!("cua:{a}:{m}"))).collect()
}

/// An agent's profile config: its model, through the model intercept, as
/// that agent (`x-fragment-agent` on every call, the main model's and the
/// auxiliary ones'), and its vision model (the route's `vision`); and what
/// its terminal is given: who it is
/// (`PROFILE_ENV`) and every credential's environment variable the
/// deployment may give it (`credential_env`, the guest view's: Hermes reads
/// the list once per gateway, so it names them all, held now or not, and
/// each value is the profile's `.env`'s, read again at every turn). Hermes
/// never passes a name it keeps for its own providers' keys (left out of
/// the passthrough list, which otherwise warns at every turn)
/// (`PERPLEXITY_API_KEY`, `XAI_API_KEY`, `ELEVENLABS_API_KEY`, …: its
/// `_HERMES_PROVIDER_ENV_BLOCKLIST`), so the terminal also sources the
/// profile's credentials file (`credentials_sh`) as its shell starts.
pub fn profile_config(agent: &Agent, main: &MainModel, model_base: &str, credential_env: &[String], credentials_file: &Path) -> String {
    let base = model_base.trim_end_matches('/');
    let mut y = String::new();
    y.push_str(&format!("# Written by hermes-boot for {} (model {}) at every boot, and when its model changes.\n", agent.fragment, main.name()));
    match main {
        MainModel::Tier(tier) => {
            let (provider, url) = match tier {
                // The high tier is Anthropic's Messages shape, passed through.
                Tier::High => ("anthropic", format!("{base}/anthropic")),
                Tier::Cheap | Tier::Medium => ("custom", format!("{base}/v1")),
            };
            y.push_str(&format!("model:\n  provider: {}\n  base_url: {}\n  default: {}\n", q(provider), q(&url), q(tier.name())));
            // The guest holds no credential: the intercept strips auth and adds its own.
            y.push_str("  api_key: \"fragment-model\"\n  context_length: 262144\n");
        }
        // Its owner's model, at its provider (OpenAI's chat-completions
        // shape): its key the provider's placeholder, which the swap
        // replaces on the way out. Its context length is Hermes' to find
        // (the provider's own metadata).
        MainModel::Own { base, id, placeholder, .. } => {
            y.push_str(&format!("model:\n  provider: \"custom\"\n  base_url: {}\n  default: {}\n", q(base), q(id)));
            y.push_str(&format!("  api_key: {}\n", q(placeholder)));
        }
    }
    // On every call, the main model's and the auxiliary ones': the route
    // reads it, and the swap sends no `x-fragment-` header to a provider.
    y.push_str(&format!("  default_headers:\n    x-fragment-agent: {}\n", q(&agent.fragment)));
    // The managed skills and the platform skill's view (skills.rs), below
    // the profile's own `skills/`: an agent's own wins, and a managed one
    // over the platform's, which leaves the view for it.
    let dirs: Vec<String> = crate::skills::EXTERNAL_DIRS.iter().map(|d| q(d)).collect();
    y.push_str(&format!("skills:\n  external_dirs: [{}]\n", dirs.join(", ")));
    // Its eyes: Hermes' auxiliary vision (each computer_use screenshot, and
    // an image a person attaches, described in words for the main model) on
    // the route's `vision`, the deployment's vision model, whatever the
    // agent's tier (the medium tier's GLM-5.3 reads no images). Named
    // outright, Hermes routes every capture and attachment through it (its
    // image routing, `agent/image_routing.py`: in `auto`, an explicit
    // `auxiliary.vision` makes it text, which each capture asks through
    // `tools/vision_tools.py`'s `_native_tool_result_images`) and
    // sends it, as every
    // call to a custom endpoint, with `model.default_headers`: the agent's
    // `x-fragment-agent`, so the intercept meters it to the agent's owner.
    // Always OpenAI's shape, the high tier's agents' too.
    // Memory and skill reviews are on. The image plugin forks our external
    // skills into the active profile before writes; Hermes retains its guards.
    y.push_str(&format!(
        "plugins:\n  enabled: [fragment-skill-fork]\nauxiliary:\n  background_review: {{ enabled: true }}\n  vision:\n    provider: \"custom\"\n    base_url: {}\n    model: {}\n    api_key: \"fragment-model\"\n",
        q(&format!("{base}/v1")),
        q(VISION_MODEL)
    ));
    // Its ears (decision 9: a voice memo is one the agent transcribes
    // itself): Hermes' speech-to-text, which a voice note a person attaches
    // gets before its turn, on the route's `whisper` (Workers AI's Whisper),
    // OpenAI's transcription shape through the intercept, metered to the
    // agent's owner. Hermes' STT client takes a base URL and a key and sends
    // no header of ours, so its key names the agent (`agent:<name>`, which
    // the intercept reads and sends no further). No language: Whisper
    // detects it (Hermes' own default, `en`, mangles every other).
    y.push_str(&format!(
        "stt:\n  provider: \"openai\"\n  language: \"\"\n  openai:\n    base_url: {}\n    api_key: {}\n    model: {}\n",
        q(&format!("{base}/v1")),
        q(&format!("agent:{}", agent.fragment)),
        q(TRANSCRIBE_MODEL)
    ));
    // A text tier's stall (Paul, 2026-10-09: a time-based fallback "only if
    // time-based fallback is built into hermes"; it is): Hermes' own stale
    // detector kills a call that streams nothing for `STALE_TIMEOUT_S`
    // (this model's alone: `vision`, whose answers are whole and slower,
    // keeps Hermes' default), tries it once more (`STREAM_RETRIES`), then,
    // its one retry spent (`api_max_retries`), switches to its
    // `fallback_providers`: the route's `fallback`, the deployment's
    // fallback model, through the intercept as the agent (its key names
    // the agent). The next turn starts on its tier again (Hermes'
    // `restore_primary_runtime`). The route itself falls back when a model
    // answers 429 or a 5xx (cell/src/models.rs); this is for a call that
    // answers nothing. An owner's own model and the high tier have none.
    if let MainModel::Tier(tier @ (Tier::Cheap | Tier::Medium)) = main {
        y.push_str(&format!("providers:\n  custom:\n    models:\n      {}:\n        stale_timeout_seconds: {STALE_TIMEOUT_S}\n", q(tier.name())));
        y.push_str("agent:\n  api_max_retries: 1\n");
        y.push_str(&format!(
            "fallback_providers:\n  - provider: \"custom\"\n    base_url: {}\n    model: {}\n    api_key: {}\n",
            q(&format!("{base}/v1")),
            q(FALLBACK_MODEL),
            q(&format!("agent:{}", agent.fragment))
        ));
    }
    // Stored selections, never inferred by Hermes from credential presence.
    // Only placeholders actually held by this agent count: the catalog's
    // credential_env also includes providers it cannot currently use.
    let vars = credential_vars(agent);
    let offered = |env: &str| vars.iter().any(|(name, _)| name == env);
    if offered("FIRECRAWL_API_KEY") {
        // Full-page extraction, unlike Perplexity's query-relevant snippets.
        y.push_str("web:\n  backend: \"firecrawl\"\n  search_backend: \"firecrawl\"\n  extract_backend: \"firecrawl\"\n");
    } else if offered("PERPLEXITY_API_KEY") {
        // Search remains usable on a deployment offering only Perplexity;
        // leave extraction's selection untouched.
        y.push_str("web:\n  search_backend: \"perplexity\"\n");
    }
    if offered("FAL_KEY") {
        y.push_str("image_gen:\n  provider: \"fal\"\n  model: \"fal-ai/flux-2/klein/9b\"\n");
    }
    if offered("ELEVENLABS_API_KEY") {
        y.push_str("tts:\n  provider: \"elevenlabs\"\n");
    }
    // XAI_API_KEY enables Hermes' separate x_search, not a web backend.
    // Its browser: Hermes' built-in browser tools (browser_navigate, …),
    // driving the image's own Chromium, headed, on the agent's desktop, so
    // the screen shows it, through agent-browser (in the image: its
    // Dockerfile). Left unset, Hermes picks Browser Use mode (one
    // browser_exec tool, its harness a core dependency of Hermes). Hermes
    // reads `browser` from the profile's own config file alone, never from
    // the managed overlay.
    y.push_str("browser:\n  headed: true\n  backend: \"off\"\n");
    if offered("BROWSER_USE_API_KEY") {
        // Without this Hermes auto-detects the offered key and sends the
        // built-in browser tools to the cloud. Cloud browsing is opt-in
        // through BU_NAME=remote browser-harness; the desktop stays local.
        y.push_str("  cloud_provider: \"local\"\n");
    }
    // Its approvals: its own desktop is never asked about (`DESKTOP_ACTIONS`
    // says why, and what still asks). Hermes reads the list from the
    // config of the profile whose turn it runs (`_permanent_set`).
    y.push_str(&format!("command_allowlist: [{}]\n", desktop_grants().iter().map(|k| q(k)).collect::<Vec<_>>().join(", ")));
    // Its terminal acts as the agent: the fragment CLI and the skills' helpers
    // read these from the profile's `.env` (`profile_env`), which Hermes passes
    // only to the commands of this profile's turns. Its shell's start files are
    // Hermes' own three, then the agent's credentials.
    let mut passed: Vec<String> = PROFILE_ENV.iter().map(|s| s.to_string()).collect();
    for e in credential_env.iter().filter(|e| env_name_ok(e) && !provider_env_blocked(e)) {
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

/// Hermes v0.21.6's `_HERMES_PROVIDER_ENV_BLOCKLIST`, read from the pinned
/// image's `tools/environments/local_env_policy.py`, plus its dynamic
/// `_is_hermes_internal_secret` rule (GHSA-rhgp-j443-p4rf). The Docker lane
/// checks the snapshot against the image, so a new pin cannot silently
/// change the policy. Only sandbox registration is filtered: native tools
/// still read the profile's `.env` through Hermes' secret scope, and the
/// terminal's shell still sources `credentials.sh`.
pub const PROVIDER_ENV_BLOCKLIST: &str = include_str!("provider-env-blocklist.txt");

fn provider_env_blocked(name: &str) -> bool {
    PROVIDER_ENV_BLOCKLIST.lines().any(|n| n == name)
        || (name.starts_with("AUXILIARY_") && (name.ends_with("_API_KEY") || name.ends_with("_BASE_URL")))
        || (name.starts_with("GATEWAY_RELAY_") && ["_SECRET", "_KEY", "_TOKEN"].iter().any(|suffix| name.ends_with(suffix)))
}

/// The flags Chromium needs in a container: Hermes' own
/// (`CHROMIUM_SANDBOX_BYPASS_ARGS`, its `tools/browser_tool_session.py`),
/// which it gives its browser and its desktop's Browser icon only as root
/// or where it sees Docker's marker (`/.dockerenv`). Cloudflare Containers
/// has neither that marker nor a `/dev/shm`, and Chromium without these
/// dies as it starts: no usable sandbox (no user namespaces), or no shared
/// memory (p5, 2026-10-05).
pub const CHROMIUM_FLAGS: [&str; 2] = ["--no-sandbox", "--disable-dev-shm-usage"];

/// The image's Chromium: a script that starts the one Hermes' image pins
/// with `CHROMIUM_FLAGS` whatever the runtime. Hermes is pointed at it
/// (`AGENT_BROWSER_EXECUTABLE_PATH`), so its browser and its desktop's
/// Browser icon (Hermes' `bot_desktop.browser.executable()`) both run it.
pub const CHROMIUM: &str = "/opt/fragment/bin/chromium";

/// The script at `CHROMIUM`, written at image build: Hermes' pinned full
/// Chromium, `full`, its only browser (no headless shell: the full one runs
/// headed on a desktop and headless with none, as its caller asks), its
/// scratch the container's `/tmp` (`CHROMIUM_TMP`).
pub fn chromium_script(full: &Path) -> String {
    let s = full.display().to_string();
    assert!(full.is_absolute() && !s.contains('\''), "a browser's path, absolute, quotable: {s}");
    let flags = CHROMIUM_FLAGS.join(" ");
    format!("#!/bin/sh\n# The image's Chromium (hermes-boot build-info: images/hermes/boot/src/hermes.rs):\n# Hermes' pinned Chromium, always with the flags a container needs,\n# its scratch out of /data.\nexport TMPDIR={CHROMIUM_TMP}\nexec '{s}' {flags} \"$@\"\n")
}

/// Chromium's scratch: the container's own `/tmp`, which no save keeps.
/// Hermes points `TMPDIR` under `/data`: an agent's terminal at its
/// profile's scratch (its work's `tmp_dir`, through a link), Hermes' browser
/// tool at the gateway's own (`/data/hermes/cache/scratch`).
/// Chromium keeps there a headless browser's profile when its caller names
/// none (Chrome 145, which Hermes v0.21.6 pins; 153, in its v0.21.5 image,
/// kept it under `~`), and the shared memory `--disable-dev-shm-usage` moves
/// out of `/dev/shm`: throwaway state any save would carry, and under the
/// gateway's scratch a hold while one runs would find its databases under
/// Hermes' home, locked.
pub const CHROMIUM_TMP: &str = "/tmp";

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
///   (one of `PROFILE_DIRS`), a link to its home in its work (`home_dir`).
///   Taken for a host, every agent's would be the gateway's own,
///   `/data/hermes`: one `~` for all the computer's agents, among Hermes'
///   own files (its `.env`, `config.yaml`).
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

/// An agent's work directory: its terminal's cwd, its home (`home_dir`),
/// its temp files (`tmp_dir`) and its browser's profile (`BROWSER_PROFILE`).
pub fn work_dir(agent_fragment: &str) -> PathBuf {
    Path::new(WORK).join(wire::profile(agent_fragment))
}

/// An agent's home: its terminal's `HOME` and its file tools' `~`, which
/// Hermes takes to be its profile's `home` (`TERMINAL_HOME_MODE=profile`,
/// `RUNTIME_ENV`), there a link to this. What its tools write under `~` (a
/// browser's default profile and its databases, a CLI's login) is its work,
/// saved with it, never Hermes' home (Paul, 2026-10-07).
pub fn home_dir(agent_fragment: &str) -> PathBuf {
    work_dir(agent_fragment).join("home")
}

/// An agent's temp files: what Hermes points `TMPDIR`, `TMP` and
/// `TEMP` at for each process it runs with the profile's home (its
/// terminal's commands, foreground or background), its profile's
/// `PROFILE_SCRATCH`, there a link to this. So a tool's temp file is its
/// work, saved with it, never Hermes' home.
///
/// Why a link, and not a `TMPDIR` of the image's: Hermes derives each
/// child's temp directory from the home it runs it under
/// (`hermes_constants.apply_subprocess_home_env`, then
/// `apply_scratch_tmp_env`, re-pointing a value it set itself, known by its
/// `HERMES_SCRATCH_DIR` marker), so its own path is the one place every one
/// of its ways of starting a command reads. A profile cannot name one:
/// `terminal.env_passthrough` reads no `TMPDIR` from a profile's `.env`
/// (Hermes keeps it process-wide: `agent/secret_scope.py`,
/// `_GLOBAL_ENV_EXACT`), and an export in `shell_init_files` reaches only
/// the terminal's snapshot, never a background process (a `bash -lic` of
/// the gateway's environment). And the gateway's own `TMPDIR` stays
/// Hermes': one Hermes did not set is passed to every child as it is, so
/// all agents would share it. One way misses: execute_code's scripts get
/// the gateway's own (docs/technical-debt-ledger.md, "An agent's
/// execute_code makes its temp files in Hermes' home").
pub fn tmp_dir(agent_fragment: &str) -> PathBuf {
    work_dir(agent_fragment).join("tmp")
}

/// A profile's `home`, as Hermes names it (one of `PROFILE_DIRS`).
pub const PROFILE_HOME: &str = "home";

/// A profile's scratch directory, relative to it, as Hermes names it
/// (`hermes_constants.get_scratch_dir`: `<home>/cache/scratch`).
pub const PROFILE_SCRATCH: &str = "cache/scratch";

/// What a profile's `home` or scratch is set aside as (after its own name)
/// when it has something in it from before it was the agent's work: a hard
/// cut, so nothing of it is carried over, and nothing deleted.
pub const SET_ASIDE: &str = ".before-work";

/// What `link_into_work` found and did.
#[derive(Debug, PartialEq, Eq)]
pub enum WorkLink {
    /// Already the link.
    Kept,
    /// Linked now: a fresh profile, an empty directory, or a link elsewhere.
    Linked,
    /// A directory with something in it, set aside unmoved (`SET_ASIDE`
    /// after its name, numbered when that is taken), then linked.
    SetAside(PathBuf),
}

/// Makes `profile`'s `at` (`PROFILE_HOME`, `PROFILE_SCRATCH`) a link to
/// `to` (made if missing, as is the link's parent), whatever it was. The
/// caller gives them to the agent's user.
pub fn link_into_work(profile: &Path, at: &str, to: &Path) -> std::io::Result<WorkLink> {
    std::fs::create_dir_all(to)?;
    let link = profile.join(at);
    if let Some(parent) = link.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let found = match std::fs::symlink_metadata(&link) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => WorkLink::Linked,
        Err(e) => return Err(e),
        Ok(m) if m.file_type().is_symlink() => {
            if std::fs::read_link(&link)? == to {
                return Ok(WorkLink::Kept);
            }
            std::fs::remove_file(&link)?;
            WorkLink::Linked
        }
        Ok(m) if m.is_dir() && std::fs::read_dir(&link)?.next().is_none() => {
            std::fs::remove_dir(&link)?;
            WorkLink::Linked
        }
        Ok(_) => {
            // bounded: 100 names
            let aside = (0..100)
                .map(|n| profile.join(if n == 0 { format!("{at}{SET_ASIDE}") } else { format!("{at}{SET_ASIDE}-{n}") }))
                .find(|p| std::fs::symlink_metadata(p).is_err())
                .ok_or_else(|| std::io::Error::other(format!("no name left to set {} aside as", link.display())))?;
            std::fs::rename(&link, &aside)?;
            WorkLink::SetAside(aside)
        }
    };
    std::os::unix::fs::symlink(to, &link)?;
    Ok(found)
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
    format!("# Written by hermes-boot: the gateway's own profile runs no agent's turns.\nmodel:\n  provider: \"custom\"\n  base_url: {}\n  default: {}\n  api_key: \"fragment-model\"\n  context_length: 262144\n", q(&format!("{base}/v1")), q(DEFAULT_TIER.name()))
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
    // HERMES_GATEWAY_MAX_STARTS: 0, so Hermes' respawn-storm breaker is off.
    // It counts gateway starts in the home, which a save keeps, so six wakes
    // in two minutes would sleep the seventh 10-40 s before it answers; the
    // Computer DO already paces a computer's restarts.
    // HERMES_STREAM_RETRIES: `STREAM_RETRIES`, so a stalled call reaches its
    // fallback after two stale timeouts (`profile_config`).
    format!("GATEWAY_RELAY_URL=http://{listen}\nGATEWAY_RELAY_ID={gateway_id}\nGATEWAY_RELAY_SECRET={secret}\nHERMES_GATEWAY_BUSY_INPUT_MODE=queue\nHERMES_GATEWAY_NO_SUPERVISE=1\nGATEWAY_MULTIPLEX_PROFILES=true\nRELAY_HOME_CHANNEL=none\nHERMES_AUTO_CONTINUE_FRESHNESS=1\nHERMES_GATEWAY_MAX_STARTS=0\nHERMES_STREAM_RETRIES={STREAM_RETRIES}\n")
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
        Agent { fragment: "juniper--k3x9".into(), identity: "npub1j".into(), name: "Juniper".into(), owner: "npub1paul".into(), credentials: vec![] }
    }

    #[test]
    fn tiers_read_and_fall_back() {
        assert_eq!(Tier::of(Some(br#"{"tier":"cheap"}"#), false), Tier::Cheap);
        assert_eq!(Tier::of(Some(br#"{"tier":"medium"}"#), false), Tier::Medium, "medium stays selectable");
        assert_eq!(Tier::of(Some(br#"{"tier":"high"}"#), false), Tier::Cheap, "the high tier is off until its limit is raised");
        assert_eq!(Tier::of(Some(br#"{"tier":"high"}"#), true), Tier::High);
        for setting in [None, Some(b"not json".as_slice()), Some(b"{}".as_slice()), Some(br#"{"tier":"unknown"}"#.as_slice())] {
            assert_eq!(Tier::of(setting, true), Tier::Cheap, "{setting:?}");
        }
    }

    /// Goal: a new agent without settings runs on Flash. Method: read no
    /// agent.json and check the config Hermes consumes at its first turn.
    #[test]
    fn a_new_agents_profile_defaults_to_cheap() {
        let tier = Tier::of(None, false);
        let config = profile_config(&agent(), &tier.into(), "http://model.fragment.internal", &[], Path::new("/c.sh"));
        assert!(config.contains("  default: \"cheap\"\n"), "{config}");
    }

    /// An `agent.json` the platform answered for is a tier (none there is
    /// the cheap tier); one it did not answer for is none, never the
    /// default tier in its place.
    #[test]
    fn a_tier_unread_is_no_tier() {
        use fragment_bridge::api::ApiError;
        let refused = |status| Err(ApiError::Refused { status, error: String::new(), message: String::new() });
        assert_eq!(Tier::read(&Ok(bytes::Bytes::from_static(br#"{"tier":"cheap"}"#)), false), Some(Tier::Cheap));
        assert_eq!(Tier::read(&refused(404), false), Some(Tier::Cheap), "no agent.json");
        assert_eq!(Tier::read(&refused(403), false), Some(Tier::Cheap));
        for unread in [refused(500), refused(502), Err(ApiError::Transport("no answer in 15000 ms".into())), Err(ApiError::TooLarge)] {
            assert_eq!(Tier::read(&unread, false), None, "{unread:?}");
        }
    }

    #[test]
    fn configs_say_what_hermes_needs() {
        let m = managed_config(&["platforms/discord".into(), "dashboard_auth/basic".into()], APPROVAL_TIMEOUT_S, crate::desktop::IDLE_STOP_MS);
        for want in [
            "transport: \"draft\"",
            "busy_input_mode: \"queue\"",
            "group_sessions_per_user: false",
            "disabled_toolsets: [\"cronjob\"]",
            "mode: \"smart\"",
            "    - \"platforms/discord\"",
            "bot_desktop:\n  auto_start: true\n  idle_stop_minutes: 10\n",
            "\nmodel_catalog:\n  enabled: false\n",
            "\nsecurity:\n  allow_lazy_installs: false\n",
            "\nstt:\n  echo_transcripts: false\n",
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
        assert!(managed_config(&[], 20, crate::desktop::IDLE_STOP_MS).contains("timeout: 20\n"));
        assert!(managed_config(&[], 20, 30_000).contains("  idle_stop_minutes: 0.5\n"), "a test's shorter idle bound, in Hermes' minutes");
        let creds = Path::new("/data/hermes/profiles/juniper--k3x9/credentials.sh");
        let p = profile_config(&agent(), &Tier::Medium.into(), "http://model.fragment.internal/", &[], creds);
        assert!(p.contains("base_url: \"http://model.fragment.internal/v1\""), "{p}");
        assert!(p.contains("default: \"medium\""));
        assert!(p.contains("x-fragment-agent: \"juniper--k3x9\""), "every model call names its agent");
        let h = profile_config(&agent(), &Tier::High.into(), "http://model.fragment.internal", &[], creds);
        assert!(h.contains("provider: \"anthropic\"") && h.contains("/anthropic\""), "{h}");
        // its eyes: the route's vision model, OpenAI's shape, whatever its tier
        let vision = "  vision:\n    provider: \"custom\"\n    base_url: \"http://model.fragment.internal/v1\"\n    model: \"vision\"\n    api_key: \"fragment-model\"\n";
        for (tier, config) in [("medium", &p), ("high", &h), ("cheap", &profile_config(&agent(), &Tier::Cheap.into(), "http://model.fragment.internal", &[], creds))] {
            assert!(config.contains(vision), "the {tier} tier's screenshots go to the route's vision model: {config}");
            assert!(config.contains("plugins:\n  enabled: [fragment-skill-fork]\nauxiliary:\n  background_review: { enabled: true }\n"), "the {tier} tier reviews memory and forks external skills before writes: {config}");
            assert!(!config.contains("curator:"), "the separate periodic curator keeps Hermes' defaults: {config}");
        }
        assert!(!m.contains("auxiliary:"), "each profile's own, beside the headers that name its agent: {m}");
        let ears = "stt:\n  provider: \"openai\"\n  language: \"\"\n  openai:\n    base_url: \"http://model.fragment.internal/v1\"\n    api_key: \"agent:juniper--k3x9\"\n    model: \"whisper\"\n";
        for (tier, config) in [("medium", &p), ("high", &h)] {
            assert!(config.contains(ears), "the {tier} tier's voice memos go to the route's whisper, its key naming the agent, no language forced: {config}");
        }
        assert!(!m.contains("api_key") && !m.contains("openai"), "the overlay names no agent, so a call outside a profile names none either: {m}");
        // a text tier's stall: Hermes' own stale detector, one retry, then the route's `fallback` as the agent
        let fallback = "fallback_providers:\n  - provider: \"custom\"\n    base_url: \"http://model.fragment.internal/v1\"\n    model: \"fallback\"\n    api_key: \"agent:juniper--k3x9\"\n";
        let c = profile_config(&agent(), &Tier::Cheap.into(), "http://model.fragment.internal", &[], creds);
        for (tier, config) in [("cheap", &c), ("medium", &p)] {
            assert!(config.contains(fallback), "the {tier} tier falls back to the route's fallback model: {config}");
            assert!(config.contains(&format!("providers:\n  custom:\n    models:\n      \"{tier}\":\n        stale_timeout_seconds: 20\n")), "its own model's stale timeout alone: {config}");
            assert!(config.contains("agent:\n  api_max_retries: 1\n"), "{config}");
            assert!(!config.contains("\"vision\":\n        stale_timeout_seconds"), "vision keeps Hermes' default: {config}");
        }
        assert!(!h.contains("fallback_providers") && !h.contains("stale_timeout_seconds") && !h.contains("api_max_retries"), "the high tier has none: {h}");
        assert!(!m.contains("fallback_providers") && !m.contains("stale_timeout_seconds") && !m.contains("api_max_retries"), "a profile's own, never the overlay's: {m}");
        assert!(p.contains("skills:\n  external_dirs: [\"/data/hermes/managed-skills\", \"/var/lib/fragment-run/platform-skills\"]\n"), "the managed skills and the platform skill's view, after its own: {p}");
        assert!(p.contains("browser:\n  headed: true\n  backend: \"off\"\n"), "Hermes' built-in browser, headed, in the profile's own config: {p}");
        assert!(!m.contains("browser:"), "Hermes never reads `browser` from the managed overlay: {m}");
        assert!(p.contains("terminal:\n  env_passthrough: [\"FRAGMENT_AS_AGENT\", \"FRAGMENT_FOR\"]\n"), "its terminal acts as the agent: {p}");
        assert!(p.contains("\n  cwd: \"/data/work/juniper--k3x9\"\n"), "its terminal works in its work directory: {p}");
        assert_eq!(work_dir("juniper--k3x9"), PathBuf::from("/data/work/juniper--k3x9"));
        assert!(
            p.contains("  shell_init_files: [\"~/.profile\", \"~/.bash_profile\", \"~/.bashrc\", \"/data/hermes/profiles/juniper--k3x9/credentials.sh\"]\n"),
            "its shell starts as Hermes' does, then reads its credentials: {p}"
        );
        let e = profile_env(&agent());
        assert!(e.contains("\nFRAGMENT_AS_AGENT=juniper--k3x9\n") && e.contains("\nFRAGMENT_FOR=npub1paul\n"), "{e}");
        assert!(!e.contains("KEY") && !e.contains("TOKEN"), "no credential, and none held: {e}");
        let env = gateway_env("127.0.0.1:8650", "computer", &"s".repeat(32));
        assert!(env.contains("GATEWAY_RELAY_URL=http://127.0.0.1:8650\n"));
        assert!(env.contains("HERMES_GATEWAY_BUSY_INPUT_MODE=queue"));
        assert!(env.contains("HERMES_AUTO_CONTINUE_FRESHNESS=1\n"), "a turn a restart cut short is never auto-continued");
        assert!(env.contains("HERMES_GATEWAY_MAX_STARTS=0\n"), "no start is slept for the starts before it");
        assert!(env.contains("HERMES_STREAM_RETRIES=1\n"), "a stalled stream is tried once more, then the fallback");
        assert_eq!(profile_dir(Path::new("/data/hermes"), "juniper--k3x9"), PathBuf::from("/data/hermes/profiles/juniper--k3x9"));
    }

    /// Goal (Paul on p5, 2026-10-09): an agent's own desktop never asks its
    /// owner, and its terminal's dangerous commands still do. Method: every
    /// profile's config, on every model, holds each desktop action granted
    /// in each delivery mode, and nothing that would pass a terminal
    /// command; the overlay keeps the smart guardian for those. (Run against
    /// the real Hermes in the bridge's tests/docker.rs: a scroll and a focus
    /// change with no card, and `rm -rf` still a card.)
    #[test]
    fn an_agents_own_desktop_never_asks() {
        let grants = desktop_grants();
        assert_eq!(grants.len(), DESKTOP_ACTIONS.len() * DELIVERY_MODES.len());
        for action in ["scroll", "focus_app", "click", "type", "key", "bring_to_front"] {
            for mode in ["background", "foreground"] {
                assert!(grants.contains(&format!("cua:{action}:{mode}")), "{action} in {mode}: {grants:?}");
            }
        }
        // only computer_use's keys: no terminal pattern's, no command, no glob
        for g in &grants {
            assert!(g.starts_with("cua:") && !g.contains(['*', '?', '[', ' ']), "{g}");
        }
        // the actions that read, never asked, are not granted
        for read in ["capture", "wait", "list_apps", "list_windows"] {
            assert!(!DESKTOP_ACTIONS.contains(&read), "{read}");
        }
        let line = format!("command_allowlist: [{}]\n", grants.iter().map(|g| format!("\"{g}\"")).collect::<Vec<_>>().join(", "));
        assert!(line.starts_with("command_allowlist: [\"cua:click:background\", \"cua:click:foreground\", \"cua:double_click:background\""), "{line}");
        let creds = Path::new("/c.sh");
        let mut own = agent();
        own.credentials.push(fragment_bridge::runtime::Credential { model_base: Some("https://openrouter.ai/api/v1".into()), ..credential("openrouter", &[], "fcx_openrouter_a1") });
        let (own_main, _) = MainModel::of(&own, Tier::Cheap, Some(&OwnModel { provider: "openrouter".into(), id: "anthropic/claude-sonnet-5.5".into() }));
        assert!(matches!(own_main, MainModel::Own { .. }));
        for (main, a) in [(Tier::Cheap.into(), agent()), (Tier::Medium.into(), agent()), (Tier::High.into(), agent()), (own_main, own)] {
            let p = profile_config(&a, &main, "http://model.fragment.internal", &[], creds);
            assert_eq!(p.matches("command_allowlist:").count(), 1, "{p}");
            assert!(p.contains(&format!("\n{line}")), "a top-level key of the {} profile: {p}", main.name());
            let falls_back = matches!(main, MainModel::Tier(Tier::Cheap | Tier::Medium));
            assert_eq!(p.contains("fallback_providers:"), falls_back, "only a text tier falls back to the route's fallback (an owner's own model is theirs): {p}");
        }
        let m = managed_config(&[], APPROVAL_TIMEOUT_S, crate::desktop::IDLE_STOP_MS);
        assert!(m.contains("approvals:\n  mode: \"smart\"\n"), "a flagged command still goes to the guardian, then a person: {m}");
        assert!(!m.contains("command_allowlist") && !m.contains("yolo") && !m.contains("mode: \"off\""), "nothing passes the terminal's commands: {m}");
        assert!(!default_config("http://model.fragment.internal").contains("command_allowlist"), "the gateway's own profile runs no turns");
    }

    /// The image's Chromium starts Hermes' pinned one with the container's
    /// flags, on a desktop or with none: run here by `sh`, as Hermes runs it.
    #[test]
    fn the_images_chromium_carries_the_containers_flags() {
        let s = chromium_script(Path::new("/opt/hermes/tools/chromium-1208/chrome-linux64/chrome"));
        assert!(s.starts_with("#!/bin/sh\n"), "{s}");
        assert_eq!(CHROMIUM_FLAGS, ["--no-sandbox", "--disable-dev-shm-usage"], "Hermes' CHROMIUM_SANDBOX_BYPASS_ARGS");
        let dir = std::env::temp_dir().join(format!("hermes-chromium-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // the browser, saying what it was started with, and its scratch
        let full = dir.join("full");
        std::fs::write(&full, "#!/bin/sh\necho full \"$@\" \"tmp=$TMPDIR\"\n").unwrap();
        std::fs::set_permissions(&full, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        let script = dir.join("chromium");
        std::fs::write(&script, chromium_script(&full)).unwrap();
        let run = |display: Option<&str>| {
            let mut c = std::process::Command::new("sh");
            // Hermes' scratch, under its home, as its terminal has it
            c.arg(&script).args(["--user-data-dir=/p", "https://example.com"]).env_remove("DISPLAY").env("TMPDIR", "/data/hermes/profiles/p/cache/scratch");
            if let Some(d) = display {
                c.env("DISPLAY", d);
            }
            String::from_utf8(c.output().unwrap().stdout).unwrap()
        };
        let started = "full --no-sandbox --disable-dev-shm-usage --user-data-dir=/p https://example.com tmp=/tmp\n";
        assert_eq!(run(Some(":20")), started, "on a desktop");
        assert_eq!(run(None), started, "with none: its caller asks for headless");
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
        let creds = Path::new("/data/hermes/profiles/juniper--k3x9/credentials.sh");
        let written = [managed_config(&["platforms/discord".into()], APPROVAL_TIMEOUT_S, crate::desktop::IDLE_STOP_MS), default_config("http://m"), profile_config(&agent(), &Tier::Medium.into(), "http://m", &[], creds), gateway_env("127.0.0.1:1", "c", &"s".repeat(32))];
        for (name, _) in RUNTIME_ENV {
            let key = name.strip_prefix("TERMINAL_").unwrap_or(name).to_ascii_lowercase();
            assert!(written.iter().all(|w| !w.contains(name) && !w.contains(&format!("{key}:"))), "{name} is the boot's environment's alone: {written:#?}");
        }
    }

    /// An agent's home and its temp files are in its work, and its
    /// profile's `home` and scratch the links to them: made for a fresh
    /// profile (the scratch's `cache` with it); kept when each is the link
    /// (each boot); an empty one or a link elsewhere replaced; one with
    /// something in it set aside whole, and nothing of it carried over.
    #[test]
    fn a_profiles_home_and_scratch_are_links_into_its_work() {
        assert_eq!(home_dir("juniper--k3x9"), PathBuf::from("/data/work/juniper--k3x9/home"));
        assert_eq!(tmp_dir("juniper--k3x9"), PathBuf::from("/data/work/juniper--k3x9/tmp"));
        assert!(PROFILE_DIRS.contains(&PROFILE_HOME), "Hermes' name for it: {PROFILE_DIRS:?}");
        assert_eq!(format!("{PROFILE_HOME}{SET_ASIDE}"), "home.before-work", "the name a home from before was set aside as since 2026-10-07");
        let root = std::env::temp_dir().join(format!("hermes-work-links-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let profile = root.join("hermes/profiles/juniper--k3x9");
        std::fs::create_dir_all(&profile).unwrap();
        let linked = |l: &Path| std::fs::read_link(l).ok();
        for (at, to) in [(PROFILE_HOME, root.join("work/juniper--k3x9/home")), (PROFILE_SCRATCH, root.join("work/juniper--k3x9/tmp"))] {
            let link = profile.join(at);
            // valid: a fresh profile, its directory in the work made
            assert_eq!(link_into_work(&profile, at, &to).unwrap(), WorkLink::Linked, "{at}");
            assert_eq!(linked(&link), Some(to.clone()));
            assert!(to.is_dir());
            std::fs::write(to.join("kept"), "x").unwrap();
            // replay: the next boot keeps it, and what is in it
            assert_eq!(link_into_work(&profile, at, &to).unwrap(), WorkLink::Kept, "{at}");
            assert!(link.join("kept").is_file(), "written through the link, kept in the work");
            // a link elsewhere is pointed at the work
            std::fs::remove_file(&link).unwrap();
            std::os::unix::fs::symlink(root.join("elsewhere"), &link).unwrap();
            assert_eq!(link_into_work(&profile, at, &to).unwrap(), WorkLink::Linked, "{at}");
            assert_eq!(linked(&link), Some(to.clone()));
            // an empty one (Hermes' own, made before the link) goes
            std::fs::remove_file(&link).unwrap();
            std::fs::create_dir(&link).unwrap();
            assert_eq!(link_into_work(&profile, at, &to).unwrap(), WorkLink::Linked, "{at}");
            assert!(!profile.join(format!("{at}{SET_ASIDE}")).exists(), "nothing to set aside");
            // one from before, with files: set aside unmoved, twice numbered
            for (n, aside) in [format!("{at}{SET_ASIDE}"), format!("{at}{SET_ASIDE}-1")].iter().enumerate() {
                std::fs::remove_file(&link).unwrap();
                std::fs::create_dir_all(link.join("from-before")).unwrap();
                std::fs::write(link.join("from-before/file"), format!("{n}")).unwrap();
                assert_eq!(link_into_work(&profile, at, &to).unwrap(), WorkLink::SetAside(profile.join(aside)), "{at}");
                assert_eq!(std::fs::read_to_string(profile.join(aside).join("from-before/file")).unwrap(), format!("{n}"), "set aside whole");
                assert_eq!(linked(&link), Some(to.clone()));
                assert!(!to.join("from-before").exists(), "nothing of it carried into the work");
            }
        }
        assert!(profile.join("cache").is_dir() && !profile.join("cache").is_symlink(), "the scratch's parent is Hermes' own");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    #[should_panic(expected = "a browser's path, absolute, quotable")]
    fn a_browser_path_a_shell_would_misread_is_a_bug() {
        chromium_script(Path::new("/opt/it's/chrome"));
    }

    fn credential(provider: &str, env: &[&str], placeholder: &str) -> fragment_bridge::runtime::Credential {
        fragment_bridge::runtime::Credential {
            provider: provider.into(),
            kind: "operator".into(),
            env: env.iter().map(|e| e.to_string()).collect(),
            placeholder: placeholder.into(),
            hosts: vec!["api.perplexity.ai".into()],
            model_base: None,
        }
    }

    /// Goal: only an agent's currently held placeholders select paid tool
    /// backends. Method: each category offered, removed, and merely named
    /// in credential_env; another profile remains on Hermes' defaults.
    #[test]
    fn tool_categories_select_only_offered_credentials() {
        for (provider, env, selection) in [
            ("firecrawl", "FIRECRAWL_API_KEY", "web:\n  backend: \"firecrawl\"\n  search_backend: \"firecrawl\"\n  extract_backend: \"firecrawl\"\n"),
            ("fal", "FAL_KEY", "image_gen:\n  provider: \"fal\"\n  model: \"fal-ai/flux-2/klein/9b\"\n"),
            ("elevenlabs", "ELEVENLABS_API_KEY", "tts:\n  provider: \"elevenlabs\"\n"),
            ("browser-use", "BROWSER_USE_API_KEY", "  cloud_provider: \"local\"\n"),
            ("perplexity", "PERPLEXITY_API_KEY", "web:\n  search_backend: \"perplexity\"\n"),
        ] {
            let mut a = agent();
            let write = |a: &Agent| profile_config(a, &Tier::Cheap.into(), "http://m", &[env.into()], Path::new("/c.sh"));
            let absent = write(&a);
            assert!(!absent.contains(selection), "catalog alone never selects {provider}");
            a.credentials.push(credential(provider, &[env], &format!("fck_{provider}_{}", "a".repeat(32))));
            let present = write(&a);
            assert!(present.contains(selection), "offered {provider}: {present}");
            assert!(present.contains("browser:\n  headed: true\n  backend: \"off\"\n"));
            a.credentials.clear();
            assert_eq!(write(&a), absent, "revocation restores {provider}'s defaults");
        }
        let mut a = agent();
        for (provider, env) in [("perplexity", "PERPLEXITY_API_KEY"), ("firecrawl", "FIRECRAWL_API_KEY"), ("xai", "XAI_API_KEY")] {
            a.credentials.push(credential(provider, &[env], &format!("fck_{provider}_{}", "a".repeat(32))));
        }
        let config = profile_config(&a, &Tier::Cheap.into(), "http://m", &[], Path::new("/c.sh"));
        assert!(config.contains("  search_backend: \"firecrawl\"\n"), "Firecrawl wins over Perplexity");
        assert!(!config.contains("\"xai\""), "X search is a separate native tool, never a web backend");
        assert!(profile_env(&a).contains("XAI_API_KEY=fck_xai_"));
        assert!(profile_env(&a).contains("PERPLEXITY_API_KEY=fck_perplexity_"));
    }

    /// Valid: an agent.json that names a model of a provider its owner
    /// connected runs on it: the provider's base, the model's id, the
    /// provider's placeholder as its key (the swap adds the key), and its
    /// eyes and ears stay on the route. Invalid: a model named badly, or of
    /// a provider its credentials do not hold (not connected, narrowed) or
    /// that serves none, is its tier, saying why.
    #[test]
    fn an_agent_runs_on_its_owners_model_when_it_holds_it() {
        let tag = "0123456789abcdef0123456789abcdef";
        let json = br#"{"tier":"cheap","model":{"provider":"openrouter","id":"anthropic/claude-sonnet-5.5"},"color":"blue"}"#;
        let own = OwnModel::of(json).unwrap();
        assert_eq!(own, OwnModel { provider: "openrouter".into(), id: "anthropic/claude-sonnet-5.5".into() });
        for bad in [&br#"{"model":{"provider":"OpenRouter","id":"x"}}"#[..], br#"{"model":{"provider":"openrouter","id":"a b"}}"#, br#"{"model":{"provider":"openrouter"}}"#, br#"{"model":"openrouter:x"}"#, b"{}", b"not json"] {
            assert_eq!(OwnModel::of(bad), None, "{}", String::from_utf8_lossy(bad));
        }
        let mut a = agent();
        let (main, why) = MainModel::of(&a, Tier::Cheap, Some(&own));
        assert_eq!(main, MainModel::Tier(Tier::Cheap));
        assert!(why.unwrap().contains("not connected openrouter"));
        let mut c = credential("openrouter", &["OPENROUTER_API_KEY"], &format!("fck_openrouter_{tag}"));
        a.credentials = vec![c.clone()];
        let (main, why) = MainModel::of(&a, Tier::Cheap, Some(&own));
        assert!(main == MainModel::Tier(Tier::Cheap) && why.unwrap().contains("serves no models"), "a provider with no model base");
        c.model_base = Some("https://openrouter.ai/api/v1/".into());
        a.credentials = vec![c];
        let (main, why) = MainModel::of(&a, Tier::Medium, Some(&own));
        assert_eq!(why, None);
        assert_eq!(main.name(), "openrouter:anthropic/claude-sonnet-5.5");
        assert_eq!(MainModel::of(&a, Tier::Medium, None), (MainModel::Tier(Tier::Medium), None), "none named: its tier");
        let p = profile_config(&a, &main, "http://model.fragment.internal", &["OPENROUTER_API_KEY".into()], Path::new("/c.sh"));
        let want = format!("model:\n  provider: \"custom\"\n  base_url: \"https://openrouter.ai/api/v1\"\n  default: \"anthropic/claude-sonnet-5.5\"\n  api_key: \"fck_openrouter_{tag}\"\n  default_headers:\n    x-fragment-agent: \"juniper--k3x9\"\n");
        assert!(p.contains(&want), "{p}");
        assert!(!p.contains("context_length"), "its provider says its context: {p}");
        assert!(p.contains("    base_url: \"http://model.fragment.internal/v1\"\n    model: \"vision\"\n"), "its eyes stay the route's: {p}");
        assert!(p.contains("    model: \"whisper\"\n"), "its ears too: {p}");
    }

    /// Valid: each credential's placeholder is in its environment
    /// variables, in the profile's `.env` (for Hermes and its tools) and in
    /// the terminal's credentials file; sandbox passthrough keeps allowed
    /// names and leaves Hermes' provider credentials out.
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
        let p = profile_config(&a, &Tier::Medium.into(), "http://model.fragment.internal", &env, Path::new("/c.sh"));
        assert!(p.contains("env_passthrough: [\"FRAGMENT_AS_AGENT\", \"FRAGMENT_FOR\", \"GOOGLE_OAUTH_ACCESS_TOKEN\"]"), "{p}");
    }

    /// Every pinned provider name, plus dynamic internal secrets, is
    /// refused; the skills' Google and X API placeholders still pass.
    #[test]
    fn no_blocklisted_name_is_in_profile_passthrough() {
        let mut env: Vec<String> = PROVIDER_ENV_BLOCKLIST.lines().map(str::to_string).collect();
        assert!(env.len() > 300, "the full pinned policy, not just today's offered providers");
        env.extend(["AUXILIARY_TEST_API_KEY", "AUXILIARY_TEST_BASE_URL", "GATEWAY_RELAY_TEST_TOKEN", "GATEWAY_RELAY_TEST_SECRET", "GATEWAY_RELAY_TEST_KEY", "GOOGLE_OAUTH_ACCESS_TOKEN", "X_API_BEARER_TOKEN"].map(str::to_string));
        let p = profile_config(&agent(), &Tier::Medium.into(), "http://model.fragment.internal", &env, Path::new("/c.sh"));
        let passed = p.lines().find(|l| l.starts_with("  env_passthrough:")).unwrap();
        assert_eq!(passed, "  env_passthrough: [\"FRAGMENT_AS_AGENT\", \"FRAGMENT_FOR\", \"GOOGLE_OAUTH_ACCESS_TOKEN\", \"X_API_BEARER_TOKEN\"]");
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
        let p = profile_config(&a, &Tier::Medium.into(), "http://m", &["FRAGMENT_FOR".into(), "HERMES_HOME".into(), "A_KEY".into()], Path::new("/c.sh"));
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
        let ok = br#"{"ok": true, "protocol": 1, "result": {"multiplex": true, "added": ["maple--k3x9"], "served_profiles": ["default", "maple--k3x9"]}, "id": 1}"#;
        assert_eq!(control_answer(ok).unwrap()["added"][0], "maple--k3x9");
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
        a.owner = "npub1paul\nOPENAI_API_KEY=x".into();
        profile_env(&a);
    }

    #[test]
    #[should_panic(expected = "a Relay secret of 32 characters or more")]
    fn a_short_secret_is_refused() {
        gateway_env("127.0.0.1:1", "c", "short");
    }
}
