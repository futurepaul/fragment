//! The RFB (VNC) client a page's screen runs (RFC 6143), as Hermes' screen
//! serves it: TigerVNC's Xvnc with no password, behind Hermes' display
//! socket, its bytes in a WebSocket's binary frames. Pure: the server's
//! bytes in (`feed`), what the page draws out (`Event`s), and the bytes to
//! send back (`take_out`).
//!
//! It asks for 32-bit pixels laid out R, G, B, X in memory (a canvas's
//! RGBA once each X is 255), and for raw and copy-rect updates and a
//! desktop's new size; it asks for the next update after each one
//! (incremental), and sends pointer and key events (which Hermes passes on
//! only from the viewer holding its screen's lease).

/// The largest desktop this client takes: past it, the server is refused.
pub const SIZE_MAX: u16 = 4096;
/// The most bytes it holds unparsed (a whole raw update of the largest
/// desktop, and a little).
pub const BUFFER_MAX: usize = SIZE_MAX as usize * SIZE_MAX as usize * 4 + 64 * 1024;
/// The longest name or cut text it reads.
const TEXT_MAX: usize = 64 * 1024;

const RAW: i32 = 0;
const COPY_RECT: i32 = 1;
const DESKTOP_SIZE: i32 = -223;
/// The security type it takes: none (the display socket's ticket is the gate).
const SECURITY_NONE: u8 = 1;

/// What the page draws.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// The desktop's size (at first, and when it changes): a fresh canvas.
    Size { width: u16, height: u16, name: String },
    /// Pixels, RGBA, `width * height * 4` bytes.
    Raw { x: u16, y: u16, width: u16, height: u16, rgba: Vec<u8> },
    /// A rectangle copied from elsewhere on the screen.
    Copy { from_x: u16, from_y: u16, x: u16, y: u16, width: u16, height: u16 },
    Bell,
    /// Text the desktop copied.
    Cut(String),
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum State {
    Version,
    Security,
    SecurityResult,
    ServerInit,
    Messages,
}

pub struct Client {
    state: State,
    buf: Vec<u8>,
    out: Vec<u8>,
    width: u16,
    height: u16,
}

impl Default for Client {
    fn default() -> Self {
        Self::new()
    }
}

fn u16_at(b: &[u8], at: usize) -> u16 {
    u16::from_be_bytes([b[at], b[at + 1]])
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

impl Client {
    pub fn new() -> Client {
        Client { state: State::Version, buf: Vec::new(), out: Vec::new(), width: 0, height: 0 }
    }

    /// The bytes to send the server now.
    pub fn take_out(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.out)
    }

    pub fn size(&self) -> (u16, u16) {
        (self.width, self.height)
    }

    /// The pointer at (x, y), its buttons a mask (1: left, 2: middle, 4:
    /// right, 8 and 16: the wheel up and down).
    pub fn pointer(&mut self, x: u16, y: u16, mask: u8) {
        if self.state != State::Messages {
            return;
        }
        let (x, y) = (x.min(self.width.saturating_sub(1)), y.min(self.height.saturating_sub(1)));
        self.out.push(5);
        self.out.push(mask);
        self.out.extend(x.to_be_bytes());
        self.out.extend(y.to_be_bytes());
    }

    /// A key (an X keysym) pressed or let go.
    pub fn key(&mut self, keysym: u32, down: bool) {
        if self.state != State::Messages {
            return;
        }
        self.out.extend([4, u8::from(down), 0, 0]);
        self.out.extend(keysym.to_be_bytes());
    }

    fn request(&mut self, incremental: bool) {
        self.out.extend([3, u8::from(incremental), 0, 0, 0, 0]);
        self.out.extend(self.width.to_be_bytes());
        self.out.extend(self.height.to_be_bytes());
    }

    /// Bytes from the server: what to draw, as they make whole messages.
    /// An error ends the screen (a server this client cannot speak to).
    pub fn feed(&mut self, data: &[u8]) -> Result<Vec<Event>, String> {
        if self.buf.len() + data.len() > BUFFER_MAX {
            return Err(format!("more than {BUFFER_MAX} bytes unparsed"));
        }
        self.buf.extend_from_slice(data);
        let mut events = Vec::new();
        // bounded by the buffer: each pass consumes a message or waits for more
        loop {
            let used = match self.state {
                State::Version => self.version()?,
                State::Security => self.security()?,
                State::SecurityResult => self.security_result()?,
                State::ServerInit => self.server_init(&mut events)?,
                State::Messages => self.message(&mut events)?,
            };
            match used {
                Some(n) => {
                    self.buf.drain(..n);
                }
                None => return Ok(events),
            }
        }
    }

    fn version(&mut self) -> Result<Option<usize>, String> {
        if self.buf.len() < 12 {
            return Ok(None);
        }
        if !self.buf.starts_with(b"RFB 003.") {
            return Err("not an RFB server".into());
        }
        self.out.extend_from_slice(b"RFB 003.008\n");
        self.state = State::Security;
        Ok(Some(12))
    }

    fn security(&mut self) -> Result<Option<usize>, String> {
        let Some(&n) = self.buf.first() else { return Ok(None) };
        if n == 0 {
            return Err(self.reason(1).unwrap_or_else(|| "the server refused (no reason yet)".into()));
        }
        let n = usize::from(n);
        if self.buf.len() < 1 + n {
            return Ok(None);
        }
        if !self.buf[1..1 + n].contains(&SECURITY_NONE) {
            return Err("the server asks for a password; the screen has none".into());
        }
        self.out.push(SECURITY_NONE);
        self.state = State::SecurityResult;
        Ok(Some(1 + n))
    }

    fn security_result(&mut self) -> Result<Option<usize>, String> {
        if self.buf.len() < 4 {
            return Ok(None);
        }
        if u32_at(&self.buf, 0) != 0 {
            return Err(self.reason(4).unwrap_or_else(|| "the server refused".into()));
        }
        // shared: others watch the same screen
        self.out.push(1);
        self.state = State::ServerInit;
        Ok(Some(4))
    }

    /// A refusal's reason at `at` (a length, then text), once it is whole.
    fn reason(&self, at: usize) -> Option<String> {
        let n = usize::try_from(u32_at(&self.buf, at)).ok().filter(|n| *n <= TEXT_MAX)?;
        let text = self.buf.get(at + 4..at + 4 + n)?;
        Some(format!("the server refused: {}", String::from_utf8_lossy(text)))
    }

    fn server_init(&mut self, events: &mut Vec<Event>) -> Result<Option<usize>, String> {
        if self.buf.len() < 24 {
            return Ok(None);
        }
        let (width, height) = (u16_at(&self.buf, 0), u16_at(&self.buf, 2));
        let n = usize::try_from(u32_at(&self.buf, 20)).map_err(|_| "a name's length")?;
        if n > TEXT_MAX {
            return Err("a desktop's name over the bound".into());
        }
        if self.buf.len() < 24 + n {
            return Ok(None);
        }
        if width == 0 || height == 0 || width > SIZE_MAX || height > SIZE_MAX {
            return Err(format!("a {width}x{height} desktop (at most {SIZE_MAX} a side)"));
        }
        let name = String::from_utf8_lossy(&self.buf[24..24 + n]).into_owned();
        (self.width, self.height) = (width, height);
        // SetPixelFormat: 32 bits, depth 24, little-endian, true colour,
        // 255 a channel, red at 0, green at 8, blue at 16: R, G, B, X in memory
        self.out.extend([0, 0, 0, 0, 32, 24, 0, 1, 0, 255, 0, 255, 0, 255, 0, 8, 16, 0, 0, 0]);
        // SetEncodings: copy-rect, raw, a desktop's new size
        self.out.extend([2, 0, 0, 3]);
        for e in [COPY_RECT, RAW, DESKTOP_SIZE] {
            self.out.extend(e.to_be_bytes());
        }
        self.request(false);
        events.push(Event::Size { width, height, name });
        self.state = State::Messages;
        Ok(Some(24 + n))
    }

    fn message(&mut self, events: &mut Vec<Event>) -> Result<Option<usize>, String> {
        let Some(&kind) = self.buf.first() else { return Ok(None) };
        match kind {
            0 => self.update(events),
            1 => {
                if self.buf.len() < 6 {
                    return Ok(None);
                }
                let n = 6 + usize::from(u16_at(&self.buf, 4)) * 6;
                Ok((self.buf.len() >= n).then_some(n))
            }
            2 => {
                events.push(Event::Bell);
                Ok(Some(1))
            }
            3 => {
                if self.buf.len() < 8 {
                    return Ok(None);
                }
                let n = usize::try_from(u32_at(&self.buf, 4)).map_err(|_| "a cut's length")?;
                if n > TEXT_MAX {
                    return Err("a cut text over the bound".into());
                }
                if self.buf.len() < 8 + n {
                    return Ok(None);
                }
                events.push(Event::Cut(String::from_utf8_lossy(&self.buf[8..8 + n]).into_owned()));
                Ok(Some(8 + n))
            }
            other => Err(format!("an RFB message of type {other}")),
        }
    }

    /// A framebuffer update, once all of it is here: its rectangles in
    /// order, then a request for the next.
    fn update(&mut self, events: &mut Vec<Event>) -> Result<Option<usize>, String> {
        if self.buf.len() < 4 {
            return Ok(None);
        }
        let rects = u16_at(&self.buf, 2);
        let mut at = 4;
        let mut drawn = Vec::new();
        let mut resized = false;
        // bounded by its rectangle count
        for _ in 0..rects {
            if self.buf.len() < at + 12 {
                return Ok(None);
            }
            let b = &self.buf;
            let (x, y, w, h) = (u16_at(b, at), u16_at(b, at + 2), u16_at(b, at + 4), u16_at(b, at + 6));
            let encoding = i32::from_be_bytes([b[at + 8], b[at + 9], b[at + 10], b[at + 11]]);
            at += 12;
            match encoding {
                RAW => {
                    let n = usize::from(w) * usize::from(h) * 4;
                    if self.buf.len() < at + n {
                        return Ok(None);
                    }
                    self.check(x, y, w, h)?;
                    let mut rgba = self.buf[at..at + n].to_vec();
                    for alpha in rgba.iter_mut().skip(3).step_by(4) {
                        *alpha = 255;
                    }
                    drawn.push(Event::Raw { x, y, width: w, height: h, rgba });
                    at += n;
                }
                COPY_RECT => {
                    if self.buf.len() < at + 4 {
                        return Ok(None);
                    }
                    let (from_x, from_y) = (u16_at(&self.buf, at), u16_at(&self.buf, at + 2));
                    self.check(x, y, w, h)?;
                    self.check(from_x, from_y, w, h)?;
                    drawn.push(Event::Copy { from_x, from_y, x, y, width: w, height: h });
                    at += 4;
                }
                DESKTOP_SIZE => {
                    if w == 0 || h == 0 || w > SIZE_MAX || h > SIZE_MAX {
                        return Err(format!("a {w}x{h} desktop (at most {SIZE_MAX} a side)"));
                    }
                    drawn.push(Event::Size { width: w, height: h, name: String::new() });
                    resized = true;
                }
                other => return Err(format!("an update in encoding {other}, which this client never asked for")),
            }
        }
        for e in &drawn {
            if let Event::Size { width, height, .. } = e {
                (self.width, self.height) = (*width, *height);
            }
        }
        events.extend(drawn);
        // a new size starts over; else the next change
        self.request(!resized);
        Ok(Some(at))
    }

    fn check(&self, x: u16, y: u16, w: u16, h: u16) -> Result<(), String> {
        if u32::from(x) + u32::from(w) > u32::from(self.width) || u32::from(y) + u32::from(h) > u32::from(self.height) {
            return Err(format!("a rectangle {w}x{h} at {x},{y} past the {}x{} desktop", self.width, self.height));
        }
        Ok(())
    }
}

/// The X keysym for a browser key (its `KeyboardEvent.key`): a printable
/// character's own (Latin-1 as itself, else Unicode's `0x01000000` plus its
/// code point), or one of the keys a desktop needs by name.
pub fn keysym(key: &str) -> Option<u32> {
    let named = match key {
        "Backspace" => 0xff08,
        "Tab" => 0xff09,
        "Enter" => 0xff0d,
        "Escape" => 0xff1b,
        "Delete" => 0xffff,
        "Home" => 0xff50,
        "ArrowLeft" => 0xff51,
        "ArrowUp" => 0xff52,
        "ArrowRight" => 0xff53,
        "ArrowDown" => 0xff54,
        "PageUp" => 0xff55,
        "PageDown" => 0xff56,
        "End" => 0xff57,
        "Insert" => 0xff63,
        "Shift" => 0xffe1,
        "Control" => 0xffe3,
        "Alt" => 0xffe9,
        "Meta" => 0xffe7,
        "CapsLock" => 0xffe5,
        f if f.len() >= 2 && f.starts_with('F') && f[1..].parse::<u32>().is_ok_and(|n| (1..=12).contains(&n)) => 0xffbe + f[1..].parse::<u32>().expect("checked") - 1,
        _ => {
            let mut chars = key.chars();
            let (Some(c), None) = (chars.next(), chars.next()) else { return None };
            let code = u32::from(c);
            return Some(match code {
                0x20..=0x7e | 0xa0..=0xff => code,
                _ => 0x0100_0000 + code,
            });
        }
    };
    Some(named)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A server's handshake up to its first update request: the bytes
    /// each side sends, as RFC 6143 lays them out.
    fn handshaken(width: u16, height: u16) -> (Client, Vec<Event>) {
        let mut c = Client::new();
        assert!(c.feed(b"RFB 003.008\n").unwrap().is_empty());
        assert_eq!(c.take_out(), b"RFB 003.008\n");
        // two types offered, None among them
        assert!(c.feed(&[2, 2, 1]).unwrap().is_empty());
        assert_eq!(c.take_out(), [1]);
        assert!(c.feed(&[0, 0, 0, 0]).unwrap().is_empty());
        assert_eq!(c.take_out(), [1], "shared");
        let mut init = Vec::new();
        init.extend(width.to_be_bytes());
        init.extend(height.to_be_bytes());
        init.extend([32, 24, 0, 1, 0, 255, 0, 255, 0, 255, 16, 8, 0, 0, 0, 0]);
        init.extend(5u32.to_be_bytes());
        init.extend(b"Xfce4");
        // split across two feeds, as a socket's frames may
        assert!(c.feed(&init[..10]).unwrap().is_empty());
        let events = c.feed(&init[10..]).unwrap();
        (c, events)
    }

    #[test]
    fn the_handshake_asks_for_rgba_raw_and_copies() {
        let (mut c, events) = handshaken(64, 48);
        assert_eq!(events, vec![Event::Size { width: 64, height: 48, name: "Xfce4".into() }]);
        let out = c.take_out();
        // SetPixelFormat (20 bytes): 32 bpp, true colour, red 0, green 8, blue 16
        assert_eq!(&out[..20], &[0, 0, 0, 0, 32, 24, 0, 1, 0, 255, 0, 255, 0, 255, 0, 8, 16, 0, 0, 0]);
        // SetEncodings: copy-rect, raw, desktop size
        assert_eq!(&out[20..24], &[2, 0, 0, 3]);
        assert_eq!(&out[24..36], &[0, 0, 0, 1, 0, 0, 0, 0, 0xff, 0xff, 0xff, 0x21]);
        // the whole screen, not incremental
        assert_eq!(&out[36..], &[3, 0, 0, 0, 0, 0, 0, 64, 0, 48]);
    }

    #[test]
    fn updates_draw_and_ask_for_more() {
        let (mut c, _) = handshaken(64, 48);
        c.take_out();
        let mut u = vec![0, 0, 0, 2];
        // a raw 2x1 at 3,4: two pixels, R G B X
        u.extend([0, 3, 0, 4, 0, 2, 0, 1, 0, 0, 0, 0]);
        u.extend([10, 20, 30, 0, 40, 50, 60, 0]);
        // a copy of 8x8 from 0,0 to 16,16
        u.extend([0, 16, 0, 16, 0, 8, 0, 8, 0, 0, 0, 1, 0, 0, 0, 0]);
        // the update in three pieces
        assert!(c.feed(&u[..7]).unwrap().is_empty());
        assert!(c.feed(&u[7..20]).unwrap().is_empty());
        let events = c.feed(&u[20..]).unwrap();
        assert_eq!(
            events,
            vec![
                Event::Raw { x: 3, y: 4, width: 2, height: 1, rgba: vec![10, 20, 30, 255, 40, 50, 60, 255] },
                Event::Copy { from_x: 0, from_y: 0, x: 16, y: 16, width: 8, height: 8 },
            ]
        );
        assert_eq!(c.take_out(), [3, 1, 0, 0, 0, 0, 0, 64, 0, 48], "the next change, incremental");
        // a bell and cut text between updates
        let mut more = vec![2, 3, 0, 0, 0];
        more.extend(2u32.to_be_bytes());
        more.extend(b"hi");
        assert_eq!(c.feed(&more).unwrap(), vec![Event::Bell, Event::Cut("hi".into())]);
    }

    #[test]
    fn a_new_size_starts_over() {
        let (mut c, _) = handshaken(64, 48);
        c.take_out();
        let u = [0, 0, 0, 1, 0, 0, 0, 0, 0, 128, 0, 96, 0xff, 0xff, 0xff, 0x21];
        assert_eq!(c.feed(&u).unwrap(), vec![Event::Size { width: 128, height: 96, name: String::new() }]);
        assert_eq!(c.size(), (128, 96));
        assert_eq!(c.take_out(), [3, 0, 0, 0, 0, 0, 0, 128, 0, 96], "the whole new screen");
    }

    #[test]
    fn input_goes_once_the_screen_is_up() {
        let mut c = Client::new();
        c.pointer(1, 1, 1);
        c.key(0x61, true);
        assert!(c.take_out().is_empty(), "nothing before the handshake");
        let (mut c, _) = handshaken(64, 48);
        c.take_out();
        c.pointer(10, 500, 1);
        c.key(0x61, true);
        assert_eq!(c.take_out(), [5, 1, 0, 10, 0, 47, 4, 1, 0, 0, 0, 0, 0, 0x61], "the pointer kept on the desktop");
    }

    #[test]
    fn what_a_client_cannot_speak_to_is_refused() {
        assert!(Client::new().feed(b"HTTP/1.1 200").is_err());
        let mut c = Client::new();
        c.feed(b"RFB 003.008\n").unwrap();
        assert!(c.feed(&[1, 2]).unwrap_err().contains("password"), "VNC auth only");
        let mut c = Client::new();
        c.feed(b"RFB 003.008\n").unwrap();
        let mut refused = vec![0];
        refused.extend(3u32.to_be_bytes());
        refused.extend(b"no!");
        assert!(c.feed(&refused).unwrap_err().contains("no!"));
        // a desktop past the bound, a rectangle past the desktop, an encoding never asked for
        let mut c = Client::new();
        c.feed(b"RFB 003.008\n").unwrap();
        c.feed(&[1, 1]).unwrap();
        c.feed(&[0, 0, 0, 0]).unwrap();
        let mut init = vec![0x20, 0x00, 0, 48];
        init.extend([0; 16]);
        init.extend(0u32.to_be_bytes());
        assert!(c.feed(&init).unwrap_err().contains("desktop"));
        let (mut c, _) = handshaken(64, 48);
        let mut past = vec![0, 0, 0, 1, 0, 60, 0, 0, 0, 8, 0, 1, 0, 0, 0, 0];
        past.extend([0; 32]);
        assert!(c.feed(&past).unwrap_err().contains("past"));
        let (mut c, _) = handshaken(64, 48);
        assert!(c.feed(&[0, 0, 0, 1, 0, 0, 0, 0, 0, 1, 0, 1, 0, 0, 0, 7]).unwrap_err().contains("encoding 7"));
    }

    #[test]
    fn keys_are_keysyms() {
        assert_eq!(keysym("a"), Some(0x61));
        assert_eq!(keysym("A"), Some(0x41));
        assert_eq!(keysym(" "), Some(0x20));
        assert_eq!(keysym("é"), Some(0xe9));
        assert_eq!(keysym("€"), Some(0x0100_20ac));
        assert_eq!(keysym("Enter"), Some(0xff0d));
        assert_eq!(keysym("ArrowLeft"), Some(0xff51));
        assert_eq!(keysym("F1"), Some(0xffbe));
        assert_eq!(keysym("F12"), Some(0xffc9));
        assert_eq!(keysym("Unidentified"), None);
        assert_eq!(keysym("F13"), None);
    }
}
