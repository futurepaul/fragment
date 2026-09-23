//! `fragment.json`, read at a pin. The cell takes from it what code and
//! serving need: the operations and the page metadata. Access control is
//! not in git (docs/MODEL.md: membership is live cell state), so
//! `visibility`, `editors`, and `viewers` are reported as ignored.

use std::collections::BTreeMap;

use fragment_proto::{limits, valid_op_name, OpDecl, OpKind, Role};
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
    pub meta: Option<Meta>,
    /// Top-level keys that no longer do anything here.
    pub ignored: Vec<&'static str>,
}

const ACCESS_KEYS: [&str; 3] = ["visibility", "editors", "viewers"];

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
        _ => return Err(format!("operations.{name}.kind must be \"query\" or \"mutation\"")),
    };
    let role = match obj.get("role") {
        None => match kind {
            OpKind::Query => Role::Viewer,
            OpKind::Mutation => Role::Editor,
        },
        Some(r) => r
            .as_str()
            .and_then(Role::parse)
            .ok_or_else(|| format!("operations.{name}.role must be public, viewer, editor, or owner"))?,
    };
    if obj.get("input").is_some_and(|i| !i.is_object()) {
        return Err(format!("operations.{name}.input must be a JSON Schema object"));
    }
    Ok(OpDecl { kind, role })
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
                m.operations.insert(name.clone(), operation(name, decl)?);
            }
        }
        Some(_) => return Err("operations must be an object".into()),
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
        assert_eq!(m.operations["list"], OpDecl { kind: OpKind::Query, role: Role::Viewer });
        assert_eq!(m.operations["add"], OpDecl { kind: OpKind::Mutation, role: Role::Editor });
        assert_eq!(m.operations["sign"].role, Role::Public);
        assert_eq!(m.meta.unwrap().title.as_deref(), Some("T"));
        assert_eq!(m.ignored, vec!["visibility", "editors"]);
        assert_eq!(parse(b"{}").unwrap(), Manifest::default());
    }

    #[test]
    fn refusals() {
        for bad in [
            &br#"[]"#[..],
            br#"{"operations":{"Bad":{"kind":"query"}}}"#,
            br#"{"operations":{"x":{"kind":"job"}}}"#,
            br#"{"operations":{"x":{"kind":"query","rol":"public"}}}"#,
            br#"{"operations":{"x":{"kind":"query","role":"admin"}}}"#,
            br#"{"operations":{"x":{"kind":"query","input":"string"}}}"#,
            br#"{"meta":{"title":7}}"#,
        ] {
            assert!(parse(bad).is_err(), "{}", String::from_utf8_lossy(bad));
        }
    }
}
