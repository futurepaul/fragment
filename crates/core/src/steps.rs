//! A job's steps (docs/api.md, Jobs): what a job's body asks the platform
//! to do next, as platform.mjs names it (`{kind, args}`), the Workflow
//! carries it (cell/entry.mjs), and the supervisor performs it
//! (cell/src/jobs.rs, ai.rs). `Step` is the one list of step kinds. The
//! args come from the app's realm, so a step that does not decode is the
//! job's failure (it may catch it), never a default.

use std::collections::BTreeMap;

use serde::de::value::MapDeserializer;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use crate::effects::FileContent;

/// One step, decoded from its kind and args (`Step::from_parts`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "args")]
pub enum Step {
    /// `job.call(op, input)`: an operation of this fragment, as the run's principal.
    #[serde(rename = "call")]
    Call { op: String, input: Value },
    /// `job.fetch(url, init)`: the fragment's one way out.
    #[serde(rename = "fetch")]
    Fetch(Fetch),
    /// `job.publish(channel, body, kind)`.
    #[serde(rename = "publish")]
    Publish { channel: String, kind: String, body: Value },
    /// `job.push(who, payload)`.
    #[serde(rename = "push")]
    Push { who: String, payload: Value },
    /// `job.sleep(…)`: the Workflow sleeps itself (entry.mjs); the
    /// supervisor never performs one.
    #[serde(rename = "sleep")]
    Sleep { ms: u64 },
    #[serde(rename = "files.read")]
    FilesRead { path: String },
    #[serde(rename = "files.list")]
    FilesList { prefix: String },
    #[serde(rename = "files.stat")]
    FilesStat { path: String },
    #[serde(rename = "files.write")]
    FilesWrite(FileWrite),
    #[serde(rename = "files.remove")]
    FilesRemove {
        path: String,
        /// As `FileWrite::expect`.
        #[serde(default, deserialize_with = "present", skip_serializing_if = "Option::is_none")]
        expect: Option<Option<String>>,
    },
    #[serde(rename = "ai.text")]
    AiText(AiText),
    #[serde(rename = "ai.decide")]
    AiDecide(AiDecide),
    #[serde(rename = "ai.image")]
    AiImage(AiImage),
    /// `job.ai.video`: refused, whatever it asks, until videos run on
    /// Cloudflare (crate::media).
    #[serde(rename = "ai.video")]
    AiVideo {},
    /// `job.members()`: the fragment's members, as its page's `__members`
    /// lists them (the first added first).
    #[serde(rename = "members")]
    Members {},
    /// `job.people(ids)`: names for identities, as its page's `__people`
    /// answers (at most `PEOPLE_MAX`).
    #[serde(rename = "people")]
    People { ids: Vec<String> },
    /// `job.presence()`: who is here now, as its pages' presence lists
    /// hold them (`{id, principal, data}`, one a socket that shares any).
    #[serde(rename = "presence")]
    Presence {},
    /// `job.blob(sha256)`: one of the fragment's blobs (a page's upload, a
    /// chat's attachment), its size and its first `BLOB_READ_MAX_BYTES` as
    /// text when they are UTF-8 (`blob_read`).
    #[serde(rename = "blob")]
    Blob { sha256: String },
    /// `job.records(channel, {after, limit, turn})`: a page of the
    /// fragment's own records on one of the channels its fragment.json
    /// declares (`Records`).
    #[serde(rename = "records")]
    Records(Records),
    /// `job.owner.fragments()`: the fragment's owner's other fragments,
    /// each with the described operations its owner may call there. Only a
    /// blessed template that declares the `owner` capability takes the
    /// `owner` steps (`crate::access::owner_lent`).
    #[serde(rename = "owner.fragments")]
    OwnerFragments {},
    /// `job.owner.call(fragment, op, input)`: one of those operations, as
    /// the owner (their role there), keyed by the run and the step
    /// (`owner_call_id`).
    #[serde(rename = "owner.call")]
    OwnerCall { fragment: String, op: String, input: Value },
}

/// The identities one `job.people` step names: a page's `__people` limit.
pub const PEOPLE_MAX: usize = 64;

/// The fragments one `job.owner.fragments` step asks, at most (by name).
pub const OWNER_FRAGMENTS_MAX: usize = 64;

/// An `owner.call` step's arguments, checked: a fragment's full name, an
/// operation's, and its input, an object (none is `{}`), as a tool's
/// arguments are (`crate::mcp::call_of`). Answers the input to call with.
pub fn owner_call(fragment: &str, op: &str, input: &Value) -> Result<Value, String> {
    if !fragment_proto::valid_fragment_name(fragment) {
        return Err(format!("{fragment:?} is not a fragment's name (<label>.<username>)"));
    }
    if !fragment_proto::valid_op_name(op) {
        return Err(format!("{op:?} is not an operation's name"));
    }
    crate::mcp::call_of(input).map_err(|_| "an operation's input here is an object".to_string())
}

/// The operation id an `owner.call` step calls with: its run's name beyond
/// its fragment (`run_key`: the fragment, its life, the run) and the step,
/// the same on every try and replay of the step, so the fragment it calls
/// applies it once. `job:` ids are the platform's alone: no caller of the
/// API may choose one.
pub fn owner_call_id(run_key: &str, index: u32) -> String {
    let id = format!("job:{run_key}-s{index}");
    assert!(fragment_proto::valid_op_id(&id), "an owner call's id is an operation id: {id}");
    id
}

/// What one `job.blob` step reads of a blob, at most.
pub const BLOB_READ_MAX_BYTES: usize = 64 * 1024;

/// `job.blob`'s answer for a blob of `size` bytes whose first bytes are
/// `head` (at most `BLOB_READ_MAX_BYTES`): `{sha256, size, text, cut}`.
/// `text` is the head as UTF-8, less a last character the cut split, or
/// null when it is not text (bytes that are not UTF-8, or a NUL); `cut`,
/// whether the blob goes on past its head.
pub fn blob_read(sha256: &str, size: u64, head: &[u8]) -> Value {
    assert!(head.len() <= BLOB_READ_MAX_BYTES, "a blob's head is at most {BLOB_READ_MAX_BYTES} bytes");
    let cut = size > head.len() as u64;
    let text = match std::str::from_utf8(head) {
        Ok(t) => Some(t),
        // only the cut may leave a character unfinished, at the very end
        Err(e) if cut && e.error_len().is_none() => std::str::from_utf8(&head[..e.valid_up_to()]).ok(),
        Err(_) => None,
    };
    let text = text.filter(|t| !t.contains('\0'));
    serde_json::json!({ "sha256": sha256, "size": size, "text": text, "cut": cut })
}

/// The records one `job.records` page answers, at most (and unless it asks
/// for fewer).
pub const RECORDS_PAGE_MAX: usize = 200;
/// What one page's records take, at most, about (`PageRecord::size`): the
/// record past it starts the next page. A record's body is at most
/// `limits::RECORD_BODY_MAX_BYTES`, so every page holds one.
pub const RECORDS_PAGE_MAX_BYTES: usize = 512 * 1024;
/// The seqs one page looks at past `after`, at most (`records_window`).
/// With a `turn`, the records it passes over count too, so a page reads a
/// bounded stretch of the channel however few match; a channel people post
/// to keeps this many records, so one page looks over all of it.
pub const RECORDS_SCAN_MAX: i64 = fragment_proto::limits::POSTED_KEPT;
/// A `turn` to match, at most (an agent's turn id is 24 hex).
pub const RECORDS_TURN_MAX_BYTES: usize = 128;

const _: () = assert!(fragment_proto::limits::RECORD_BODY_MAX_BYTES < RECORDS_PAGE_MAX_BYTES);

/// `job.records(channel, {after, limit, turn})`, as the job gave it
/// (`Records::checked` reads it).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Records {
    pub channel: String,
    /// The records after this seq (the channel's first kept one on, unless named).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<i64>,
    /// At most this many (`RECORDS_PAGE_MAX` unless named).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
    /// Only the records whose body's `turn` is this string (an agent's turn
    /// on a chat's `work`: docs/chat-records.md).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn: Option<String>,
}

/// A `records` step's ask, checked: a channel's name (one fragment.json
/// declares: the cell asks), where to start, how many, and the turn to match.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordsAsk {
    pub channel: String,
    pub after: i64,
    pub limit: usize,
    pub turn: Option<String>,
}

impl Records {
    /// What it asks, or why it does not fit: a channel's name that is not
    /// the platform's own (`events`, `ops`), a seq of 0 or more, a limit of
    /// 1 to `RECORDS_PAGE_MAX`, a turn of 1 to `RECORDS_TURN_MAX_BYTES`.
    pub fn checked(self) -> Result<RecordsAsk, String> {
        if !fragment_proto::valid_channel_name(&self.channel) {
            return Err(format!("{:?} is not a channel's name (^[a-z][a-z0-9_-]{{0,63}}$)", self.channel));
        }
        if fragment_proto::BUILTIN_CHANNELS.contains(&self.channel.as_str()) {
            return Err(format!("{} is the platform's: a job reads the channels its fragment.json declares", self.channel));
        }
        let after = self.after.unwrap_or(0);
        if after < 0 {
            return Err(format!("after is a record's seq, 0 or more, not {after}"));
        }
        let limit = self.limit.unwrap_or(RECORDS_PAGE_MAX);
        if !(1..=RECORDS_PAGE_MAX).contains(&limit) {
            return Err(format!("limit is 1 to {RECORDS_PAGE_MAX}, not {limit}"));
        }
        if let Some(turn) = &self.turn {
            if turn.is_empty() || turn.len() > RECORDS_TURN_MAX_BYTES {
                return Err(format!("turn is 1 to {RECORDS_TURN_MAX_BYTES} bytes"));
            }
        }
        Ok(RecordsAsk { channel: self.channel, after, limit, turn: self.turn })
    }
}

/// The seqs a `records` page looks at, `(from, to]`: past `after`, and past
/// what the channel no longer keeps (its oldest go first, so the rest run on
/// from its first kept record), at most `RECORDS_SCAN_MAX` of them, up to
/// its newest. `kept` is the channel's first and last kept seq, `None` when
/// it keeps none; the answer is `None` when no record is past `after`.
pub fn records_window(after: i64, kept: Option<(i64, i64)>) -> Option<(i64, i64)> {
    assert!(after >= 0, "a page starts at a seq: {after}");
    let (first, last) = kept?;
    assert!(0 < first && first <= last, "a channel's records number from 1, in order: {first}..{last}");
    let from = after.max(first - 1);
    (from < last).then(|| (from, last.min(from.saturating_add(RECORDS_SCAN_MAX))))
}

/// One record of a `records` page: `{seq, at, principal, kind, body}`, its
/// body the JSON the cell stored, as a channel's reader gets it.
#[derive(Debug, Clone, Serialize)]
pub struct PageRecord {
    pub seq: i64,
    pub at: i64,
    pub principal: String,
    pub kind: String,
    pub body: Box<serde_json::value::RawValue>,
}

impl PageRecord {
    /// About what it takes of its page's JSON.
    fn size(&self) -> usize {
        self.body.get().len() + self.principal.len() + self.kind.len() + 64
    }
}

/// `job.records`' answer: `{records, next}`, where `next` is the next
/// page's `after`, or `null` when this page reached the channel's newest
/// record.
#[derive(Debug, Clone, Serialize)]
pub struct RecordsPage {
    pub records: Vec<PageRecord>,
    pub next: Option<i64>,
}

impl RecordsPage {
    /// A page past the channel's newest record (or of a channel with none).
    pub fn end() -> RecordsPage {
        RecordsPage { records: vec![], next: None }
    }
}

/// Fills a page from the records of a window `(_, to]` that match its ask,
/// taken one at a time in seq order (`take`), so a reader stops reading
/// where the page is full: at `limit` records, or at the record that would
/// take it past `RECORDS_PAGE_MAX_BYTES`. `last` is the channel's newest seq.
#[derive(Debug)]
pub struct Pager {
    limit: usize,
    to: i64,
    last: i64,
    records: Vec<PageRecord>,
    bytes: usize,
    full: bool,
}

impl Pager {
    pub fn new(limit: usize, to: i64, last: i64) -> Pager {
        assert!((1..=RECORDS_PAGE_MAX).contains(&limit), "a page holds 1 to {RECORDS_PAGE_MAX} records: {limit}");
        assert!(to <= last, "a window ends at the channel's newest record at the latest: {to} > {last}");
        Pager { limit, to, last, records: Vec::new(), bytes: 0, full: false }
    }

    /// Takes the next record; `false` once the page is full, this record
    /// left for the next.
    pub fn take(&mut self, record: PageRecord) -> bool {
        assert!(!self.full, "a full page takes no more records");
        assert!(record.seq <= self.to, "record {} is past its window's end {}", record.seq, self.to);
        assert!(self.records.last().is_none_or(|r| r.seq < record.seq), "a page's records come in seq order");
        let size = record.size();
        // its first record always fits (a body is at most RECORD_BODY_MAX_BYTES)
        if self.records.len() == self.limit || (!self.records.is_empty() && self.bytes + size > RECORDS_PAGE_MAX_BYTES) {
            self.full = true;
            return false;
        }
        self.bytes += size;
        self.records.push(record);
        true
    }

    /// The page: the next one starts after its last record when it filled
    /// (a record was left for it), else after the window it looked over.
    pub fn page(self) -> RecordsPage {
        let read_to = match self.records.last() {
            Some(r) if self.full => r.seq,
            _ => self.to,
        };
        assert!(!self.full || read_to < self.last, "a page that filled left a record for the next");
        RecordsPage { next: (read_to < self.last).then_some(read_to), records: self.records }
    }
}

/// `job.fetch`'s request. Header values may name secrets as `{{NAME}}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Fetch {
    pub url: String,
    /// As the job gave it (the supervisor accepts the usual methods in any case).
    pub method: String,
    pub headers: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
}

/// `job.files.write(path, content, {expect})`: one commit to `main`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "FileWriteFields", into = "FileWriteFields")]
pub struct FileWrite {
    pub path: String,
    pub content: FileContent,
    /// `None`: no check. `Some(None)`: the file must not exist. `Some(Some(sha))`:
    /// it must be that blob (`stat`'s `sha`) at the head the commit lands on.
    pub expect: Option<Option<String>>,
}

/// A write as it travels: the content is one of two keys beside the path.
#[derive(Serialize, Deserialize)]
struct FileWriteFields {
    path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    base64: Option<String>,
    #[serde(default, deserialize_with = "present", skip_serializing_if = "Option::is_none")]
    expect: Option<Option<String>>,
}

impl TryFrom<FileWriteFields> for FileWrite {
    type Error = String;

    fn try_from(f: FileWriteFields) -> Result<FileWrite, String> {
        let content = match (f.text, f.base64) {
            (Some(text), None) => FileContent::Text(text),
            (None, Some(b64)) => FileContent::Base64(b64),
            _ => return Err("a file's content is text or base64".into()),
        };
        Ok(FileWrite { path: f.path, content, expect: f.expect })
    }
}

impl From<FileWrite> for FileWriteFields {
    fn from(w: FileWrite) -> FileWriteFields {
        let (text, base64) = match w.content {
            FileContent::Text(t) => (Some(t), None),
            FileContent::Base64(b) => (None, Some(b)),
        };
        FileWriteFields { path: w.path, text, base64, expect: w.expect }
    }
}

/// A key that is present, even as `null`, is `Some`: `expect: null` says
/// the file must not exist, while no `expect` checks nothing.
fn present<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Option<String>>, D::Error> {
    Option::<String>::deserialize(d).map(Some)
}

/// `job.ai.text`: chat completions through the platform's model route
/// (cell/src/models.rs). Keys the platform does not read are ignored.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AiText {
    /// A tier (`fragment_proto::Tier`: `cheap` unless named), never a model id.
    pub model: Option<String>,
    /// The conversation, OpenAI's messages as given (an assistant's
    /// `tool_calls` and `role: "tool"` results among them); without it,
    /// `prompt` is one user message.
    pub messages: Option<Vec<Value>>,
    pub prompt: Option<String>,
    /// GLM's reasoning control (`low` or `high`; anything else is `low`:
    /// crate::models).
    pub reasoning_effort: Option<String>,
    pub max_tokens: Option<u64>,
    /// Functions the model may call (crate::models bounds them).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<Tool>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<ToolChoice>,
    /// The call streams, and its text so far is that channel's draft.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draft: Option<Draft>,
    /// Whose model the payer's choice gives it (crate::providers::Role):
    /// `chat` or `memory`; unnamed, its tier's (`cheap` memory, else chat).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
}

/// A function the model may call, in OpenAI's shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tool {
    #[serde(rename = "type")]
    pub kind: FunctionKind,
    pub function: Function,
}

/// The one kind of tool: `"function"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FunctionKind {
    Function,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Function {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Its arguments' JSON Schema.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parameters: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strict: Option<bool>,
}

/// OpenAI's `tool_choice`: a mode, or one function by name.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ToolChoice {
    Mode(ToolMode),
    Named {
        #[serde(rename = "type")]
        kind: FunctionKind,
        function: Called,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolMode {
    None,
    Auto,
    Required,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Called {
    pub name: String,
}

/// Where a text step's draft goes: one of the app's channels, under a turn
/// (`PUT …/channels/<channel>/draft`'s `{turn, text}`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Draft {
    pub channel: String,
    pub turn: String,
}

/// `job.ai.decide`: Clef, Workers AI's decision model (crate::decide), as
/// its catalog's input: typed questions about a state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AiDecide {
    pub model: Clef,
    /// What is decided about: text, or JSON (an object or an array).
    pub state: Value,
    /// By id, each answered under its id.
    pub questions: BTreeMap<String, Question>,
    /// `data:` URLs of PNG, JPEG or WebP images, shown before the state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub images: Option<Vec<String>>,
}

/// Clef's two sizes, as its input's `model` names them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Clef {
    #[serde(rename = "clef")]
    Clef,
    #[serde(rename = "clef-flash")]
    Flash,
}

/// A question, as Clef's input types it: yes or no (`noul`), one option of
/// a set (`choice`), or a level of an ordered rubric (`score`).
/// `instructions` is the question: text, or JSON holding it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase", deny_unknown_fields)]
pub enum Question {
    Noul {
        instructions: Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        criteria: Option<NoulCriteria>,
    },
    /// `criteria`: each option's id, and what it means (`null`: nothing to say).
    Choice { instructions: Value, criteria: BTreeMap<String, Value> },
    /// `criteria`: the levels, lowest first.
    Score { instructions: Value, criteria: Vec<Value> },
}

/// What a yes and a no mean.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NoulCriteria {
    #[serde(rename = "true", default, skip_serializing_if = "Option::is_none")]
    pub yes: Option<Value>,
    #[serde(rename = "false", default, skip_serializing_if = "Option::is_none")]
    pub no: Option<Value>,
}

/// `job.ai.image`: a JPEG drawn by the one image model (crate::media),
/// written to `main` at `path`. A key it does not take (a model, an
/// aspect ratio) is refused, not ignored: there is no other model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AiImage {
    pub prompt: String,
    pub path: String,
    /// Diffusion steps (crate::media: 4 unless named, at most 8).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub steps: Option<u32>,
}

impl Step {
    /// A step from the kind and args the Workflow carries, decoded without
    /// rebuilding their JSON; the error says what does not fit.
    pub fn from_parts(kind: &str, args: Value) -> Result<Step, String> {
        let parts = [("kind", Value::String(kind.to_string())), ("args", args)];
        let step = Step::deserialize(MapDeserializer::<_, serde_json::Error>::new(parts.into_iter())).map_err(|e| e.to_string())?;
        assert_eq!(step.kind(), kind, "a step decodes as the kind it names");
        Ok(step)
    }

    /// Its kind, as platform.mjs names it and its result carries it back.
    pub fn kind(&self) -> &'static str {
        match self {
            Step::Call { .. } => "call",
            Step::Fetch(_) => "fetch",
            Step::Publish { .. } => "publish",
            Step::Push { .. } => "push",
            Step::Sleep { .. } => "sleep",
            Step::FilesRead { .. } => "files.read",
            Step::FilesList { .. } => "files.list",
            Step::FilesStat { .. } => "files.stat",
            Step::FilesWrite(_) => "files.write",
            Step::FilesRemove { .. } => "files.remove",
            Step::AiText(_) => "ai.text",
            Step::AiDecide(_) => "ai.decide",
            Step::AiImage(_) => "ai.image",
            Step::AiVideo {} => "ai.video",
            Step::Members {} => "members",
            Step::People { .. } => "people",
            Step::Presence {} => "presence",
            Step::Blob { .. } => "blob",
            Step::Records(_) => "records",
            Step::OwnerFragments {} => "owner.fragments",
            Step::OwnerCall { .. } => "owner.call",
        }
    }
}

/// A step as the Workflow takes it from the supervisor (entry.mjs):
/// `{index, kind, args}`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NextStep {
    pub index: u32,
    #[serde(flatten)]
    pub step: Step,
}

/// A step's result as the Workflow records it and the job's body reads it
/// back (platform.mjs, `Job`): `{kind, value}`, or `{kind, error}` for a
/// failure the job may catch.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StepResult {
    pub kind: String,
    #[serde(flatten)]
    pub outcome: StepOutcome,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StepOutcome {
    Value(Value),
    Error(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn step(kind: &str, args: Value) -> Result<Step, String> {
        Step::from_parts(kind, args)
    }

    /// One of each kind, with the args platform.mjs builds for it.
    fn every_kind() -> Vec<(&'static str, Value)> {
        vec![
            ("call", json!({ "op": "save", "input": { "texts": [] } })),
            ("fetch", json!({ "url": "https://example.com/", "method": "GET", "headers": { "authorization": "Bearer {{KEY}}" } })),
            ("publish", json!({ "channel": "feed", "kind": "digest", "body": { "n": 1 } })),
            ("push", json!({ "who": "*", "payload": { "title": "hi" } })),
            ("sleep", json!({ "ms": 20000 })),
            ("files.read", json!({ "path": "log.txt" })),
            ("files.list", json!({ "prefix": "" })),
            ("files.stat", json!({ "path": "log.txt" })),
            ("files.write", json!({ "path": "log.txt", "text": "a\n", "expect": null })),
            ("files.remove", json!({ "path": "log.txt" })),
            ("ai.text", json!({ "model": "medium", "prompt": "hi", "reasoning_effort": "low", "max_tokens": 100 })),
            (
                "ai.text",
                json!({
                    "messages": [{ "role": "user", "content": "hi" }, { "role": "assistant", "content": "", "tool_calls": [{ "id": "c1", "type": "function", "function": { "name": "zoom", "arguments": "{}" } }] }, { "role": "tool", "tool_call_id": "c1", "content": "x" }],
                    "tools": [{ "type": "function", "function": { "name": "zoom", "description": "d", "parameters": { "type": "object" } } }],
                    "tool_choice": { "type": "function", "function": { "name": "zoom" } },
                    "draft": { "channel": "log", "turn": "turn:t_1" },
                }),
            ),
            (
                "ai.decide",
                json!({
                    "model": "clef-flash", "state": { "title": "garden" },
                    "questions": {
                        "a": { "type": "noul", "instructions": "Plants?", "criteria": { "true": "about plants" } },
                        "b": { "type": "choice", "instructions": "Which?", "criteria": { "x": "an x", "y": null } },
                        "c": { "type": "score", "instructions": "How much?", "criteria": ["none", "some"] },
                    },
                    "images": ["data:image/png;base64,iVBORw0KGgo="],
                }),
            ),
            ("ai.image", json!({ "prompt": "a cat", "path": "cat.jpg", "steps": 6 })),
            ("ai.video", json!({})),
            ("members", json!({})),
            ("people", json!({ "ids": ["id:00112233445566778899aabbccddeeff"] })),
            ("presence", json!({})),
            ("blob", json!({ "sha256": "ab".repeat(32) })),
            ("records", json!({ "channel": "work", "after": 12, "limit": 50, "turn": "0123456789abcdef01234567" })),
            ("owner.fragments", json!({})),
            ("owner.call", json!({ "fragment": "todo.paul", "op": "add", "input": { "text": "milk" } })),
        ]
    }

    #[test]
    fn every_kind_platform_mjs_sends_decodes_as_itself() {
        let kinds = every_kind();
        assert_eq!(kinds.len(), 22, "a new kind of step is added here too");
        for (kind, args) in kinds {
            let s = step(kind, args.clone()).unwrap_or_else(|e| panic!("{kind}: {e}"));
            assert_eq!(s.kind(), kind);
            // what the supervisor would hand on (the advance's call step) reads back the same
            let wire = serde_json::to_value(&s).unwrap();
            assert_eq!(wire["kind"], kind);
            assert_eq!(step(kind, wire["args"].clone()), Ok(s), "{kind} round trips");
        }
    }

    #[test]
    fn the_args_are_typed() {
        assert_eq!(
            step("call", json!({ "op": "save", "input": 7 })),
            Ok(Step::Call { op: "save".into(), input: json!(7) })
        );
        let Ok(Step::Fetch(f)) = step("fetch", json!({ "url": "https://x/", "method": "post", "headers": {}, "body": "{}" })) else { panic!() };
        assert_eq!((f.method.as_str(), f.body.as_deref()), ("post", Some("{}")));
        let Ok(Step::AiImage(i)) = step("ai.image", json!({ "prompt": "p", "path": "a.jpg" })) else { panic!() };
        assert_eq!(i.steps, None, "the model's own default unless named");
        assert_eq!(
            step("ai.video", json!({ "prompt": "waves", "path": "v.mp4", "duration": 6 })),
            Ok(Step::AiVideo {}),
            "a video's args are not read: it is refused whatever it asks"
        );
        let Ok(Step::AiText(t)) = step("ai.text", json!({ "model": "cheap", "messages": [{ "role": "user", "content": "hi" }], "extra": true })) else { panic!() };
        assert_eq!((t.prompt, t.messages.map(|m| m.len())), (None, Some(1)), "a key the platform does not read is ignored");
        let Ok(Step::AiText(t)) = step("ai.text", json!({ "prompt": "hi" })) else { panic!() };
        assert_eq!(t.model, None, "the tier is the default unless named");
    }

    #[test]
    fn a_step_that_does_not_fit_says_why() {
        let refused = |kind: &str, args: Value, says: &str| {
            let e = step(kind, args.clone()).expect_err(&format!("{kind} {args}"));
            assert!(e.contains(says), "{kind} {args}: {e}");
        };
        refused("ai.foo", json!({}), "unknown variant `ai.foo`");
        refused("call", json!({ "input": {} }), "missing field `op`");
        refused("call", json!({ "op": "save" }), "missing field `input`");
        refused("ai.text", json!({ "model": "cheap", "max_tokens": "100" }), "invalid type");
        refused("ai.text", json!({ "model": 7 }), "invalid type");
        refused("ai.text", json!({ "model": "cheap", "reasoning_effort": true }), "invalid type");
        let tool = |t: Value| json!({ "prompt": "p", "tools": [t] });
        refused("ai.text", tool(json!({ "type": "retrieval", "function": { "name": "f" } })), "unknown variant `retrieval`");
        refused("ai.text", tool(json!({ "type": "function" })), "missing field `function`");
        refused("ai.text", tool(json!({ "type": "function", "function": { "name": "f", "code": "x" } })), "unknown field `code`");
        refused("ai.text", json!({ "prompt": "p", "tool_choice": "sometimes" }), "did not match any variant");
        refused("ai.text", json!({ "prompt": "p", "draft": { "channel": "log" } }), "missing field `turn`");
        refused("ai.decide", json!({ "model": "clef-pro", "state": "s", "questions": {} }), "unknown variant `clef-pro`");
        refused("ai.decide", json!({ "model": "clef", "questions": {} }), "missing field `state`");
        refused("ai.decide", json!({ "model": "clef", "state": "s", "questions": { "a": { "type": "rank", "instructions": "?" } } }), "unknown variant `rank`");
        refused("ai.decide", json!({ "model": "clef", "state": "s", "questions": { "a": { "type": "choice", "instructions": "?" } } }), "missing field `criteria`");
        refused("ai.decide", json!({ "model": "clef", "state": "s", "questions": { "a": { "type": "score", "instructions": "?", "criteria": {} } } }), "invalid type");
        refused("ai.decide", json!({ "model": "clef", "state": "s", "questions": {}, "temperature": 0 }), "unknown field `temperature`");
        refused("ai.image", json!({ "prompt": "p" }), "missing field `path`");
        refused("ai.image", json!({ "prompt": "p", "path": "a.jpg", "model": "google/gemini-3.1-flash-lite-image" }), "unknown field `model`");
        refused("ai.image", json!({ "prompt": "p", "path": "a.jpg", "steps": -1 }), "invalid value");
        refused("ai.video.start", json!({ "prompt": "p" }), "unknown variant `ai.video.start`");
        refused("fetch", json!({ "url": "https://x/", "method": "GET", "headers": { "n": 1 } }), "invalid type");
        refused("publish", json!({ "channel": "feed", "body": {} }), "missing field `kind`");
        refused("push", json!({ "payload": {} }), "missing field `who`");
        refused("files.read", json!("log.txt"), "invalid type");
        refused("people", json!({}), "missing field `ids`");
        refused("people", json!({ "ids": "id:x" }), "invalid type");
        refused("people", json!({ "ids": [7] }), "invalid type");
        refused("owner.call", json!({ "fragment": "todo.paul", "input": {} }), "missing field `op`");
        refused("owner.call", json!({ "op": "add", "input": {} }), "missing field `fragment`");
        refused("owner.call", json!({ "fragment": "todo.paul", "op": "add" }), "missing field `input`");
        refused("owner.call", json!({ "fragment": 7, "op": "add", "input": {} }), "invalid type");
        refused("records", json!({ "after": 1 }), "missing field `channel`");
        refused("records", json!({ "channel": "work", "after": "1" }), "invalid type");
        refused("records", json!({ "channel": "work", "after": 1.5 }), "invalid type");
        refused("records", json!({ "channel": "work", "limit": -1 }), "invalid value");
        refused("records", json!({ "channel": "work", "turn": 7 }), "invalid type");
        refused("records", json!({ "channel": "work", "before": 9 }), "unknown field `before`");
    }

    /// Goal: an owner call names a fragment and an operation as the API
    /// does, and takes an object, as a tool does; its id is the same on
    /// every try of its step, and no caller of the API could have chosen it.
    /// Method: valid and invalid names and inputs; the id's form.
    #[test]
    fn an_owner_call_names_a_fragment_and_an_operation_and_takes_an_object() {
        assert_eq!(owner_call("todo.paul", "add", &json!({ "text": "milk" })), Ok(json!({ "text": "milk" })));
        assert_eq!(owner_call("todo.paul", "list", &Value::Null), Ok(json!({})), "none is the empty object");
        for (fragment, op, input, says) in [
            ("todo", "add", json!({}), "not a fragment's name"),
            ("Todo.paul", "add", json!({}), "not a fragment's name"),
            ("../todo.paul", "add", json!({}), "not a fragment's name"),
            ("todo.paul", "Add", json!({}), "not an operation's name"),
            ("todo.paul", "add/x", json!({}), "not an operation's name"),
            ("todo.paul", "", json!({}), "not an operation's name"),
            ("todo.paul", "add", json!("milk"), "an object"),
            ("todo.paul", "add", json!([1]), "an object"),
        ] {
            let e = owner_call(fragment, op, &input).expect_err(&format!("{fragment} {op} {input}"));
            assert!(e.contains(says), "{fragment} {op} {input}: {e}");
        }
        let id = owner_call_id("0123456789abcdef0123-1700000000000-r12", 4);
        assert_eq!(id, "job:0123456789abcdef0123-1700000000000-r12-s4");
        assert_eq!(id, owner_call_id("0123456789abcdef0123-1700000000000-r12", 4), "a step's every try calls with the same id");
        assert_ne!(id, owner_call_id("0123456789abcdef0123-1700000000000-r12", 5));
        assert!(fragment_proto::valid_op_id(&id) && id.starts_with("job:"), "an id the API refuses its callers (ops.rs `call_op`)");
    }

    #[test]
    fn a_writes_content_is_text_or_base64() {
        let write = |args: Value| match step("files.write", args) {
            Ok(Step::FilesWrite(w)) => Ok(w),
            Ok(other) => panic!("{other:?}"),
            Err(e) => Err(e),
        };
        let w = write(json!({ "path": "a", "text": "hi" })).unwrap();
        assert_eq!((w.content.clone(), w.expect.clone()), (FileContent::Text("hi".into()), None));
        assert_eq!(w.content.into_bytes(), Ok(b"hi".to_vec()));
        let w = write(json!({ "path": "a", "base64": "AAE=" })).unwrap();
        assert_eq!(w.content.into_bytes(), Ok(vec![0, 1]));
        assert!(write(json!({ "path": "a", "text": "hi", "base64": "AAE=" })).unwrap_err().contains("text or base64"));
        assert!(write(json!({ "path": "a" })).unwrap_err().contains("text or base64"));
    }

    #[test]
    fn expect_null_is_not_the_same_as_no_expect() {
        let expect = |args: Value| match step("files.remove", args) {
            Ok(Step::FilesRemove { expect, .. }) => expect,
            other => panic!("{other:?}"),
        };
        assert_eq!(expect(json!({ "path": "a" })), None, "no check");
        assert_eq!(expect(json!({ "path": "a", "expect": null })), Some(None), "must not exist");
        assert_eq!(expect(json!({ "path": "a", "expect": "abc" })), Some(Some("abc".into())), "must be that blob");
        let Ok(Step::FilesWrite(w)) = step("files.write", json!({ "path": "a", "text": "", "expect": null })) else { panic!() };
        assert_eq!(w.expect, Some(None));
        assert!(step("files.remove", json!({ "path": "a", "expect": 5 })).is_err(), "an expect that is no sha is refused, not read as absent");
    }

    #[test]
    fn the_workflow_takes_a_step_with_its_index() {
        let next = NextStep { index: 0, step: Step::Call { op: "save".into(), input: json!({ "n": 1 }) } };
        assert_eq!(serde_json::to_value(&next).unwrap(), json!({ "index": 0, "kind": "call", "args": { "op": "save", "input": { "n": 1 } } }));
    }

    #[test]
    fn a_result_is_a_value_or_an_error() {
        let value = StepResult { kind: "call".into(), outcome: StepOutcome::Value(json!({ "n": 1 })) };
        assert_eq!(serde_json::to_value(&value).unwrap(), json!({ "kind": "call", "value": { "n": 1 } }));
        let error = StepResult { kind: "fetch".into(), outcome: StepOutcome::Error("no".into()) };
        assert_eq!(serde_json::to_value(&error).unwrap(), json!({ "kind": "fetch", "error": "no" }));
        let slept: StepResult = serde_json::from_value(json!({ "kind": "sleep", "value": null })).unwrap();
        assert_eq!(slept.outcome, StepOutcome::Value(Value::Null));
        assert!(serde_json::from_value::<StepResult>(json!({ "kind": "call" })).is_err(), "a result names its outcome");
    }

    /// A blob's head is text when it is UTF-8 with no NUL, less a last
    /// character its cut split; else null, cut or whole.
    #[test]
    fn a_blob_reads_as_text_when_it_is() {
        let sha = "ab".repeat(32);
        let read = |size: u64, head: &[u8]| blob_read(&sha, size, head);
        assert_eq!(read(5, b"hello"), json!({ "sha256": sha, "size": 5, "text": "hello", "cut": false }));
        let snow = "a☃".as_bytes();
        assert_eq!(read(100, &snow[..3])["text"], "a", "a character the cut split is dropped");
        assert_eq!(read(100, &snow[..3])["cut"], true);
        assert_eq!(read(3, &snow[..3])["text"], Value::Null, "an unfinished character with nothing cut is no text");
        assert_eq!(read(4, &[0x61, 0xff, 0x62, 0x63])["text"], Value::Null, "bytes that are not UTF-8");
        assert_eq!(read(3, b"a\0b")["text"], Value::Null, "a NUL is binary's");
        assert_eq!(read(0, b""), json!({ "sha256": sha, "size": 0, "text": "", "cut": false }));
    }

    fn ask(args: Value) -> Result<RecordsAsk, String> {
        match step("records", args)? {
            Step::Records(r) => r.checked(),
            other => panic!("{other:?}"),
        }
    }

    /// Goal: a records step names one of the app's channels by a name the
    /// API takes, starts at a seq, asks for at most a page, and matches a
    /// turn of a turn id's size; what it leaves out is the first page, whole.
    /// Method: valid asks, each default, and each way an ask is refused.
    #[test]
    fn a_records_step_asks_for_an_app_channels_page() {
        assert_eq!(ask(json!({ "channel": "work" })), Ok(RecordsAsk { channel: "work".into(), after: 0, limit: RECORDS_PAGE_MAX, turn: None }));
        assert_eq!(
            ask(json!({ "channel": "work", "after": 41, "limit": 1, "turn": "ab12" })),
            Ok(RecordsAsk { channel: "work".into(), after: 41, limit: 1, turn: Some("ab12".into()) })
        );
        assert_eq!(ask(json!({ "channel": "work", "after": null, "limit": null, "turn": null })).unwrap().limit, RECORDS_PAGE_MAX, "null is unnamed");
        assert_eq!(ask(json!({ "channel": "work", "limit": RECORDS_PAGE_MAX })).unwrap().limit, RECORDS_PAGE_MAX);
        let long = "a".repeat(RECORDS_TURN_MAX_BYTES);
        assert_eq!(ask(json!({ "channel": "work", "turn": long })).unwrap().turn.map(|t| t.len()), Some(RECORDS_TURN_MAX_BYTES));
        for (args, says) in [
            (json!({ "channel": "Work" }), "not a channel's name"),
            (json!({ "channel": "" }), "not a channel's name"),
            (json!({ "channel": "../work" }), "not a channel's name"),
            (json!({ "channel": "events" }), "the platform's"),
            (json!({ "channel": "ops" }), "the platform's"),
            (json!({ "channel": "work", "after": -1 }), "0 or more"),
            (json!({ "channel": "work", "limit": 0 }), "limit is 1 to 200"),
            (json!({ "channel": "work", "limit": RECORDS_PAGE_MAX + 1 }), "limit is 1 to 200"),
            (json!({ "channel": "work", "turn": "" }), "turn is 1 to"),
            (json!({ "channel": "work", "turn": "a".repeat(RECORDS_TURN_MAX_BYTES + 1) }), "turn is 1 to"),
        ] {
            let e = ask(args.clone()).expect_err(&args.to_string());
            assert!(e.contains(says), "{args}: {e}");
        }
    }

    /// Goal: a page looks at a bounded stretch of seqs, from where it was
    /// asked or the channel's first kept record, whichever is later, up to
    /// its newest. Method: windows of an empty channel, one that dropped its
    /// oldest records, one longer than a scan, and an ask past the end.
    #[test]
    fn a_records_page_looks_over_a_bounded_window() {
        assert_eq!(records_window(0, None), None, "a channel with no records");
        assert_eq!(records_window(0, Some((1, 5))), Some((0, 5)));
        assert_eq!(records_window(3, Some((1, 5))), Some((3, 5)));
        assert_eq!(records_window(5, Some((1, 5))), None, "nothing past the newest");
        assert_eq!(records_window(9, Some((1, 5))), None);
        assert_eq!(records_window(0, Some((40_001, 50_000))), Some((40_000, 50_000)), "the dropped seqs are not looked at");
        assert_eq!(records_window(45_000, Some((40_001, 50_000))), Some((45_000, 50_000)));
        assert_eq!(records_window(0, Some((1, 25_000))), Some((0, RECORDS_SCAN_MAX)), "at most a scan's worth");
        assert_eq!(records_window(RECORDS_SCAN_MAX, Some((1, 25_000))), Some((RECORDS_SCAN_MAX, 2 * RECORDS_SCAN_MAX)));
        assert_eq!(records_window(i64::MAX - 1, Some((1, i64::MAX))), Some((i64::MAX - 1, i64::MAX)), "no overflow at the end");
    }

    fn record(seq: i64, body: Value) -> PageRecord {
        PageRecord {
            seq,
            at: 1_700_000_000_000 + seq,
            principal: "id:00112233445566778899aabbccddeeff".into(),
            kind: "message".into(),
            body: serde_json::value::to_raw_value(&body).unwrap(),
        }
    }

    fn fill(limit: usize, to: i64, last: i64, records: impl IntoIterator<Item = PageRecord>) -> Value {
        let mut pager = Pager::new(limit, to, last);
        for r in records {
            if !pager.take(r) {
                break;
            }
        }
        serde_json::to_value(pager.page()).unwrap()
    }

    fn seqs(page: &Value) -> Vec<i64> {
        page["records"].as_array().unwrap().iter().map(|r| r["seq"].as_i64().unwrap()).collect()
    }

    /// Goal: a page is the records it was given, in their shape, up to its
    /// limit and its bytes; the next page starts after its last record when
    /// it filled, after its window when it did not, and there is none past
    /// the channel's newest record. Method: pages that end at the channel's
    /// end, at the window's, at the limit, and at the bytes, and the page of
    /// one record as large as a record may be.
    #[test]
    fn a_records_page_fills_to_its_limit_and_says_where_the_next_starts() {
        let page = fill(200, 5, 5, [record(2, json!({ "turn": "t", "n": 1 })), record(4, json!("x"))]);
        assert_eq!(
            page,
            json!({ "records": [
                { "seq": 2, "at": 1_700_000_000_002_i64, "principal": "id:00112233445566778899aabbccddeeff", "kind": "message", "body": { "turn": "t", "n": 1 } },
                { "seq": 4, "at": 1_700_000_000_004_i64, "principal": "id:00112233445566778899aabbccddeeff", "kind": "message", "body": "x" },
            ], "next": null }),
            "the window reached the channel's end"
        );
        assert_eq!(fill(200, 10_000, 25_000, [record(7, json!(1))])["next"], 10_000, "the next page looks past this one's window, even with nothing matched");
        assert_eq!(fill(200, 10_000, 25_000, []), json!({ "records": [], "next": 10_000 }));
        assert_eq!(fill(200, 9, 9, []), json!({ "records": [], "next": null }));
        let page = fill(2, 100, 100, (1..=5).map(|s| record(s, json!(s))));
        assert_eq!((seqs(&page), &page["next"]), (vec![1, 2], &json!(2)), "at the limit, the next page starts after its last record");
        let page = fill(3, 3, 3, (1..=3).map(|s| record(s, json!(s))));
        assert_eq!((seqs(&page), &page["next"]), (vec![1, 2, 3], &Value::Null), "exactly the limit, and nothing left");
        // records of 64 KiB: a page takes as many as its bytes hold, and the next takes the rest
        let big = |s: i64| record(s, json!("x".repeat(fragment_proto::limits::RECORD_BODY_MAX_BYTES - 2)));
        let page = fill(200, 20, 20, (1..=20).map(big));
        let held = seqs(&page);
        assert_eq!(held, (1..=7).collect::<Vec<_>>(), "seven 64 KiB records fit 512 KiB with their keys; an eighth does not");
        assert_eq!(page["next"], 7);
        let page = fill(1, 1, 1, [big(1)]);
        assert_eq!(seqs(&page), vec![1], "a page always holds its first record");
    }

    #[test]
    #[should_panic(expected = "seq order")]
    fn a_page_refuses_records_out_of_order() {
        fill(200, 10, 10, [record(5, json!(1)), record(3, json!(1))]);
    }
}
