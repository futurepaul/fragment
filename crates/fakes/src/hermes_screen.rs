//! A Hermes' screen (its bot desktop), as the sandcastle fake's Hermes
//! serves it (docs/one-home.md, phase 4), after Hermes v0.21.5
//! (`tui_gateway/methods_display.py`, `hermes_cli/web_routers/display.py`):
//!
//! - On its `/api/ws`: `display.status`, `display.start`, `display.observe
//!   {viewer_id?}` (a single-use ticket for 30 s, and a viewer id, honoured
//!   later only on the connection that minted it), `display.lease.acquire
//!   {viewer_id}` and `display.lease.release {viewer_id}`, each change
//!   told as a `display.lease` event (on the connection that made it).
//! - `/api/display/ws?display_ticket=`: RFB with no security in binary
//!   frames, a `WIDTH`x`HEIGHT` desktop of one colour, a whole frame on
//!   each request (an incremental one a moment later). A ticket missing,
//!   used, or expired closes it 4401; a text frame 1003.
//! - Input (pointer and keys) reaches the desktop only from the viewer
//!   holding the lease, as Hermes' filter passes it; each press of the
//!   left button it passes flips the desktop's colour.
//!
//! Levers: `input` (what reached a desktop), `dropped` (what the filter
//! dropped), `sockets` (display sockets opened).

use std::collections::{HashMap, HashSet, VecDeque};
use std::net::TcpStream;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::sandcastle::{read_frame, write_frame};

pub const WIDTH: u16 = 64;
pub const HEIGHT: u16 = 48;
/// The desktop's two colours (R, G, B): the first at the start.
pub const COLOURS: [[u8; 3]; 2] = [[30, 90, 200], [40, 180, 90]];
const TICKET_LIFE: Duration = Duration::from_secs(30);
/// How long an incremental request waits before its frame.
const INCREMENTAL_WAIT: Duration = Duration::from_millis(150);
/// The most bytes a client message may wait in (a cut text and a little).
const IN_MAX: usize = 300 * 1024;

#[derive(Default)]
pub struct Screens {
    /// Per computer: the viewer holding its lease (None: Hermes), its epoch.
    lease: HashMap<String, (Option<String>, u64)>,
    /// Ticket → (computer, viewer, its end).
    tickets: HashMap<String, (String, String, Instant)>,
    /// Per computer: which of `COLOURS` it shows.
    colour: HashMap<String, usize>,
    next: u64,
    pub input: Vec<Value>,
    pub dropped: u64,
    pub sockets: u64,
}

impl Screens {
    fn lease_of(&self, computer: &str) -> Value {
        let (holder, epoch) = self.lease.get(computer).cloned().unwrap_or((None, 0));
        let hash = holder.as_deref().map(|v| hex::encode(Sha256::digest(v.as_bytes()))[..12].to_string());
        json!({ "holder": if holder.is_some() { "human" } else { "agent" }, "viewer_id": null, "viewer_hash": hash, "epoch": epoch, "reason": null })
    }

    fn status(&self, computer: &str) -> Value {
        json!({ "supported": true, "installed": true, "running": true, "geometry": format!("{WIDTH}x{HEIGHT}"), "lease": self.lease_of(computer) })
    }

    /// Whether the desktop holds its lease for `viewer`.
    pub fn holds(&self, computer: &str, viewer: &str) -> bool {
        self.lease.get(computer).is_some_and(|(h, _)| h.as_deref() == Some(viewer))
    }
}

/// A `display.*` method on a `/api/ws` connection (`minted`: the viewer ids
/// it minted): its result and, for a lease's change, the event to tell.
pub fn rpc(s: &mut Screens, computer: &str, minted: &mut HashSet<String>, method: &str, params: &Value) -> Result<(Value, Option<Value>), &'static str> {
    let mine = |minted: &HashSet<String>| params["viewer_id"].as_str().filter(|v| minted.contains(*v)).map(str::to_string);
    let changed = |s: &Screens| json!({ "jsonrpc": "2.0", "method": "event", "params": { "type": "display.lease", "session_id": "", "payload": { "profile_key": "default", "lease": s.lease_of(computer) } } });
    match method {
        "display.status" | "display.start" => Ok((s.status(computer), None)),
        "display.observe" => {
            s.next += 1;
            let viewer = mine(minted).unwrap_or_else(|| format!("viewer-{}", s.next));
            minted.insert(viewer.clone());
            let ticket = format!("ticket-{}-{}", s.next, &hex::encode(Sha256::digest(format!("{computer}{}", s.next).as_bytes()))[..16]);
            s.tickets.insert(ticket.clone(), (computer.to_string(), viewer.clone(), Instant::now() + TICKET_LIFE));
            let mut v = s.status(computer);
            v["ticket"] = json!(ticket);
            v["path"] = json!("/api/display/ws");
            v["viewer_id"] = json!(viewer);
            Ok((v, None))
        }
        "display.lease.acquire" => {
            let viewer = mine(minted).ok_or("viewer_mismatch")?;
            let e = s.lease.entry(computer.to_string()).or_default();
            *e = (Some(viewer), e.1 + 1);
            Ok((json!({ "lease": s.lease_of(computer) }), Some(changed(s))))
        }
        "display.lease.release" => {
            let viewer = mine(minted);
            let e = s.lease.entry(computer.to_string()).or_default();
            let forced = params["force"] == true;
            if !forced && (viewer.is_none() || e.0 != viewer) {
                return Err("viewer_mismatch");
            }
            *e = (None, e.1 + 1);
            Ok((json!({ "lease": s.lease_of(computer) }), Some(changed(s))))
        }
        _ => Err("method not found"),
    }
}

/// A client's bytes, across its binary frames.
struct In {
    stream: TcpStream,
    buf: VecDeque<u8>,
}

enum Ended {
    /// The client closed, or the connection ended.
    Gone,
    /// A text frame, or a message RFB does not have: Hermes closes 1003.
    Refused,
}

impl In {
    fn take(&mut self, n: usize) -> Result<Vec<u8>, Ended> {
        // bounded by IN_MAX: a frame adds bytes or the socket ends
        while self.buf.len() < n {
            let (op, payload) = read_frame(&mut self.stream).ok_or(Ended::Gone)?;
            match op {
                0x0 | 0x2 => self.buf.extend(payload),
                0x1 => return Err(Ended::Refused),
                0x8 => return Err(Ended::Gone),
                0x9 => write_frame(&mut self.stream, 0xA, &payload).map_err(|_| Ended::Gone)?,
                _ => {}
            }
            if self.buf.len() > IN_MAX {
                return Err(Ended::Gone);
            }
        }
        Ok(self.buf.drain(..n).collect())
    }
}

fn close(stream: &mut TcpStream, code: u16, reason: &str) {
    let mut payload = code.to_be_bytes().to_vec();
    payload.extend(reason.as_bytes());
    let _ = write_frame(stream, 0x8, &payload);
}

fn frame_of(colour: [u8; 3]) -> Vec<u8> {
    let mut f = vec![0, 0, 0, 1, 0, 0, 0, 0];
    f.extend(WIDTH.to_be_bytes());
    f.extend(HEIGHT.to_be_bytes());
    f.extend(0i32.to_be_bytes());
    for _ in 0..usize::from(WIDTH) * usize::from(HEIGHT) {
        f.extend([colour[0], colour[1], colour[2], 0]);
    }
    f
}

/// `/api/display/ws?display_ticket=`: RFB for the ticket's viewer.
pub fn display(mut stream: TcpStream, screens: &Arc<Mutex<Screens>>, computer: &str, ticket: &str) {
    let viewer = {
        let mut s = screens.lock().unwrap();
        match s.tickets.remove(ticket) {
            Some((c, v, end)) if c == computer && Instant::now() < end => {
                s.sockets += 1;
                v
            }
            _ => return close(&mut stream, 4401, "display ticket missing, expired or used"),
        }
    };
    let Ok(reader) = stream.try_clone() else { return };
    let mut rx = In { stream: reader, buf: VecDeque::new() };
    match serve(&mut stream, &mut rx, screens, computer, &viewer) {
        Err(Ended::Refused) => close(&mut stream, 1003, "RFB is binary"),
        Err(Ended::Gone) | Ok(()) => close(&mut stream, 1000, ""),
    }
}

fn serve(out: &mut TcpStream, rx: &mut In, screens: &Arc<Mutex<Screens>>, computer: &str, viewer: &str) -> Result<(), Ended> {
    let send = |out: &mut TcpStream, b: &[u8]| write_frame(out, 0x2, b).map_err(|_| Ended::Gone);
    send(out, b"RFB 003.008\n")?;
    rx.take(12)?;
    send(out, &[1, 1])?;
    if rx.take(1)? != [1] {
        return Ok(());
    }
    send(out, &[0, 0, 0, 0])?;
    rx.take(1)?;
    let name = b"hermes";
    let mut init = vec![];
    init.extend(WIDTH.to_be_bytes());
    init.extend(HEIGHT.to_be_bytes());
    // 32 bits, depth 24, little-endian, true colour, R at 16, G at 8, B at 0
    init.extend([32, 24, 0, 1, 0, 255, 0, 255, 0, 255, 16, 8, 0, 0, 0, 0]);
    init.extend((name.len() as u32).to_be_bytes());
    init.extend(name);
    send(out, &init)?;
    let mut buttons = 0u8;
    // bounded by the socket: it ends when the viewer closes
    loop {
        let kind = rx.take(1)?[0];
        match kind {
            // SetPixelFormat: this fake sends what this platform's client asks (R, G, B, X)
            0 => drop(rx.take(19)?),
            2 => {
                let head = rx.take(3)?;
                rx.take(usize::from(u16::from_be_bytes([head[1], head[2]])) * 4)?;
            }
            3 => {
                let incremental = rx.take(9)?[0] == 1;
                if incremental {
                    std::thread::sleep(INCREMENTAL_WAIT);
                }
                let colour = COLOURS[screens.lock().unwrap().colour.get(computer).copied().unwrap_or(0)];
                send(out, &frame_of(colour))?;
            }
            4 => {
                let b = rx.take(7)?;
                let (down, key) = (b[0] == 1, u32::from_be_bytes([b[3], b[4], b[5], b[6]]));
                let mut s = screens.lock().unwrap();
                match s.holds(computer, viewer) {
                    true => s.input.push(json!({ "computer": computer, "kind": "key", "down": down, "keysym": key })),
                    false => s.dropped += 1,
                }
            }
            5 => {
                let b = rx.take(5)?;
                let (mask, x, y) = (b[0], u16::from_be_bytes([b[1], b[2]]), u16::from_be_bytes([b[3], b[4]]));
                let mut s = screens.lock().unwrap();
                match s.holds(computer, viewer) {
                    true => {
                        s.input.push(json!({ "computer": computer, "kind": "pointer", "x": x, "y": y, "mask": mask }));
                        if mask & 1 == 1 && buttons & 1 == 0 {
                            let c = s.colour.entry(computer.to_string()).or_default();
                            *c = (*c + 1) % COLOURS.len();
                        }
                        buttons = mask;
                    }
                    false => s.dropped += 1,
                }
            }
            6 => {
                let b = rx.take(7)?;
                let n = u32::from_be_bytes([b[3], b[4], b[5], b[6]]) as usize;
                if n > 256 * 1024 {
                    return Err(Ended::Gone);
                }
                rx.take(n)?;
            }
            _ => return Err(Ended::Refused),
        }
    }
}
