//! `fragment.json`, read at a pin. The cell takes from it what code and
//! serving need: the operations and the page metadata. Access control is
//! not in git (docs/MODEL.md: membership is live cell state), so
//! `visibility`, `editors`, and `viewers` are reported as ignored.

use std::collections::BTreeMap;

use fragment_proto::{
    limits, valid_channel_name, valid_op_name, ChannelDecl, OpDecl, OpKind, Role, TriggerDecl, TriggerOn, BUILTIN_CHANNELS, RESERVED_OP_NAMES,
};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub struct Meta {
    pub title: Option<String>,
    pub description: Option<String>,
    pub image: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Manifest {
    pub operations: BTreeMap<String, OpDecl>,
    /// App channels (the built-ins, `events` and `ops`, are not listed).
    pub channels: BTreeMap<String, ChannelDecl>,
    pub meta: Option<Meta>,
    pub triggers: Vec<TriggerDecl>,
    /// Where a `changed` frame goes on each move of `main`.
    pub notify_urls: Vec<String>,
    /// Platform powers the page asks for; each is granted only to the
    /// fragment's owner viewing it ([`CAPABILITIES`]), `frame` only once
    /// they allow it too.
    pub capabilities: Vec<String>,
    /// Top-level keys that no longer do anything here.
    pub ignored: Vec<&'static str>,
}

const ACCESS_KEYS: [&str; 3] = ["visibility", "editors", "viewers"];
/// The capabilities a manifest may declare. `fragments`: the page may list
/// the fragments its owner belongs to (`__fragments`), as a dashboard does.
/// `frame`: the page may show them inside it, signed in as its owner
/// (`__frame`), once its owner allows it (the share sheet).
pub const CAPABILITIES: [&str; 2] = ["fragments", "frame"];

fn text(v: &Value, key: &str, max: usize) -> Result<Option<String>, String> {
    match &v[key] {
        Value::Null => Ok(None),
        Value::String(s) if s.chars().count() <= max => Ok(Some(s.clone())),
        Value::String(_) => Err(format!("meta.{key} is longer than {max} characters")),
        _ => Err(format!("meta.{key} must be a string")),
    }
}

fn operation(name: &str, v: &Value) -> Result<OpDecl, String> {
    let obj = v.as_object().ok_or_else(|| format!("operations.{name} must be an object"))?;
    if let Some(k) = obj.keys().find(|k| !matches!(k.as_str(), "kind" | "role" | "input")) {
        return Err(format!("operations.{name} has an unknown key {k:?} (kind, role, input)"));
    }
    let kind = match obj.get("kind").and_then(Value::as_str) {
        Some("query") => OpKind::Query,
        Some("mutation") => OpKind::Mutation,
        Some("job") => OpKind::Job,
        _ => return Err(format!("operations.{name}.kind must be \"query\", \"mutation\", or \"job\"")),
    };
    let role = match obj.get("role") {
        None => match kind {
            OpKind::Query => Role::Viewer,
            OpKind::Mutation | OpKind::Job => Role::Editor,
        },
        Some(r) => r
            .as_str()
            .and_then(Role::parse)
            .ok_or_else(|| format!("operations.{name}.role must be public, viewer, editor, or owner"))?,
    };
    let input = match obj.get("input") {
        None => None,
        Some(schema @ Value::Object(_)) => {
            crate::schema::check(schema).map_err(|e| format!("operations.{name}.input{e}"))?;
            Some(schema.clone())
        }
        Some(_) => return Err(format!("operations.{name}.input must be a JSON Schema object")),
    };
    Ok(OpDecl { kind, role, input })
}

/// One entry of `channels`: who reads it (default `viewer`), and who may
/// post to it (no one by default). Whoever may post may read what they
/// posted: a `post` weaker than `read` is refused.
fn channel(name: &str, v: &Value) -> Result<ChannelDecl, String> {
    let obj = v.as_object().ok_or_else(|| format!("channels.{name} must be an object"))?;
    if let Some(k) = obj.keys().find(|k| !matches!(k.as_str(), "read" | "post")) {
        return Err(format!("channels.{name} has an unknown key {k:?} (read, post)"));
    }
    let role = |key: &str| -> Result<Option<Role>, String> {
        obj.get(key)
            .map(|r| r.as_str().and_then(Role::parse).ok_or_else(|| format!("channels.{name}.{key} must be public, viewer, editor, or owner")))
            .transpose()
    };
    let read = role("read")?.unwrap_or(Role::Viewer);
    let post = role("post")?;
    if let Some(post) = post.filter(|p| *p < read) {
        return Err(format!(
            "channels.{name}.post ({}) is looser than its read ({}): whoever may post to a channel may read it",
            post.as_str(),
            read.as_str()
        ));
    }
    Ok(ChannelDecl { read, post })
}

/// One entry of `triggers`: `{"cron" | "channel" | "files": …, "run": op}`.
fn trigger(i: usize, v: &Value, m: &Manifest) -> Result<TriggerDecl, String> {
    let at = format!("triggers[{i}]");
    let obj = v.as_object().ok_or_else(|| format!("{at} must be an object"))?;
    if let Some(k) = obj.keys().find(|k| !matches!(k.as_str(), "cron" | "channel" | "files" | "run")) {
        return Err(format!("{at} has an unknown key {k:?} (cron, channel, files, run)"));
    }
    let text = |k: &str| obj.get(k).map(|v| v.as_str().map(str::to_string).ok_or_else(|| format!("{at}.{k} must be a string"))).transpose();
    let on = match (text("cron")?, text("channel")?, text("files")?) {
        (Some(c), None, None) => {
            crate::cron::Cron::parse(&c).map_err(|e| format!("{at}.cron: {e}"))?;
            TriggerOn::Cron(c)
        }
        (None, Some(c), None) => {
            if c != "inbox" && !m.channels.contains_key(&c) {
                return Err(format!("{at}.channel must be inbox or a channel this fragment.json declares, not {c:?}"));
            }
            TriggerOn::Channel(c)
        }
        (None, None, Some(f)) => {
            if !crate::glob::valid(&f) {
                return Err(format!("{at}.files must be a relative path pattern (* ** ?), not {f:?}"));
            }
            TriggerOn::Files(f)
        }
        _ => return Err(format!("{at} needs exactly one of cron, channel, files")),
    };
    let run = text("run")?.ok_or_else(|| format!("{at} needs run: the operation it starts"))?;
    let decl = m.operations.get(&run).ok_or_else(|| format!("{at}.run names no declared operation: {run:?}"))?;
    if decl.kind == OpKind::Query {
        return Err(format!("{at}.run: {run} is a query; a trigger runs a mutation or a job"));
    }
    // A triggered run acts as the fragment itself, with an editor's reach.
    if decl.role > Role::Editor {
        return Err(format!("{at}.run: {run} needs the owner role; a trigger acts as an editor"));
    }
    Ok(TriggerDecl { on, run })
}

pub fn parse(bytes: &[u8]) -> Result<Manifest, String> {
    if bytes.len() > limits::MANIFEST_MAX_BYTES {
        return Err(format!("fragment.json is over {} bytes", limits::MANIFEST_MAX_BYTES));
    }
    let v: Value = serde_json::from_slice(bytes).map_err(|e| format!("fragment.json is not JSON: {e}"))?;
    let obj = v.as_object().ok_or("fragment.json must be an object")?;
    let mut m = Manifest { ignored: ACCESS_KEYS.into_iter().filter(|k| obj.contains_key(*k)).collect(), ..Manifest::default() };
    match obj.get("operations") {
        None | Some(Value::Null) => {}
        Some(Value::Object(ops)) => {
            if ops.len() > limits::OPERATIONS_MAX {
                return Err(format!("at most {} operations", limits::OPERATIONS_MAX));
            }
            for (name, decl) in ops {
                if !valid_op_name(name) {
                    return Err(format!("operation name {name:?} must match ^[a-z][a-z0-9_]{{0,63}}$"));
                }
                if RESERVED_OP_NAMES.contains(&name.as_str()) {
                    return Err(format!("operation name {name:?} is reserved (the App class's own handlers)"));
                }
                m.operations.insert(name.clone(), operation(name, decl)?);
            }
        }
        Some(_) => return Err("operations must be an object".into()),
    }
    match obj.get("channels") {
        None | Some(Value::Null) => {}
        Some(Value::Object(chs)) => {
            if chs.len() > limits::CHANNELS_MAX {
                return Err(format!("at most {} channels", limits::CHANNELS_MAX));
            }
            for (name, decl) in chs {
                if !valid_channel_name(name) || BUILTIN_CHANNELS.contains(&name.as_str()) {
                    return Err(format!("channel name {name:?} must match ^[a-z][a-z0-9_-]{{0,63}}$ and not be events or ops"));
                }
                m.channels.insert(name.clone(), channel(name, decl)?);
            }
        }
        Some(_) => return Err("channels must be an object".into()),
    }
    match obj.get("meta") {
        None | Some(Value::Null) => {}
        Some(meta @ Value::Object(_)) => {
            m.meta = Some(Meta {
                title: text(meta, "title", 120)?,
                description: text(meta, "description", 400)?,
                image: text(meta, "image", 500)?,
            })
        }
        Some(_) => return Err("meta must be an object".into()),
    }
    match obj.get("notifyUrls") {
        None | Some(Value::Null) => {}
        Some(Value::Array(urls)) => {
            if urls.len() > limits::NOTIFY_URLS_MAX {
                return Err(format!("at most {} notifyUrls", limits::NOTIFY_URLS_MAX));
            }
            for u in urls {
                let u = u.as_str().ok_or("notifyUrls are strings")?;
                let parsed = url::Url::parse(u).map_err(|e| format!("notifyUrls: {u:?}: {e}"))?;
                if !matches!(parsed.scheme(), "http" | "https") {
                    return Err(format!("notifyUrls: {u:?} is not http(s)"));
                }
                m.notify_urls.push(u.to_string());
            }
        }
        Some(_) => return Err("notifyUrls must be an array of URLs".into()),
    }
    match obj.get("capabilities") {
        None | Some(Value::Null) => {}
        Some(Value::Array(list)) => {
            for c in list {
                match c.as_str() {
                    Some(c) if CAPABILITIES.contains(&c) => m.capabilities.push(c.to_string()),
                    _ => return Err(format!("capabilities: {c} is not one of {CAPABILITIES:?}")),
                }
            }
        }
        Some(_) => return Err("capabilities must be an array".into()),
    }
    match obj.get("triggers") {
        None | Some(Value::Null) => {}
        Some(Value::Array(list)) => {
            if list.len() > limits::TRIGGERS_MAX {
                return Err(format!("at most {} triggers", limits::TRIGGERS_MAX));
            }
            for (i, t) in list.iter().enumerate() {
                let t = trigger(i, t, &m)?;
                m.triggers.push(t);
            }
        }
        Some(_) => return Err("triggers must be an array".into()),
    }
    Ok(m)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operations_and_defaults() {
        let m = parse(br#"{"name":"t","visibility":"public","editors":[],"workflows":[],
            "operations":{"list":{"kind":"query"},"add":{"kind":"mutation","input":{"type":"object"}},
            "sign":{"kind":"mutation","role":"public"}},"meta":{"title":"T"}}"#)
        .unwrap();
        assert_eq!(m.operations["list"], OpDecl { kind: OpKind::Query, role: Role::Viewer, input: None });
        assert_eq!(m.operations["add"].role, Role::Editor);
        assert_eq!(m.operations["add"].input, Some(serde_json::json!({"type":"object"})));
        assert_eq!(m.operations["sign"].role, Role::Public);
        assert_eq!(m.meta.unwrap().title.as_deref(), Some("T"));
        assert_eq!(m.ignored, vec!["visibility", "editors"]);
        assert_eq!(parse(b"{}").unwrap(), Manifest::default());
        assert_eq!(parse(br#"{"capabilities":["fragments"]}"#).unwrap().capabilities, vec!["fragments"]);
        assert!(parse(br#"{"capabilities":["everything"]}"#).is_err());
        let m = parse(br#"{"channels":{"chat":{},"news":{"read":"public"}}}"#).unwrap();
        assert_eq!(m.channels["chat"].read, Role::Viewer);
        assert_eq!(m.channels["news"].read, Role::Public);
        assert_eq!(m.channels["chat"].post, None, "a channel takes no posts unless it names who may");
        let m = parse(br#"{"operations":{"digest":{"kind":"job"},"save":{"kind":"mutation"}},"channels":{"chat":{}},
            "triggers":[{"cron":"0 9 * * *","run":"digest"},{"channel":"inbox","run":"save"},{"channel":"chat","run":"digest"},
            {"files":"notes/**","run":"digest"}]}"#)
        .unwrap();
        assert_eq!(m.operations["digest"], OpDecl { kind: OpKind::Job, role: Role::Editor, input: None });
        assert_eq!(m.triggers.len(), 4);
        assert_eq!(m.triggers[0], TriggerDecl { on: TriggerOn::Cron("0 9 * * *".into()), run: "digest".into() });
        assert_eq!(m.triggers[1].on, TriggerOn::Channel("inbox".into()));
        assert_eq!(m.triggers[3].on, TriggerOn::Files("notes/**".into()));
        let back: TriggerDecl = serde_json::from_value(serde_json::to_value(&m.triggers[0]).unwrap()).unwrap();
        assert_eq!(back, m.triggers[0], "stored triggers read back");
    }

    #[test]
    fn postable_channels() {
        let m = parse(br#"{"channels":{"chat":{"read":"public","post":"public"},"notes":{"post":"editor"},"desk":{"read":"editor","post":"owner"}}}"#).unwrap();
        assert_eq!(m.channels["chat"], ChannelDecl { read: Role::Public, post: Some(Role::Public) }, "a post role as loose as read");
        assert_eq!(m.channels["notes"], ChannelDecl { read: Role::Viewer, post: Some(Role::Editor) }, "read defaults to viewer beside a post role");
        assert_eq!(m.channels["desk"].post, Some(Role::Owner), "a post role tighter than read");
        for (bad, says) in [
            (&br#"{"channels":{"chat":{"read":"viewer","post":"public"}}}"#[..], "channels.chat.post (public) is looser than its read (viewer)"),
            (br#"{"channels":{"chat":{"post":"public"}}}"#, "channels.chat.post (public) is looser than its read (viewer)"),
            (br#"{"channels":{"chat":{"read":"owner","post":"editor"}}}"#, "channels.chat.post (editor) is looser than its read (owner)"),
            (br#"{"channels":{"chat":{"post":"anyone"}}}"#, "channels.chat.post must be public, viewer, editor, or owner"),
            (br#"{"channels":{"chat":{"post":true}}}"#, "channels.chat.post must be public"),
        ] {
            let why = parse(bad).expect_err(&String::from_utf8_lossy(bad));
            assert!(why.contains(says), "{why}");
        }
    }

    #[test]
    fn refusals() {
        for bad in [
            &br#"[]"#[..],
            br#"{"operations":{"Bad":{"kind":"query"}}}"#,
            br#"{"operations":{"x":{"kind":"task"}}}"#,
            br#"{"operations":{"x":{"kind":"job"}},"triggers":[{"cron":"* * * *","run":"x"}]}"#,
            br#"{"operations":{"x":{"kind":"query"}},"triggers":[{"cron":"* * * * *","run":"x"}]}"#,
            br#"{"operations":{"x":{"kind":"job","role":"owner"}},"triggers":[{"cron":"* * * * *","run":"x"}]}"#,
            br#"{"operations":{"x":{"kind":"job"}},"triggers":[{"channel":"events","run":"x"}]}"#,
            br#"{"operations":{"x":{"kind":"job"}},"triggers":[{"channel":"chat","run":"x"}]}"#,
            br#"{"operations":{"x":{"kind":"job"}},"triggers":[{"files":"/abs","run":"x"}]}"#,
            br#"{"operations":{"x":{"kind":"job"}},"triggers":[{"cron":"* * * * *","files":"a","run":"x"}]}"#,
            br#"{"operations":{"x":{"kind":"job"}},"triggers":[{"cron":"* * * * *","run":"y"}]}"#,
            br#"{"operations":{"x":{"kind":"job"}},"triggers":[{"cron":"* * * * *"}]}"#,
            br#"{"operations":{"x":{"kind":"job"}},"triggers":{"cron":"* * * * *","run":"x"}}"#,
            br#"{"operations":{"x":{"kind":"query","rol":"public"}}}"#,
            br#"{"operations":{"x":{"kind":"query","role":"admin"}}}"#,
            br#"{"operations":{"x":{"kind":"query","input":"string"}}}"#,
            br#"{"meta":{"title":7}}"#,
            br#"{"operations":{"x":{"kind":"query","input":{"pattern":"^a"}}}}"#,
            br#"{"channels":{"events":{}}}"#,
            br#"{"operations":{"fetch":{"kind":"query"}}}"#,
            br#"{"operations":{"constructor":{"kind":"mutation"}}}"#,
            br#"{"channels":{"Chat":{}}}"#,
            br#"{"channels":{"chat":{"write":"public"}}}"#,
        ] {
            assert!(parse(bad).is_err(), "{}", String::from_utf8_lossy(bad));
        }
    }
}
