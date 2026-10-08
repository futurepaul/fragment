//! A viewer of an agent's screen as the screen page's noVNC is one: the
//! RFB stream over the bridge's `websockify?viewer=&agent=` socket, and
//! Take over over its `control?viewer=&agent=` socket (docs/computers.md,
//! Ports). Just
//! enough RFB 3.8 to read a whole frame (raw, 32-bit true colour) and send
//! pointer and key events; and, as noVNC 1.7.0 does, it asks for the
//! extended clipboard and answers the server's capabilities with its own
//! (a ClientCutText of negative length, which the bridge's input gate once
//! misread: no input reached the screen after Take over).

#![allow(dead_code)]

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio_tungstenite::tungstenite::Message;

use fragment_bridge::net::{self, Base, ClientWs};

/// A frame is read within this.
const FRAME_WAIT: Duration = Duration::from_secs(30);

/// The extended clipboard's pseudo-encoding (0xC0A1E5CE).
const EXTENDED_CLIPBOARD: i32 = -1_063_131_698;
/// Its capabilities action.
const CAPS: u32 = 1 << 24;

pub struct Viewer {
    ws: ClientWs,
    buf: Vec<u8>,
    pub width: u16,
    pub height: u16,
    pub name: String,
    /// The server offered the extended clipboard, and this viewer answered.
    pub clipboard_caps: bool,
}

impl Viewer {
    /// Opens `agent`'s screen as viewer `id`: the RFB handshake (no auth,
    /// shared), then 32-bit true colour, raw encoding only. The bridge starts
    /// the display when it is down, so the first bytes may take a while.
    pub async fn open(base: &Base, id: &str, agent: &str, wait: Duration) -> Result<Viewer, String> {
        let ws = net::connect_ws(base, &format!("/websockify?viewer={id}&agent={agent}"), &[]).await?;
        let mut v = Viewer { ws, buf: Vec::new(), width: 0, height: 0, name: String::new(), clipboard_caps: false };
        let version = v.take(12, wait).await?;
        if &version[..4] != b"RFB " {
            return Err(format!("not RFB: {version:?}"));
        }
        v.send(b"RFB 003.008\n".to_vec()).await?;
        let n = v.take(1, FRAME_WAIT).await?[0] as usize;
        if n == 0 {
            let len = u32::from_be_bytes(v.take(4, FRAME_WAIT).await?.try_into().unwrap()) as usize;
            return Err(format!("the server refused: {}", String::from_utf8_lossy(&v.take(len, FRAME_WAIT).await?)));
        }
        let types = v.take(n, FRAME_WAIT).await?;
        if !types.contains(&1) {
            return Err(format!("no security type None among {types:?}"));
        }
        v.send(vec![1]).await?;
        let result = v.take(4, FRAME_WAIT).await?;
        if result != [0, 0, 0, 0] {
            return Err(format!("security result {result:?}"));
        }
        v.send(vec![1]).await?; // ClientInit: shared
        let init = v.take(24, FRAME_WAIT).await?;
        v.width = u16::from_be_bytes([init[0], init[1]]);
        v.height = u16::from_be_bytes([init[2], init[3]]);
        let len = u32::from_be_bytes([init[20], init[21], init[22], init[23]]) as usize;
        v.name = String::from_utf8_lossy(&v.take(len, FRAME_WAIT).await?).into_owned();
        // SetPixelFormat: 32 bpp, depth 24, little-endian, true colour, 8 bits a channel
        v.send(vec![0u8, 0, 0, 0, 32, 24, 0, 1, 0, 255, 0, 255, 0, 255, 16, 8, 0, 0, 0, 0]).await?;
        // SetEncodings: raw, and the extended clipboard as noVNC asks for it
        let mut enc = vec![2u8, 0, 0, 2, 0, 0, 0, 0];
        enc.extend_from_slice(&EXTENDED_CLIPBOARD.to_be_bytes());
        v.send(enc).await?;
        // its first frames, until the server's clipboard caps are answered
        // (Xvnc sends them as it reads the encodings)
        for _ in 0..3 {
            if v.clipboard_caps {
                break;
            }
            v.frame().await?;
        }
        Ok(v)
    }

    async fn send(&mut self, bytes: Vec<u8>) -> Result<(), String> {
        self.ws.send(Message::binary(bytes)).await.map_err(|e| e.to_string())
    }

    /// The next `n` bytes of the server's stream, within `wait`.
    async fn take(&mut self, n: usize, wait: Duration) -> Result<Vec<u8>, String> {
        let fill = async {
            // bounded by the socket and the timeout around it
            while self.buf.len() < n {
                match self.ws.next().await {
                    Some(Ok(Message::Binary(b))) => self.buf.extend_from_slice(&b),
                    Some(Ok(Message::Close(_))) | None => return Err("the screen's socket closed".to_string()),
                    Some(Ok(_)) => {}
                    Some(Err(e)) => return Err(e.to_string()),
                }
            }
            Ok(())
        };
        tokio::time::timeout(wait, fill).await.map_err(|_| format!("no {n} bytes from the screen within {wait:?}"))??;
        Ok(self.buf.drain(..n).collect())
    }

    /// The whole screen, now: `width * height` pixels as `0x00RRGGBB`.
    pub async fn frame(&mut self) -> Result<Vec<u32>, String> {
        let (w, h) = (self.width, self.height);
        let mut req = vec![3u8, 0, 0, 0, 0, 0];
        req.extend_from_slice(&w.to_be_bytes());
        req.extend_from_slice(&h.to_be_bytes());
        self.send(req).await?;
        let mut pixels = vec![0u32; w as usize * h as usize];
        // bounded: one update answers the request; other messages are skipped
        loop {
            let t = self.take(1, FRAME_WAIT).await?[0];
            match t {
                0 => {
                    let head = self.take(3, FRAME_WAIT).await?;
                    let rects = u16::from_be_bytes([head[1], head[2]]);
                    for _ in 0..rects {
                        let r = self.take(12, FRAME_WAIT).await?;
                        let be = |i: usize| u16::from_be_bytes([r[i], r[i + 1]]) as usize;
                        let (x, y, rw, rh) = (be(0), be(2), be(4), be(6));
                        let enc = i32::from_be_bytes([r[8], r[9], r[10], r[11]]);
                        if enc != 0 {
                            return Err(format!("an encoding not asked for: {enc}"));
                        }
                        let data = self.take(rw * rh * 4, FRAME_WAIT).await?;
                        for row in 0..rh {
                            for col in 0..rw {
                                let i = (row * rw + col) * 4;
                                let px = u32::from_le_bytes([data[i], data[i + 1], data[i + 2], data[i + 3]]) & 0x00ff_ffff;
                                if let Some(p) = pixels.get_mut((y + row) * w as usize + x + col) {
                                    *p = px;
                                }
                            }
                        }
                    }
                    return Ok(pixels);
                }
                2 => {} // bell
                3 => {
                    let head = self.take(7, FRAME_WAIT).await?;
                    let len = i32::from_be_bytes([head[3], head[4], head[5], head[6]]);
                    let body = self.take(len.unsigned_abs() as usize, FRAME_WAIT).await?;
                    let flags = if body.len() >= 4 { u32::from_be_bytes([body[0], body[1], body[2], body[3]]) } else { 0 };
                    if len < 0 && flags & CAPS != 0 && !self.clipboard_caps {
                        // noVNC's answer (its extendedClipboardCaps): caps,
                        // request, peek, notify and provide; text, size 0
                        let caps = [0x1fu8, 0, 0, 1, 0, 0, 0, 0];
                        let mut m = vec![6u8, 0, 0, 0];
                        m.extend_from_slice(&(-(caps.len() as i32)).to_be_bytes());
                        m.extend_from_slice(&caps);
                        self.send(m).await?;
                        self.clipboard_caps = true;
                    }
                }
                other => return Err(format!("a server message this viewer does not read: {other}")),
            }
        }
    }

    /// A pointer event: the pointer at (x, y) with `buttons` held.
    pub async fn pointer(&mut self, x: u16, y: u16, buttons: u8) -> Result<(), String> {
        let mut m = vec![5u8, buttons];
        m.extend_from_slice(&x.to_be_bytes());
        m.extend_from_slice(&y.to_be_bytes());
        self.send(m).await
    }

    /// A key pressed and released (an X keysym).
    pub async fn key(&mut self, keysym: u32) -> Result<(), String> {
        for down in [1u8, 0] {
            let mut m = vec![4u8, down, 0, 0];
            m.extend_from_slice(&keysym.to_be_bytes());
            self.send(m).await?;
        }
        Ok(())
    }
}

/// How many distinct colours a frame holds: a black or blank screen has one.
pub fn colours(frame: &[u32]) -> usize {
    let mut seen: Vec<u32> = frame.iter().step_by(7).copied().collect();
    seen.sort_unstable();
    seen.dedup();
    seen.len()
}

/// An agent's screen's control socket, as the page's `control?viewer=&agent=`.
pub struct Control {
    ws: ClientWs,
}

impl Control {
    pub async fn open(base: &Base, id: &str, agent: &str) -> Result<Control, String> {
        Ok(Control { ws: net::connect_ws(base, &format!("/control?viewer={id}&agent={agent}"), &[]).await? })
    }

    /// The next `{type: "control", agent, name, holder}` it says.
    pub async fn next(&mut self) -> Result<Value, String> {
        let read = async {
            // bounded by the socket and the timeout around it
            loop {
                match self.ws.next().await {
                    Some(Ok(Message::Text(t))) => return serde_json::from_str::<Value>(&t).map_err(|e| e.to_string()),
                    Some(Ok(Message::Close(_))) | None => return Err("the control socket closed".to_string()),
                    Some(Ok(_)) => {}
                    Some(Err(e)) => return Err(e.to_string()),
                }
            }
        };
        tokio::time::timeout(FRAME_WAIT, read).await.map_err(|_| "no word from the control socket".to_string())?
    }

    pub async fn say(&mut self, kind: &str) -> Result<(), String> {
        self.ws.send(Message::text(serde_json::json!({ "type": kind }).to_string())).await.map_err(|e| e.to_string())
    }
}
