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
    #[serde(rename = "ai.image")]
    AiImage(AiImage),
    /// `job.ai.video`: refused, whatever it asks, until videos run on
    /// Cloudflare (crate::media).
    #[serde(rename = "ai.video")]
    AiVideo {},
    /// `job.agent`'s first step: a turn of the fragment's own agent for
    /// the run's principal, named by the run and this step, so a retried or
    /// replayed step reattaches to it rather than start another.
    #[serde(rename = "agent.start")]
    AgentStart(AgentTurn),
    /// A started turn's state, by its id (the start's answer).
    #[serde(rename = "agent.poll")]
    AgentPoll { turn: String },
}

/// `job.agent({prompt, conversation?, channel?})`: the message, the
/// conversation it continues (a key the job chooses; none: the run's own),
/// and the channel its steps and answer are posted to (none: nowhere).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentTurn {
    pub prompt: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
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
    /// The conversation; without it, `prompt` is one user message.
    pub messages: Option<Vec<Value>>,
    pub prompt: Option<String>,
    /// GLM's reasoning control (`low` or `high`; anything else is `low`:
    /// crate::models).
    pub reasoning_effort: Option<String>,
    pub max_tokens: Option<u64>,
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
            Step::AiImage(_) => "ai.image",
            Step::AiVideo {} => "ai.video",
            Step::AgentStart(_) => "agent.start",
            Step::AgentPoll { .. } => "agent.poll",
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
            ("ai.image", json!({ "prompt": "a cat", "path": "cat.jpg", "steps": 6 })),
            ("ai.video", json!({})),
            ("agent.start", json!({ "prompt": "summarize today", "conversation": "daily", "channel": "ask" })),
            ("agent.poll", json!({ "turn": "0123456789abcdef01234567" })),
        ]
    }

    #[test]
    fn every_kind_platform_mjs_sends_decodes_as_itself() {
        let kinds = every_kind();
        assert_eq!(kinds.len(), 15, "a new kind of step is added here too");
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
        let Ok(Step::AgentStart(a)) = step("agent.start", json!({ "prompt": "p" })) else { panic!() };
        assert_eq!((a.conversation, a.channel), (None, None), "the run's own conversation, posted nowhere");
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
        refused("ai.image", json!({ "prompt": "p" }), "missing field `path`");
        refused("ai.image", json!({ "prompt": "p", "path": "a.jpg", "model": "google/gemini-3.1-flash-lite-image" }), "unknown field `model`");
        refused("ai.image", json!({ "prompt": "p", "path": "a.jpg", "steps": -1 }), "invalid value");
        refused("ai.video.start", json!({ "prompt": "p" }), "unknown variant `ai.video.start`");
        refused("fetch", json!({ "url": "https://x/", "method": "GET", "headers": { "n": 1 } }), "invalid type");
        refused("publish", json!({ "channel": "feed", "body": {} }), "missing field `kind`");
        refused("push", json!({ "payload": {} }), "missing field `who`");
        refused("files.read", json!("log.txt"), "invalid type");
        refused("agent.start", json!({ "conversation": "daily" }), "missing field `prompt`");
        refused("agent.start", json!({ "prompt": 7 }), "invalid type");
        refused("agent.poll", json!({}), "missing field `turn`");
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
