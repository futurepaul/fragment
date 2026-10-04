//! The server's half of a WebSocket (RFC 6455), as much as the renderer's
//! CDP socket needs: the handshake's accept key, a reader that joins a
//! message's frames, and unmasked frames out. Reading and writing are
//! apart, so one thread reads while others write (under the caller's lock).

use std::io::{self, Read, Write};

/// What RFC 6455 appends to a client's key before hashing it.
const GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
/// A control frame's payload is at most this (RFC 6455, 5.5).
const CONTROL_MAX: u64 = 125;

pub const TEXT: u8 = 0x1;
pub const BINARY: u8 = 0x2;
pub const CLOSE: u8 = 0x8;
pub const PING: u8 = 0x9;
pub const PONG: u8 = 0xA;
const CONTINUATION: u8 = 0x0;

/// `Sec-WebSocket-Accept` for a client's `Sec-WebSocket-Key`.
pub fn accept_key(key: &str) -> String {
    use base64::Engine;
    use sha1::Digest;
    base64::engine::general_purpose::STANDARD.encode(sha1::Sha1::digest(format!("{key}{GUID}").as_bytes()))
}

/// What the client said.
#[derive(Debug, PartialEq, Eq)]
pub enum Event {
    /// A whole message: its opcode (`TEXT` or `BINARY`) and payload.
    Message(u8, Vec<u8>),
    Ping(Vec<u8>),
    Pong,
    /// The client closes, with its code if it gave one.
    Close(Option<u16>),
}

/// Why the socket cannot go on: the close code to send, and why.
#[derive(Debug)]
pub enum WsError {
    Io(io::Error),
    /// The client broke the protocol (1002), sent text that is not UTF-8
    /// (1007), or a message past the limit (1009).
    Refused { code: u16, why: &'static str },
}

impl From<io::Error> for WsError {
    fn from(e: io::Error) -> WsError {
        WsError::Io(e)
    }
}

fn refused(code: u16, why: &'static str) -> WsError {
    WsError::Refused { code, why }
}

/// Reads a client's frames, a fragmented message's kept between calls (a
/// control frame may come between its fragments).
pub struct Reader {
    /// A message's payload is at most this.
    max: usize,
    partial: Option<(u8, Vec<u8>)>,
}

impl Reader {
    pub fn new(max: usize) -> Reader {
        Reader { max, partial: None }
    }

    /// The next event. Bounded: each frame read is a message's fragment or
    /// a control frame, and a message's fragments are at most `max` bytes
    /// together, each frame at least two.
    pub fn next(&mut self, r: &mut impl Read) -> Result<Event, WsError> {
        loop {
            let mut head = [0u8; 2];
            r.read_exact(&mut head)?;
            let fin = head[0] & 0x80 != 0;
            if head[0] & 0x70 != 0 {
                return Err(refused(1002, "a reserved bit is set"));
            }
            let opcode = head[0] & 0x0F;
            if head[1] & 0x80 == 0 {
                return Err(refused(1002, "a client's frame is unmasked"));
            }
            let len = match head[1] & 0x7F {
                126 => {
                    let mut b = [0u8; 2];
                    r.read_exact(&mut b)?;
                    u64::from(u16::from_be_bytes(b))
                }
                127 => {
                    let mut b = [0u8; 8];
                    r.read_exact(&mut b)?;
                    let n = u64::from_be_bytes(b);
                    if n >> 63 != 0 {
                        return Err(refused(1002, "a frame's length has its top bit set"));
                    }
                    n
                }
                n => u64::from(n),
            };
            let control = opcode & 0x8 != 0;
            if control && (!fin || len > CONTROL_MAX) {
                return Err(refused(1002, "a control frame is fragmented or longer than 125 bytes"));
            }
            let sofar = self.partial.as_ref().map_or(0, |(_, p)| p.len());
            if !control && sofar as u64 + len > self.max as u64 {
                return Err(refused(1009, "a message is past the limit"));
            }
            let mut mask = [0u8; 4];
            r.read_exact(&mut mask)?;
            let mut payload = vec![0u8; len as usize];
            r.read_exact(&mut payload)?;
            for (i, b) in payload.iter_mut().enumerate() {
                *b ^= mask[i % 4];
            }
            match opcode {
                PING => return Ok(Event::Ping(payload)),
                PONG => return Ok(Event::Pong),
                CLOSE => {
                    let code = (payload.len() >= 2).then(|| u16::from_be_bytes([payload[0], payload[1]]));
                    return Ok(Event::Close(code));
                }
                TEXT | BINARY if self.partial.is_some() => return Err(refused(1002, "a new message began inside another")),
                TEXT | BINARY if fin => return finished(opcode, payload),
                TEXT | BINARY => self.partial = Some((opcode, payload)),
                CONTINUATION => {
                    let Some((first, mut sofar)) = self.partial.take() else { return Err(refused(1002, "a continuation with no message to continue")) };
                    sofar.extend_from_slice(&payload);
                    if fin {
                        return finished(first, sofar);
                    }
                    self.partial = Some((first, sofar));
                }
                _ => return Err(refused(1002, "an unknown opcode")),
            }
        }
    }
}

fn finished(opcode: u8, payload: Vec<u8>) -> Result<Event, WsError> {
    if opcode == TEXT && std::str::from_utf8(&payload).is_err() {
        return Err(refused(1007, "a text message is not UTF-8"));
    }
    Ok(Event::Message(opcode, payload))
}

/// One unmasked frame, its whole payload, flushed.
pub fn write_frame(w: &mut impl Write, opcode: u8, payload: &[u8]) -> io::Result<()> {
    assert!(opcode & 0x8 == 0 || payload.len() as u64 <= CONTROL_MAX, "a control frame's payload fits in one frame");
    let mut head = Vec::with_capacity(10);
    head.push(0x80 | opcode);
    match payload.len() {
        n if n < 126 => head.push(n as u8),
        n if n <= usize::from(u16::MAX) => {
            head.push(126);
            head.extend_from_slice(&(n as u16).to_be_bytes());
        }
        n => {
            head.push(127);
            head.extend_from_slice(&(n as u64).to_be_bytes());
        }
    }
    w.write_all(&head)?;
    w.write_all(payload)?;
    w.flush()
}

/// A close frame with `code` (and no reason).
pub fn write_close(w: &mut impl Write, code: u16) -> io::Result<()> {
    write_frame(w, CLOSE, &code.to_be_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A client's frame: masked, as a client must.
    fn masked(fin: bool, opcode: u8, payload: &[u8]) -> Vec<u8> {
        let mask = [0x12, 0x34, 0x56, 0x78];
        let mut out = vec![if fin { 0x80 } else { 0 } | opcode];
        match payload.len() {
            n if n < 126 => out.push(0x80 | n as u8),
            n if n <= 0xFFFF => {
                out.push(0x80 | 126);
                out.extend_from_slice(&(n as u16).to_be_bytes());
            }
            n => {
                out.push(0x80 | 127);
                out.extend_from_slice(&(n as u64).to_be_bytes());
            }
        }
        out.extend_from_slice(&mask);
        out.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
        out
    }

    fn events(bytes: Vec<u8>, max: usize) -> Vec<Result<Event, String>> {
        let mut r = io::Cursor::new(bytes);
        let mut reader = Reader::new(max);
        let mut out = vec![];
        loop {
            match reader.next(&mut r) {
                Ok(e) => out.push(Ok(e)),
                Err(WsError::Io(_)) => return out,
                Err(WsError::Refused { code, why }) => {
                    out.push(Err(format!("{code}: {why}")));
                    return out;
                }
            }
        }
    }

    /// RFC 6455's own example: the key `dGhlIHNhbXBsZSBub25jZQ==`.
    #[test]
    fn the_accept_key_is_rfc_6455s() {
        assert_eq!(accept_key("dGhlIHNhbXBsZSBub25jZQ=="), "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
    }

    /// Valid: a short, a 16-bit and a 64-bit length; a message in three
    /// fragments with a ping between them; a close with its code.
    #[test]
    fn a_clients_frames_become_its_messages() {
        let long = vec![b'a'; 70_000];
        let mut bytes = masked(true, TEXT, b"{\"id\":1}");
        bytes.extend(masked(true, BINARY, &[7u8; 300]));
        bytes.extend(masked(true, TEXT, &long));
        bytes.extend(masked(false, TEXT, b"one "));
        bytes.extend(masked(true, PING, b"p"));
        bytes.extend(masked(false, CONTINUATION, b"two "));
        bytes.extend(masked(true, CONTINUATION, b"three"));
        bytes.extend(masked(true, PONG, b""));
        bytes.extend(masked(true, CLOSE, &1000u16.to_be_bytes()));
        bytes.extend(masked(true, CLOSE, b""));
        let got = events(bytes, 1 << 20);
        assert_eq!(
            got,
            vec![
                Ok(Event::Message(TEXT, b"{\"id\":1}".to_vec())),
                Ok(Event::Message(BINARY, vec![7u8; 300])),
                Ok(Event::Message(TEXT, long)),
                Ok(Event::Ping(b"p".to_vec())),
                Ok(Event::Message(TEXT, b"one two three".to_vec())),
                Ok(Event::Pong),
                Ok(Event::Close(Some(1000))),
                Ok(Event::Close(None)),
            ]
        );
    }

    /// Invalid: each refused with its close code, nothing after it read.
    #[test]
    fn a_frame_that_breaks_the_protocol_is_refused() {
        let unmasked = vec![0x81, 0x02, b'h', b'i'];
        let reserved = {
            let mut f = masked(true, TEXT, b"x");
            f[0] |= 0x40;
            f
        };
        let mut nested = masked(false, TEXT, b"a");
        nested.extend(masked(true, TEXT, b"b"));
        let cases: Vec<(&str, Vec<u8>, &str)> = vec![
            ("unmasked", unmasked, "1002"),
            ("a reserved bit", reserved, "1002"),
            ("an unknown opcode", masked(true, 0x3, b"x"), "1002"),
            ("a stray continuation", masked(true, CONTINUATION, b"x"), "1002"),
            ("a message inside another", nested, "1002"),
            ("a fragmented ping", masked(false, PING, b"x"), "1002"),
            ("a long ping", masked(true, PING, &[0u8; 126]), "1002"),
            ("text that is not UTF-8", masked(true, TEXT, &[0xff, 0xfe]), "1007"),
            ("a message past the limit", masked(true, BINARY, &[0u8; 65]), "1009"),
        ];
        for (what, bytes, code) in cases {
            let got = events(bytes, 64);
            assert!(matches!(got.last(), Some(Err(e)) if e.starts_with(code)), "{what}: {got:?}");
        }
        // fragments are counted together against the limit
        let mut split = masked(false, BINARY, &[0u8; 40]);
        split.extend(masked(true, CONTINUATION, &[0u8; 40]));
        assert!(matches!(events(split, 64).last(), Some(Err(e)) if e.starts_with("1009")));
    }

    /// What `write_frame` writes, read back as a client reads it (unmasked).
    #[test]
    fn frames_out_carry_their_whole_payload() {
        for n in [0usize, 5, 125, 126, 65_535, 65_536, 200_000] {
            let mut out = vec![];
            write_frame(&mut out, TEXT, &vec![b'x'; n]).unwrap();
            let (len, at) = match out[1] {
                126 => (usize::from(u16::from_be_bytes([out[2], out[3]])), 4),
                127 => (u64::from_be_bytes(out[2..10].try_into().unwrap()) as usize, 10),
                l => (usize::from(l), 2),
            };
            assert_eq!((out[0], out[1] & 0x80, len, out.len() - at), (0x81, 0, n, n), "{n} bytes");
        }
        let mut out = vec![];
        write_close(&mut out, 1011).unwrap();
        assert_eq!(out, vec![0x88, 2, 0x03, 0xF3]);
    }
}
