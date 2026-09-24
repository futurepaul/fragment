//! Operation input schemas: a bounded subset of JSON Schema, checked when
//! `fragment.json` is read and enforced on every call. An operation's
//! schema is also its tool schema for agents, so the subset is the part
//! model tool-calling uses.
//!
//! Validation keywords: `type` (one or a list of `null`, `boolean`,
//! `integer`, `number`, `string`, `array`, `object`), `enum`, `const`,
//! `minLength`/`maxLength` (characters), `minimum`/`maximum`/
//! `exclusiveMinimum`/`exclusiveMaximum`, `items`, `minItems`/`maxItems`,
//! `uniqueItems`, `properties`, `required`, `additionalProperties`
//! (boolean or schema), `minProperties`/`maxProperties`. Annotations
//! (`title`, `description`, `default`, `examples`, `$comment`, `$schema`)
//! are allowed and ignored. Anything else is refused at deploy rather than
//! silently not enforced.

use std::collections::HashSet;

use serde_json::{Map, Value};

/// A schema's JSON text.
pub const SCHEMA_MAX_BYTES: usize = 16 * 1024;
/// Nesting of schemas within a schema.
pub const SCHEMA_MAX_DEPTH: usize = 16;

const ANNOTATIONS: [&str; 6] = ["title", "description", "default", "examples", "$comment", "$schema"];
const KEYWORDS: [&str; 18] = [
    "type",
    "enum",
    "const",
    "minLength",
    "maxLength",
    "minimum",
    "maximum",
    "exclusiveMinimum",
    "exclusiveMaximum",
    "items",
    "minItems",
    "maxItems",
    "uniqueItems",
    "properties",
    "required",
    "additionalProperties",
    "minProperties",
    "maxProperties",
];
const TYPES: [&str; 7] = ["null", "boolean", "integer", "number", "string", "array", "object"];

/// Checks that `schema` is inside the subset.
pub fn check(schema: &Value) -> Result<(), String> {
    let text = schema.to_string();
    if text.len() > SCHEMA_MAX_BYTES {
        return Err(format!("the schema is over {SCHEMA_MAX_BYTES} bytes"));
    }
    check_at(schema, "", 0)
}

fn check_at(schema: &Value, at: &str, depth: usize) -> Result<(), String> {
    if depth > SCHEMA_MAX_DEPTH {
        return Err(format!("{at}: schemas nest at most {SCHEMA_MAX_DEPTH} deep"));
    }
    let obj = match schema {
        Value::Bool(_) => return Ok(()),
        Value::Object(o) => o,
        _ => return Err(format!("{at}: a schema is an object or a boolean")),
    };
    for (k, v) in obj {
        if ANNOTATIONS.contains(&k.as_str()) {
            continue;
        }
        if !KEYWORDS.contains(&k.as_str()) {
            return Err(format!("{at}: `{k}` is not in the supported schema subset"));
        }
        let bad = |what: &str| Err(format!("{at}/{k}: must be {what}"));
        match k.as_str() {
            "type" => {
                let ok = |t: &Value| t.as_str().is_some_and(|t| TYPES.contains(&t));
                let valid = match v {
                    Value::Array(a) => !a.is_empty() && a.iter().all(ok),
                    t => ok(t),
                };
                if !valid {
                    return bad("a type name or a list of them");
                }
            }
            "enum" if !v.as_array().is_some_and(|a| !a.is_empty()) => return bad("a non-empty array"),
            "minLength" | "maxLength" | "minItems" | "maxItems" | "minProperties" | "maxProperties" if v.as_u64().is_none() => {
                return bad("a non-negative integer")
            }
            "minimum" | "maximum" | "exclusiveMinimum" | "exclusiveMaximum" if !v.is_number() => return bad("a number"),
            "uniqueItems" if !v.is_boolean() => return bad("a boolean"),
            "required" if !v.as_array().is_some_and(|a| a.iter().all(Value::is_string)) => return bad("an array of property names"),
            "items" => check_at(v, &format!("{at}/items"), depth + 1)?,
            "additionalProperties" => check_at(v, &format!("{at}/additionalProperties"), depth + 1)?,
            "properties" => {
                let props = v.as_object().ok_or_else(|| format!("{at}/properties: must be an object"))?;
                for (name, s) in props {
                    check_at(s, &format!("{at}/properties/{name}"), depth + 1)?;
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn type_of(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(n) if n.is_i64() || n.is_u64() => "integer",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn type_matches(want: &str, v: &Value) -> bool {
    let got = type_of(v);
    want == got || (want == "number" && got == "integer") || (want == "integer" && v.as_f64().is_some_and(|f| f.fract() == 0.0 && f.is_finite()))
}

/// Validates `v` against a schema that passed [`check`]. The error names
/// the JSON pointer of the first offending value.
pub fn validate(schema: &Value, v: &Value) -> Result<(), String> {
    validate_at(schema, v, "")
}

fn plural(n: u64, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

fn at_or_root(at: &str) -> &str {
    if at.is_empty() {
        "the input"
    } else {
        at
    }
}

fn validate_at(schema: &Value, v: &Value, at: &str) -> Result<(), String> {
    let s = match schema {
        Value::Bool(true) => return Ok(()),
        Value::Bool(false) => return Err(format!("{}: no value is allowed here", at_or_root(at))),
        Value::Object(o) => o,
        _ => return Ok(()),
    };
    let fail = |msg: String| Err(format!("{}: {msg}", at_or_root(at)));
    let num = |k: &str| s.get(k).and_then(Value::as_f64);
    let count = |k: &str| s.get(k).and_then(Value::as_u64);
    if let Some(t) = s.get("type") {
        let wants: Vec<&str> = match t {
            Value::Array(a) => a.iter().filter_map(Value::as_str).collect(),
            t => t.as_str().into_iter().collect(),
        };
        if !wants.iter().any(|w| type_matches(w, v)) {
            return fail(format!("must be {}, not {}", wants.join(" or "), type_of(v)));
        }
    }
    if let Some(allowed) = s.get("enum").and_then(Value::as_array) {
        if !allowed.contains(v) {
            return fail(format!("must be one of {}", Value::Array(allowed.clone())));
        }
    }
    if let Some(c) = s.get("const") {
        if c != v {
            return fail(format!("must be {c}"));
        }
    }
    match v {
        Value::String(text) => {
            let n = text.chars().count() as u64;
            if let Some(m) = count("minLength").filter(|m| n < *m) {
                return fail(format!("must be at least {}", plural(m, "character")));
            }
            if let Some(m) = count("maxLength").filter(|m| n > *m) {
                return fail(format!("must be at most {}", plural(m, "character")));
            }
        }
        Value::Number(n) => {
            let x = n.as_f64().unwrap_or(f64::NAN);
            if num("minimum").is_some_and(|m| x < m) {
                return fail(format!("must be at least {}", s["minimum"]));
            }
            if num("maximum").is_some_and(|m| x > m) {
                return fail(format!("must be at most {}", s["maximum"]));
            }
            if num("exclusiveMinimum").is_some_and(|m| x <= m) {
                return fail(format!("must be more than {}", s["exclusiveMinimum"]));
            }
            if num("exclusiveMaximum").is_some_and(|m| x >= m) {
                return fail(format!("must be less than {}", s["exclusiveMaximum"]));
            }
        }
        Value::Array(items) => {
            let n = items.len() as u64;
            if let Some(m) = count("minItems").filter(|m| n < *m) {
                return fail(format!("must have at least {}", plural(m, "item")));
            }
            if let Some(m) = count("maxItems").filter(|m| n > *m) {
                return fail(format!("must have at most {}", plural(m, "item")));
            }
            if s.get("uniqueItems") == Some(&Value::Bool(true)) {
                if let Some(i) = first_repeat(items) {
                    return fail(format!("item {i} repeats an earlier item"));
                }
            }
            if let Some(item_schema) = s.get("items") {
                for (i, item) in items.iter().enumerate() {
                    validate_at(item_schema, item, &format!("{at}/{i}"))?;
                }
            }
        }
        Value::Object(fields) => validate_object(s, fields, at)?,
        _ => {}
    }
    Ok(())
}

/// The first item equal to an earlier one. Public callers reach this with
/// up to 256 KiB of input, so each item is keyed once by its canonical
/// JSON (a set lookup each) rather than compared with every earlier item.
fn first_repeat(items: &[Value]) -> Option<usize> {
    let mut seen = HashSet::with_capacity(items.len());
    for (i, item) in items.iter().enumerate() {
        let mut key = String::new();
        unique_key(item, &mut key);
        if !seen.insert(key) {
            return Some(i);
        }
    }
    None
}

/// A key equal for two values exactly when they are equal as JSON values
/// (`Value`'s own equality): object keys in order, and a float zero written
/// one way, since `-0.0 == 0.0`. Recursion is bounded by the input's
/// nesting, which parsing already limits.
fn unique_key(v: &Value, out: &mut String) {
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
                unique_key(&map[k.as_str()], out);
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                unique_key(item, out);
            }
            out.push(']');
        }
        Value::Number(n) if n.is_f64() && n.as_f64() == Some(0.0) => out.push_str("0.0"),
        other => out.push_str(&other.to_string()),
    }
}

fn validate_object(s: &Map<String, Value>, fields: &Map<String, Value>, at: &str) -> Result<(), String> {
    let n = fields.len() as u64;
    let count = |k: &str| s.get(k).and_then(Value::as_u64);
    if count("minProperties").is_some_and(|m| n < m) {
        return Err(format!("{}: must have at least {} properties", at_or_root(at), count("minProperties").unwrap_or(0)));
    }
    if count("maxProperties").is_some_and(|m| n > m) {
        return Err(format!("{}: must have at most {} properties", at_or_root(at), count("maxProperties").unwrap_or(0)));
    }
    for name in s.get("required").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str) {
        if !fields.contains_key(name) {
            return Err(format!("{at}/{name}: is required"));
        }
    }
    let props = s.get("properties").and_then(Value::as_object);
    for (name, value) in fields {
        let child = format!("{at}/{name}");
        match props.and_then(|p| p.get(name)) {
            Some(ps) => validate_at(ps, value, &child)?,
            None => {
                if let Some(extra) = s.get("additionalProperties") {
                    validate_at(extra, value, &child).map_err(|e| if extra == &Value::Bool(false) { format!("{child}: is not an allowed property") } else { e })?;
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn todo() -> Value {
        json!({
            "type": "object",
            "required": ["text"],
            "additionalProperties": false,
            "properties": {
                "text": { "type": "string", "minLength": 1, "maxLength": 5, "description": "the todo" },
                "tags": { "type": "array", "items": { "enum": ["a", "b"] }, "maxItems": 2, "uniqueItems": true },
                "priority": { "type": "integer", "minimum": 1, "maximum": 3 },
                "done": { "type": ["boolean", "null"] }
            }
        })
    }

    #[test]
    fn the_subset() {
        check(&todo()).unwrap();
        check(&json!(true)).unwrap();
        for bad in [
            json!({ "pattern": "^a" }),
            json!({ "oneOf": [] }),
            json!({ "$ref": "#/x" }),
            json!({ "type": "text" }),
            json!({ "properties": { "x": { "format": "email" } } }),
            json!({ "maxLength": -1 }),
            json!({ "required": [1] }),
            json!("string"),
        ] {
            assert!(check(&bad).is_err(), "{bad}");
        }
        let mut deep = json!({});
        for _ in 0..20 {
            deep = json!({ "items": deep });
        }
        assert!(check(&deep).is_err());
    }

    #[test]
    fn validation() {
        let s = todo();
        validate(&s, &json!({ "text": "milk", "tags": ["a"], "priority": 2, "done": null })).unwrap();
        validate(&s, &json!({ "text": "milk", "priority": 2.0 })).unwrap();
        let err = |v: Value| validate(&s, &v).unwrap_err();
        assert_eq!(err(json!({})), "/text: is required");
        assert_eq!(err(json!({ "text": "" })), "/text: must be at least 1 character");
        assert_eq!(err(json!({ "text": "toolong" })), "/text: must be at most 5 characters");
        assert_eq!(err(json!({ "text": "x", "extra": 1 })), "/extra: is not an allowed property");
        assert_eq!(err(json!({ "text": "x", "tags": ["c"] })), r#"/tags/0: must be one of ["a","b"]"#);
        assert_eq!(err(json!({ "text": "x", "tags": ["a", "a"] })), "/tags: item 1 repeats an earlier item");
        assert_eq!(err(json!({ "text": "x", "priority": 1.5 })), "/priority: must be integer, not number");
        assert_eq!(err(json!({ "text": "x", "priority": 9 })), "/priority: must be at most 3");
        assert_eq!(err(json!("x")), "the input: must be object, not string");
        assert_eq!(err(json!({ "text": 3 })), "/text: must be string, not integer");
    }

    /// The quadratic check `uniqueItems` used to run: the meaning kept.
    fn first_repeat_reference(items: &[Value]) -> Option<usize> {
        (0..items.len()).find(|&i| items[..i].contains(&items[i]))
    }

    #[test]
    fn unique_items_keeps_value_equality() {
        let cases = [
            json!([1, 2, 3]),
            json!([1, 2, 1]),
            json!([1, 1.0]),
            json!([0.0, -0.0]),
            json!([2.5, 2.5]),
            json!(["a", "b", "a"]),
            json!([{ "a": 1, "b": [1, 2] }, { "b": [1, 2], "a": 1 }]),
            json!([{ "a": 1 }, { "a": 2 }, [1], [1]]),
            json!([null, false, 0, "", [], {}]),
            json!([null, false, 0, "", [], {}, {}]),
            json!(["1", 1]),
            json!([[0.0], [-0.0]]),
        ];
        for c in &cases {
            let items = c.as_array().expect("an array");
            assert_eq!(first_repeat(items), first_repeat_reference(items), "{c}");
        }
        assert_eq!(first_repeat(&[json!(1), json!(2), json!(1)]), Some(2));
    }

    #[test]
    fn unique_items_is_linear_at_the_input_limit() {
        // About 43,000 small distinct items fill 256 KiB; the quadratic
        // check made about 10^9 comparisons for one call.
        let items: Vec<Value> = (0..43_000).map(|i| json!(i)).collect();
        let input = Value::Array(items);
        assert!(input.to_string().len() <= fragment_proto::limits::INPUT_MAX_BYTES);
        let schema = json!({ "type": "array", "uniqueItems": true });
        let t0 = std::time::Instant::now();
        validate(&schema, &input).unwrap();
        let mut repeated = input.as_array().cloned().unwrap_or_default();
        repeated.push(json!(42_999));
        assert_eq!(validate(&schema, &Value::Array(repeated)).unwrap_err(), "the input: item 43000 repeats an earlier item");
        assert!(t0.elapsed() < std::time::Duration::from_secs(2), "{:?}", t0.elapsed());
    }
}
