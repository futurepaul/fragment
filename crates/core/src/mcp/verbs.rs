//! The platform's verbs as MCP tools (docs/api.md, The platform's MCP
//! server): the CLI's daily loop at `<platform>/mcp`, each tool one route
//! of the API, called as the person a client is connected as. What a tool
//! is (its schema and its hints) and which route it asks are here; the
//! router asks them (cell/src/mcp.rs).

use fragment_proto::{limits, valid_fragment_name, valid_label, valid_repo_path, valid_username, Role, Visibility};
use serde_json::{json, Value};

use crate::npub;

/// The templates a tool's `create` offers (the API's, less the blessed
/// ones, which the shell makes).
pub const TEMPLATES: [&str; 4] = ["blank", "todo", "inbox", "calories"];

/// What a verb asks of the API, as the person.
#[derive(Debug, Clone, PartialEq)]
pub enum Verb {
    /// `GET /api/fragments`: their list.
    List,
    /// `POST /api/fragments`.
    Create { label: String, template: Option<String>, visibility: Option<Visibility> },
    /// A fragment's route (`/api/f/{name}/{rest}` on the platform; `rest`
    /// with its query): `file` when its answer is a file's bytes.
    Fragment { name: String, method: &'static str, rest: String, body: Option<Value>, file: bool },
    /// A member set (`role`) or removed (`None`), named by username or identity.
    Share { name: String, member: String, role: Option<Role> },
}

fn string<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    args[key].as_str().ok_or_else(|| format!("{key} is required, a string"))
}

/// A fragment's name as a tool takes it: `<label>.<username>`, or a bare
/// label for one of the person's own (the router qualifies it).
fn fragment(args: &Value) -> Result<String, String> {
    let name = string(args, "name")?;
    if valid_fragment_name(name) || valid_label(name) {
        Ok(name.to_string())
    } else {
        Err(format!("{name:?} is no fragment's name: <label>.<username>, or a label of yours"))
    }
}

fn query_value(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

/// The verb a tool's call asks, or why its arguments do not fit.
pub fn verb(tool: &str, args: &Value) -> Result<Verb, String> {
    let args = if args.is_null() { &json!({}) } else { args };
    if !args.is_object() {
        return Err("a tool's arguments are an object".into());
    }
    let on = |method: &'static str, rest: String, body: Option<Value>| -> Result<Verb, String> { Ok(Verb::Fragment { name: fragment(args)?, method, rest, body, file: false }) };
    match tool {
        "list" => Ok(Verb::List),
        "create" => {
            let label = string(args, "label")?;
            if !valid_label(label) {
                return Err(format!("{label:?}: a label is lowercase letters, digits and single dashes, at most 63"));
            }
            let template = args["template"].as_str().map(str::to_string);
            if template.as_deref().is_some_and(|t| !TEMPLATES.contains(&t)) {
                return Err(format!("the templates are {}", TEMPLATES.join(", ")));
            }
            let visibility = match args["visibility"].as_str() {
                None => None,
                Some(v) => Some(Visibility::parse(v).ok_or("visibility is public, link, or members")?),
            };
            Ok(Verb::Create { label: label.to_string(), template, visibility })
        }
        "status" => on("GET", "status".into(), None),
        "files" => on("GET", "files".into(), None),
        "read" => {
            let path = string(args, "path")?;
            if !valid_repo_path(path) {
                return Err(format!("{path:?} is no file's path (relative, no . or ..)"));
            }
            Ok(Verb::Fragment { name: fragment(args)?, method: "GET", rest: format!("file?path={}", query_value(path)), body: None, file: true })
        }
        "write" => {
            let files = args["files"].as_array().filter(|f| !f.is_empty()).ok_or("files is a list of {path, text} or {path, delete: true}")?;
            let key = string(args, "key")?;
            let mut body = json!({ "files": files, "key": key });
            if let Some(m) = args["message"].as_str() {
                body["message"] = json!(m);
            }
            on("POST", "files".into(), Some(body))
        }
        "deploy" => on("POST", "deploy".into(), Some(args.get("note").map_or_else(|| json!({}), |n| json!({ "note": n })))),
        "members" => on("GET", "members".into(), None),
        "share" => {
            let member = string(args, "member")?;
            if !npub::is_identity(member) && !valid_username(member.trim_start_matches('@')) {
                return Err(format!("{member:?}: a member is a username or an identity (id:…)"));
            }
            let role = match string(args, "role")? {
                "remove" => None,
                r => Some(Role::parse(r).filter(|r| matches!(r, Role::Viewer | Role::Editor)).ok_or("role is viewer, editor, or remove")?),
            };
            Ok(Verb::Share { name: fragment(args)?, member: member.trim_start_matches('@').to_string(), role })
        }
        "visibility" => {
            let v = Visibility::parse(string(args, "visibility")?).ok_or("visibility is public, link, or members")?;
            on("PUT", "visibility".into(), Some(json!({ "visibility": v })))
        }
        "call" => {
            let op = string(args, "op")?;
            if !fragment_proto::valid_op_name(op) {
                return Err(format!("{op:?} is no operation's name"));
            }
            let id = match &args["id"] {
                Value::Null => None,
                Value::String(id) if fragment_proto::valid_op_id(id) => Some(id.clone()),
                _ => return Err(format!("id matches ^[A-Za-z0-9._:-]{{1,{}}}$", limits::OP_ID_MAX_BYTES)),
            };
            let body = json!({ "id": id, "input": args.get("input").cloned().unwrap_or(Value::Null) });
            on("POST", format!("ops/{op}"), Some(body))
        }
        "events" => {
            let tail = args["tail"].as_u64().unwrap_or(30);
            if !(1..=500).contains(&tail) {
                return Err("tail is 1 to 500".into());
            }
            on("GET", format!("events?tail={tail}"), None)
        }
        other => Err(format!("Unknown tool: {other}")),
    }
}

fn name_property() -> Value {
    json!({ "type": "string", "description": "The fragment: <label>.<username>, or the label of one of yours." })
}

fn tool(name: &str, description: &str, properties: Value, required: &[&str], annotations: Value) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": { "type": "object", "properties": properties, "required": required, "additionalProperties": false },
        "annotations": annotations,
    })
}

/// The platform's tools, in a fixed order.
pub fn tools() -> Vec<Value> {
    let read = json!({ "readOnlyHint": true, "openWorldHint": false });
    let changes = |idempotent: bool| json!({ "readOnlyHint": false, "destructiveHint": true, "idempotentHint": idempotent, "openWorldHint": false });
    let name = name_property();
    vec![
        tool("list", "Your fragments, and your role on each: [{name, role, kind, title?}].", json!({}), &[], read.clone()),
        tool(
            "create",
            "Makes a fragment of yours at <label>--<your username> on the fragments' domain, from a template (blank: a page; todo: a shared list; inbox: webhooks into a live feed; calories: a log a model reads). Answers its name and links.",
            json!({
                "label": { "type": "string", "pattern": "^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?$", "description": "Its label: lowercase letters, digits and single dashes." },
                "template": { "type": "string", "enum": TEMPLATES },
                "visibility": { "type": "string", "enum": ["link", "members", "public"], "description": "Who can open it: anyone with its link (the default), its members, or anyone." },
            }),
            &["label"],
            json!({ "readOnlyHint": false, "destructiveHint": false, "idempotentHint": false, "openWorldHint": false }),
        ),
        tool("status", "A fragment's links, its live commit, its operations, and why its code was refused, if it was (code.error).", json!({ "name": name }), &["name"], read.clone()),
        tool("files", "A fragment's files at main: [{path, size}].", json!({ "name": name }), &["name"], read.clone()),
        tool(
            "read",
            "One file of a fragment, at main, as text.",
            json!({ "name": name, "path": { "type": "string", "description": "Its path, as files lists it (site/index.html, app.mjs, fragment.json)." } }),
            &["name", "path"],
            read.clone(),
        ),
        tool(
            "write",
            "Writes files to a fragment's main in one commit (at most 16, 1 MiB in all): site/ is its pages, app.mjs its app, fragment.json its operations. Nothing goes live until deploy. The same key again answers the first commit.",
            json!({
                "name": name,
                "files": { "type": "array", "minItems": 1, "maxItems": limits::FILE_WRITES_MAX, "items": { "type": "object", "properties": {
                    "path": { "type": "string" }, "text": { "type": "string" }, "delete": { "type": "boolean" } }, "required": ["path"], "additionalProperties": false } },
                "message": { "type": "string" },
                "key": { "type": "string", "description": "Your key for this write: the same key again commits nothing new." },
            }),
            &["name", "files", "key"],
            changes(true),
        ),
        tool("deploy", "Makes main live: its site and its app. Then status says whether its code was taken.", json!({ "name": name, "note": { "type": "string" } }), &["name"], changes(true)),
        tool("members", "Who is in a fragment, and their roles.", json!({ "name": name }), &["name"], read.clone()),
        tool(
            "share",
            "Adds someone to a fragment of yours at a role, changes their role, or removes them (remove).",
            json!({ "name": name, "member": { "type": "string", "description": "Their username, or their identity (id:…)." }, "role": { "type": "string", "enum": ["viewer", "editor", "remove"] } }),
            &["name", "member", "role"],
            changes(true),
        ),
        tool(
            "visibility",
            "Who can open a fragment of yours: anyone with its link (link), its members (members), or anyone (public).",
            json!({ "name": name, "visibility": { "type": "string", "enum": ["link", "members", "public"] } }),
            &["name", "visibility"],
            changes(true),
        ),
        tool(
            "call",
            "Calls one of a fragment's operations (status lists them) with its input. A mutation or a job takes an id you choose: the same id again answers with the first result and runs nothing again.",
            json!({ "name": name, "op": { "type": "string" }, "id": { "type": "string", "pattern": "^[A-Za-z0-9._:-]{1,128}$" }, "input": {} }),
            &["name", "op"],
            json!({ "readOnlyHint": false, "destructiveHint": true, "idempotentHint": true, "openWorldHint": true }),
        ),
        tool(
            "events",
            "What happened in a fragment, newest last: deploys, refusals, runs, members, and what connected clients did. Believe it over memory.",
            json!({ "name": name, "tail": { "type": "integer", "minimum": 1, "maximum": 500 } }),
            &["name"],
            read,
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_tool_is_a_verb_and_each_is_described() {
        let tools = tools();
        let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
        assert_eq!(names, ["list", "create", "status", "files", "read", "write", "deploy", "members", "share", "visibility", "call", "events"]);
        for t in &tools {
            assert!(t["description"].as_str().is_some_and(|d| !d.is_empty()), "{t}");
            assert_eq!(t["inputSchema"]["additionalProperties"], false, "{t}");
            let required = t["inputSchema"]["required"].as_array().unwrap();
            assert!(required.iter().all(|r| t["inputSchema"]["properties"].get(r.as_str().unwrap()).is_some()), "what it requires it declares: {t}");
        }
        let read = |n: &str| tools.iter().find(|t| t["name"] == n).unwrap()["annotations"]["readOnlyHint"] == true;
        assert!(read("list") && read("status") && read("files") && read("read") && read("members") && read("events"));
        assert!(!read("write") && !read("deploy") && !read("call") && !read("create"));
    }

    #[test]
    fn a_calls_arguments_name_its_route() {
        assert_eq!(verb("list", &Value::Null), Ok(Verb::List));
        assert_eq!(
            verb("create", &json!({ "label": "groceries", "template": "todo" })),
            Ok(Verb::Create { label: "groceries".into(), template: Some("todo".into()), visibility: None })
        );
        assert!(verb("create", &json!({ "label": "Bad Label" })).is_err());
        assert!(verb("create", &json!({ "label": "x", "template": "chat" })).is_err(), "blessed templates are the shell's");
        assert_eq!(
            verb("read", &json!({ "name": "todo.paul", "path": "site/index.html" })),
            Ok(Verb::Fragment { name: "todo.paul".into(), method: "GET", rest: "file?path=site%2Findex.html".into(), body: None, file: true })
        );
        assert!(verb("read", &json!({ "name": "todo", "path": "../x" })).is_err());
        let Ok(Verb::Fragment { method: "POST", rest, body: Some(body), .. }) = verb("write", &json!({ "name": "todo", "files": [{ "path": "a.txt", "text": "a" }], "key": "k1" })) else { panic!("a write") };
        assert_eq!((rest.as_str(), body["key"].clone()), ("files", json!("k1")));
        assert!(verb("write", &json!({ "name": "todo", "files": [{ "path": "a.txt", "text": "a" }] })).is_err(), "a write's key is required");
        assert_eq!(verb("share", &json!({ "name": "todo", "member": "@ana", "role": "editor" })), Ok(Verb::Share { name: "todo".into(), member: "ana".into(), role: Some(Role::Editor) }));
        assert_eq!(verb("share", &json!({ "name": "todo", "member": "ana", "role": "remove" })), Ok(Verb::Share { name: "todo".into(), member: "ana".into(), role: None }));
        assert!(verb("share", &json!({ "name": "todo", "member": "ana", "role": "owner" })).is_err(), "no one is made an owner");
        let Ok(Verb::Fragment { rest, body: Some(body), .. }) = verb("call", &json!({ "name": "todo.paul", "op": "add", "id": "a1", "input": { "text": "x" } })) else { panic!("a call") };
        assert_eq!((rest.as_str(), body), ("ops/add", json!({ "id": "a1", "input": { "text": "x" } })));
        assert!(verb("call", &json!({ "name": "todo", "op": "add", "id": "bad id" })).is_err());
        assert!(matches!(verb("events", &json!({ "name": "todo" })), Ok(Verb::Fragment { rest, .. }) if rest == "events?tail=30"));
        assert!(verb("events", &json!({ "name": "todo", "tail": 501 })).is_err());
        assert!(verb("status", &json!({ "name": "Not A Name" })).is_err());
        assert!(verb("nope", &json!({})).is_err());
        assert!(verb("list", &json!([1])).is_err());
    }
}
