//! The agent's eyes on its own desktop, for a model that reads no images
//! (goose's, GLM): a screenshot read from the display's RFB socket, then
//!
//! - `screen_look {question}`: the screenshot and the question to the
//!   model route's `vision` model (the deployment's vision model), its
//!   answer in words;
//! - `screen_click {target, click?, button?, double?}`: Clef finds the
//!   target, set-of-marks: the screenshot with a numbered grid drawn on it
//!   goes to Clef as a `choice` among the numbers ("which cell holds the
//!   target?"), then the chosen cell's neighbourhood, enlarged, with a finer
//!   numbered grid; the finer cell's centre is clicked (xdotool, on the
//!   agent's display) or only said.
//!
//! Both are paid calls through the computer's model intercept as the agent
//! (`FRAGMENT_MODEL`: `/v1/chat/completions`, `/v1/decide`), metered on its
//! owner's ledger.

use std::path::Path;
use std::time::Duration;

use base64::Engine;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::desk::Desk;
use crate::mcp;

/// A screenshot is read within this.
pub const CAPTURE_MS_MAX: u64 = 10_000;
/// A display larger than this is no desktop of ours.
pub const SIDE_MAX: u16 = 8192;
/// A model call is answered within this.
pub const MODEL_MS_MAX: u64 = 90_000;
/// The first grid: columns and rows over the whole screen.
pub const COARSE: (u32, u32) = (12, 8);
/// The second: over the chosen cell and its neighbours, enlarged.
pub const FINE: (u32, u32) = (8, 6);
/// The neighbourhood's enlargement.
pub const ZOOM: u32 = 2;
/// JPEG quality of what a model is shown.
pub const QUALITY: u8 = 80;
/// A question or target is at most this long.
pub const ASK_MAX_BYTES: usize = 2_000;

const _: () = assert!(COARSE.0 * COARSE.1 <= 255 && FINE.0 * FINE.1 <= 255, "a choice has at most 255 options");

/// An RGB image.
#[derive(Debug, Clone, PartialEq)]
pub struct Image {
    pub w: u32,
    pub h: u32,
    pub rgb: Vec<u8>,
}

impl Image {
    pub fn new(w: u32, h: u32) -> Image {
        Image { w, h, rgb: vec![0; (w * h * 3) as usize] }
    }

    fn set(&mut self, x: u32, y: u32, c: [u8; 3]) {
        if x < self.w && y < self.h {
            let i = ((y * self.w + x) * 3) as usize;
            self.rgb[i..i + 3].copy_from_slice(&c);
        }
    }

    pub fn get(&self, x: u32, y: u32) -> [u8; 3] {
        let i = ((y * self.w + x) * 3) as usize;
        [self.rgb[i], self.rgb[i + 1], self.rgb[i + 2]]
    }

    /// The part at (x, y) of w × h, clamped to the image.
    pub fn crop(&self, x: u32, y: u32, w: u32, h: u32) -> Image {
        let (x, y) = (x.min(self.w - 1), y.min(self.h - 1));
        let (w, h) = (w.min(self.w - x), h.min(self.h - y));
        let mut out = Image::new(w, h);
        for row in 0..h {
            let from = (((y + row) * self.w + x) * 3) as usize;
            let to = (row * w * 3) as usize;
            out.rgb[to..to + (w * 3) as usize].copy_from_slice(&self.rgb[from..from + (w * 3) as usize]);
        }
        out
    }

    /// Enlarged `k` times, each pixel a k × k block.
    pub fn scale(&self, k: u32) -> Image {
        let mut out = Image::new(self.w * k, self.h * k);
        for y in 0..out.h {
            for x in 0..out.w {
                let c = self.get(x / k, y / k);
                out.set(x, y, c);
            }
        }
        out
    }

    fn fill(&mut self, x: u32, y: u32, w: u32, h: u32, c: [u8; 3]) {
        for yy in y..(y + h).min(self.h) {
            for xx in x..(x + w).min(self.w) {
                self.set(xx, yy, c);
            }
        }
    }

    pub fn jpeg(&self) -> Result<Vec<u8>, String> {
        let mut out = Vec::new();
        let enc = jpeg_encoder::Encoder::new(&mut out, QUALITY);
        enc.encode(&self.rgb, u16::try_from(self.w).map_err(|_| "too wide")?, u16::try_from(self.h).map_err(|_| "too tall")?, jpeg_encoder::ColorType::Rgb).map_err(|e| e.to_string())?;
        Ok(out)
    }

    pub fn data_url(&self) -> Result<String, String> {
        Ok(format!("data:image/jpeg;base64,{}", base64::engine::general_purpose::STANDARD.encode(self.jpeg()?)))
    }
}

/// A 5 × 7 bitmap of each digit, its rows' low five bits, left first.
const DIGITS: [[u8; 7]; 10] = [
    [0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110],
    [0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110],
    [0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0b01000, 0b11111],
    [0b11111, 0b00010, 0b00100, 0b00010, 0b00001, 0b10001, 0b01110],
    [0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010],
    [0b11111, 0b10000, 0b11110, 0b00001, 0b00001, 0b10001, 0b01110],
    [0b00110, 0b01000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110],
    [0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000],
    [0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110],
    [0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00010, 0b01100],
];

const MARK: [u8; 3] = [255, 32, 160];
const LABEL_BG: [u8; 3] = [0, 0, 0];
const LABEL_FG: [u8; 3] = [255, 255, 0];

/// `n` written at (x, y), each bitmap pixel `k` × `k`, yellow on black.
fn number(img: &mut Image, n: u32, x: u32, y: u32, k: u32) {
    let digits: Vec<u32> = n.to_string().bytes().map(|b| u32::from(b - b'0')).collect();
    let (w, h) = (digits.len() as u32 * 6 * k + k, 9 * k);
    img.fill(x, y, w, h, LABEL_BG);
    for (i, d) in digits.iter().enumerate() {
        for (row, bits) in DIGITS[*d as usize].iter().enumerate() {
            for col in 0..5 {
                if bits & (1 << (4 - col)) != 0 {
                    img.fill(x + k + (i as u32 * 6 + col) * k, y + k + row as u32 * k, k, k, LABEL_FG);
                }
            }
        }
    }
}

/// The grid's cells over an image of w × h, numbered from 1, row by row:
/// each cell's (number, x, y, w, h).
pub fn cells(w: u32, h: u32, (cols, rows): (u32, u32)) -> Vec<(u32, u32, u32, u32, u32)> {
    let mut out = Vec::new();
    for r in 0..rows {
        for c in 0..cols {
            let (x0, x1) = (c * w / cols, (c + 1) * w / cols);
            let (y0, y1) = (r * h / rows, (r + 1) * h / rows);
            out.push((r * cols + c + 1, x0, y0, x1 - x0, y1 - y0));
        }
    }
    out
}

/// The image with the grid drawn on it: each cell's border, its number in
/// its top-left corner.
pub fn marked(img: &Image, grid: (u32, u32)) -> Image {
    let mut out = img.clone();
    let k = if img.w >= 1000 { 2 } else { 1 };
    for (n, x, y, w, h) in cells(img.w, img.h, grid) {
        out.fill(x, y, w, 1, MARK);
        out.fill(x, y, 1, h, MARK);
        number(&mut out, n, x + 2, y + 2, k);
    }
    out
}

// ---- the display's pixels, over RFB ----

async fn read_u8(s: &mut tokio::net::UnixStream) -> std::io::Result<u8> {
    s.read_u8().await
}

async fn skip(s: &mut tokio::net::UnixStream, n: usize) -> std::io::Result<()> {
    let mut buf = vec![0u8; n];
    s.read_exact(&mut buf).await.map(|_| ())
}

/// One whole frame of the display at `sock` (RFB 3.8, no authentication,
/// raw 32-bit true colour, as the screen's viewers read it).
pub async fn capture(sock: &Path) -> Result<Image, String> {
    let read = async {
        let mut s = tokio::net::UnixStream::connect(sock).await?;
        let mut version = [0u8; 12];
        s.read_exact(&mut version).await?;
        if !version.starts_with(b"RFB 003.") {
            return Err(std::io::Error::other("no RFB greeting"));
        }
        s.write_all(b"RFB 003.008\n").await?;
        let n = read_u8(&mut s).await? as usize;
        if n == 0 {
            return Err(std::io::Error::other("the display refused the viewer"));
        }
        let mut types = vec![0u8; n];
        s.read_exact(&mut types).await?;
        if !types.contains(&1) {
            return Err(std::io::Error::other("the display asks a password"));
        }
        s.write_all(&[1]).await?;
        if s.read_u32().await? != 0 {
            return Err(std::io::Error::other("the display refused"));
        }
        s.write_all(&[1]).await?;
        let (w, h) = (s.read_u16().await?, s.read_u16().await?);
        skip(&mut s, 16).await?;
        let name_len = s.read_u32().await? as usize;
        if name_len > 4096 || w == 0 || h == 0 || w > SIDE_MAX || h > SIDE_MAX {
            return Err(std::io::Error::other("no desktop of ours"));
        }
        skip(&mut s, name_len).await?;
        // 32 bits, true colour, little-endian, red at 16, green at 8, blue at 0
        s.write_all(&[0, 0, 0, 0, 32, 24, 0, 1, 0, 255, 0, 255, 0, 255, 16, 8, 0, 0, 0, 0]).await?;
        s.write_all(&[2, 0, 0, 1, 0, 0, 0, 0]).await?;
        let mut ask = vec![3u8, 0, 0, 0, 0, 0];
        ask.extend_from_slice(&w.to_be_bytes());
        ask.extend_from_slice(&h.to_be_bytes());
        s.write_all(&ask).await?;
        let mut img = Image::new(u32::from(w), u32::from(h));
        let mut covered = 0u64;
        // bounded by CAPTURE_MS_MAX: messages until one whole frame is read
        while covered < u64::from(w) * u64::from(h) {
            match read_u8(&mut s).await? {
                0 => {
                    skip(&mut s, 1).await?;
                    let rects = s.read_u16().await?;
                    for _ in 0..rects {
                        let (x, y, rw, rh) = (s.read_u16().await?, s.read_u16().await?, s.read_u16().await?, s.read_u16().await?);
                        if s.read_i32().await? != 0 || u32::from(x) + u32::from(rw) > u32::from(w) || u32::from(y) + u32::from(rh) > u32::from(h) {
                            return Err(std::io::Error::other("a rectangle that is not raw, or off the screen"));
                        }
                        let mut px = vec![0u8; rw as usize * rh as usize * 4];
                        s.read_exact(&mut px).await?;
                        for row in 0..u32::from(rh) {
                            for col in 0..u32::from(rw) {
                                let i = ((row * u32::from(rw) + col) * 4) as usize;
                                img.set(u32::from(x) + col, u32::from(y) + row, [px[i + 2], px[i + 1], px[i]]);
                            }
                        }
                        covered += u64::from(rw) * u64::from(rh);
                    }
                }
                1 => {
                    skip(&mut s, 3).await?;
                    let n = s.read_u16().await? as usize;
                    skip(&mut s, n * 6).await?;
                }
                2 => {}
                3 => {
                    skip(&mut s, 3).await?;
                    let n = s.read_u32().await? as usize;
                    if n > fragment_bridge::screen::CUT_TEXT_MAX {
                        return Err(std::io::Error::other("a clipboard past its bound"));
                    }
                    skip(&mut s, n).await?;
                }
                t => return Err(std::io::Error::other(format!("an RFB message this reader does not know ({t})"))),
            }
        }
        Ok(img)
    };
    match tokio::time::timeout(Duration::from_millis(CAPTURE_MS_MAX), read).await {
        Ok(r) => r.map_err(|e| format!("reading the screen: {e}")),
        Err(_) => Err("reading the screen took too long".into()),
    }
}

// ---- the models ----

/// POSTs `body` to the model intercept's `path` as the agent: its answer.
pub async fn model_call(agent: &str, path: &str, body: &Value) -> Result<Value, String> {
    let base = std::env::var("FRAGMENT_MODEL").map_err(|_| "no FRAGMENT_MODEL: this is no computer")?;
    let file = std::env::temp_dir().join(format!("model-{}-{}.json", std::process::id(), fragment_bridge::log::now_ms()));
    std::fs::write(&file, body.to_string()).map_err(|e| e.to_string())?;
    let out = tokio::process::Command::new("curl")
        .args(["-sS", "--max-time", &(MODEL_MS_MAX / 1000).to_string(), "-H", "content-type: application/json", "-H"])
        .arg(format!("x-fragment-agent: {agent}"))
        .arg("--data-binary")
        .arg(format!("@{}", file.display()))
        .args(["-w", "\n%{http_code}"])
        .arg(format!("{}{path}", base.trim_end_matches('/')))
        .kill_on_drop(true)
        .output()
        .await;
    let _ = std::fs::remove_file(&file);
    let out = out.map_err(|e| format!("curl: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let (body, status) = text.rsplit_once('\n').unwrap_or((&text, "0"));
    let v: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    if status.trim() != "200" {
        let said = v["message"].as_str().or(v["error"]["message"].as_str()).map(str::to_string).unwrap_or_else(|| mcp::cut(body, 300).to_string());
        return Err(format!("the model route answered {}: {said}", status.trim()));
    }
    Ok(v)
}

/// The vision model's answer to `question` about `img`.
pub async fn ask_vision(agent: &str, img: &Image, question: &str) -> Result<String, String> {
    let body = json!({
        "model": "vision",
        "max_tokens": 1200,
        "messages": [{ "role": "user", "content": [
            { "type": "image_url", "image_url": { "url": img.data_url()? } },
            { "type": "text", "text": format!("This is a screenshot of a computer's desktop ({} × {} pixels). {question}\nAnswer plainly and precisely; quote any text exactly as it appears.", img.w, img.h) }
        ] }]
    });
    let v = model_call(agent, "/v1/chat/completions", &body).await?;
    v["choices"][0]["message"]["content"].as_str().map(str::to_string).filter(|s| !s.trim().is_empty()).ok_or_else(|| "the vision model said nothing".into())
}

/// Clef's choice among the numbered cells of `marked`: the cell's number
/// and Clef's probability for it.
pub async fn ask_clef(agent: &str, marked: &Image, cells: usize, target: &str, stage: &str) -> Result<(u32, f64), String> {
    let criteria: serde_json::Map<String, Value> = (1..=cells).map(|n| (n.to_string(), Value::Null)).collect();
    let body = json!({
        "model": "clef",
        "state": format!("A screenshot of a computer's desktop{stage}, with a numbered grid drawn over it in magenta: each cell's number is in yellow on black at its top-left corner. The user wants to click: {target}"),
        "questions": { "cell": { "type": "choice", "instructions": format!("Which numbered grid cell holds the centre of {target}?"), "criteria": criteria } },
        "images": [marked.data_url()?],
    });
    let v = model_call(agent, "/v1/decide", &body).await?;
    let a = &v["answers"]["cell"];
    let n: u32 = a["choice"].as_str().and_then(|c| c.parse().ok()).ok_or_else(|| format!("Clef chose no cell: {a}"))?;
    let p = a["probabilities"][n.to_string()].as_f64().unwrap_or(0.0);
    Ok((n, p))
}

/// Where `target` is on the screen: (x, y), and Clef's two probabilities.
pub async fn find(agent: &str, img: &Image, target: &str) -> Result<(u32, u32, f64, f64), String> {
    let coarse = cells(img.w, img.h, COARSE);
    let (n, p1) = ask_clef(agent, &marked(img, COARSE), coarse.len(), target, "").await?;
    let &(_, cx, cy, cw, ch) = coarse.get(n as usize - 1).ok_or("a cell off the grid")?;
    // the cell and half a cell around it
    let (x0, y0) = (cx.saturating_sub(cw / 2), cy.saturating_sub(ch / 2));
    let near = img.crop(x0, y0, cw * 2, ch * 2);
    let zoomed = near.scale(ZOOM);
    let fine = cells(zoomed.w, zoomed.h, FINE);
    let (m, p2) = ask_clef(agent, &marked(&zoomed, FINE), fine.len(), target, ", enlarged around the part that holds it").await?;
    let &(_, fx, fy, fw, fh) = fine.get(m as usize - 1).ok_or("a cell off the grid")?;
    let x = x0 + (fx + fw / 2) / ZOOM;
    let y = y0 + (fy + fh / 2) / ZOOM;
    Ok((x.min(img.w - 1), y.min(img.h - 1), p1, p2))
}

/// Clicks at (x, y) on the agent's display.
async fn click(display: u32, x: u32, y: u32, button: u8, double: bool) -> Result<(), String> {
    let mut cmd = tokio::process::Command::new("xdotool");
    cmd.env("DISPLAY", format!(":{display}")).args(["mousemove", "--sync", &x.to_string(), &y.to_string(), "click"]);
    if double {
        cmd.args(["--repeat", "2"]);
    }
    let out = cmd.arg(button.to_string()).output().await.map_err(|e| format!("xdotool: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// The screen tools, on `desk`.
pub struct Screen {
    pub desk: Desk,
}

impl Screen {
    async fn shot(&self) -> Result<Image, String> {
        crate::proxy::ensure_up(&self.desk).await?;
        self.desk.touch();
        capture(&self.desk.rfb()).await
    }

    pub async fn look(&self, args: &Value) -> Value {
        let Some(q) = args["question"].as_str().map(str::trim).filter(|q| !q.is_empty() && q.len() <= ASK_MAX_BYTES) else {
            return mcp::text_result(&format!("screen_look needs `question`, at most {ASK_MAX_BYTES} bytes"), true);
        };
        // no look while a person holds the screen: they may be typing a password
        if self.desk.held() {
            return mcp::text_result(crate::proxy::HELD, true);
        }
        let t = std::time::Instant::now();
        let img = match self.shot().await {
            Ok(i) => i,
            Err(e) => return mcp::text_result(&e, true),
        };
        let shot_ms = t.elapsed().as_millis() as u64;
        let said = ask_vision(&self.desk.agent, &img, q).await;
        fragment_bridge::ev!("screen.look", { "agent": self.desk.agent, "shotMs": shot_ms, "ms": t.elapsed().as_millis() as u64, "ok": said.is_ok() });
        match said {
            Ok(said) => mcp::text_result(&said, false),
            Err(e) => mcp::text_result(&format!("the vision model did not answer: {e}"), true),
        }
    }

    pub async fn click(&self, args: &Value) -> Value {
        let Some(target) = args["target"].as_str().map(str::trim).filter(|t| !t.is_empty() && t.len() <= ASK_MAX_BYTES) else {
            return mcp::text_result(&format!("screen_click needs `target`, what to click in words, at most {ASK_MAX_BYTES} bytes"), true);
        };
        if self.desk.held() {
            return mcp::text_result(crate::proxy::HELD, true);
        }
        let img = match self.shot().await {
            Ok(i) => i,
            Err(e) => return mcp::text_result(&e, true),
        };
        let (x, y, p1, p2) = match find(&self.desk.agent, &img, target).await {
            Ok(found) => found,
            Err(e) => return mcp::text_result(&format!("Clef did not find it: {e}"), true),
        };
        let sure = if p1 * p2 >= 0.25 { "" } else { " (Clef was unsure: check with screen_look before relying on it)" };
        if args["click"].as_bool() == Some(false) {
            return mcp::text_result(&format!("{target} is at x={x}, y={y}{sure}"), false);
        }
        let button = match args["button"].as_str() {
            Some("right") => 3,
            Some("middle") => 2,
            _ => 1,
        };
        let display = match crate::desk::allocate(&self.desk) {
            Ok(n) => n,
            Err(e) => return mcp::text_result(&e, true),
        };
        match click(display, x, y, button, args["double"].as_bool() == Some(true)).await {
            Ok(()) => mcp::text_result(&format!("clicked {target} at x={x}, y={y}{sure}"), false),
            Err(e) => mcp::text_result(&format!("found {target} at x={x}, y={y}, but the click failed: {e}"), true),
        }
    }
}

pub fn tools() -> Vec<Value> {
    vec![
        mcp::tool(
            "screen_look",
            "Ask what your desktop's screen shows (you read no images: a vision model looks for you and answers in words). Use it to check what is on screen, read text in an app or an image, or see whether something worked. Each call is a paid model call: ask one precise question rather than many.",
            json!({ "type": "object", "required": ["question"], "properties": { "question": { "type": "string", "description": "what you want to know about the screen" } } }),
        ),
        mcp::tool(
            "screen_click",
            "Click something on your desktop's screen, described in words (\"the blue Sign in button\", \"the search box at the top\"): Clef finds it on a screenshot and it is clicked. For desktop apps and pages the browser tools cannot reach (canvas, plugins, odd widgets). In the browser, prefer the browser tools' refs. `click: false` only says where it is.",
            json!({ "type": "object", "required": ["target"], "properties": {
                "target": { "type": "string", "description": "what to click, as a person would describe it" },
                "click": { "type": "boolean", "description": "false: only find it (default true)" },
                "button": { "type": "string", "enum": ["left", "right", "middle"] },
                "double": { "type": "boolean" }
            } }),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The grid covers the screen, numbered from 1 row by row, at most 255
    /// cells (a choice's options).
    #[test]
    fn the_grids_cells() {
        let c = cells(1280, 800, COARSE);
        assert_eq!(c.len(), 96);
        assert_eq!(c[0], (1, 0, 0, 106, 100));
        assert_eq!(c[12].0, 13);
        assert_eq!((c[12].1, c[12].2), (0, 100), "row two starts at the left");
        let area: u32 = c.iter().map(|(_, _, _, w, h)| w * h).sum();
        assert_eq!(area, 1280 * 800, "every pixel in exactly one cell");
    }

    /// Marks change the image only on the grid's lines and labels; crops
    /// and enlargements keep the pixels.
    #[test]
    fn marks_crops_and_zooms() {
        let mut img = Image::new(120, 80);
        img.fill(0, 0, 120, 80, [10, 20, 30]);
        let m = marked(&img, (2, 2));
        assert_eq!(m.get(0, 0), MARK, "a cell's corner is on its border");
        assert_eq!(m.get(3, 3), LABEL_BG, "its label");
        assert_eq!(m.get(30, 30), [10, 20, 30], "the rest as it was");
        let c = img.crop(110, 70, 50, 50);
        assert_eq!((c.w, c.h), (10, 10), "clamped to the image");
        let z = c.scale(2);
        assert_eq!((z.w, z.h, z.get(19, 19)), (20, 20, [10, 20, 30]));
        let j = img.jpeg().unwrap();
        assert!(j.starts_with(&[0xFF, 0xD8]), "a JPEG");
        assert!(img.data_url().unwrap().starts_with("data:image/jpeg;base64,"));
    }

    /// A fake display's frame is read whole, its colours as they are.
    #[tokio::test]
    async fn a_frame_is_read_over_rfb() {
        let dir = std::env::temp_dir().join(format!("look-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("rfb.sock");
        let listener = tokio::net::UnixListener::bind(&sock).unwrap();
        tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            s.write_all(b"RFB 003.008\n").await.unwrap();
            let mut v = [0u8; 12];
            s.read_exact(&mut v).await.unwrap();
            s.write_all(&[1, 1]).await.unwrap();
            let mut b = [0u8; 1];
            s.read_exact(&mut b).await.unwrap();
            s.write_all(&[0, 0, 0, 0]).await.unwrap();
            s.read_exact(&mut b).await.unwrap();
            let mut init = vec![0, 4, 0, 2];
            init.extend_from_slice(&[32, 24, 0, 1, 0, 255, 0, 255, 0, 255, 16, 8, 0, 0, 0, 0]);
            init.extend_from_slice(&[0, 0, 0, 1, b'x']);
            s.write_all(&init).await.unwrap();
            let mut asks = [0u8; 20 + 8 + 10];
            s.read_exact(&mut asks).await.unwrap();
            // a bell first, then the frame in two rectangles
            let mut m = vec![2u8, 0, 0, 0, 2];
            for (y, colour) in [(0u16, [3u8, 2, 1, 0]), (1, [6, 5, 4, 0])] {
                m.extend_from_slice(&[0, 0]);
                m.extend_from_slice(&y.to_be_bytes());
                m.extend_from_slice(&[0, 4, 0, 1, 0, 0, 0, 0]);
                for _ in 0..4 {
                    m.extend_from_slice(&colour);
                }
            }
            s.write_all(&m).await.unwrap();
        });
        let img = capture(&sock).await.unwrap();
        assert_eq!((img.w, img.h), (4, 2));
        assert_eq!(img.get(0, 0), [1, 2, 3], "little-endian, red at 16");
        assert_eq!(img.get(3, 1), [4, 5, 6]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
