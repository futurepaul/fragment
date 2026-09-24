//! A mutation's effects, decoded and checked before any is applied
//! (docs/MODEL.md, Operations). Platform code in the app facet checks the
//! same rules first, so an author sees a refusal while the mutation can
//! still roll back; but that code shares a realm with the author's, which
//! can patch whatever it relies on, so this is the check that holds. Job
//! steps that publish use the same record check.

use std::collections::BTreeMap;

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use fragment_proto::{limits, valid_repo_path, ChannelDecl, BUILTIN_CHANNELS};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::{blob, webpush};

/// A push's `who`: the tag a page subscribed with.
pub const PUSH_WHO_MAX_CHARS: usize = 64;

/// A file's content as an effect, a job step, or a read carries it:
/// `{"text": …}` when it is UTF-8, `{"base64": …}` otherwise.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FileContent {
    Text(String),
    Base64(String),
}

impl FileContent {
    /// Bytes as they travel: text when they are UTF-8.
    pub fn of(bytes: Vec<u8>) -> FileContent {
        match String::from_utf8(bytes) {
            Ok(text) => FileContent::Text(text),
            Err(e) => FileContent::Base64(STANDARD.encode(e.into_bytes())),
        }
    }

    /// The content an object's `text` or `base64` names; `None` when it
    /// names neither (an effect's removal). Both, or either not a string,
    /// is refused.
    pub fn from_fields(obj: &Map<String, Value>) -> Result<Option<FileContent>, String> {
        match (obj.get("text"), obj.get("base64")) {
            (None, None) => Ok(None),
            (Some(Value::String(t)), None) => Ok(Some(FileContent::Text(t.clone()))),
            (None, Some(Value::String(b))) => Ok(Some(FileContent::Base64(b.clone()))),
            _ => Err("a file's content is text or base64".into()),
        }
    }

    pub fn into_bytes(self) -> Result<Vec<u8>, String> {
        match self {
            FileContent::Text(t) => Ok(t.into_bytes()),
            FileContent::Base64(b) => STANDARD.decode(b).map_err(|e| format!("base64: {e}")),
        }
    }
}

/// One thing a committed mutation asked for.
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    /// A record appended to one of the app's declared channels.
    Record { channel: String, kind: String, body: Value },
    /// A file written to `main` (`None` removes it); a mutation's files are one commit.
    File { path: String, bytes: Option<Vec<u8>> },
    /// A web push to the subscriptions tagged `who` (`*`: all).
    Push { who: String, payload: Value },
}

/// A record's kind: `^[a-z][a-z0-9._-]{0,63}$`.
pub fn valid_kind(kind: &str) -> bool {
    let b = kind.as_bytes();
    !b.is_empty()
        && b.len() <= 64
        && b[0].is_ascii_lowercase()
        && b.iter().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b"._-".contains(c))
}

/// Checks a record an app publishes, from a mutation or a job step: only to
/// a channel its fragment.json declares (never `events`, `ops`, or `inbox`,
/// which are the platform's), with a valid kind and a bounded body.
pub fn check_record(channel: &str, kind: &str, body: &Value, declared: &BTreeMap<String, ChannelDecl>) -> Result<(), String> {
    if BUILTIN_CHANNELS.contains(&channel) {
        return Err(format!("channel {channel:?} is the platform's; an app publishes to the channels its fragment.json declares"));
    }
    if !declared.contains_key(channel) {
        return Err(format!("channel {channel:?} is not declared in fragment.json"));
    }
    if !valid_kind(kind) {
        return Err("kind must match ^[a-z][a-z0-9._-]{0,63}$".into());
    }
    let size = body.to_string().len();
    if size > limits::RECORD_BODY_MAX_BYTES {
        return Err(format!("a record's body is at most {} bytes", limits::RECORD_BODY_MAX_BYTES));
    }
    Ok(())
}

fn string<'a>(obj: &'a Map<String, Value>, key: &str) -> Result<&'a str, String> {
    match obj.get(key) {
        Some(Value::String(s)) => Ok(s),
        Some(_) => Err(format!("{key} must be a string")),
        None => Err(format!("{key} is missing")),
    }
}

fn only(obj: &Map<String, Value>, keys: &[&str]) -> Result<(), String> {
    match obj.keys().find(|k| !keys.contains(&k.as_str())) {
        Some(k) => Err(format!("an unknown key {k:?}")),
        None => Ok(()),
    }
}

fn decode_one(v: &Value, declared: &BTreeMap<String, ChannelDecl>) -> Result<Effect, String> {
    let obj = v.as_object().ok_or("an effect is an object")?;
    if obj.contains_key("channel") {
        only(obj, &["channel", "kind", "body"])?;
        let (channel, kind) = (string(obj, "channel")?, string(obj, "kind")?);
        let body = obj.get("body").ok_or("body is missing")?;
        check_record(channel, kind, body, declared)?;
        Ok(Effect::Record { channel: channel.to_string(), kind: kind.to_string(), body: body.clone() })
    } else if obj.contains_key("file") {
        only(obj, &["file", "text", "base64"])?;
        let path = string(obj, "file")?;
        if !valid_repo_path(path) {
            return Err(format!("{path:?} is not a file path (relative, no . or .. segments, at most {} bytes)", limits::PATH_MAX_BYTES));
        }
        let bytes = FileContent::from_fields(obj)?.map(FileContent::into_bytes).transpose()?;
        if bytes.as_deref().is_some_and(|b| blob::parse(b).is_some()) {
            return Err(format!("{path}: an app does not write blob pointers"));
        }
        Ok(Effect::File { path: path.to_string(), bytes })
    } else if obj.contains_key("push") {
        only(obj, &["push", "payload"])?;
        let who = string(obj, "push")?;
        if who.chars().count() > PUSH_WHO_MAX_CHARS {
            return Err(format!("a push's who is at most {PUSH_WHO_MAX_CHARS} characters"));
        }
        let payload = obj.get("payload").ok_or("payload is missing")?;
        if payload.to_string().len() > webpush::PAYLOAD_MAX_BYTES {
            return Err(format!("a push payload is at most {} bytes", webpush::PAYLOAD_MAX_BYTES));
        }
        Ok(Effect::Push { who: who.to_string(), payload: payload.clone() })
    } else {
        Err("an effect is a record (channel), a file (file), or a push (push)".into())
    }
}

/// Decodes what a committed mutation asked for, all or nothing: one
/// refusal refuses every effect, before any is applied.
pub fn decode(effects: &Value, declared: &BTreeMap<String, ChannelDecl>) -> Result<Vec<Effect>, String> {
    let list = effects.as_array().ok_or("a mutation's effects are an array")?;
    if list.len() > limits::EFFECTS_MAX {
        return Err(format!("a mutation has at most {} effects, not {}", limits::EFFECTS_MAX, list.len()));
    }
    let mut out = Vec::with_capacity(list.len());
    let (mut files, mut file_bytes) = (0usize, 0usize);
    for (i, v) in list.iter().enumerate() {
        let effect = decode_one(v, declared).map_err(|why| format!("effect {i}: {why}"))?;
        if let Effect::File { bytes, .. } = &effect {
            files += 1;
            file_bytes += bytes.as_ref().map_or(0, Vec::len);
            if files > limits::FILE_WRITES_MAX {
                return Err(format!("a mutation writes at most {} files", limits::FILE_WRITES_MAX));
            }
            if file_bytes > limits::FILE_WRITE_MAX_BYTES {
                return Err(format!("a mutation writes at most {} bytes of files", limits::FILE_WRITE_MAX_BYTES));
            }
        }
        out.push(effect);
    }
    assert!(out.len() == list.len(), "every effect decoded or the list was refused");
    assert!(out.len() <= limits::EFFECTS_MAX, "the list is within its limit");
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fragment_proto::Role;
    use serde_json::json;

    fn declared() -> BTreeMap<String, ChannelDecl> {
        BTreeMap::from([("room".to_string(), ChannelDecl { read: Role::Viewer })])
    }

    fn refusal(effects: Value) -> String {
        decode(&effects, &declared()).expect_err("refused")
    }

    const SHA: &str = "4d7a214614ab2935c943f9e0ff69d22eadbb8f32b1258daaa5e2ca24d17e2393";

    #[test]
    fn decodes_each_kind() {
        let got = decode(
            &json!([
                { "channel": "room", "kind": "said", "body": { "text": "hi" } },
                { "file": "notes/a.md", "text": "# A\n" },
                { "file": "b.bin", "base64": "AJ+Slg==" },
                { "file": "gone.txt" },
                { "push": "*", "payload": { "title": "t" } },
            ]),
            &declared(),
        )
        .unwrap();
        assert_eq!(got[0], Effect::Record { channel: "room".into(), kind: "said".into(), body: json!({ "text": "hi" }) });
        assert_eq!(got[1], Effect::File { path: "notes/a.md".into(), bytes: Some(b"# A\n".to_vec()) });
        assert_eq!(got[2], Effect::File { path: "b.bin".into(), bytes: Some(vec![0, 159, 146, 150]) });
        assert_eq!(got[3], Effect::File { path: "gone.txt".into(), bytes: None });
        assert_eq!(got[4], Effect::Push { who: "*".into(), payload: json!({ "title": "t" }) });
        assert!(decode(&json!([]), &declared()).unwrap().is_empty());
    }

    #[test]
    fn records_go_only_to_declared_app_channels() {
        for builtin in BUILTIN_CHANNELS {
            assert!(refusal(json!([{ "channel": builtin, "kind": "forged", "body": {} }])).contains("the platform's"), "{builtin}");
        }
        assert!(refusal(json!([{ "channel": "nowhere", "kind": "k", "body": null }])).contains("not declared"));
        assert!(refusal(json!([{ "channel": "room", "kind": "Bad", "body": null }])).contains("kind must match"));
        assert!(refusal(json!([{ "channel": "room", "kind": "k".repeat(65), "body": null }])).contains("kind must match"));
        assert!(refusal(json!([{ "channel": "room", "kind": "k" }])).contains("body is missing"));
        let body = json!({ "blob": "x".repeat(limits::RECORD_BODY_MAX_BYTES) });
        assert!(refusal(json!([{ "channel": "room", "kind": "k", "body": body }])).contains(&limits::RECORD_BODY_MAX_BYTES.to_string()));
        // the job step's check is this one
        assert!(check_record("room", "k", &json!("x".repeat(100)), &declared()).is_ok());
        assert!(check_record("inbox", "k", &Value::Null, &declared()).is_err());
    }

    #[test]
    fn counts_are_bounded_before_any_effect() {
        let one = json!({ "channel": "room", "kind": "k", "body": null });
        let at_limit = Value::Array(vec![one.clone(); limits::EFFECTS_MAX]);
        assert_eq!(decode(&at_limit, &declared()).unwrap().len(), limits::EFFECTS_MAX);
        let over = Value::Array(vec![one; limits::EFFECTS_MAX + 1]);
        assert!(decode(&over, &declared()).unwrap_err().contains(&limits::EFFECTS_MAX.to_string()));
        let files: Vec<Value> = (0..=limits::FILE_WRITES_MAX).map(|i| json!({ "file": format!("f{i}.txt"), "text": "x" })).collect();
        assert!(refusal(Value::Array(files)).contains(&format!("at most {} files", limits::FILE_WRITES_MAX)));
        let half = "x".repeat(limits::FILE_WRITE_MAX_BYTES / 2 + 1);
        assert!(refusal(json!([{ "file": "a", "text": half }, { "file": "b", "text": half }])).contains("bytes of files"));
    }

    #[test]
    fn files_are_checked_in_bytes_and_never_pointers() {
        assert!(refusal(json!([{ "file": "../escape.txt", "text": "no" }])).contains("not a file path"));
        // 101 characters of three bytes each: within 300 characters, over 300 bytes
        let wide = "\u{6587}".repeat(101);
        assert!(wide.chars().count() <= limits::PATH_MAX_BYTES && wide.len() > limits::PATH_MAX_BYTES);
        assert!(refusal(json!([{ "file": wide, "text": "x" }])).contains("not a file path"));
        let pointer = blob::pointer(SHA, 5);
        assert!(refusal(json!([{ "file": "big.bin", "text": pointer }])).contains("blob pointers"));
        use base64::Engine;
        let encoded = base64::engine::general_purpose::STANDARD.encode(pointer.as_bytes());
        assert!(refusal(json!([{ "file": "big.bin", "base64": encoded }])).contains("blob pointers"));
        assert!(refusal(json!([{ "file": "a", "text": "x", "base64": "eA==" }])).contains("text or base64"));
        assert!(refusal(json!([{ "file": "a", "base64": "not base64!" }])).contains("base64"));
        assert!(refusal(json!([{ "file": "a", "text": 7 }])).contains("text or base64"));
    }

    #[test]
    fn file_content_travels_as_text_or_base64() {
        assert_eq!(serde_json::to_value(FileContent::of(b"# A\n".to_vec())).unwrap(), json!({ "text": "# A\n" }));
        let bin = vec![0u8, 159, 146, 150];
        assert_eq!(serde_json::to_value(FileContent::of(bin.clone())).unwrap(), json!({ "base64": "AJ+Slg==" }));
        let back: FileContent = serde_json::from_value(json!({ "base64": "AJ+Slg==" })).unwrap();
        assert_eq!(back.into_bytes().unwrap(), bin);
        let fields = |v: Value| FileContent::from_fields(v.as_object().unwrap());
        assert_eq!(fields(json!({ "file": "a" })).unwrap(), None);
        assert_eq!(fields(json!({ "text": "x" })).unwrap(), Some(FileContent::Text("x".into())));
        assert!(fields(json!({ "text": "x", "base64": "eA==" })).is_err());
        assert!(fields(json!({ "base64": 7 })).is_err());
        assert!(FileContent::Base64("not base64!".into()).into_bytes().is_err());
    }

    #[test]
    fn pushes_and_shapes() {
        assert!(refusal(json!([{ "push": "w".repeat(PUSH_WHO_MAX_CHARS + 1), "payload": {} }])).contains("who"));
        assert!(decode(&json!([{ "push": "\u{e9}".repeat(PUSH_WHO_MAX_CHARS), "payload": {} }]), &declared()).is_ok());
        let big = json!({ "body": "x".repeat(webpush::PAYLOAD_MAX_BYTES) });
        assert!(refusal(json!([{ "push": "*", "payload": big }])).contains("push payload"));
        assert!(refusal(json!({ "channel": "room" })).contains("an array"));
        assert!(refusal(json!(["x"])).contains("an object"));
        assert!(refusal(json!([{ "shout": "hi" }])).contains("a record (channel)"));
        assert!(refusal(json!([{ "channel": "room", "kind": "k", "body": 1, "extra": 1 }])).contains("unknown key"));
        assert!(refusal(json!([{ "channel": 5, "kind": "k", "body": 1 }])).contains("channel must be a string"));
        // the refusal names the effect it found
        assert!(refusal(json!([{ "channel": "room", "kind": "k", "body": 1 }, { "channel": "events", "kind": "k", "body": 1 }])).starts_with("effect 1:"));
    }
}
