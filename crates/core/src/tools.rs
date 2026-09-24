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

/// A computer call's id (`fragment computer serve` journals calls by it,
/// in `[A-Za-z0-9_-]`): the same tool call always names the same id, so a
/// replayed call re-attaches to its run instead of starting another.
pub fn call_id(tool_call_id: &str) -> String {
    format!("tc-{}", &hex::encode(Sha256::digest(tool_call_id.as_bytes()))[..40])
}

/// The operation id of an agent's reply to a chat, from its message id:
/// the same message is posted once, however often the post is retried.
pub fn reply_id(message_id: &str) -> String {
    format!("rp:{}", &hex::encode(Sha256::digest(message_id.as_bytes()))[..40])
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
        assert!(fragment_proto::valid_op_id(&reply_id("msg_0b6e7e3c-1d2f-4c55-9d0e-4f7b1d2a3c4e")));
        assert_ne!(reply_id("a"), op_id("a"));
        let call = call_id("call_abc123");
        assert_eq!(call, call_id("call_abc123"));
        assert!(call.len() == 43 && call.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'));
    }
}
