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
}

/// The identities one `job.people` step names: a page's `__people` limit.
pub const PEOPLE_MAX: usize = 64;

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
        ]
    }

    #[test]
    fn every_kind_platform_mjs_sends_decodes_as_itself() {
        let kinds = every_kind();
        assert_eq!(kinds.len(), 18, "a new kind of step is added here too");
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
}
