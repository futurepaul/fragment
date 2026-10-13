//! A person's export of everything that is theirs (docs/api.md, Export),
//! pure: the archive's layout and its README, its limits, the tar it is
//! written as, the parts it is uploaded in, when a press starts one, and
//! the transcript a chat's records read as. The cell builds it (cell
//! export.rs): in the person's list (`Principal`), on its alarm, reading
//! each fragment and computer through the routes their owner reads them by.
//!
//! **Why tar.** An archive is written front to back in fixed-size parts
//! (R2's multipart upload), and a crash or a restart goes on from the last
//! part written, so a format that needs nothing but what came before suits
//! it: a tar entry is a header that names its size, then its bytes. Zip
//! needs each entry's CRC-32 before its bytes (or a data descriptor after
//! them, which a reader of a stored entry cannot find its end by) and a
//! directory of them all at its end. Every system opens a tar (macOS's
//! Archive Utility, `tar -xf` on Linux, macOS and Windows 10 and later,
//! 7-Zip), and a computer's saves are tars already (`tar+zstd`). It is not
//! compressed: most of its bytes (saves, pictures, audio) are already, and
//! the build stays within one alarm's CPU.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// What a manifest names its format, and the version of it this writes.
pub const FORMAT: &str = "fragment-export";
pub const VERSION: u32 = 1;

/// The one folder an archive holds, so extracting it spills nothing.
pub const ROOT: &str = "fragment-export";

/// The limits of an export: each enforced, and each a typed failure its
/// view shows (`fragment_proto::export::ExportFailure`).
pub mod limits {
    /// The whole archive: past it, the export fails `too_large`.
    pub const ARCHIVE_MAX_BYTES: u64 = 8 * 1024 * 1024 * 1024;
    /// One fragment's part of it (its files, database, channels, transcript
    /// and blobs): past it, `too_large`.
    pub const FRAGMENT_MAX_BYTES: u64 = 2 * 1024 * 1024 * 1024;
    /// Fragments exported (the person's own and their agents'): past it, `too_many`.
    pub const FRAGMENTS_MAX: usize = 1000;
    /// Fragments named under `sharedWithMe`: past it, `too_many`.
    pub const SHARED_MAX: usize = 2000;
    /// Files one fragment's main holds: past it, `too_many`.
    pub const FILES_MAX: usize = 20_000;
    /// Blobs one fragment's records attach: past it, `too_many`.
    pub const BLOBS_MAX: usize = 10_000;
    /// Tables in one app's database: past it, `too_many`.
    pub const TABLES_MAX: usize = 256;
    /// Entries in the archive (`tar`): past it, `too_many`.
    pub const ENTRIES_MAX: u32 = 200_000;
    /// Exports a person starts in any 24 hours: one more is 429.
    pub const PER_DAY: usize = 3;
    pub const DAY_MS: i64 = 24 * 3600 * 1000;
    /// How long a ready archive is kept, then deleted.
    pub const KEPT_MS: i64 = 7 * DAY_MS;
    /// How long a build may take: past it, `timed_out`.
    pub const BUILD_MAX_MS: i64 = 6 * 3600 * 1000;
    /// The archive's parts (R2's multipart upload: at least 5 MiB, all but
    /// the last the same size): what the build holds of it at most.
    pub const PART_BYTES: usize = 8 * 1024 * 1024;
    /// Most of an entry of unknown size (a channel's records, a table's
    /// rows) the build holds before it puts it aside in R2, and most of a
    /// stored source (a save, a blob) it reads at once.
    pub const PIECE_BYTES: usize = 8 * 1024 * 1024;
    /// One alarm's work, then its place is kept and the next alarm goes on.
    pub const SLICE_MS: i64 = 20_000;
    /// Tries of a part that fails for a reason that may pass, then the
    /// export fails `unavailable`.
    pub const ATTEMPTS_MAX: u32 = 8;
    /// Records read from a channel a page (at most 6.4 MiB of bodies).
    pub const CHANNEL_PAGE: usize = 100;
    /// Rows of a table read a page, and the most of their JSON a page holds.
    pub const TABLE_PAGE: usize = 1000;
    pub const TABLE_PAGE_MAX_BYTES: usize = 4 * 1024 * 1024;
    /// Exports a person's list keeps the rows of (for the day's count).
    pub const KEPT_ROWS: usize = 16;
}

// ---- the layout ----

/// `README.md`, at the root.
pub fn readme_path() -> String {
    format!("{ROOT}/README.md")
}

/// `manifest.json`, at the root.
pub fn manifest_path() -> String {
    format!("{ROOT}/manifest.json")
}

/// A fragment's folder.
pub fn fragment_dir(name: &str) -> String {
    assert!(fragment_proto::valid_fragment_name(name), "a fragment's folder is its name");
    format!("{ROOT}/fragments/{name}")
}

/// One of its files, at its path in git.
pub fn file_path(fragment: &str, path: &str) -> String {
    assert!(fragment_proto::valid_repo_path(path), "a file's path is a repo path");
    format!("{}/files/{path}", fragment_dir(fragment))
}

pub fn meta_path(fragment: &str) -> String {
    format!("{}/meta.json", fragment_dir(fragment))
}

pub fn schema_path(fragment: &str) -> String {
    format!("{}/database/schema.sql", fragment_dir(fragment))
}

/// A table's rows; its name made a file's (`segment`).
pub fn table_path(fragment: &str, table: &str) -> String {
    format!("{}/database/{}.jsonl", fragment_dir(fragment), segment(table))
}

pub fn channel_path(fragment: &str, channel: &str) -> String {
    assert!(fragment_proto::valid_channel_name(channel), "a channel's file is its name");
    format!("{}/channels/{channel}.jsonl", fragment_dir(fragment))
}

pub fn transcript_path(fragment: &str) -> String {
    format!("{}/transcript.md", fragment_dir(fragment))
}

/// A blob its records attach, by its SHA-256.
pub fn blob_path(fragment: &str, sha: &str) -> String {
    assert!(crate::blob::valid_sha(sha), "a blob's file is its hash");
    format!("{}/blobs/{sha}", fragment_dir(fragment))
}

/// A computer's folder: its id's 24 hex (a `:` is no file name everywhere).
pub fn computer_dir(id: &str) -> String {
    let hex = id.strip_prefix("computer:").filter(|h| h.len() == 24 && h.bytes().all(|b| b.is_ascii_hexdigit()));
    format!("{ROOT}/computers/{}", hex.expect("a computer's id is computer:<24 hex>"))
}

pub fn computer_meta_path(id: &str) -> String {
    format!("{}/meta.json", computer_dir(id))
}

/// A save's record of `dir` (`/data` → `data.tar.zst`, `/data/work` →
/// `data-work.tar.zst`), as stored: a tar compressed with zstd.
pub fn save_path(id: &str, dir: &str) -> String {
    let name = dir.trim_matches('/').split('/').map(segment).collect::<Vec<_>>().join("-");
    format!("{}/{}.tar.zst", computer_dir(id), if name.is_empty() { "root".to_string() } else { name })
}

/// `path` as the JSON file in `dir` names it: relative to that folder (a
/// manifest's to the root, a meta's to its fragment's or computer's).
pub fn within(path: &str, dir: &str) -> String {
    let rest = path.strip_prefix(dir).and_then(|r| r.strip_prefix('/'));
    rest.expect("a file named is under the folder that names it").to_string()
}

/// A name made one path segment: ASCII letters, digits, `.`, `_` and `-`
/// as they are, any other byte as `%XX` (so `.`, `..` and the empty name,
/// which name no file, are written `%2E`, `%2E%2E` and `%`).
pub fn segment(name: &str) -> String {
    if name.is_empty() {
        return "%".into();
    }
    if name == "." || name == ".." {
        return name.replace('.', "%2E");
    }
    let mut out = String::with_capacity(name.len());
    for b in name.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'.' | b'_' | b'-' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

// ---- tar ----

/// A tar block.
pub const BLOCK: usize = 512;

/// The archive's end: two empty blocks.
pub const END: [u8; 2 * BLOCK] = [0; 2 * BLOCK];

/// The largest entry a ustar header's size field holds (11 octal digits):
/// past every limit here.
const USTAR_SIZE_MAX: u64 = 0o777_7777_7777;

/// The zeros after an entry's `size` bytes, to its block's end.
pub fn padding(size: u64) -> usize {
    ((BLOCK as u64 - size % BLOCK as u64) % BLOCK as u64) as usize
}

/// An entry's header: one ustar block, or, for a path ustar cannot hold
/// (past its 100 + 155 bytes, or not ASCII), a pax header naming it first.
/// A regular file, `0644`, modified at `mtime_s`.
pub fn header(path: &str, size: u64, mtime_s: u64) -> Vec<u8> {
    assert!(!path.is_empty() && !path.starts_with('/') && !path.split('/').any(|s| s.is_empty() || s == "." || s == ".."), "an entry's path is relative, with no empty, . or .. segment: {path:?}");
    assert!(size <= USTAR_SIZE_MAX, "an entry fits a ustar header's size");
    if let Some((prefix, name)) = ustar_split(path) {
        return ustar(name, prefix, size, mtime_s, b'0').to_vec();
    }
    // a pax header: its one record names the path in UTF-8
    let record = pax_record("path", path);
    let mut out = ustar(&format!("{ROOT}/PaxHeader"), "", record.len() as u64, mtime_s, b'x').to_vec();
    out.extend_from_slice(record.as_bytes());
    out.resize(out.len() + padding(record.len() as u64), 0);
    // what a reader without pax finds: the path's ASCII, cut to fit
    let plain: String = path.chars().map(|c| if c.is_ascii() && !c.is_ascii_control() { c } else { '_' }).collect();
    let cut = &plain[plain.len().saturating_sub(100)..];
    out.extend_from_slice(&ustar(cut.trim_start_matches('/'), "", size, mtime_s, b'0'));
    assert_eq!(out.len() % BLOCK, 0, "a header is whole blocks");
    out
}

/// A path as ustar holds one: all of it as its name (at most 100 bytes), or
/// split at a `/` into a prefix (at most 155) and a name; `None` when it
/// cannot be, or is not ASCII.
fn ustar_split(path: &str) -> Option<(&str, &str)> {
    if !path.is_ascii() || path.bytes().any(|b| b.is_ascii_control()) {
        return None;
    }
    if path.len() <= 100 {
        return Some(("", path));
    }
    path.match_indices('/').map(|(i, _)| (&path[..i], &path[i + 1..])).find(|(p, n)| p.len() <= 155 && n.len() <= 100 && !n.is_empty())
}

/// One pax record, `<length> <key>=<value>\n`, its length counting itself.
fn pax_record(key: &str, value: &str) -> String {
    let rest = key.len() + value.len() + 3;
    let mut len = rest + 1;
    // bounded: the length's digits settle within two rounds
    for _ in 0..3 {
        let total = rest + len.to_string().len();
        if total == len {
            break;
        }
        len = total;
    }
    let record = format!("{len} {key}={value}\n");
    assert_eq!(record.len(), len, "a pax record's length counts itself");
    record
}

fn ustar(name: &str, prefix: &str, size: u64, mtime_s: u64, kind: u8) -> [u8; BLOCK] {
    assert!(name.len() <= 100 && prefix.len() <= 155, "ustar's fields hold them");
    let mut h = [0u8; BLOCK];
    let put = |h: &mut [u8; BLOCK], at: usize, bytes: &[u8]| h[at..at + bytes.len()].copy_from_slice(bytes);
    put(&mut h, 0, name.as_bytes());
    put(&mut h, 100, b"0000644\0");
    put(&mut h, 108, b"0000000\0");
    put(&mut h, 116, b"0000000\0");
    put(&mut h, 124, format!("{size:011o}\0").as_bytes());
    put(&mut h, 136, format!("{:011o}\0", mtime_s.min(USTAR_SIZE_MAX)).as_bytes());
    put(&mut h, 148, b"        ");
    h[156] = kind;
    put(&mut h, 257, b"ustar\0");
    put(&mut h, 263, b"00");
    put(&mut h, 345, prefix.as_bytes());
    let sum: u32 = h.iter().map(|b| u32::from(*b)).sum();
    put(&mut h, 148, format!("{sum:06o}\0 ").as_bytes());
    h
}

// ---- the parts ----

/// What the build holds of the archive that is not uploaded yet: whole
/// parts leave it as they fill (`full`), all the same size, and its last,
/// smaller one leaves it at the end. It is what each alarm keeps between
/// alarms, so it is never more than a part.
#[derive(Debug, Default)]
pub struct Tail {
    part: usize,
    bytes: Vec<u8>,
}

impl Tail {
    pub fn new(part: usize, kept: Vec<u8>) -> Tail {
        assert!(part > 0 && kept.len() < part, "a tail kept is less than a part");
        Tail { part, bytes: kept }
    }

    /// Takes `data`, answering the whole parts it filled, in order.
    pub fn push(&mut self, data: &[u8]) -> Vec<Vec<u8>> {
        self.bytes.extend_from_slice(data);
        let mut parts = Vec::with_capacity(self.bytes.len() / self.part);
        // bounded: each round takes a part's bytes
        while self.bytes.len() >= self.part {
            let rest = self.bytes.split_off(self.part);
            parts.push(std::mem::replace(&mut self.bytes, rest));
        }
        assert!(self.bytes.len() < self.part && parts.iter().all(|p| p.len() == self.part), "only whole parts leave, and less than one stays");
        parts
    }

    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// What it holds: kept between alarms, or the archive's last part.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

// ---- starting one ----

/// An export a person's list keeps, as a press reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Begun {
    pub id: String,
    pub started_at: i64,
    pub building: bool,
}

/// What a press of "Export everything" does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Start {
    /// One is building: the same one, nothing started.
    Replay(String),
    /// `limits::PER_DAY` started in the last 24 hours: the next may start at `next_at`.
    Refused { next_at: i64 },
    /// A new one.
    New,
}

/// What a press does, at `now`, given what the list keeps.
pub fn start(begun: &[Begun], now: i64) -> Start {
    if let Some(b) = begun.iter().find(|b| b.building) {
        return Start::Replay(b.id.clone());
    }
    let mut recent: Vec<i64> = begun.iter().map(|b| b.started_at).filter(|at| *at > now - limits::DAY_MS).collect();
    if recent.len() >= limits::PER_DAY {
        recent.sort_unstable();
        // the day's oldest leaves the window first
        return Start::Refused { next_at: recent[recent.len() - limits::PER_DAY] + limits::DAY_MS };
    }
    Start::New
}

/// The wait before a part's next try: 5 s, doubling, at most 5 minutes.
pub fn retry_ms(attempt: u32) -> i64 {
    (5_000i64 << attempt.min(6)).min(300_000)
}

// ---- what it says ----

/// `manifest.json`: what the archive holds, and whose it is.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Manifest {
    pub format: String,
    pub version: u32,
    pub deployment: Deployment,
    /// When the export began (ms), and as an RFC 3339 time.
    pub made_at: i64,
    pub made: String,
    pub export: String,
    pub person: Person,
    /// The person's agents (their npubs).
    pub agents: Vec<String>,
    pub fragments: Vec<Listed>,
    pub computers: Vec<ComputerListed>,
    /// Fragments others own that the person, or an agent of theirs, is in:
    /// named, never copied.
    pub shared_with_me: Vec<Shared>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Deployment {
    pub id: String,
    pub platform: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Person {
    pub npub: String,
    pub email: Option<String>,
}

/// A fragment the archive holds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Listed {
    pub name: String,
    pub kind: fragment_proto::FragmentKind,
    pub title: Option<String>,
    /// Whose it is: the person, or an agent of theirs.
    pub owner: String,
    /// Its folder in the archive.
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ComputerListed {
    pub id: String,
    pub path: String,
}

/// A fragment someone else owns, shared with the person or their agent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Shared {
    pub name: String,
    pub kind: fragment_proto::FragmentKind,
    pub title: Option<String>,
    /// Its owner (an npub), as its status names them; `None` when it would
    /// not say (it was deleted meanwhile).
    pub owner: Option<String>,
    pub role: fragment_proto::Role,
    /// Whose membership it is: the person's, or an agent's (its npub).
    pub member: String,
}

/// Something of a fragment's (or a computer's) the export could not take,
/// and why: a refusal (an agent's fragment's invites, which only its owner
/// reads) or what is not there (no app code, so no database).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Skipped {
    pub part: String,
    pub why: String,
}

/// A part of a fragment exported, and its file in the archive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Counted {
    pub name: String,
    pub count: u64,
    pub file: String,
}

/// A fragment's `meta.json`. `status`, `members`, `invites` and `channels`
/// are what its owner reads at those routes (docs/api.md, Control API),
/// less every token: `shareLink` and `inboxToken` say only whether they are.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FragmentMeta {
    pub name: String,
    pub kind: fragment_proto::FragmentKind,
    pub title: Option<String>,
    /// The blessed template it runs (`chat`, `agent`, `brain`, `skills`), if one.
    pub template: Option<String>,
    pub npub: Option<String>,
    pub owner: Option<String>,
    pub visibility: Option<fragment_proto::Visibility>,
    /// Its share link opens it (`link` or `public`, with a token).
    pub share_link: bool,
    /// It has an inbox token (its webhooks' door).
    pub inbox_token: bool,
    pub pins: Option<fragment_proto::Pins>,
    /// The code live runs (`code.id` of status), and why the newest was not installed.
    pub code: Option<String>,
    pub code_error: Option<String>,
    /// Its members, each with their email (people) or name (agents) as its members see them.
    pub members: Vec<Value>,
    /// The invites waiting on an email.
    pub invites: Vec<Value>,
    /// Its secrets' names: their values never leave.
    pub secrets: Vec<String>,
    /// Its files at `ref` (main's commit), and their bytes.
    pub files: FilesCounted,
    /// Its app's tables, their rows counted; `None` without app code.
    pub database: Option<Vec<Counted>>,
    /// Its channels, their records counted.
    pub channels: Vec<Counted>,
    /// Its blobs its records attach, by SHA-256, and their sizes.
    pub blobs: Vec<BlobCounted>,
    pub transcript: Option<String>,
    pub skipped: Vec<Skipped>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FilesCounted {
    #[serde(rename = "ref")]
    pub reference: Option<String>,
    pub count: u64,
    pub bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BlobCounted {
    pub sha256: String,
    pub size: u64,
}

/// A computer's `meta.json`: its view as its owner reads it, and the save
/// exported (`None`: it was never saved), each record's file beside it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ComputerMeta {
    pub view: Value,
    pub save: Option<SaveExported>,
    pub skipped: Vec<Skipped>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SaveExported {
    pub number: u64,
    pub id: String,
    pub at: i64,
    pub generation: u64,
    pub held: bool,
    /// Each `DirectoryBackup` record (`{id, dir, size, sha256, format}`) and its file.
    pub records: Vec<Value>,
}

/// A JSON file's bytes, as the archive holds it: pretty, and ending in a newline.
pub fn json_file<T: Serialize>(v: &T) -> Vec<u8> {
    let mut bytes = serde_json::to_vec_pretty(v).expect("export metadata serializes");
    bytes.push(b'\n');
    bytes
}

// ---- time ----

/// `ms` as an RFC 3339 time, to the second, in UTC.
pub fn rfc3339(ms: i64) -> String {
    let (y, mo, d, h, mi, s) = clock(ms);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

/// `ms` as a person reads a time: `2026-10-13 14:02 UTC`.
pub fn readable(ms: i64) -> String {
    let (y, mo, d, h, mi, _) = clock(ms);
    format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02} UTC")
}

fn clock(ms: i64) -> (i64, u32, u32, i64, i64, i64) {
    let s = ms.div_euclid(1000);
    let (y, mo, d) = crate::cron::civil(s.div_euclid(86_400));
    let t = s.rem_euclid(86_400);
    (y, mo, d, t / 3600, t / 60 % 60, t % 60)
}

// ---- a chat's transcript ----

/// Who a principal is, as a transcript names them: a member's email or an
/// agent's name, a short npub for anyone the fragment no longer names.
#[derive(Debug, Default, Clone)]
pub struct Names(pub BTreeMap<String, String>);

impl Names {
    pub fn of(&self, principal: &str) -> String {
        if let Some(n) = self.0.get(principal) {
            return n.clone();
        }
        if principal == "platform" {
            return "Fragment".into();
        }
        if principal.starts_with("anon:") {
            return "A visitor".into();
        }
        let shown = crate::npub::display(principal);
        match shown.len() {
            n if n > 20 => format!("{}…{}", &shown[..12], &shown[n - 6..]),
            _ => shown,
        }
    }
}

/// One record of a chat, as the transcript reads it.
#[derive(Debug, Clone, Copy)]
pub struct Said<'a> {
    pub channel: &'a str,
    pub seq: i64,
    pub at: i64,
    pub principal: &'a str,
    pub body: &'a Value,
}

impl Said<'_> {
    /// Where it falls in the transcript: by time, a message before the
    /// work of the same moment, then by its place in its channel.
    pub fn order(&self) -> (i64, bool, i64) {
        (self.at, self.channel != "chat", self.seq)
    }
}

/// The transcript's first lines.
pub fn transcript_head(name: &str, title: Option<&str>, made_at: i64) -> String {
    let title = title.filter(|t| !t.trim().is_empty()).unwrap_or(name);
    format!(
        "# {}\n\nThe conversation in {name}, as its records hold it (`channels/chat.jsonl` and\n`channels/work.jsonl`), exported {}. Times are UTC; a file attached is in\n`blobs/`, named by its SHA-256.\n",
        one_line(title),
        readable(made_at)
    )
}

/// What one record adds to a transcript (docs/chat-records.md), or nothing
/// for a record a person never reads there (a turn's claim, a menu, a
/// page's own kinds).
pub fn transcript_part(r: &Said, names: &Names) -> Option<String> {
    let who = names.of(r.principal);
    let when = readable(r.at);
    let s = |k: &str| r.body.get(k).and_then(Value::as_str).unwrap_or_default();
    match r.channel {
        "chat" => {
            let kind = r.body.get("kind").and_then(Value::as_str);
            match kind {
                None | Some("message") => {
                    let text = r.body.as_str().unwrap_or_else(|| s("text"));
                    let files = attachments(r.body);
                    if text.trim().is_empty() && files.is_empty() {
                        return None;
                    }
                    let mut out = format!("\n**{}** · {when}\n\n", one_line(&who));
                    if !text.trim().is_empty() {
                        out.push_str(text.trim_end());
                        out.push('\n');
                    }
                    for f in files {
                        out.push_str(&f);
                    }
                    Some(out)
                }
                Some("stop") => Some(format!("\n*{} stopped a turn · {when}*\n", one_line(&who))),
                Some("prompt_response") => {
                    let said = if s("text").is_empty() { String::new() } else { format!(": {}", one_line(s("text"))) };
                    Some(format!("\n*{} answered {}{said} · {when}*\n", one_line(&who), code(s("option"))))
                }
                Some("command") => Some(format!("\n**{}** · {when}\n\n{}\n", one_line(&who), code(format!("/{} {}", s("command"), s("args")).trim_end()))),
                Some(_) => None,
            }
        }
        "work" => {
            let kind = r.body.get("kind").and_then(Value::as_str)?;
            let line = match kind {
                "turn.step" => {
                    let verb = match s("category") {
                        "shell" => "ran",
                        "web" => "searched the web for",
                        "read" => "read",
                        "write" => "wrote",
                        "browser" => "used the browser:",
                        "image" => "made an image:",
                        "delegate" => "handed off",
                        "memory" => "changed its memory:",
                        _ => "used",
                    };
                    let what = if s("args").is_empty() { s("tool").to_string() } else { s("args").to_string() };
                    let failed = if r.body.get("ok") == Some(&Value::Bool(false)) { " (it failed)" } else { "" };
                    format!("{} {verb} {}{failed}", one_line(&who), code(&what))
                }
                "turn.prompt" => {
                    let options: Vec<&str> = r.body.get("options").and_then(Value::as_array).map(|o| o.iter().filter_map(|x| x["label"].as_str()).collect()).unwrap_or_default();
                    format!("{} asked: {} ({})", one_line(&who), one_line(s("text")), options.join(" / "))
                }
                "turn.prompt.closed" => match s("outcome") {
                    "answered" => format!("Answered {} by {}", code(s("option")), names.of(s("by"))),
                    "expired" => "The question expired".to_string(),
                    _ => "The question was stopped".to_string(),
                },
                "turn.notice" => format!("{}: {}", one_line(&who), one_line(s("text"))),
                "turn.end" => match s("outcome") {
                    "error" => format!("{}'s turn ended: {}", one_line(&who), one_line(s("error"))),
                    "stopped" => format!("{}'s turn was stopped", one_line(&who)),
                    _ => return None,
                },
                _ => return None,
            };
            Some(format!("\n> {line} · {when}\n"))
        }
        _ => None,
    }
}

/// A record's attachments (docs/chat-records.md), each a line naming its file.
fn attachments(body: &Value) -> Vec<String> {
    let Some(list) = body.get("attachments").and_then(Value::as_array) else { return vec![] };
    list.iter()
        .filter_map(|a| {
            let sha = a.get("sha256")?.as_str().filter(|s| crate::blob::valid_sha(s))?;
            let name = a.get("name").and_then(Value::as_str).map(one_line).unwrap_or_else(|| sha[..12].to_string());
            let kind = a.get("type").and_then(Value::as_str).unwrap_or("a file");
            Some(format!("\nAttached: [{name}](blobs/{sha}) ({})\n", one_line(kind)))
        })
        .collect()
}

/// A text on one line, for a line of markdown.
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A code span around `text`, fenced past any run of backticks in it.
fn code(text: impl AsRef<str>) -> String {
    let text = one_line(text.as_ref());
    let mut run = 0;
    let mut longest = 0;
    for c in text.chars() {
        run = if c == '`' { run + 1 } else { 0 };
        longest = longest.max(run);
    }
    let fence = "`".repeat(longest + 1);
    let pad = if text.starts_with('`') || text.ends_with('`') { " " } else { "" };
    format!("{fence}{pad}{text}{pad}{fence}")
}

// ---- the README ----

/// `README.md`: the archive's layout and every format in it. The same in
/// every export (its manifest says whose, from where, and when).
pub const README: &str = r#"# Your Fragment export

This is everything that is yours on a Fragment deployment: the fragments you
own and the ones your agents own, your computer's newest good save, and the
names of the fragments other people share with you. `manifest.json` says
whose it is, which deployment it is from, and when it was made.

It is one tar file, not compressed. Open it with your system's archive tool,
or `tar -xf <file>.tar`; everything is in the folder `fragment-export/`.

## Layout

    fragment-export/
      README.md                     this file
      manifest.json                 what the export holds, and whose it is
      fragments/<name>/             each fragment you own, and each one your agents own
        meta.json                   what it is, who is in it, how it is shared
        files/<path>                its files, as main (its working copy) holds them
        database/schema.sql         its app's database: the statements that make it
        database/<table>.jsonl      that table's rows
        channels/<channel>.jsonl    a channel's records, in order
        transcript.md               a chat's conversation, to read (chats only)
        blobs/<sha256>              the files its records attach
      computers/<id>/               your computer (its id's 24 hex digits)
        meta.json                   its image, its agents, and the save exported
        data.tar.zst                the save of /data, but /data/work
        data-work.tar.zst           the same save's /data/work: its agents' workspace

A `<name>` is a fragment's name (`todo--k3x9`). A name in `database/` is its
table's, with any byte but `A-Z a-z 0-9 . _ -` written `%XX`.

## manifest.json

    {
      "format": "fragment-export", "version": 1,
      "deployment": { "id": "...", "platform": "https://..." },
      "madeAt": 1791000000000, "made": "2026-10-13T16:00:00Z",
      "export": "<16 hex>",
      "person": { "npub": "npub1...", "email": "you@example.com" },
      "agents": ["npub1..."],
      "fragments": [{ "name", "kind", "title", "owner", "path" }],
      "computers": [{ "id": "computer:<24 hex>", "path" }],
      "sharedWithMe": [{ "name", "kind", "title", "owner", "role", "member" }]
    }

`kind` is `app`, `chat`, `agent`, `brain` or `skills`. People and agents are
named by their npubs. `sharedWithMe` names each fragment someone else owns
that you (or an agent of yours: `member`) are in, with your role there; it
copies none of them. Ask their owners for theirs. Every `path` and `file` a
JSON file names is relative to its own folder.

## A fragment

`meta.json` holds what its owner reads of it: its kind and title, the blessed
template it runs (if one), its npub, its owner, its visibility, whether a
share link or an inbox token exists (never the tokens), the commits its
`main` and `live` are at, the code live runs, its members (each with the
email or name its members see), the invites waiting on an email, its secrets'
names (never their values), and how many files, rows, records and blobs it
holds. `skipped` names anything the export could not take, and why.

`files/` is the fragment's files at `main`, its working copy, byte for byte:
a file of 1 MiB or more is there whole (git holds it as a pointer to its
blob). An agent's soul, memories and skills are its fragment's files
(`SOUL.md`, `memories/`, `skills/`). Git's history is not in the archive.

`database/schema.sql` is the `CREATE` statements of the app's SQLite
database, each ending in `;`. Each `database/<table>.jsonl` holds one row a
line: a JSON object of its columns. A text is a string; an integer or a real
is a number (one past 2^53 may be rounded); a blob is
`{"$base64": "..."}`; a null is `null`. The platform's own table (its
operation ledger) is left out.

Each `channels/<channel>.jsonl` holds one record a line, oldest first:

    { "seq": 1, "at": 1791000000000, "principal": "npub1...", "kind": "message", "body": { ... } }

`at` is milliseconds since 1970 (UTC); `principal` is who wrote it (an npub,
`anon:...` for a visitor, `platform` for the platform). `events` is the
fragment's audit trail, `ops` its applied operations, `inbox` its webhooks,
and the rest are its app's. A chat's are its template's: `chat` holds
what was said (a message `{"text", "attachments"?, "reply_to"?}`, an
agent's reply `{"text", "turn"}`, `{"kind": "stop"}`, a prompt's answer),
and `work` each agent's turns (`turn.start`, `turn.step`, `turn.prompt`,
`turn.prompt.closed`, `turn.notice`, `turn.end`); Fragment's repository
documents them whole (docs/chat-records.md).

`transcript.md` is a chat's `chat` and `work` records, read in order: who
said what and when, each agent's steps, its questions and their answers.

`blobs/` holds each file a record attaches (a record's `attachments`), named
by its SHA-256, as it was uploaded.

## A computer

`meta.json` holds the computer's view as its owner reads it (its image, its
version, its agents, its saves) and the save exported: its number, when it
was taken, and its records.

Each `.tar.zst` is one record of that save as it is stored: a tar of the
directory, compressed with zstd (`tar --zstd -xf data.tar.zst`, or
`zstd -d data.tar.zst && tar -xf data.tar`). `data.tar.zst` is `/data` less
`/data/work`: the agent runtime's own state (our Hermes image keeps its
sessions, databases and profiles there). `data-work.tar.zst` is
`/data/work`: what its agents' tools wrote (their workspace). A save taken
before the two were apart is one `data.tar.zst`. Each record's `sha256` in
`meta.json` is that file's. What the computer did after that save is in no
save yet.

## What is not here

Your sign-ins and keys, your connections' tokens, your own keys (sealed in
your computer), secrets' values, share links' and inbox tokens, your usage
ledger, git history, the runs of jobs, apps' preview cards, and anything
of the fragments others share with you.
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    const NAME: &str = "todo--k3x9";

    /// An archive of `entries`, as the build writes one: each header, its
    /// bytes and their padding, then the end, through parts of `part`.
    fn archive(entries: &[(String, Vec<u8>)], part: usize) -> (Vec<u8>, Vec<usize>) {
        let mut tail = Tail::new(part, vec![]);
        let (mut out, mut sizes) = (Vec::new(), Vec::new());
        let mut take = |parts: Vec<Vec<u8>>, out: &mut Vec<u8>| {
            for p in parts {
                sizes.push(p.len());
                out.extend(p);
            }
        };
        for (path, bytes) in entries {
            let parts = tail.push(&header(path, bytes.len() as u64, 1_791_000_000));
            take(parts, &mut out);
            take(tail.push(bytes), &mut out);
            take(tail.push(&vec![0; padding(bytes.len() as u64)]), &mut out);
        }
        take(tail.push(&END), &mut out);
        let last = tail.into_bytes();
        sizes.push(last.len());
        out.extend(last);
        (out, sizes)
    }

    fn read(tar_bytes: &[u8]) -> Vec<(String, Vec<u8>)> {
        let mut a = tar::Archive::new(tar_bytes);
        a.entries()
            .unwrap()
            .map(|e| {
                let mut e = e.unwrap();
                let path = e.path().unwrap().to_string_lossy().to_string();
                let mut b = vec![];
                e.read_to_end(&mut b).unwrap();
                (path, b)
            })
            .collect()
    }

    /// Goal: what the build writes is a tar every reader takes, with each
    /// path and its bytes as written: short ones, one only ustar's prefix
    /// holds, one past both (pax), one not ASCII, empty and odd sizes.
    #[test]
    fn a_tar_of_every_path() {
        let deep = format!("a/{}/b.txt", "d".repeat(140));
        let past = format!("{}/x", "p".repeat(300));
        let entries = vec![
            (readme_path(), README.as_bytes().to_vec()),
            (manifest_path(), b"{}\n".to_vec()),
            (file_path(NAME, "index.html"), b"<p>hi</p>\n".to_vec()),
            (file_path(NAME, &deep), vec![7; 513]),
            (file_path(NAME, "notes/caf\u{e9} \u{1f600}.md"), b"accents".to_vec()),
            (format!("{ROOT}/{past}"), vec![]),
            (blob_path(NAME, &"a".repeat(64)), vec![1; 1024]),
        ];
        let (bytes, sizes) = archive(&entries, 5 * BLOCK);
        assert_eq!(bytes.len() % BLOCK, 0, "an archive is whole blocks");
        assert!(sizes[..sizes.len() - 1].iter().all(|s| *s == 5 * BLOCK), "every part but the last is a part: {sizes:?}");
        assert!(*sizes.last().unwrap() <= 5 * BLOCK);
        assert_eq!(read(&bytes), entries, "every entry reads back, path and bytes");
        // a reader without pax still finds an entry in ASCII
        let pax = header(&file_path(NAME, "caf\u{e9}"), 3, 0);
        assert_eq!(pax.len(), 3 * BLOCK, "a pax header, its record, then the entry's header");
        assert_eq!(pax[156], b'x');
        assert_eq!(pax[2 * BLOCK + 156], b'0');
    }

    /// Goal: a header's checksum is ustar's, and a path that is no relative
    /// path (absolute, `..`, empty segments) is never written.
    #[test]
    fn headers_hold_only_relative_paths() {
        let h = header("fragment-export/README.md", 10, 0);
        let field = std::str::from_utf8(&h[148..154]).unwrap();
        let mut blank = h.clone();
        blank[148..156].copy_from_slice(b"        ");
        let sum: u32 = blank.iter().map(|b| u32::from(*b)).sum();
        assert_eq!(u32::from_str_radix(field, 8).unwrap(), sum);
        for bad in ["/etc/passwd", "a/../b", "a//b", "", "./a"] {
            assert!(std::panic::catch_unwind(|| header(bad, 0, 0)).is_err(), "{bad:?} is refused");
        }
        assert_eq!(padding(0), 0);
        assert_eq!(padding(1), 511);
        assert_eq!(padding(512), 0);
        assert_eq!(padding(513), 511);
    }

    /// Goal: a pax record's length counts its own digits, at every boundary.
    #[test]
    fn pax_lengths() {
        for n in 0..300 {
            let r = pax_record("path", &"x".repeat(n));
            let (len, _) = r.split_once(' ').unwrap();
            assert_eq!(len.parse::<usize>().unwrap(), r.len());
        }
    }

    /// Goal: the parts leave whole and the same size, and less than one is
    /// ever held: a tail kept across alarms is under a part.
    #[test]
    fn parts_are_whole() {
        let mut t = Tail::new(4, vec![]);
        assert!(t.push(b"abc").is_empty());
        assert_eq!(t.push(b"defghij"), vec![b"abcd".to_vec(), b"efgh".to_vec()]);
        assert_eq!(t.bytes(), b"ij");
        let mut again = Tail::new(4, t.bytes().to_vec());
        assert_eq!(again.push(b"kl"), vec![b"ijkl".to_vec()]);
        assert!(again.is_empty());
        assert!(std::panic::catch_unwind(|| Tail::new(4, b"abcd".to_vec())).is_err(), "a tail kept is less than a part");
    }

    /// Goal: a press while one builds is that one (replay); a fourth in 24
    /// hours is refused, saying when the next may start; a day later, one
    /// starts again.
    #[test]
    fn presses() {
        let at = |id: &str, t: i64, building: bool| Begun { id: id.into(), started_at: t, building };
        let now = 10 * limits::DAY_MS;
        assert_eq!(start(&[], now), Start::New);
        assert_eq!(start(&[at("a", now - 5, true)], now), Start::Replay("a".into()));
        let three = [at("a", now - 3000, false), at("b", now - 2000, false), at("c", now - 1000, false)];
        assert_eq!(start(&three, now), Start::Refused { next_at: now - 3000 + limits::DAY_MS });
        assert_eq!(start(&three, now - 3000 + limits::DAY_MS), Start::New, "the oldest left the day");
        // a building one is answered even past the day's count
        let mut busy = three.to_vec();
        busy.push(at("d", now - 10, true));
        assert_eq!(start(&busy, now), Start::Replay("d".into()));
        assert_eq!(retry_ms(0), 5_000);
        assert_eq!(retry_ms(3), 40_000);
        assert_eq!(retry_ms(30), 300_000);
    }

    /// Goal: every name becomes one safe path segment, and the layout's
    /// paths are under the one folder.
    #[test]
    fn layout() {
        assert_eq!(segment("notes"), "notes");
        assert_eq!(segment("a/b c"), "a%2Fb%20c");
        assert_eq!(segment(".."), "%2E%2E");
        assert_eq!(segment(""), "%");
        assert_eq!(segment("caf\u{e9}"), "caf%C3%A9");
        assert_eq!(table_path(NAME, "../x"), "fragment-export/fragments/todo--k3x9/database/..%2Fx.jsonl");
        assert_eq!(channel_path(NAME, "chat"), "fragment-export/fragments/todo--k3x9/channels/chat.jsonl");
        let id = format!("computer:{}", "ab".repeat(12));
        assert_eq!(save_path(&id, "/data"), format!("fragment-export/computers/{}/data.tar.zst", "ab".repeat(12)));
        assert_eq!(save_path(&id, "/data/work"), format!("fragment-export/computers/{}/data-work.tar.zst", "ab".repeat(12)));
        assert!(std::panic::catch_unwind(|| computer_dir("computer:../x")).is_err());
        assert!(std::panic::catch_unwind(|| fragment_dir("../x")).is_err());
        assert!(std::panic::catch_unwind(|| file_path(NAME, "../x")).is_err());
        assert!(std::panic::catch_unwind(|| channel_path(NAME, "../x")).is_err());
    }

    /// Goal: a chat's records read as a conversation: who said what and
    /// when, an attachment's file, each step, a question and its answer, a
    /// turn's error; the records a person never reads there add nothing.
    #[test]
    fn a_transcript() {
        let mut names = Names::default();
        names.0.insert("npub1person".into(), "paul@example.com".into());
        names.0.insert("npub1agent".into(), "Juniper".into());
        let rec = |channel, seq, principal, body: Value| (channel, seq, principal, body);
        let sha = "b".repeat(64);
        let recs = vec![
            rec("chat", 1, "npub1person", serde_json::json!({ "text": "hi @juniper", "attachments": [{ "sha256": sha, "name": "cat.png", "type": "image/png" }] })),
            rec("work", 1, "npub1agent", serde_json::json!({ "kind": "turn.start", "turn": "t" })),
            rec("work", 2, "npub1agent", serde_json::json!({ "kind": "turn.step", "category": "shell", "tool": "terminal", "args": "ls `-la`", "ok": false })),
            rec("work", 3, "npub1agent", serde_json::json!({ "kind": "turn.prompt", "text": "Run it?", "options": [{ "id": "once", "label": "Allow once" }, { "id": "deny", "label": "Deny" }] })),
            rec("work", 4, "npub1agent", serde_json::json!({ "kind": "turn.prompt.closed", "outcome": "answered", "option": "once", "by": "npub1person" })),
            rec("chat", 2, "npub1agent", serde_json::json!({ "text": "Hello!\n\nI am Juniper.", "turn": "t" })),
            rec("work", 5, "npub1agent", serde_json::json!({ "kind": "turn.end", "outcome": "error", "error": "the runtime failed" })),
            rec("work", 6, "npub1agent", serde_json::json!({ "kind": "turn.end", "outcome": "idle" })),
            rec("chat", 3, "npub1gone0000000000000000000000000000000000000000000000000000", serde_json::json!({ "kind": "stop" })),
            rec("chat", 4, "anon:ab", serde_json::json!("a bare string")),
            rec("chat", 5, "npub1person", serde_json::json!({ "kind": "draft-ui-thing" })),
        ];
        let mut out = transcript_head(NAME, Some("Garden"), 1_791_000_000_000);
        for (i, (channel, seq, principal, body)) in recs.iter().enumerate() {
            let said = Said { channel, seq: *seq, at: 1_791_000_000_000 + i as i64 * 60_000, principal, body };
            out.extend(transcript_part(&said, &names));
        }
        assert!(out.starts_with("# Garden\n"), "{out}");
        assert!(out.contains("**paul@example.com** · 2026-10-03 04:00 UTC\n\nhi @juniper\n"), "{out}");
        assert!(out.contains(&format!("Attached: [cat.png](blobs/{sha}) (image/png)")), "{out}");
        assert!(out.contains("\n> Juniper ran `` ls `-la` `` (it failed)"), "{out}");
        assert!(out.contains("> Juniper asked: Run it? (Allow once / Deny)"), "{out}");
        assert!(out.contains("> Answered `once` by paul@example.com"), "{out}");
        assert!(out.contains("**Juniper** · 2026-10-03 04:05 UTC\n\nHello!\n\nI am Juniper.\n"), "{out}");
        assert!(out.contains("> Juniper's turn ended: the runtime failed"), "{out}");
        assert!(out.contains("*npub1gone000…000000 stopped a turn"), "{out}");
        assert!(out.contains("**A visitor**"), "{out}");
        assert!(!out.contains("turn.start") && !out.contains("draft-ui-thing") && !out.contains("idle"), "{out}");
        // the order: time, then a message before work of the same moment
        let chat = serde_json::json!({});
        let a = Said { channel: "work", seq: 1, at: 5, principal: "x", body: &chat };
        let b = Said { channel: "chat", seq: 9, at: 5, principal: "x", body: &chat };
        assert!(b.order() < a.order());
    }

    /// Goal: times read as RFC 3339 and as a person reads them, in UTC.
    #[test]
    fn times() {
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339(1_791_000_000_000), "2026-10-03T04:00:00Z");
        assert_eq!(readable(1_791_000_059_999), "2026-10-03 04:00 UTC");
    }

    /// Goal: the manifest and a meta read back as written, and the README
    /// names every file the layout writes.
    #[test]
    fn what_it_says() {
        let m = Manifest {
            format: FORMAT.into(),
            version: VERSION,
            deployment: Deployment { id: "dev".into(), platform: "https://fragment.club".into() },
            made_at: 1,
            made: rfc3339(1),
            export: "0123456789abcdef".into(),
            person: Person { npub: "npub1p".into(), email: Some("p@example.com".into()) },
            agents: vec!["npub1a".into()],
            fragments: vec![Listed { name: NAME.into(), kind: fragment_proto::FragmentKind::Chat, title: None, owner: "npub1p".into(), path: fragment_dir(NAME) }],
            computers: vec![],
            shared_with_me: vec![Shared { name: "x--abcd".into(), kind: fragment_proto::FragmentKind::App, title: None, owner: Some("npub1o".into()), role: fragment_proto::Role::Viewer, member: "npub1p".into() }],
        };
        let bytes = json_file(&m);
        assert!(bytes.ends_with(b"}\n"));
        let back: Manifest = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(back, m);
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        assert!(v["sharedWithMe"][0]["role"] == "viewer" && v["madeAt"] == 1 && v["fragments"][0]["kind"] == "chat");
        for file in ["manifest.json", "meta.json", "files/", "schema.sql", ".jsonl", "transcript.md", "blobs/", "data.tar.zst", "data-work.tar.zst", "sharedWithMe"] {
            assert!(README.contains(file), "the README names {file}");
        }
    }
}
