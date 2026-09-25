//! The fragment's live socket (`__live`), defined once: every frame a page
//! sends (`LiveIn`) and every frame the fragment answers (`LiveOut`), as
//! JSON objects tagged by `type`. The cell decodes and encodes through
//! these (cell/src/live.rs), the CLI's `channel --follow` decodes them, and
//! the browser library (cell/client.mjs) speaks the same field names.

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use crate::{ChannelRecord, Role};

/// A frame a page sends. One that does not decode is answered with
/// `LiveOut::Error`, naming what was wrong.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum LiveIn {
    /// One page of a channel's records from a cursor; the page that
    /// reaches the end makes the socket live on the channel.
    Subscribe(Subscribe),
    Unsubscribe { channel: String },
    /// Shares `data` with everyone on the page (at most
    /// `limits::PRESENCE_MAX_BYTES`); `null`, or no `data`, clears it. At
    /// most `limits::PRESENCE_PER_S` a second, after a burst: faster
    /// changes are dropped, each with an error.
    Presence {
        #[serde(default)]
        data: Value,
    },
    Ping,
}

/// `{type: "subscribe", channel, after}` or `{type: "subscribe", channel,
/// last}`: exactly one cursor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "SubscribeFrame", into = "SubscribeFrame")]
pub struct Subscribe {
    pub channel: String,
    pub from: Cursor,
}

/// Where a subscribe's page starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cursor {
    /// The records with `seq` after this one (0: from the start).
    After(i64),
    /// The channel's last this many records (at most `limits::CHANNEL_PAGE`).
    Last(i64),
}

/// A subscribe as it travels: the cursor is one of two keys.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SubscribeFrame {
    channel: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    after: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last: Option<i64>,
}

impl TryFrom<SubscribeFrame> for Subscribe {
    type Error = String;

    fn try_from(f: SubscribeFrame) -> Result<Subscribe, String> {
        let from = match (f.after, f.last) {
            (Some(after), None) => Cursor::After(after),
            (None, Some(last)) => Cursor::Last(last),
            (Some(_), Some(_)) => return Err("a subscribe starts after a seq or at the last records, not both".into()),
            (None, None) => return Err("a subscribe names where it starts: after or last".into()),
        };
        Ok(Subscribe { channel: f.channel, from })
    }
}

impl From<Subscribe> for SubscribeFrame {
    fn from(s: Subscribe) -> SubscribeFrame {
        let (after, last) = match s.from {
            Cursor::After(n) => (Some(n), None),
            Cursor::Last(n) => (None, Some(n)),
        };
        SubscribeFrame { channel: s.channel, after, last }
    }
}

/// A frame the fragment sends.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LiveOut {
    /// First on every socket: who the page is to the fragment (the role is
    /// fixed while the socket stays open), and everyone on the page who
    /// shares presence.
    Hello { id: String, principal: String, role: Role, presence: Vec<Present> },
    /// A record of a channel: one of a page, or a new one while live.
    Record(#[serde(deserialize_with = "record_through_value")] ChannelRecord),
    /// After each page: the cursor past it, and whether more follow. With
    /// `more`, the socket is not live on the channel yet: subscribe again
    /// from `next`.
    Subscribed { channel: String, next: i64, more: bool },
    /// One socket's presence changed: its data, or `null` once it cleared
    /// it or left.
    Presence(Present),
    /// A mutation applied: re-run live queries.
    Changed { op: String },
    /// A frame the fragment refused, and why.
    Error { message: String },
    Pong,
}

/// One socket's presence (`data` is never `null` in a `hello`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Present {
    pub id: String,
    pub principal: String,
    pub data: Value,
}

/// A record frame's record, read through a `Value`: serde buffers a tagged
/// frame's fields, and a body kept as raw JSON text cannot pass through
/// that buffer, while a `Value` can hand one over.
fn record_through_value<'de, D: Deserializer<'de>>(d: D) -> Result<ChannelRecord, D::Error> {
    ChannelRecord::deserialize(Value::deserialize(d)?).map_err(serde::de::Error::custom)
}

impl LiveIn {
    pub fn encode(&self) -> String {
        serde_json::to_string(self).expect("a live frame serializes")
    }
}

impl LiveOut {
    pub fn encode(&self) -> String {
        serde_json::to_string(self).expect("a live frame serializes")
    }

    /// A record's frame, the text `LiveOut::Record(record).encode()` makes,
    /// from a borrowed record: an append broadcasts its record uncopied.
    pub fn record_frame(record: &ChannelRecord) -> String {
        #[derive(Serialize)]
        #[serde(tag = "type", rename = "record")]
        struct Frame<'a> {
            #[serde(flatten)]
            record: &'a ChannelRecord,
        }
        serde_json::to_string(&Frame { record }).expect("a record frame serializes")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn decode_in(v: Value) -> Result<LiveIn, String> {
        serde_json::from_value(v).map_err(|e| e.to_string())
    }

    #[test]
    fn a_page_sends_these_frames() {
        assert_eq!(
            decode_in(json!({ "type": "subscribe", "channel": "chat", "after": 7 })),
            Ok(LiveIn::Subscribe(Subscribe { channel: "chat".into(), from: Cursor::After(7) }))
        );
        assert_eq!(
            decode_in(json!({ "type": "subscribe", "channel": "chat", "last": 100 })),
            Ok(LiveIn::Subscribe(Subscribe { channel: "chat".into(), from: Cursor::Last(100) }))
        );
        assert_eq!(decode_in(json!({ "type": "unsubscribe", "channel": "chat" })), Ok(LiveIn::Unsubscribe { channel: "chat".into() }));
        assert_eq!(decode_in(json!({ "type": "presence", "data": { "name": "paul" } })), Ok(LiveIn::Presence { data: json!({ "name": "paul" }) }));
        assert_eq!(decode_in(json!({ "type": "presence", "data": null })), Ok(LiveIn::Presence { data: Value::Null }));
        assert_eq!(decode_in(json!({ "type": "presence" })), Ok(LiveIn::Presence { data: Value::Null }), "no data clears, as null does");
        assert_eq!(decode_in(json!({ "type": "ping" })), Ok(LiveIn::Ping));
    }

    #[test]
    fn a_malformed_frame_is_an_error_not_a_default() {
        let refused = |v: Value, says: &str| {
            let e = decode_in(v.clone()).expect_err(&v.to_string());
            assert!(e.contains(says), "{v}: {e}");
        };
        refused(json!({ "type": "subscribe", "after": 0 }), "channel");
        refused(json!({ "type": "subscribe", "channel": "chat" }), "after or last");
        refused(json!({ "type": "subscribe", "channel": "chat", "after": 0, "last": 5 }), "not both");
        refused(json!({ "type": "subscribe", "channel": "chat", "after": "0" }), "invalid type");
        refused(json!({ "type": "subscribe", "channel": "chat", "after": 0, "extra": 1 }), "unknown field");
        refused(json!({ "type": "unsubscribe" }), "channel");
        refused(json!({ "type": "shout" }), "unknown variant");
        refused(json!({ "channel": "chat" }), "type");
        assert!(serde_json::from_str::<LiveIn>("not json").is_err());
    }

    /// The JSON the browser library reads: the field names are the contract.
    #[test]
    fn the_fragment_sends_these_frames() {
        let record = ChannelRecord { channel: "chat".into(), seq: 3, at: 9, principal: "platform".into(), kind: "said".into(), body: serde_json::value::to_raw_value(&json!({ "text": "hi" })).unwrap() };
        let frames = [
            (
                LiveOut::Hello {
                    id: "ab".into(),
                    principal: "anon:x".into(),
                    role: Role::Public,
                    presence: vec![Present { id: "cd".into(), principal: "id:y".into(), data: json!({ "name": "paul" }) }],
                },
                json!({ "type": "hello", "id": "ab", "principal": "anon:x", "role": "public", "presence": [{ "id": "cd", "principal": "id:y", "data": { "name": "paul" } }] }),
            ),
            (
                LiveOut::Record(record),
                json!({ "type": "record", "channel": "chat", "seq": 3, "at": 9, "principal": "platform", "kind": "said", "body": { "text": "hi" } }),
            ),
            (LiveOut::Subscribed { channel: "chat".into(), next: 3, more: true }, json!({ "type": "subscribed", "channel": "chat", "next": 3, "more": true })),
            (
                LiveOut::Presence(Present { id: "ab".into(), principal: "anon:x".into(), data: json!({ "name": "paul" }) }),
                json!({ "type": "presence", "id": "ab", "principal": "anon:x", "data": { "name": "paul" } }),
            ),
            (
                LiveOut::Presence(Present { id: "ab".into(), principal: "anon:x".into(), data: Value::Null }),
                json!({ "type": "presence", "id": "ab", "principal": "anon:x", "data": null }),
            ),
            (LiveOut::Changed { op: "say".into() }, json!({ "type": "changed", "op": "say" })),
            (LiveOut::Error { message: "no".into() }, json!({ "type": "error", "message": "no" })),
            (LiveOut::Pong, json!({ "type": "pong" })),
        ];
        for (frame, wire) in frames {
            assert_eq!(serde_json::from_str::<Value>(&frame.encode()).unwrap(), wire);
            // the CLI decodes what the cell encodes, from its text
            let decoded: LiveOut = serde_json::from_str(&wire.to_string()).unwrap();
            assert_eq!(serde_json::to_value(decoded).unwrap(), wire);
        }
    }

    #[test]
    fn a_record_frame_from_a_borrowed_record_is_the_same_text() {
        let body = serde_json::value::to_raw_value(&json!({ "text": "hi", "n": [1, 2] })).unwrap();
        let record = ChannelRecord { channel: "chat".into(), seq: 3, at: 9, principal: "id:ab".into(), kind: "said".into(), body };
        let frame = LiveOut::record_frame(&record);
        assert_eq!(frame, LiveOut::Record(record).encode());
        assert!(frame.starts_with(r#"{"type":"record","channel":"chat""#), "{frame}");
    }

    #[test]
    fn a_subscribe_encodes_one_cursor() {
        let after = LiveIn::Subscribe(Subscribe { channel: "chat".into(), from: Cursor::After(0) });
        assert_eq!(serde_json::from_str::<Value>(&after.encode()).unwrap(), json!({ "type": "subscribe", "channel": "chat", "after": 0 }));
        let last = LiveIn::Subscribe(Subscribe { channel: "chat".into(), from: Cursor::Last(5) });
        assert_eq!(serde_json::from_str::<Value>(&last.encode()).unwrap(), json!({ "type": "subscribe", "channel": "chat", "last": 5 }));
        assert_eq!(serde_json::from_str::<Value>(&LiveIn::Ping.encode()).unwrap(), json!({ "type": "ping" }));
    }
}
