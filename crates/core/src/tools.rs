//! How an agent names the operations it may call (agent/src/tools.rs).

use sha2::{Digest, Sha256};

/// OpenAI-style tool names: 1-64 of [a-zA-Z0-9_-].
pub const TOOL_NAME_MAX: usize = 64;

/// A tool's name: the fragment and the operation, joined by `__` (`None`
/// when it would not be a valid tool name).
pub fn tool_name(fragment: &str, op: &str) -> Option<String> {
    let name = format!("{fragment}__{op}");
    let ok = name.len() <= TOOL_NAME_MAX && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    ok.then_some(name)
}

/// The operation id of a tool call: the same call always names the same
/// id (operation ids take `[A-Za-z0-9._:-]`, tool-call ids need not), so
/// a replayed call is a replayed operation.
pub fn op_id(tool_call_id: &str) -> String {
    format!("tc:{}", &hex::encode(Sha256::digest(tool_call_id.as_bytes()))[..40])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_ids() {
        assert_eq!(tool_name("todo-1", "add").as_deref(), Some("todo-1__add"));
        assert_eq!(tool_name(&"x".repeat(62), "add"), None);
        let id = op_id("call_abc123");
        assert_eq!(id, op_id("call_abc123"));
        assert_ne!(id, op_id("call_abc124"));
        assert!(fragment_proto::valid_op_id(&id));
    }
}
