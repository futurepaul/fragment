//! The browser's half (wasm32 only): its exports to JavaScript.


use std::cell::RefCell;
use std::rc::Rc;

use crate::rfb;

use http_body_util::{BodyExt, Full};
use hyper::body::Bytes;
use iroh::endpoint::{presets, Connection};
use iroh::{Endpoint, EndpointAddr, RelayMode, RelayUrl, SecretKey};
use tokio::io::{AsyncReadExt, AsyncWriteExt, ReadHalf, WriteHalf};
use wasm_bindgen::prelude::*;

const ALPN: &[u8] = b"sandcastle/1";
const STREAM_ADMISSION: u8 = b'A';
const STREAM_HTTP: u8 = b'H';
/// The largest answer `fetch` reads whole, and the largest message a
/// socket takes: Hermes' largest answers are far below either.
const BODY_BYTES_MAX: usize = 16 * 1024 * 1024;

fn err(e: impl std::fmt::Display) -> JsError {
    JsError::new(&e.to_string())
}

/// A page's own iroh key, made here and used for this page's life. Its
/// endpoint binds at the first `connect`, homed on that computer's relay:
/// a page names its key to get an admission before it knows the relay.
#[wasm_bindgen]
pub struct Peer {
    secret: SecretKey,
    ep: tokio::sync::OnceCell<Endpoint>,
}

#[wasm_bindgen]
impl Peer {
    #[wasm_bindgen(constructor)]
    #[allow(clippy::new_without_default)]
    pub fn new() -> Peer {
        console_error_panic_hook::set_once();
        Peer { secret: SecretKey::generate(), ep: tokio::sync::OnceCell::new() }
    }

    /// Its public key, 64 hex: what an admission names.
    pub fn id(&self) -> String {
        self.secret.public().to_string()
    }

    /// Connects to a computer's key through its relay and presents the
    /// admission. `host` is the Host header its requests carry.
    pub async fn connect(&self, endpoint: String, relay: String, admission: String, host: String) -> Result<Computer, JsError> {
        let relay: RelayUrl = relay.parse().map_err(err)?;
        let ep = self
            .ep
            .get_or_try_init(|| {
                Endpoint::builder(presets::Minimal).secret_key(self.secret.clone()).relay_mode(RelayMode::Custom(relay.clone().into())).bind()
            })
            .await
            .map_err(err)?;
        let addr = EndpointAddr::new(endpoint.parse().map_err(err)?).with_relay_url(relay);
        let conn = ep.connect(addr, ALPN).await.map_err(err)?;
        let computer = Computer { conn, host };
        computer.admit(admission).await?;
        Ok(computer)
    }

    pub async fn close(&self) {
        if let Some(ep) = self.ep.get() {
            ep.close().await;
        }
    }
}

/// An admitted connection to one computer.
#[wasm_bindgen]
pub struct Computer {
    conn: Connection,
    host: String,
}

#[wasm_bindgen]
impl Computer {
    /// Presents an admission (the first, or one that renews it before it
    /// ends): its end, seconds since the epoch.
    pub async fn admit(&self, admission: String) -> Result<f64, JsError> {
        let (mut send, mut recv) = self.conn.open_bi().await.map_err(err)?;
        send.write_all(&[STREAM_ADMISSION]).await.map_err(err)?;
        send.write_u16(u16::try_from(admission.len()).map_err(err)?).await.map_err(err)?;
        send.write_all(admission.as_bytes()).await.map_err(err)?;
        send.finish().map_err(err)?;
        let len = recv.read_u16().await.map_err(|e| err(format!("no answer to the admission: {e}")))?;
        let mut body = vec![0u8; usize::from(len)];
        recv.read_exact(&mut body).await.map_err(err)?;
        let answer: serde_json::Value = serde_json::from_slice(&body).map_err(err)?;
        match answer["until"].as_f64() {
            Some(until) if answer["admitted"] == true => Ok(until),
            _ => Err(err(format!("not admitted: {}", answer["reason"].as_str().unwrap_or("no reason")))),
        }
    }

    /// Whether the connection goes through the relay or direct.
    pub fn path(&self) -> String {
        let paths = self.conn.paths();
        match paths.iter().find(|p| p.is_selected()) {
            Some(p) if p.is_relay() => "relayed".into(),
            Some(_) => "direct".into(),
            None => "none".into(),
        }
    }

    /// One request on a stream of its own: `{status, headers, body}`, the
    /// body a `Uint8Array`, the headers a JSON object (a header sent more
    /// than once is its values a line apiece). `headers` is a JSON object.
    pub async fn fetch(&self, method: String, path: String, headers: String, body: Vec<u8>) -> Result<JsValue, JsError> {
        let mut sender = self.http().await?;
        let mut req = hyper::Request::builder().method(method.as_str()).uri(path).header("host", &self.host);
        let headers: serde_json::Map<String, serde_json::Value> = serde_json::from_str(&headers).map_err(err)?;
        for (k, v) in headers {
            req = req.header(k, v.as_str().unwrap_or(""));
        }
        let resp = sender.send_request(req.body(Full::new(Bytes::from(body))).map_err(err)?).await.map_err(err)?;
        let status = resp.status().as_u16();
        // A header sent more than once (Set-Cookie) keeps each value, a line apiece.
        let mut seen: serde_json::Map<String, serde_json::Value> = serde_json::Map::new();
        for (k, v) in resp.headers() {
            let v = v.to_str().unwrap_or("");
            let joined = match seen.get(k.as_str()).and_then(|x| x.as_str()) {
                Some(before) => format!("{before}\n{v}"),
                None => v.to_string(),
            };
            seen.insert(k.to_string(), serde_json::Value::String(joined));
        }
        let mut incoming = resp.into_body();
        let mut data = Vec::new();
        // Bounded by BODY_BYTES_MAX.
        while let Some(frame) = incoming.frame().await {
            if let Ok(chunk) = frame.map_err(err)?.into_data() {
                if data.len() + chunk.len() > BODY_BYTES_MAX {
                    return Err(err(format!("an answer over {BODY_BYTES_MAX} bytes")));
                }
                data.extend_from_slice(&chunk);
            }
        }
        let out = js_sys::Object::new();
        js_sys::Reflect::set(&out, &"status".into(), &JsValue::from(status)).map_err(|_| err("building the answer"))?;
        js_sys::Reflect::set(&out, &"headers".into(), &JsValue::from_str(&serde_json::Value::Object(seen).to_string())).map_err(|_| err("building the answer"))?;
        js_sys::Reflect::set(&out, &"body".into(), &js_sys::Uint8Array::from(data.as_slice())).map_err(|_| err("building the answer"))?;
        Ok(out.into())
    }

    /// A WebSocket on a stream of its own, offering `protocols`.
    pub async fn websocket(&self, path: String, protocols: Vec<String>) -> Result<Socket, JsError> {
        let mut sender = self.http().await?;
        let mut key = [0u8; 16];
        getrandom::fill(&mut key).map_err(err)?;
        let mut req = hyper::Request::get(path)
            .header("host", &self.host)
            .header("connection", "upgrade")
            .header("upgrade", "websocket")
            .header("sec-websocket-version", "13")
            .header("sec-websocket-key", base64(&key));
        if !protocols.is_empty() {
            req = req.header("sec-websocket-protocol", protocols.join(", "));
        }
        let req = req.body(Full::new(Bytes::new())).map_err(err)?;
        let mut resp = sender.send_request(req).await.map_err(err)?;
        if resp.status() != hyper::StatusCode::SWITCHING_PROTOCOLS {
            return Err(err(format!("the socket answered {}", resp.status())));
        }
        let protocol = resp.headers().get("sec-websocket-protocol").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
        let io = hyper_util::rt::TokioIo::new(hyper::upgrade::on(&mut resp).await.map_err(err)?);
        let (read, write) = tokio::io::split(io);
        let write = Rc::new(tokio::sync::Mutex::new(write));
        let (tx, rx) = tokio::sync::mpsc::channel(1024);
        let pinger = write.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let mut read = read;
            // Bounded by the stream: it ends when the service closes.
            while let Ok(Some(m)) = read_message(&mut read, &pinger).await {
                if tx.send(m).await.is_err() {
                    return;
                }
            }
        });
        Ok(Socket { write, messages: tokio::sync::Mutex::new(rx), protocol })
    }

    /// The computer's screen: its RFB over a WebSocket at `path` (Hermes':
    /// `/api/display/ws?display_ticket=…`), each thing to draw handed to
    /// `on_event`: `{kind: "size", width, height}` (a fresh canvas),
    /// `{kind: "raw", x, y, width, height, rgba}` (a `Uint8ClampedArray`, for
    /// an `ImageData`), `{kind: "copy", fromX, fromY, x, y, width, height}`,
    /// `{kind: "bell"}`, `{kind: "cut", text}`, and `{kind: "closed",
    /// reason}` once it ends.
    pub async fn screen(&self, path: String, on_event: js_sys::Function) -> Result<Screen, JsError> {
        let socket = Rc::new(self.websocket(path, vec![]).await?);
        let client = Rc::new(RefCell::new(rfb::Client::new()));
        let (s, c) = (socket.clone(), client.clone());
        wasm_bindgen_futures::spawn_local(async move {
            // Bounded by the socket: it ends when the screen closes.
            let reason: String = loop {
                let next = s.messages.lock().await.recv().await;
                match next {
                    Some(Message::Binary(bytes)) => {
                        let fed = c.borrow_mut().feed(&bytes);
                        let events = match fed {
                            Ok(events) => events,
                            Err(e) => break e,
                        };
                        for e in events {
                            let _ = on_event.call1(&JsValue::NULL, &event_js(e));
                        }
                        let out = c.borrow_mut().take_out();
                        if !out.is_empty() && frame(&s.write, 0x2, &out).await.is_err() {
                            break "the screen's socket closed".into();
                        }
                    }
                    Some(Message::Text(_)) => {}
                    None => break "the screen closed".into(),
                }
            };
            let closed = object(&[("kind", JsValue::from_str("closed")), ("reason", JsValue::from_str(&reason))]);
            let _ = on_event.call1(&JsValue::NULL, &closed);
        });
        Ok(Screen { socket, client })
    }

    pub fn close(&self) {
        self.conn.close(0u32.into(), b"closed by the page");
    }

    async fn http(&self) -> Result<hyper::client::conn::http1::SendRequest<Full<Bytes>>, JsError> {
        let (mut send, recv) = self.conn.open_bi().await.map_err(err)?;
        send.write_all(&[STREAM_HTTP]).await.map_err(err)?;
        let (sender, driver) = hyper::client::conn::http1::handshake(hyper_util::rt::TokioIo::new(tokio::io::join(recv, send))).await.map_err(err)?;
        wasm_bindgen_futures::spawn_local(async move {
            let _ = driver.with_upgrades().await;
        });
        Ok(sender)
    }
}

type Io = hyper_util::rt::TokioIo<hyper::upgrade::Upgraded>;
type Writer = Rc<tokio::sync::Mutex<WriteHalf<Io>>>;

/// A whole message of a socket's.
enum Message {
    Text(String),
    Binary(Vec<u8>),
}

fn object(fields: &[(&str, JsValue)]) -> JsValue {
    let o = js_sys::Object::new();
    for (k, v) in fields {
        let _ = js_sys::Reflect::set(&o, &JsValue::from_str(k), v);
    }
    o.into()
}

fn event_js(e: rfb::Event) -> JsValue {
    let n = |v: u16| JsValue::from(v);
    match e {
        rfb::Event::Size { width, height, .. } => object(&[("kind", "size".into()), ("width", n(width)), ("height", n(height))]),
        rfb::Event::Raw { x, y, width, height, rgba } => {
            object(&[("kind", "raw".into()), ("x", n(x)), ("y", n(y)), ("width", n(width)), ("height", n(height)), ("rgba", js_sys::Uint8ClampedArray::from(rgba.as_slice()).into())])
        }
        rfb::Event::Copy { from_x, from_y, x, y, width, height } => {
            object(&[("kind", "copy".into()), ("fromX", n(from_x)), ("fromY", n(from_y)), ("x", n(x)), ("y", n(y)), ("width", n(width)), ("height", n(height))])
        }
        rfb::Event::Bell => object(&[("kind", "bell".into())]),
        rfb::Event::Cut(text) => object(&[("kind", "cut".into()), ("text", JsValue::from_str(&text))]),
    }
}

/// A computer's screen, open: its pointer and keys go to it (as they would
/// from a viewer at its desktop; Hermes takes them only from the holder of
/// its screen's lease).
#[wasm_bindgen]
pub struct Screen {
    socket: Rc<Socket>,
    client: Rc<RefCell<rfb::Client>>,
}

#[wasm_bindgen]
impl Screen {
    /// The pointer at (x, y) on the desktop, its buttons a mask (1 left, 2
    /// middle, 4 right, 8 and 16 the wheel).
    pub async fn pointer(&self, x: u16, y: u16, mask: u8) -> Result<(), JsError> {
        let out = {
            let mut c = self.client.borrow_mut();
            c.pointer(x, y, mask);
            c.take_out()
        };
        frame(&self.socket.write, 0x2, &out).await.map_err(err)
    }

    /// A key (a `KeyboardEvent.key`) pressed or let go: whether the screen
    /// knows it.
    pub async fn key(&self, key: String, down: bool) -> Result<bool, JsError> {
        let Some(keysym) = rfb::keysym(&key) else { return Ok(false) };
        let out = {
            let mut c = self.client.borrow_mut();
            c.key(keysym, down);
            c.take_out()
        };
        frame(&self.socket.write, 0x2, &out).await.map_err(err)?;
        Ok(true)
    }

    pub async fn close(&self) {
        self.socket.close().await;
    }
}

/// A WebSocket read continuously, as a browser's is: pings answered at
/// once, messages queued until `next`.
#[wasm_bindgen]
pub struct Socket {
    write: Writer,
    messages: tokio::sync::Mutex<tokio::sync::mpsc::Receiver<Message>>,
    protocol: String,
}

#[wasm_bindgen]
impl Socket {
    /// The protocol the service chose.
    pub fn protocol(&self) -> String {
        self.protocol.clone()
    }

    pub async fn send(&self, text: String) -> Result<(), JsError> {
        frame(&self.write, 0x1, text.as_bytes()).await.map_err(err)
    }

    #[wasm_bindgen(js_name = sendBytes)]
    pub async fn send_bytes(&self, bytes: Vec<u8>) -> Result<(), JsError> {
        frame(&self.write, 0x2, &bytes).await.map_err(err)
    }

    /// The next message: a string, or a `Uint8Array` for a binary one;
    /// undefined once the service closed.
    pub async fn next(&self) -> JsValue {
        match self.messages.lock().await.recv().await {
            Some(Message::Text(t)) => JsValue::from_str(&t),
            Some(Message::Binary(b)) => js_sys::Uint8Array::from(b.as_slice()).into(),
            None => JsValue::UNDEFINED,
        }
    }

    /// Closes it as a page leaving does (1000: a clean close, which tells
    /// Hermes' screen a holder let go).
    pub async fn close(&self) {
        let _ = frame(&self.write, 0x8, &1000u16.to_be_bytes()).await;
    }
}

fn base64(bytes: &[u8]) -> String {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, b)| n | (u32::from(*b) << (16 - 8 * i)));
        for i in 0..4 {
            out.push(if i <= chunk.len() { char::from(ABC[((n >> (18 - 6 * i)) & 63) as usize]) } else { '=' });
        }
    }
    out
}

/// One frame, masked as a client's must be.
async fn frame(write: &Writer, opcode: u8, payload: &[u8]) -> Result<(), String> {
    let mut mask = [0u8; 4];
    getrandom::fill(&mut mask).map_err(|e| e.to_string())?;
    let mut f = vec![0x80 | opcode];
    match payload.len() {
        n if n < 126 => f.push(0x80 | n as u8),
        n if n <= 0xffff => {
            f.push(0x80 | 126);
            f.extend((n as u16).to_be_bytes());
        }
        n => {
            f.push(0x80 | 127);
            f.extend((n as u64).to_be_bytes());
        }
    }
    f.extend(mask);
    f.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
    write.lock().await.write_all(&f).await.map_err(|e| format!("websocket write: {e}"))
}

/// The next whole message; None when the service closed.
async fn read_message(read: &mut ReadHalf<Io>, write: &Writer) -> Result<Option<Message>, String> {
    let mut message: Vec<u8> = Vec::new();
    // the message's own opcode (a continuation keeps it)
    let mut binary = false;
    // Bounded by the stream, and by BODY_BYTES_MAX a message.
    loop {
        let mut head = [0u8; 2];
        if read.read_exact(&mut head).await.is_err() {
            return Ok(None);
        }
        let (fin, opcode) = (head[0] & 0x80 != 0, head[0] & 0x0f);
        let len = match head[1] & 0x7f {
            126 => u64::from(read.read_u16().await.map_err(|e| e.to_string())?),
            127 => read.read_u64().await.map_err(|e| e.to_string())?,
            n => u64::from(n),
        };
        if head[1] & 0x80 != 0 {
            return Err("a service's frame was masked".into());
        }
        if message.len() as u64 + len > BODY_BYTES_MAX as u64 {
            return Err(format!("a websocket message over {BODY_BYTES_MAX} bytes"));
        }
        let mut payload = vec![0u8; usize::try_from(len).expect("bounded above")];
        read.read_exact(&mut payload).await.map_err(|e| e.to_string())?;
        match opcode {
            0x8 => return Ok(None),
            0x9 => frame(write, 0xA, &payload).await?,
            0xA => {}
            0x0..=0x2 => {
                if opcode != 0 {
                    binary = opcode == 0x2;
                }
                message.extend(payload);
                if fin {
                    return match binary {
                        true => Ok(Some(Message::Binary(message))),
                        false => String::from_utf8(message).map(|t| Some(Message::Text(t))).map_err(|_| "a text message that is not UTF-8".into()),
                    };
                }
            }
            other => return Err(format!("websocket opcode {other}")),
        }
    }
}
