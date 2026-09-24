//! The wire contract between the fragment cell, the CLI, and the e2e.
//!
//! Every request body, response body, error code, and limit that crosses
//! the HTTP boundary is defined here once. The cell compiles this crate to
//! wasm32, so it holds data and pure functions only.

use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use serde_json::Value;

/// Limits every side enforces the same way (docs/MODEL.md, Limits).
pub mod limits {
    /// A label: one part of a name (a fragment's label, a username, an
    /// agent's name), `^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?$` with no `--`.
    pub const NAME_MAX_BYTES: usize = 63;
    /// A username: a label of 3 to 32 bytes, not a reserved word.
    pub const USERNAME_MIN_BYTES: usize = 3;
    pub const USERNAME_MAX_BYTES: usize = 32;
    /// A profile picture (PNG, JPEG, WebP, or GIF).
    pub const PICTURE_MAX_BYTES: usize = 256 * 1024;
    /// An operation id: `^[A-Za-z0-9._:-]{1,128}$`.
    pub const OP_ID_MAX_BYTES: usize = 128;
    /// An operation name: `^[a-z][a-z0-9_]{0,63}$`.
    pub const OP_NAME_MAX_BYTES: usize = 64;
    /// Operation input is a request, not an upload.
    pub const INPUT_MAX_BYTES: usize = 256 * 1024;
    /// Operation results must fit a Workflows step result.
    pub const RESULT_MAX_BYTES: usize = 1024 * 1024;
    /// The app's source (`app.mjs` at the live pin).
    pub const SOURCE_MAX_BYTES: usize = 1024 * 1024;
    /// `fragment.json` at a pin.
    pub const MANIFEST_MAX_BYTES: usize = 256 * 1024;
    /// Declared operations per fragment.
    pub const OPERATIONS_MAX: usize = 256;
    /// CPU per call into author code.
    pub const APP_CPU_MS: u32 = 30_000;
    /// Subrequests per call into author code.
    pub const APP_SUBREQUESTS: u32 = 50;
    /// A NIP-98 event's `created_at` may differ from the cell's clock by this much.
    pub const AUTH_WINDOW_S: i64 = 60;
    /// Request bodies the router reads (to hash for NIP-98).
    pub const BODY_MAX_BYTES: usize = 2 * 1024 * 1024;
    /// A secret's value.
    pub const SECRET_MAX_BYTES: usize = 64 * 1024;
    /// The app facet's database (docs/MODEL.md): a mutation that leaves it
    /// larger rolls back (the platform checks). The node stops every write
    /// 4 MiB above it (`CELLD_FACET_MAX_BYTES`, our celld fork).
    pub const APP_DB_MAX_BYTES: u64 = 16 * 1024 * 1024;
    /// Secrets per fragment.
    pub const SECRETS_MAX: usize = 100;
    /// Members per fragment.
    pub const MEMBERS_MAX: usize = 1000;
    /// Open invites per fragment.
    pub const INVITES_MAX: usize = 100;
    /// People one invite may admit.
    pub const INVITE_USES_MAX: u32 = 1000;
    /// An invite's default and longest lifetime.
    pub const INVITE_TTL_DEFAULT_S: i64 = 7 * 24 * 3600;
    pub const INVITE_TTL_MAX_S: i64 = 30 * 24 * 3600;
    /// A storage token's lifetime.
    pub const STORAGE_TOKEN_TTL_S: i64 = 900;
    /// A file path in the repo.
    pub const PATH_MAX_BYTES: usize = 300;
    /// A code.storage webhook's timestamp may differ from the cell's clock by this much.
    pub const WEBHOOK_WINDOW_S: i64 = 300;
    /// Events per page of `GET events`.
    pub const EVENTS_PAGE: usize = 500;
    /// Records `events` and `ops` each keep (the oldest go first).
    pub const AUDIT_KEPT: i64 = 10_000;
    /// `public`-role calls per minute by one principal that is not a member.
    pub const PUBLIC_CALLS_PER_MIN: u32 = 60;
    /// `public`-role calls per minute into one fragment by non-members.
    pub const PUBLIC_CALLS_PER_MIN_FRAGMENT: u32 = 600;
    /// A channel record's body.
    pub const RECORD_BODY_MAX_BYTES: usize = 64 * 1024;
    /// Records per page of a channel read.
    pub const CHANNEL_PAGE: usize = 1000;
    /// A page of a channel on the live socket also ends at about this many
    /// bytes of record frames (a record is at most `RECORD_BODY_MAX_BYTES`).
    pub const CHANNEL_PAGE_MAX_BYTES: usize = 1024 * 1024;
    /// Records one mutation may publish.
    pub const EFFECTS_MAX: usize = 64;
    /// App channels per fragment.
    pub const CHANNELS_MAX: usize = 32;
    /// How long `events` and `ops` records are kept.
    pub const AUDIT_RETENTION_MS: i64 = 90 * 24 * 3600 * 1000;
    /// A socket's presence data.
    pub const PRESENCE_MAX_BYTES: usize = 4 * 1024;
    /// Modules an app may load besides `app.mjs` (`applib/`), and their total size.
    pub const APPLIB_FILES_MAX: usize = 64;
    pub const APP_MODULES_MAX_BYTES: usize = 4 * 1024 * 1024;
    /// Triggers per fragment.
    pub const TRIGGERS_MAX: usize = 32;
    /// How many hops a chain of triggered runs may take (a mutation's record
    /// triggering a run whose record triggers another …) before the next is
    /// blocked as a loop.
    pub const HOP_DEPTH_MAX: u32 = 16;
    /// Inbox records whose runs have not succeeded; past this a post is 429.
    pub const INBOX_PENDING_MAX: u64 = 1000;
    /// Steps one job run may take (a video waits in polls and sleeps).
    pub const JOB_STEPS_MAX: usize = 256;
    /// Every step result of one run together (the job re-reads them at each step).
    pub const JOB_RESULTS_MAX_BYTES: usize = 4 * 1024 * 1024;
    /// A job's `fetch`: its request body, its response body, and how long it may take.
    pub const FETCH_BODY_MAX_BYTES: usize = 256 * 1024;
    pub const FETCH_RESPONSE_MAX_BYTES: usize = 1024 * 1024;
    pub const FETCH_TIMEOUT_MS: u64 = 120_000;
    /// A triggered operation pauses itself after this many held runs within the window …
    pub const AUTO_PAUSE_HELD: u64 = 5;
    pub const AUTO_PAUSE_WINDOW_MS: i64 = 10 * 60 * 1000;
    /// … or after this many triggered runs within an hour.
    pub const TRIGGERED_RUNS_PER_HOUR: u64 = 120;
    /// Sign-ins begun and not yet finished that the platform keeps when the
    /// fleet does not say (`FRAGMENT_SIGNINS_PENDING_MAX`): each is good for
    /// ten minutes, so the oldest is let go only past about 166 starts a
    /// second, sustained.
    pub const SIGNINS_PENDING_MAX_DEFAULT: u64 = 100_000;
    /// The sessions one browser's platform session keeps on one fragment's
    /// origin (past this, the oldest go first).
    pub const SITE_SESSIONS_PER_FRAGMENT_MAX: u64 = 4;
    /// Unspent single-use redemptions (a fragment origin's way in) one
    /// platform session holds: past this, the oldest go first.
    pub const REDEMPTIONS_PER_SESSION_MAX: u64 = 16;
    /// Keys one identity has held (active and revoked), and agents one person owns.
    pub const KEYS_PER_IDENTITY_MAX: u64 = 64;
    pub const AGENTS_PER_OWNER_MAX: u64 = 100;
    /// A file an app reads (a larger one is served from the site).
    pub const FILE_READ_MAX_BYTES: usize = 1024 * 1024;
    /// What one mutation or one job step may write to files, and in how many.
    pub const FILE_WRITE_MAX_BYTES: usize = 256 * 1024;
    pub const FILE_WRITES_MAX: usize = 16;
    /// Push subscriptions a fragment holds.
    pub const PUSH_SUBS_MAX: u64 = 10_000;
    /// A push's `who`, the tag a page subscribed with, in characters
    /// (Unicode scalar values: `chars()` here, `[...who]` in JavaScript).
    pub const PUSH_WHO_MAX_CHARS: usize = 64;
    /// URLs `fragment.json`'s `notifyUrls` may name.
    pub const NOTIFY_URLS_MAX: usize = 3;
    /// The largest blob an upload may carry (files of 1 MiB or more are blobs).
    pub const BLOB_MAX_BYTES: u64 = 256 * 1024 * 1024;
    /// Finished runs are kept this long, and at most this many.
    pub const RUN_RETENTION_MS: i64 = 30 * 24 * 3600 * 1000;
    pub const RUNS_KEPT: i64 = 10_000;
}

/// A secret's name: `^[A-Z][A-Z0-9_]{0,63}$`.
pub fn valid_secret_name(name: &str) -> bool {
    let b = name.as_bytes();
    !b.is_empty() && b.len() <= 64 && b[0].is_ascii_uppercase() && b.iter().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || *c == b'_')
}

/// A repo path the platform reads: relative, no `.` or `..` segments, no
/// empty segments, no control bytes or backslashes, at most
/// `PATH_MAX_BYTES` bytes of UTF-8.
pub fn valid_repo_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= limits::PATH_MAX_BYTES
        && !path.starts_with('/')
        && path.split('/').all(|seg| !seg.is_empty() && seg != "." && seg != "..")
        && !path.bytes().any(|b| b < 0x20 || b == b'\\')
}

/// `valid_repo_path` for the app facet's in-app check (its `limits.js`,
/// `fragment_core::facet`), branch for branch: bytes are counted in UTF-8
/// as here, not in UTF-16 units, and a control byte of UTF-8 is a control
/// unit of UTF-16.
pub const VALID_REPO_PATH_JS: &str = r#"export function validRepoPath(path) {
  if (typeof path !== "string" || path.length === 0 || utf8Bytes(path) > PATH_MAX_BYTES || path.startsWith("/")) return false;
  if (!path.split("/").every((seg) => seg !== "" && seg !== "." && seg !== "..")) return false;
  return !/[\x00-\x1f\\]/.test(path);
}"#;

/// One part of a name, and one DNS label: a fragment's label, a username,
/// an agent's name. Never `--` (a repo's name joins two with it).
pub fn valid_label(label: &str) -> bool {
    let b = label.as_bytes();
    let alnum = |c: &u8| c.is_ascii_lowercase() || c.is_ascii_digit();
    !b.is_empty()
        && b.len() <= limits::NAME_MAX_BYTES
        && alnum(&b[0])
        && alnum(&b[b.len() - 1])
        && b.iter().all(|c| alnum(c) || *c == b'-')
        && !label.contains("--")
}

/// Words no one may take as a username: the platform's own hosts and paths.
pub const RESERVED_USERNAMES: [&str; 24] = [
    "www", "api", "app", "apps", "auth", "admin", "root", "system", "platform", "fragment", "fragments", "static", "assets",
    "cdn", "mail", "docs", "blog", "help", "support", "status", "new", "cli", "anonymous", "operator",
];

/// A username: chosen once, a label of 3 to 32 bytes, not reserved.
pub fn valid_username(username: &str) -> bool {
    (limits::USERNAME_MIN_BYTES..=limits::USERNAME_MAX_BYTES).contains(&username.len())
        && valid_label(username)
        && !RESERVED_USERNAMES.contains(&username)
}

/// A fragment's name: `<label>.<username>` (decision 16), served at
/// `<label>--<username>.<suffix>` ([`flat_name`]).
pub fn valid_fragment_name(name: &str) -> bool {
    split_fragment_name(name).is_some()
}

/// (label, username) of a fragment's name.
pub fn split_fragment_name(name: &str) -> Option<(&str, &str)> {
    let (label, username) = name.split_once('.')?;
    (valid_label(label) && valid_username(username)).then_some((label, username))
}

/// A fragment's name from its label and its owner's username.
pub fn fragment_name(label: &str, username: &str) -> String {
    format!("{label}.{username}")
}

/// A fragment's name as one DNS label, `<label>--<username>` (neither part
/// contains `--`): its host under the fleet's suffix, so one wildcard
/// certificate covers every fragment, and its code.storage repo (repo
/// names are global in the org).
pub fn flat_name(name: &str) -> Option<String> {
    let (label, username) = split_fragment_name(name)?;
    Some(format!("{label}--{username}"))
}

/// The fragment a flat name (`<label>--<username>`) names.
pub fn from_flat_name(flat: &str) -> Option<String> {
    let (label, username) = flat.split_once("--")?;
    (valid_label(label) && valid_username(username)).then(|| fragment_name(label, username))
}

pub fn valid_op_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= limits::OP_ID_MAX_BYTES
        && id.bytes().all(|c| c.is_ascii_alphanumeric() || b"._:-".contains(&c))
}

pub fn valid_op_name(name: &str) -> bool {
    let b = name.as_bytes();
    !b.is_empty()
        && b.len() <= limits::OP_NAME_MAX_BYTES
        && b[0].is_ascii_lowercase()
        && b.iter().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'_')
}

/// Names the App class answers for the runtime or the platform, never
/// operations: `fragment.json` may not declare one (a name that could not
/// match `valid_op_name` is listed too, since the facet's platform code
/// refuses to call any of them), and the facet never calls one as a
/// method (its `limits.js`). Methods beginning `__` are the platform's.
pub const RESERVED_OP_NAMES: [&str; 6] = ["constructor", "fetch", "alarm", "webSocketMessage", "webSocketClose", "webSocketError"];

/// A record's kind: `^[a-z][a-z0-9._-]{0,63}$`.
pub fn valid_kind(kind: &str) -> bool {
    let b = kind.as_bytes();
    !b.is_empty()
        && b.len() <= 64
        && b[0].is_ascii_lowercase()
        && b.iter().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b"._-".contains(c))
}

/// `valid_kind` for the app facet's in-app check (its `limits.js`): the
/// pattern is ASCII, so a string of UTF-16 units matches it exactly when
/// its UTF-8 bytes do.
pub const VALID_KIND_JS: &str = r#"export function validKind(kind) {
  return typeof kind === "string" && /^[a-z][a-z0-9._-]{0,63}$/.test(kind);
}"#;

/// Every refusal the platform answers with, as `{"error": code, "message"}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// 400: the request is malformed or breaks a rule.
    InvalidRequest,
    /// 401: no valid NIP-98 signature.
    Unauthenticated,
    /// 403: signed, but this principal may not do this.
    Forbidden,
    /// 404: no such fragment or route.
    NotFound,
    /// 404: the fragment has no operation by that name.
    UnknownOperation,
    /// 404: the fragment's live commit has no `app.mjs`.
    NoCode,
    /// 409: the fragment already exists.
    AlreadyExists,
    /// 409: this operation id was used with a different input.
    ConflictingBody,
    /// 413: a body, input, or result is over its limit.
    TooLarge,
    /// 422: the author's code refused or failed.
    AppFailed,
    /// 429: too many calls; retry later.
    RateLimited,
    /// 500: the platform failed.
    HostFailed,
    /// 502: code.storage (or another service the platform calls) failed.
    UpstreamFailed,
    /// 503: the identity registry did not answer; nothing signed is
    /// decided without it (docs/finite-integration.md, rule 7).
    RegistryUnavailable,
    /// 402: the paying person's budget for the month cannot cover the step.
    BudgetUsedUp,
    /// 507: the app's database is at its cap (`limits::APP_DB_MAX_BYTES`);
    /// the mutation was rolled back.
    StorageFull,
    /// 503: the node serving this fragment cannot load another app until it
    /// restarts (celld's loaded-worker limit; docs/hardening.md).
    NodeFull,
}

impl ErrorCode {
    pub fn status(self) -> u16 {
        match self {
            ErrorCode::InvalidRequest => 400,
            ErrorCode::Unauthenticated => 401,
            ErrorCode::Forbidden => 403,
            ErrorCode::NotFound | ErrorCode::UnknownOperation | ErrorCode::NoCode => 404,
            ErrorCode::AlreadyExists | ErrorCode::ConflictingBody => 409,
            ErrorCode::TooLarge => 413,
            ErrorCode::AppFailed => 422,
            ErrorCode::RateLimited => 429,
            ErrorCode::HostFailed => 500,
            ErrorCode::UpstreamFailed => 502,
            ErrorCode::RegistryUnavailable => 503,
            ErrorCode::BudgetUsedUp => 402,
            ErrorCode::StorageFull => 507,
            ErrorCode::NodeFull => 503,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ErrorBody {
    pub error: ErrorCode,
    pub message: String,
}

/// A member's role in a fragment. `Public` is the floor anyone who can see
/// the fragment holds; the others are granted. Ordered weakest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Public,
    Viewer,
    Editor,
    Owner,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Public => "public",
            Role::Viewer => "viewer",
            Role::Editor => "editor",
            Role::Owner => "owner",
        }
    }

    pub fn parse(s: &str) -> Option<Role> {
        match s {
            "public" => Some(Role::Public),
            "viewer" => Some(Role::Viewer),
            "editor" => Some(Role::Editor),
            "owner" => Some(Role::Owner),
            _ => None,
        }
    }
}

/// Who can see a fragment: everyone, holders of its share link, or members.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Visibility {
    Public,
    Link,
    Members,
}

impl Visibility {
    pub fn as_str(self) -> &'static str {
        match self {
            Visibility::Public => "public",
            Visibility::Link => "link",
            Visibility::Members => "members",
        }
    }

    pub fn parse(s: &str) -> Option<Visibility> {
        match s {
            "public" => Some(Visibility::Public),
            "link" => Some(Visibility::Link),
            "members" => Some(Visibility::Members),
            _ => None,
        }
    }
}

/// `POST /api/fragments`. The fragment's own key is made by the node's
/// `KEYS` and stays sealed there; no client sends one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CreateFragment {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visibility: Option<Visibility>,
    /// Starts the fragment from one of the platform's templates (`chat`,
    /// `todo`, `blank`, …): its files are the first commit, and live.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,
}

/// The answer to a create: the only time the webhook secret is shown
/// besides a rotation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Created {
    pub name: String,
    pub npub: String,
    pub owner: String,
    pub visibility: Visibility,
    pub view_token: String,
    pub inbox_token: String,
    pub webhook_secret: String,
    pub repo: String,
    pub canonical: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Pins {
    pub main: Option<String>,
    pub live: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Counts {
    pub files: u64,
    pub events: u64,
    pub members: u64,
}

/// The app code installed from the `live` pin.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeStatus {
    /// The live commit the code was read from; `None` when live has no `app.mjs`.
    pub sha: Option<String>,
    pub operations: std::collections::BTreeMap<String, OpDecl>,
    /// Why the latest live commit's code was not installed, if it was not.
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Urls {
    pub canonical: String,
}

/// `GET /api/f/<name>/status` (viewer and up).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FragmentStatus {
    pub name: String,
    pub npub: String,
    pub owner: String,
    /// The caller's role.
    pub role: Role,
    pub visibility: Visibility,
    pub repo: String,
    pub pins: Pins,
    pub counts: Counts,
    pub code: CodeStatus,
    /// Viewers and up see the share link's token.
    pub view_token: Option<String>,
    /// Editors and up see the inbox token.
    pub inbox_token: Option<String>,
    pub urls: Urls,
    /// Files this large or larger are stored as blobs, their pointers in
    /// git (absent from hosts without blobs: the TypeScript runtime).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blob_min_bytes: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Member {
    /// The member's identity (`id:…`).
    pub principal: String,
    pub role: Role,
    /// The identity that granted it (the owner), or `invite:<id>`.
    pub added_by: String,
    pub added_at: i64,
    /// `person` or `agent`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<IdentityKind>,
    /// An agent member's owner, who reads what it reads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
}

/// What an identity is (docs/finite-integration.md). A fragment's own key
/// stays the fragment's and is not registered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdentityKind {
    Person,
    Agent,
}

impl IdentityKind {
    pub fn as_str(self) -> &'static str {
        match self {
            IdentityKind::Person => "person",
            IdentityKind::Agent => "agent",
        }
    }

    pub fn parse(s: &str) -> Option<IdentityKind> {
        match s {
            "person" => Some(IdentityKind::Person),
            "agent" => Some(IdentityKind::Agent),
            _ => None,
        }
    }
}

/// Who a key, a browser's session, or a lookup names, as the registry
/// holds them: the one description of an identity every hop shares (the
/// router's signer, a fragment's caller, a member being added).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identity {
    /// `id:` and 32 hex.
    pub id: String,
    pub kind: IdentityKind,
    /// An agent's owner.
    #[serde(default)]
    pub owner: Option<String>,
    /// A person's username; an agent's owner's (the namespace its fragments
    /// go in). `None` until a person chooses one.
    #[serde(default)]
    pub username: Option<String>,
}

/// One of a person's sign-ins, as their identity shows it: the email is an
/// attribute, refreshed at each sign-in and never the key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Subject {
    /// `workos:<client id>`: the environment that vouches for it.
    pub issuer: String,
    pub email: Option<String>,
    pub linked_at: i64,
}

/// Headers only the router sets on a request it hands a fragment's
/// supervisor (the cell's `routed.rs` writes and reads them) or the
/// agents' script; the router drops any a client sends.
pub mod routed {
    /// The fragment's full name.
    pub const NAME: &str = "x-fragment-name";
    /// The URL the request arrived on.
    pub const URL: &str = "x-fragment-url";
    /// How the site was addressed: `host` (its own origin) or `path` (`/f/<name>/`).
    pub const MODE: &str = "x-fragment-mode";
    /// Who is asking (JSON: the identity and the key it signed with).
    pub const SIGNED: &str = "x-fragment-signed";
    pub const ALL: [&str; 4] = [NAME, URL, MODE, SIGNED];
    /// The caller's identity on a request the router hands the agents'
    /// script (`agent/`), which trusts nothing else.
    pub const AGENT_PRINCIPAL: &str = "x-agent-principal";
}

/// `POST /api/identities`: register an agent the signer owns, with a key
/// proof by the agent's key. People come from sign-in.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Register {
    pub kind: IdentityKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proof: Option<String>,
}

/// `POST /api/identities/<id>/keys`: a key proof by the key to add.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AddKey {
    pub proof: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyView {
    pub npub: String,
    pub added_at: i64,
    /// The identity that added it.
    pub added_by: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked_at: Option<i64>,
}

/// An identity as it, or its owner, sees it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IdentityView {
    pub id: String,
    pub kind: IdentityKind,
    /// An agent's owner.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    /// A person's username (decision 16): their fragments are
    /// `<label>.<username>`. An agent's fragments go under its owner's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    /// Where a person's picture is served (`/api/users/<username>/picture`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub picture: Option<String>,
    pub created_at: i64,
    pub keys: Vec<KeyView>,
    /// A person's agents.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub agents: Vec<String>,
    /// A person's sign-ins.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subjects: Vec<Subject>,
    /// Whether this answer made it (a registration's replay answers false).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created: Option<bool>,
}

/// `PUT /api/f/<name>/members/<npub>` (owner)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetRole {
    pub role: Role,
}

/// `POST /api/f/<name>/invites` (owner)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CreateInvite {
    pub role: Role,
    /// How many people may join with it (default 1).
    #[serde(default)]
    pub uses: Option<u32>,
    /// Seconds until it expires (default 7 days).
    #[serde(default)]
    pub ttl_s: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Invite {
    pub id: String,
    pub role: Role,
    pub uses_left: u32,
    pub expires_at: i64,
    pub created_by: String,
    /// Only in the answer to the create: the cell keeps its hash.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
}

/// `POST /api/f/<name>/join` (any signed principal)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Join {
    pub token: String,
}

/// `PUT /api/f/<name>/visibility` (owner)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetVisibility {
    pub visibility: Visibility,
}

/// `GET /api/f/<name>/storage-token` (editor and up): a code.storage JWT
/// scoped to this fragment's repo, `git:read` + `git:write`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageToken {
    pub token: String,
    pub repo: String,
    pub api: String,
    pub expires_at: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpKind {
    Query,
    Mutation,
    /// Runs as a Workflow, one durable step at a time.
    Job,
}

impl OpKind {
    pub fn as_str(self) -> &'static str {
        match self {
            OpKind::Query => "query",
            OpKind::Mutation => "mutation",
            OpKind::Job => "job",
        }
    }

    pub fn parse(s: &str) -> Option<OpKind> {
        [OpKind::Query, OpKind::Mutation, OpKind::Job].into_iter().find(|k| k.as_str() == s)
    }
}

/// An operation as `fragment.json` declares it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpDecl {
    pub kind: OpKind,
    /// The weakest role that may call it.
    pub role: Role,
    /// Its input's JSON Schema (the supported subset); also its tool schema.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<Value>,
}

/// A channel as `fragment.json` declares it (app channels), or a built-in.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChannelDecl {
    /// The weakest role that may read it.
    pub read: Role,
}

/// One record in a channel. Records are appended by the platform (`events`,
/// `ops`) or by mutations' effects (app channels), never by clients.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelRecord {
    pub channel: String,
    pub seq: i64,
    pub at: i64,
    /// npub, `anon:…`, or `platform`
    pub principal: String,
    pub kind: String,
    /// The JSON text the cell stored, passed through as it is: the cell
    /// neither parses a body it just wrote nor one it reads back.
    pub body: Box<RawValue>,
}

/// Channel names: `^[a-z][a-z0-9_-]{0,63}$`.
pub fn valid_channel_name(name: &str) -> bool {
    let b = name.as_bytes();
    !b.is_empty() && b.len() <= 64 && b[0].is_ascii_lowercase() && b.iter().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'_' || *c == b'-')
}

/// The channels every fragment has.
pub const BUILTIN_CHANNELS: [&str; 3] = ["events", "ops", "inbox"];

/// What starts a triggered run, as `fragment.json`'s `triggers` declares it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TriggerOn {
    /// A five-field schedule, UTC.
    Cron(String),
    /// Each record appended to this channel (`inbox`, or an app channel).
    Channel(String),
    /// A move of `main` that changes a path matching this pattern.
    Files(String),
}

impl TriggerOn {
    /// What starts it, and on what: `("cron", schedule)`, `("channel",
    /// channel)`, or `("files", pattern)`; the names are its wire keys.
    pub fn parts(&self) -> (&'static str, &str) {
        match self {
            TriggerOn::Cron(c) => ("cron", c),
            TriggerOn::Channel(c) => ("channel", c),
            TriggerOn::Files(f) => ("files", f),
        }
    }

    pub fn from_parts(kind: &str, target: String) -> Option<TriggerOn> {
        match kind {
            "cron" => Some(TriggerOn::Cron(target)),
            "channel" => Some(TriggerOn::Channel(target)),
            "files" => Some(TriggerOn::Files(target)),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TriggerDecl {
    #[serde(flatten)]
    pub on: TriggerOn,
    /// The operation it runs (a mutation or a job).
    pub run: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    /// Recorded; its Workflow is being started.
    Queued,
    Running,
    Succeeded,
    /// Failed after its retries: kept for a replay.
    Held,
    /// Not started: its operation was paused, or the chain was too deep.
    Blocked,
}

impl RunStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            RunStatus::Queued => "queued",
            RunStatus::Running => "running",
            RunStatus::Succeeded => "succeeded",
            RunStatus::Held => "held",
            RunStatus::Blocked => "blocked",
        }
    }

    pub fn parse(s: &str) -> Option<RunStatus> {
        [RunStatus::Queued, RunStatus::Running, RunStatus::Succeeded, RunStatus::Held, RunStatus::Blocked].into_iter().find(|r| r.as_str() == s)
    }
}

/// One run of an operation: a job, or a triggered mutation (`GET /api/f/<name>/runs`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Run {
    pub id: i64,
    pub op: String,
    /// `call`, `cron`, `channel`, `files`, or `job` (started by another run's step)
    pub via: String,
    /// The cron schedule, channel, file pattern, or parent run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger: Option<String>,
    pub principal: String,
    pub status: RunStatus,
    pub attempt: u32,
    pub depth: u32,
    pub created_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Only when one run is read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<Value>,
    /// What its paid steps cost the budget that paid (micro-dollars).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_micros: Option<i64>,
}

/// A month of a billing org's budget (`GET /api/budget`). Money is in
/// micro-dollars; the allowance is the budget plus the month's top-ups.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BudgetView {
    pub billing_org: String,
    pub period: String,
    pub budget_micros: i64,
    pub topped_up_micros: i64,
    pub allowance_micros: i64,
    pub spent_micros: i64,
    pub reserved_micros: i64,
    pub remaining_micros: i64,
    /// At or past 80% of the allowance.
    pub warn: bool,
    /// The newest usage first.
    #[serde(default)]
    pub usage: Vec<UsageRow>,
}

/// One paid step, in the shape finite.computer's Core takes usage
/// (FIN-10: source reference, agent, billing org, period, unit, quantity),
/// recorded once per source reference.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageRow {
    /// `<fragment>@<incarnation>/run/<run>/step/<index>`
    pub source_ref: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    pub billing_org: String,
    pub period: String,
    /// `usd_micro`
    pub unit: String,
    /// What it cost (settled), or what it holds (reserved).
    pub quantity: i64,
    /// `reserved` or `settled`.
    pub state: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub fragment: String,
    /// Who started the run (it may be a visitor; the owner pays).
    pub principal: String,
    pub at: i64,
}

/// `POST /api/f/<name>/pause` (editor): triggers stop starting runs of
/// `op`; calls still work.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SetPaused {
    pub op: String,
    pub paused: bool,
}

/// `POST /api/f/<name>/replay` (editor): runs a held or blocked run again
/// with its original input.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Replay {
    pub run: i64,
}

/// `POST /api/f/<name>/ops/<op>`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpCall {
    pub id: String,
    #[serde(default)]
    pub input: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpResult {
    pub result: Value,
    /// True when this operation id already ran and the stored result is returned.
    pub replayed: bool,
}

/// The canonical form of an operation input: object keys sorted at every
/// level, so equal inputs serialize equally. Explicit rather than relying on
/// serde_json's map order, which a `preserve_order` feature anywhere in the
/// dependency graph would change.
pub fn canonical_json(v: &Value) -> String {
    let mut out = String::new();
    write_canonical(v, &mut out);
    out
}

fn write_canonical(v: &Value, out: &mut String) {
    match v {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (i, k) in keys.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&Value::String((*k).clone()).to_string());
                out.push(':');
                write_canonical(&map[k.as_str()], out);
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_canonical(item, out);
            }
            out.push(']');
        }
        scalar => out.push_str(&scalar.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Goal: the typed identity and sign-in reads the same JSON the
    /// registry and `GET /api/identities/me` carried before (T4: the wire
    /// is unchanged). Method: the literal old forms, decoded and encoded.
    #[test]
    fn identity_and_subject_keep_their_wire_form() {
        let facts = r#"{"id":"id:0123456789abcdef0123456789abcdef","kind":"agent","owner":"id:ffffffffffffffffffffffffffffffff","username":null}"#;
        let who: Identity = serde_json::from_str(facts).unwrap();
        assert_eq!(who.kind, IdentityKind::Agent);
        assert_eq!(who.owner.as_deref(), Some("id:ffffffffffffffffffffffffffffffff"));
        assert_eq!(who.username, None);
        let bare: Identity = serde_json::from_str(r#"{"id":"id:0123456789abcdef0123456789abcdef","kind":"person"}"#).unwrap();
        assert_eq!((bare.owner, bare.username), (None, None));
        assert!(serde_json::from_str::<Identity>(r#"{"id":"id:0123456789abcdef0123456789abcdef","kind":"robot"}"#).is_err(), "an unknown kind is refused");
        let subject = Subject { issuer: "workos:client_1".into(), email: None, linked_at: 7 };
        assert_eq!(serde_json::to_value(&subject).unwrap(), serde_json::json!({ "issuer": "workos:client_1", "email": null, "linkedAt": 7 }));
    }

    #[test]
    fn names() {
        assert!(valid_label("todo-1"));
        assert!(!valid_label("-todo"));
        assert!(!valid_label("todo-"));
        assert!(!valid_label("to--do"));
        assert!(!valid_label("Todo"));
        assert!(!valid_label(&"a".repeat(64)));
        assert!(valid_username("futurepaul"));
        assert!(!valid_username("ab"));
        assert!(!valid_username("www"));
        assert!(!valid_username(&"a".repeat(33)));
        assert!(valid_fragment_name("desktop.futurepaul"));
        assert!(!valid_fragment_name("desktop"));
        assert!(!valid_fragment_name("desktop.www"));
        assert!(!valid_fragment_name("a.b.futurepaul"));
        assert_eq!(split_fragment_name("todo-1.futurepaul"), Some(("todo-1", "futurepaul")));
        assert_eq!(fragment_name("todo", "paul"), "todo.paul");
        assert_eq!(flat_name("todo-1.futurepaul").as_deref(), Some("todo-1--futurepaul"));
        assert_eq!(flat_name("todo"), None);
        assert_eq!(from_flat_name("todo-1--futurepaul").as_deref(), Some("todo-1.futurepaul"));
        for not in ["todo", "todo--", "--paul", "a--b--c", "Todo--paul", "todo--pa", "todo.x--paul"] {
            assert_eq!(from_flat_name(not), None, "{not}");
        }
        assert!(valid_op_name("add_todo"));
        assert!(!valid_op_name("__mutate"));
        assert!(!valid_op_name("Add"));
        assert!(valid_op_id("turn-1:tool-0"));
        assert!(!valid_op_id("bad id"));
        assert!(!valid_op_id(""));
        assert!(valid_secret_name("OPENROUTER_API_KEY"));
        assert!(!valid_secret_name("lower"));
        assert!(!valid_secret_name("_X"));
        assert!(valid_repo_path("site/index.html"));
        assert!(!valid_repo_path("../x"));
        assert!(!valid_repo_path("/abs"));
        assert!(!valid_repo_path("a//b"));
        assert!(!valid_repo_path("a/./b"));
    }

    #[test]
    fn stored_names_are_the_wire_names() {
        for k in [OpKind::Query, OpKind::Mutation, OpKind::Job] {
            assert_eq!(OpKind::parse(k.as_str()), Some(k));
            assert_eq!(serde_json::to_value(k).unwrap(), k.as_str());
        }
        for on in [TriggerOn::Cron("0 9 * * *".into()), TriggerOn::Channel("inbox".into()), TriggerOn::Files("notes/**".into())] {
            let (kind, target) = on.parts();
            assert_eq!(TriggerOn::from_parts(kind, target.to_string()), Some(on.clone()));
            assert_eq!(serde_json::to_value(&on).unwrap(), serde_json::json!({ kind: target }));
        }
        assert_eq!(TriggerOn::from_parts("webhook", "x".into()), None);
    }

    #[test]
    fn kinds() {
        assert!(valid_kind("said") && valid_kind("a.b_c-9") && valid_kind(&"k".repeat(64)));
        assert!(!valid_kind("Said") && !valid_kind("9a") && !valid_kind("") && !valid_kind(&"k".repeat(65)) && !valid_kind("k\u{e9}"));
    }

    #[test]
    fn reserved_names_cover_the_app_class_handlers() {
        // `constructor` once deployed as an operation the facet then refused
        for name in ["constructor", "fetch", "alarm"] {
            assert!(valid_op_name(name) && RESERVED_OP_NAMES.contains(&name), "{name}");
        }
    }

    #[test]
    fn a_record_body_passes_through_as_stored() {
        // keys out of order and a float's spelling: a parsed body would sort
        // the keys and could respell the number; the stored text is kept
        let stored = r#"{"z":1,"a":[1.50,"x"]}"#;
        let record = ChannelRecord {
            channel: "room".into(),
            seq: 7,
            at: 1,
            principal: "platform".into(),
            kind: "said".into(),
            body: RawValue::from_string(stored.into()).unwrap(),
        };
        let text = serde_json::to_string(&record).unwrap();
        assert_eq!(text, format!(r#"{{"channel":"room","seq":7,"at":1,"principal":"platform","kind":"said","body":{stored}}}"#));
    }

    #[test]
    fn channel_names() {
        assert!(valid_channel_name("chat") && valid_channel_name("room-1") && valid_channel_name("a_b"));
        assert!(!valid_channel_name("Chat") && !valid_channel_name("1a") && !valid_channel_name(""));
    }

    #[test]
    fn roles_order_weakest_first() {
        assert!(Role::Public < Role::Viewer && Role::Viewer < Role::Editor && Role::Editor < Role::Owner);
        for r in [Role::Public, Role::Viewer, Role::Editor, Role::Owner] {
            assert_eq!(Role::parse(r.as_str()), Some(r));
            assert_eq!(serde_json::to_value(r).unwrap(), r.as_str());
        }
    }

    #[test]
    fn canonical_json_sorts_keys() {
        let a: Value = serde_json::from_str(r#"{"b":1,"a":{"d":2,"c":3}}"#).unwrap();
        let b: Value = serde_json::from_str(r#"{"a":{"c":3,"d":2},"b":1}"#).unwrap();
        assert_eq!(canonical_json(&a), canonical_json(&b));
        assert_eq!(canonical_json(&a), r#"{"a":{"c":3,"d":2},"b":1}"#);
    }

    #[test]
    fn error_codes_serialize_snake_case() {
        let body = ErrorBody { error: ErrorCode::ConflictingBody, message: "x".into() };
        assert_eq!(serde_json::to_string(&body).unwrap(), r#"{"error":"conflicting_body","message":"x"}"#);
        assert_eq!(ErrorCode::ConflictingBody.status(), 409);
    }
}
