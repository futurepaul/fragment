//! The owner's browser (docs/explorations/owner-browser.md, a spike): a
//! computer's guest reaches a browser its owner connected, over the Chrome
//! DevTools Protocol, through the Computer DO. The owner's extension holds
//! one socket to the computer; each of the guest's CDP sockets is carried
//! over it, its frames named by the guest socket they belong to. These are
//! the pure rules of that carrying: what each side may say, how a frame
//! names its socket, what a browser's link and name are, and which ends of
//! its socket it comes back from. The cell reads no CDP: it carries it.

use serde_json::Value;

/// The host a guest reaches its owner's browser on (`FRAGMENT_BROWSER` is
/// `http://` and it): the computer's own intercept, never a network's.
pub const GUEST_HOST: &str = "browser.fragment.internal";
/// Where a guest opens a CDP socket on `GUEST_HOST`, as a browser's
/// DevTools serves one: a client given the full URL connects to it as
/// given, with no `VERSION_PATH` lookup first.
pub const GUEST_PATH: &str = "/devtools/browser";
/// Where a guest asks whether its owner's browser is there (`GET`), as a
/// CDP client asks a browser's DevTools before it connects: while one is
/// connected, `version`; otherwise the platform's words that it is not.
/// The bridge asks before each of its owner's turns, and tells the agent.
pub const VERSION_PATH: &str = "/json/version";
/// The ping an extension sends to keep its service worker alive (Chrome
/// keeps one alive while its socket carries a message every 30 s), which
/// the computer answers with `PONG` and carries nowhere.
pub const PING: &str = r#"{"type":"ping"}"#;
pub const PONG: &str = r#"{"type":"pong"}"#;
/// The word a frame carries for a guest socket that ended, either way.
pub const CLOSE: &str = "close";
/// A guest socket's name: hex digits the computer picks, unique among its
/// open ones.
pub const SOCKET_HEX: usize = 8;
/// The longest path a guest's socket may name below `GUEST_PATH` (a
/// browser's own names a UUID there; the computer reads none of it).
pub const GUEST_PATH_MAX_BYTES: usize = 128;
/// A browser's link: the secret its extension keeps, made the first time
/// its owner connects it, with which it comes back (32 random bytes, kept
/// hashed); and the link's id, which names it to its owner.
pub const LINK_HEX: usize = 64;
pub const LINK_ID_HEX: usize = 8;
/// A browser's name, as its extension says it ("Chrome on macOS").
pub const NAME_MAX_BYTES: usize = 64;

/// How the computer ends its owner's browser's socket, beside the
/// protocol's own codes: each of these means its extension must not come
/// back on its own (any other end, a dropped network or a restart, it
/// does, with its link). The extension (extension/link.js) keeps the same.
pub const REPLACED: u16 = 4001;
pub const FORGOTTEN: u16 = 4002;
pub const WIPED: u16 = 4003;

/// Whether an extension whose socket ended with `code` comes back on its
/// own, with its link.
pub fn comes_back(code: u16) -> bool {
    !matches!(code, REPLACED | FORGOTTEN | WIPED)
}

/// What one frame from the owner's extension says.
#[derive(Debug, PartialEq, Eq)]
pub enum Said<'a> {
    /// A CDP message for the guest socket `socket`, as the browser answered.
    Message { socket: &'a str, message: &'a str },
    /// The extension ended the guest socket `socket`.
    Close { socket: &'a str },
    /// Its keepalive.
    Ping,
    /// Its first word: the browser's name, as its owner sees it.
    Hello { name: String },
}

/// Whether `s` names a guest socket.
pub fn valid_socket(s: &str) -> bool {
    s.len() == SOCKET_HEX && lower_hex(s)
}

/// Whether `s` is a link (its secret) or a link's id.
pub fn valid_link(s: &str) -> bool {
    s.len() == LINK_HEX && lower_hex(s)
}
pub fn valid_link_id(s: &str) -> bool {
    s.len() == LINK_ID_HEX && lower_hex(s)
}

fn lower_hex(s: &str) -> bool {
    s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// A browser's name as kept: its words trimmed, nothing but visible
/// characters and spaces, at most `NAME_MAX_BYTES`; `None` for none left.
pub fn name(s: &str) -> Option<String> {
    let s = s.trim();
    let ok = !s.is_empty() && s.len() <= NAME_MAX_BYTES && s.chars().all(|c| c == ' ' || (!c.is_control() && !c.is_whitespace()));
    ok.then(|| s.to_string())
}

/// The frame that carries `message` (a CDP message, or `CLOSE`) for the
/// guest socket `socket`.
pub fn frame(socket: &str, message: &str) -> String {
    assert!(valid_socket(socket), "a guest socket is named by the computer: {socket:?}");
    format!("{socket} {message}")
}

/// The frame that gives a browser its link, once, as its first connection
/// opens: `{"type":"link","id":…,"link":…}`.
pub fn link_frame(id: &str, link: &str) -> String {
    assert!(valid_link_id(id) && valid_link(link), "a link is made by the computer");
    serde_json::json!({ "type": "link", "id": id, "link": link }).to_string()
}

/// What a frame from the owner's extension says, or `None` for one that
/// breaks the rules (the computer drops it, and says so in its log).
pub fn read(frame: &str) -> Option<Said<'_>> {
    if frame.starts_with('{') {
        let v: Value = serde_json::from_str(frame).ok()?;
        return match v["type"].as_str()? {
            "ping" => Some(Said::Ping),
            "hello" => Some(Said::Hello { name: name(v["name"].as_str()?)? }),
            _ => None,
        };
    }
    let (socket, message) = frame.split_once(' ')?;
    if !valid_socket(socket) || message.is_empty() {
        return None;
    }
    Some(if message == CLOSE { Said::Close { socket } } else { Said::Message { socket, message } })
}

/// `VERSION_PATH`'s answer while browser `name` is connected, in a
/// browser's own fields: the protocol the extension speaks, and where a
/// CDP client opens its socket (`webSocketDebuggerUrl`).
pub fn version(name: &str) -> Value {
    serde_json::json!({ "Browser": name, "Protocol-Version": "1.3", "webSocketDebuggerUrl": format!("ws://{GUEST_HOST}{GUEST_PATH}") })
}

/// Whether a guest's request names the CDP socket: `GUEST_PATH`, or a path
/// of visible characters below it.
pub fn guest_path(path: &str) -> bool {
    match path.strip_prefix(GUEST_PATH) {
        Some("") => true,
        Some(rest) => rest.starts_with('/') && rest.len() <= GUEST_PATH_MAX_BYTES && rest.bytes().all(|b| b.is_ascii_graphic()),
        None => false,
    }
}

/// Whether `origin` is a Chrome extension's, as Chrome sends it on the
/// extension's own sockets and fetches: `chrome-extension://` and its id,
/// 32 letters `a` to `p`. Only an extension opens the owner's socket, or
/// comes back with its link; a page holding either could not. Any
/// extension: which one is the platform's own is question 3's (the
/// write-up), as is shipping it.
pub fn extension_origin(origin: &str) -> bool {
    origin.strip_prefix("chrome-extension://").is_some_and(|id| id.len() == 32 && id.bytes().all(|b| (b'a'..=b'p').contains(&b)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frame_names_its_socket_and_reads_back() {
        let f = frame("0a1b2c3d", r#"{"id":1,"method":"Browser.getVersion"}"#);
        assert_eq!(f, r#"0a1b2c3d {"id":1,"method":"Browser.getVersion"}"#);
        assert_eq!(read(&f), Some(Said::Message { socket: "0a1b2c3d", message: r#"{"id":1,"method":"Browser.getVersion"}"# }));
        assert_eq!(read(&frame("0a1b2c3d", CLOSE)), Some(Said::Close { socket: "0a1b2c3d" }));
        assert_eq!(read(PING), Some(Said::Ping));
        // a message may hold spaces of its own: only the first one splits
        assert_eq!(read("0a1b2c3d {\"a\": 1}"), Some(Said::Message { socket: "0a1b2c3d", message: "{\"a\": 1}" }));
    }

    #[test]
    fn a_frame_that_breaks_the_rules_reads_as_none() {
        for bad in ["", "0a1b2c3d", "0a1b2c3d ", "0A1B2C3D {}", "0a1b2c3 {}", "0a1b2c3d4 {}", "zzzzzzzz {}", " {}", "{\"type\":\"pong\"}", "{\"type\":\"hello\"}", "{\"type\":\"hello\",\"name\":\"  \"}", "{not json"] {
            assert_eq!(read(bad), None, "{bad:?}");
        }
    }

    #[test]
    #[should_panic(expected = "a guest socket is named by the computer")]
    fn a_frame_for_no_socket_is_a_bug() {
        frame("nope", "{}");
    }

    #[test]
    fn the_guest_names_the_cdp_socket_or_nothing() {
        for ok in ["/devtools/browser", "/devtools/browser/owner", "/devtools/browser/5dfbf847-391b-4056-b6eb-5f91e653ffe6"] {
            assert!(guest_path(ok), "{ok}");
        }
        let long = format!("/devtools/browser/{}", "a".repeat(GUEST_PATH_MAX_BYTES));
        for bad in ["/", "/json/version", "/devtools/browserx", "/devtools/page/1", "/devtools/browser/a b", long.as_str()] {
            assert!(!guest_path(bad), "{bad}");
        }
    }

    #[test]
    fn a_connected_browser_answers_as_a_browsers_devtools_does() {
        let v = version("Chromium on macOS");
        assert_eq!(v["Browser"], "Chromium on macOS");
        assert_eq!(v["Protocol-Version"], "1.3");
        assert_eq!(v["webSocketDebuggerUrl"], "ws://browser.fragment.internal/devtools/browser");
        let at = v["webSocketDebuggerUrl"].as_str().unwrap().strip_prefix(&format!("ws://{GUEST_HOST}")).unwrap();
        assert!(guest_path(at) && !guest_path(VERSION_PATH), "its socket is the guest's, and the lookup is not one");
    }

    #[test]
    fn only_an_extension_opens_the_owners_socket() {
        assert!(extension_origin("chrome-extension://abcdefghijklmnopabcdefghijklmnop"));
        for bad in [
            "",
            "null",
            "http://127.0.0.1:8790",
            "chrome-extension://",
            "chrome-extension://abcdefghijklmnopabcdefghijklmno",
            "chrome-extension://abcdefghijklmnopabcdefghijklmnoq",
            "chrome-extension://abcdefghijklmnopabcdefghijklmnop/",
            "moz-extension://abcdefghijklmnopabcdefghijklmnop",
        ] {
            assert!(!extension_origin(bad), "{bad}");
        }
    }

    /// A browser's first word names it; a name is kept trimmed, visible,
    /// and short, and anything else is no name.
    #[test]
    fn a_browser_says_its_name() {
        assert_eq!(read(r#"{"type":"hello","name":" Chrome on macOS "}"#), Some(Said::Hello { name: "Chrome on macOS".into() }));
        assert_eq!(name("Chromium 148 on Linux").as_deref(), Some("Chromium 148 on Linux"));
        for bad in ["", "   ", "tab\there", "line\nbreak", &"x".repeat(NAME_MAX_BYTES + 1)] {
            assert_eq!(name(bad), None, "{bad:?}");
        }
    }

    /// The link frame carries a link the computer made, and only that.
    #[test]
    fn a_link_is_the_computers_to_give() {
        let (id, link) = ("0a1b2c3d", "ab".repeat(32));
        let v: Value = serde_json::from_str(&link_frame(id, &link)).unwrap();
        assert_eq!((v["type"].as_str(), v["id"].as_str(), v["link"].as_str()), (Some("link"), Some(id), Some(link.as_str())));
        assert!(valid_link(&link) && valid_link_id(id));
        assert!(!valid_link(&"AB".repeat(32)) && !valid_link(&"ab".repeat(31)) && !valid_link_id("0a1b2c3"));
        assert!(std::panic::catch_unwind(|| link_frame("nope", &link)).is_err());
    }

    /// An extension comes back on its own after any end but its owner's or
    /// the computer's word: replaced by a newer browser, forgotten, wiped.
    #[test]
    fn a_browser_comes_back_unless_told_not_to() {
        for back in [1000, 1001, 1006, 1011] {
            assert!(comes_back(back), "{back}");
        }
        for stay in [REPLACED, FORGOTTEN, WIPED] {
            assert!(!comes_back(stay), "{stay}");
        }
    }
}
