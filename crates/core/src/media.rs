//! A job's media steps (docs/api.md, AI). An image (`job.ai.image`) is
//! FLUX.1 [schnell] on Workers AI, called through the deployment's AI
//! Gateway as text is (cell/src/models.rs; decisions 17 and 23), and
//! metered on the payer's ledger in the neurons Workers AI prices it at:
//! its 512×512 tiles and its diffusion steps (`price::Usage::Neurons`). A
//! step reserves its worst case first, so a step the payer cannot cover
//! never reaches the model; it settles at the image it got, its tiles read
//! from the JPEG's own header. Videos are off until they run on Cloudflare
//! (the debt ledger): `job.ai.video` is refused, saying so.

use base64::Engine;
use serde_json::{json, Value};

use crate::price::Usage;
use crate::steps::AiImage;

/// The image model, by its Workers AI catalog id (its catalog entry,
/// 2026-10-03: `cloudflare-docs` `workers-ai-models/flux-1-schnell.json`).
pub const IMAGE_MODEL: &str = "@cf/black-forest-labs/flux-1-schnell";
/// What it answers: a JPEG (its catalog's output schema).
pub const IMAGE_MEDIA_TYPE: &str = "image/jpeg";
/// Its input's bounds, from the same schema: a prompt of 1 to 2048
/// characters, and at most 8 diffusion steps (4 unless asked).
pub const PROMPT_MAX_CHARS: usize = 2048;
pub const STEPS_DEFAULT: u32 = 4;
pub const STEPS_MAX: u32 = 8;
/// Workers AI's price for it (its pricing page, 2026-10-03): 4.80 neurons
/// per 512×512 tile and 9.60 per step, which at $0.011 per thousand
/// neurons (`price::DEFAULT_NEURONS`) is its listed $0.0000528 a tile and
/// $0.0001056 a step. Here in thousandths of a neuron, the ledger's unit.
pub const TILE_MILLI_NEURONS: u64 = 4_800;
pub const STEP_MILLI_NEURONS: u64 = 9_600;
pub const TILE_PX: u64 = 512;
/// The tiles a worst case holds: FLUX.1 [schnell] draws 1024×1024. An
/// image with more is charged them all when it settles (a settle above its
/// reservation is charged in full: docs/ledger.md).
pub const TILES_WORST: u64 = 4;
/// The largest image kept: a 1024×1024 JPEG is a few hundred KiB, and its
/// base64 stays inside the model route's answer limit.
pub const IMAGE_MAX_BYTES: usize = 4 * 1024 * 1024;

/// Why a media step is refused before it reserves anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// A prompt that is empty or over `PROMPT_MAX_CHARS`.
    Prompt,
    /// Steps outside 1 to `STEPS_MAX`.
    Steps,
    /// A path that does not end in `.jpg` or `.jpeg`: the model draws
    /// JPEGs, and a file is served by its extension (bug 5).
    Path,
    /// `job.ai.video`: videos are off until they run on Cloudflare.
    VideoOff,
}

impl Refusal {
    /// The refusal for people (and the job, which may catch it).
    pub fn message(self) -> String {
        match self {
            Refusal::Prompt => format!("ai.image's prompt is 1 to {PROMPT_MAX_CHARS} characters"),
            Refusal::Steps => format!("ai.image's steps is 1 to {STEPS_MAX} ({STEPS_DEFAULT} unless named)"),
            Refusal::Path => "ai.image draws a JPEG: its path ends in .jpg or .jpeg".into(),
            Refusal::VideoOff => "video steps are off until they run on Cloudflare".into(),
        }
    }
}

/// Why the model's answer is no image the platform keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnswerFault {
    /// Not JSON with a base64 `image`.
    NoImage,
    /// Over `IMAGE_MAX_BYTES`.
    TooLarge,
    /// Not a JPEG whose size reads from its header.
    NotJpeg,
}

impl AnswerFault {
    pub fn message(self) -> String {
        match self {
            AnswerFault::NoImage => "the model's answer has no image".into(),
            AnswerFault::TooLarge => format!("the model's image is over {IMAGE_MAX_BYTES} bytes"),
            AnswerFault::NotJpeg => "the model's image is not a JPEG whose size reads".into(),
        }
    }
}

/// An image step, checked: the model's input, and its steps (which its
/// price counts).
#[derive(Debug, Clone, PartialEq)]
pub struct ImageCall {
    pub input: Value,
    pub steps: u32,
}

/// The call an image step makes, or why it is refused.
pub fn image_call(step: &AiImage) -> Result<ImageCall, Refusal> {
    let chars = step.prompt.chars().count();
    if chars == 0 || chars > PROMPT_MAX_CHARS {
        return Err(Refusal::Prompt);
    }
    let steps = step.steps.unwrap_or(STEPS_DEFAULT);
    if !(1..=STEPS_MAX).contains(&steps) {
        return Err(Refusal::Steps);
    }
    let lower = step.path.to_ascii_lowercase();
    if !(lower.ends_with(".jpg") || lower.ends_with(".jpeg")) {
        return Err(Refusal::Path);
    }
    Ok(ImageCall { input: json!({ "prompt": step.prompt, "steps": steps }), steps })
}

impl ImageCall {
    /// What the call reserves: a 1024×1024 image at its steps.
    pub fn worst(&self) -> Usage {
        neurons(TILES_WORST, self.steps)
    }

    /// What an image of `width` × `height` at its steps cost.
    pub fn usage(&self, width: u16, height: u16) -> Usage {
        neurons(tiles(width, height), self.steps)
    }
}

/// The 512×512 tiles an image of `width` × `height` covers, a part tile
/// counted whole.
pub fn tiles(width: u16, height: u16) -> u64 {
    u64::from(width).div_ceil(TILE_PX) * u64::from(height).div_ceil(TILE_PX)
}

fn neurons(tiles: u64, steps: u32) -> Usage {
    // at most 128² tiles (u16 sides) and STEPS_MAX steps: far under QUANTITY_MAX
    Usage::Neurons { milli: tiles * TILE_MILLI_NEURONS + u64::from(steps) * STEP_MILLI_NEURONS }
}

/// An image the model drew: its bytes and its size.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    pub bytes: Vec<u8>,
    pub width: u16,
    pub height: u16,
}

/// The image in the model's answer: `{"image": "<base64>"}`, its catalog's
/// output schema, as the binding answers it raw, or the same inside
/// `result`, as Workers AI's REST API wraps an answer.
pub fn image_of(answer: &[u8]) -> Result<Image, AnswerFault> {
    let v: Value = serde_json::from_slice(answer).map_err(|_| AnswerFault::NoImage)?;
    let b64 = v["image"].as_str().or_else(|| v["result"]["image"].as_str()).ok_or(AnswerFault::NoImage)?;
    // base64 is 4 characters for 3 bytes: a longer one decodes past the limit
    if b64.len() / 4 * 3 > IMAGE_MAX_BYTES + 3 {
        return Err(AnswerFault::TooLarge);
    }
    let bytes = base64::engine::general_purpose::STANDARD.decode(b64).map_err(|_| AnswerFault::NoImage)?;
    if bytes.len() > IMAGE_MAX_BYTES {
        return Err(AnswerFault::TooLarge);
    }
    let (width, height) = jpeg_size(&bytes).ok_or(AnswerFault::NotJpeg)?;
    Ok(Image { bytes, width, height })
}

/// A JPEG's width and height, from its first frame header (SOFn); `None`
/// when the bytes are no JPEG, or end before a frame header that reads.
pub fn jpeg_size(b: &[u8]) -> Option<(u16, u16)> {
    if !b.starts_with(&[0xFF, 0xD8]) {
        return None;
    }
    let mut i = 2;
    // bounded: each pass moves `i` forward at least one byte
    while i + 4 <= b.len() {
        if b[i] != 0xFF {
            return None;
        }
        let marker = b[i + 1];
        match marker {
            // a fill byte before a marker
            0xFF => i += 1,
            // markers that stand alone, with no length
            0x01 | 0xD0..=0xD7 => i += 2,
            // a scan, or the end, before any frame header
            0xD9 | 0xDA => return None,
            _ => {
                let len = usize::from(u16::from_be_bytes([b[i + 2], b[i + 3]]));
                if len < 2 {
                    return None;
                }
                // SOF0 to SOF15, less DHT (C4), JPG (C8) and DAC (CC)
                if (0xC0..=0xCF).contains(&marker) && !matches!(marker, 0xC4 | 0xC8 | 0xCC) {
                    // its length, the sample precision, then height and width
                    let f = b.get(i + 4..i + 9)?;
                    let (height, width) = (u16::from_be_bytes([f[1], f[2]]), u16::from_be_bytes([f[3], f[4]]));
                    // a height of 0 is set later by a DNL marker: no size here
                    return (width > 0 && height > 0).then_some((width, height));
                }
                i += 2 + len;
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::price::PriceBook;
    use crate::steps::Step;

    /// The head of a baseline JPEG of `width` × `height`: SOI, an APP0
    /// (JFIF) segment, a fill byte, SOF0, and the bytes a scan starts with.
    fn jpeg(width: u16, height: u16) -> Vec<u8> {
        let mut b = vec![0xFF, 0xD8];
        b.extend([0xFF, 0xE0, 0x00, 0x10, b'J', b'F', b'I', b'F', 0, 1, 1, 0, 0, 1, 0, 1, 0, 0]);
        b.extend([0xFF, 0xFF, 0xC0, 0x00, 0x11, 8]);
        b.extend(height.to_be_bytes());
        b.extend(width.to_be_bytes());
        b.extend([3, 1, 0x22, 0, 2, 0x11, 1, 3, 0x11, 1]);
        b.extend([0xFF, 0xDA, 0x00, 0x0C]);
        b
    }

    fn image(args: Value) -> AiImage {
        match Step::from_parts("ai.image", args) {
            Ok(Step::AiImage(i)) => i,
            other => panic!("{other:?}"),
        }
    }

    /// Goal: a step's input is the catalog's, its steps defaulted and
    /// bounded, its path a JPEG's. Method: valid and invalid steps, each
    /// bound at and past its edge.
    #[test]
    fn an_image_step_is_checked_before_it_reserves() {
        let call = image_call(&image(json!({ "prompt": "a lighthouse", "path": "art/a.jpg" }))).unwrap();
        assert_eq!(call, ImageCall { input: json!({ "prompt": "a lighthouse", "steps": 4 }), steps: 4 });
        let call = image_call(&image(json!({ "prompt": "p", "path": "A.JPEG", "steps": 8 }))).unwrap();
        assert_eq!(call.steps, 8);
        let refused = |args: Value| image_call(&image(args)).unwrap_err();
        assert_eq!(refused(json!({ "prompt": "", "path": "a.jpg" })), Refusal::Prompt);
        assert_eq!(refused(json!({ "prompt": "é".repeat(PROMPT_MAX_CHARS + 1), "path": "a.jpg" })), Refusal::Prompt);
        assert!(image_call(&image(json!({ "prompt": "é".repeat(PROMPT_MAX_CHARS), "path": "a.jpg" }))).is_ok(), "characters, not bytes");
        assert_eq!(refused(json!({ "prompt": "p", "path": "a.jpg", "steps": 0 })), Refusal::Steps);
        assert_eq!(refused(json!({ "prompt": "p", "path": "a.jpg", "steps": STEPS_MAX + 1 })), Refusal::Steps);
        assert_eq!(refused(json!({ "prompt": "p", "path": "a.png" })), Refusal::Path, "a JPEG at .png is bug 5");
        assert_eq!(refused(json!({ "prompt": "p", "path": "jpg" })), Refusal::Path);
    }

    /// Goal: an image costs its tiles and steps at Workers AI's neurons,
    /// so the book charges its listed price. Method: the default 1024×1024
    /// at 4 steps lists at 57.6 neurons ($0.0006336), part tiles count
    /// whole, and the worst case is the 1024×1024 image at the call's steps.
    #[test]
    fn an_image_costs_its_tiles_and_steps() {
        let call = image_call(&image(json!({ "prompt": "p", "path": "a.jpg" }))).unwrap();
        assert_eq!(call.usage(1024, 1024), Usage::Neurons { milli: 57_600 });
        assert_eq!(call.worst(), call.usage(1024, 1024));
        let list = PriceBook::defaults().price(&call.usage(1024, 1024)).unwrap().list;
        assert_eq!(list, 634, "57.6 neurons × $0.011 a thousand: $0.0006336, rounded up");
        assert_eq!(PriceBook::defaults().price(&neurons(1, 0)).unwrap().list, 53, "a tile lists at $0.0000528, rounded up");
        assert_eq!(PriceBook::defaults().price(&neurons(0, 1)).unwrap().list, 106, "a step at $0.0001056, rounded up");
        assert_eq!((tiles(512, 512), tiles(513, 512), tiles(1024, 768), tiles(1, 1)), (1, 2, 4, 1));
        assert_eq!(tiles(u16::MAX, u16::MAX), 128 * 128);
        let eight = image_call(&image(json!({ "prompt": "p", "path": "a.jpg", "steps": 8 }))).unwrap();
        assert_eq!(eight.worst(), Usage::Neurons { milli: 4 * 4_800 + 8 * 9_600 });
        assert!(neurons(tiles(u16::MAX, u16::MAX), STEPS_MAX).validate().is_ok(), "the largest image's usage prices");
    }

    /// Goal: a JPEG's size reads from its frame header, past the segments
    /// before it; anything else is no size. Method: a valid header,
    /// progressive (SOF2), and broken, truncated or foreign bytes.
    #[test]
    fn a_jpegs_size_reads_from_its_header() {
        assert_eq!(jpeg_size(&jpeg(1024, 768)), Some((1024, 768)));
        let mut progressive = jpeg(640, 480);
        let at = progressive.windows(2).position(|w| w == [0xFF, 0xC0]).unwrap();
        progressive[at + 1] = 0xC2;
        assert_eq!(jpeg_size(&progressive), Some((640, 480)));
        let mut dht_first = vec![0xFF, 0xD8, 0xFF, 0xC4, 0x00, 0x03, 0x00];
        dht_first.extend(&jpeg(16, 16)[2..]);
        assert_eq!(jpeg_size(&dht_first), Some((16, 16)), "a DHT segment is no frame header");
        assert_eq!(jpeg_size(b"\x89PNG\r\n\x1a\n"), None);
        assert_eq!(jpeg_size(&[0xFF, 0xD8]), None);
        let whole = jpeg(1024, 1024);
        // the frame header's width ends 9 bytes past its marker
        let end = whole.windows(2).position(|w| w == [0xFF, 0xC0]).unwrap() + 9;
        for n in 0..end {
            assert_eq!(jpeg_size(&whole[..n]), None, "cut at {n}");
        }
        assert_eq!(jpeg_size(&whole[..end]), Some((1024, 1024)));
        assert_eq!(jpeg_size(&jpeg(1024, 0)), None, "a height set later by DNL");
        assert_eq!(jpeg_size(&jpeg(0, 1024)), None, "no width");
        assert_eq!(jpeg_size(&[0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x01, 0, 0]), None, "a length under 2");
        assert_eq!(jpeg_size(&[0xFF, 0xD8, 0x00, 0x00, 0x00, 0x00]), None, "no marker");
        assert_eq!(jpeg_size(&[0xFF, 0xD8, 0xFF, 0xDA, 0x00, 0x02, 0, 0]), None, "a scan before a frame");
    }

    /// Goal: the image in the binding's raw answer (and the REST API's
    /// wrapped one) is decoded and sized; any other answer is refused,
    /// typed. Method: each shape, and each fault.
    #[test]
    fn the_image_reads_from_the_models_answer() {
        let b64 = |b: &[u8]| base64::engine::general_purpose::STANDARD.encode(b);
        let drawn = jpeg(1024, 1024);
        let raw = json!({ "image": b64(&drawn) }).to_string();
        assert_eq!(image_of(raw.as_bytes()), Ok(Image { bytes: drawn.clone(), width: 1024, height: 1024 }));
        let wrapped = json!({ "result": { "image": b64(&drawn) }, "success": true }).to_string();
        assert_eq!(image_of(wrapped.as_bytes()).map(|i| i.width), Ok(1024));
        assert_eq!(image_of(b"not json"), Err(AnswerFault::NoImage));
        assert_eq!(image_of(br#"{"response": "a lighthouse"}"#), Err(AnswerFault::NoImage));
        assert_eq!(image_of(br#"{"image": "not base64!"}"#), Err(AnswerFault::NoImage));
        let png = json!({ "image": b64(b"\x89PNG\r\n\x1a\nrest") }).to_string();
        assert_eq!(image_of(png.as_bytes()), Err(AnswerFault::NotJpeg));
        let mut big = jpeg(1024, 1024);
        big.resize(IMAGE_MAX_BYTES + 1, 0);
        assert_eq!(image_of(json!({ "image": b64(&big) }).to_string().as_bytes()), Err(AnswerFault::TooLarge));
        let mut at_limit = jpeg(1024, 1024);
        at_limit.resize(IMAGE_MAX_BYTES, 0);
        assert_eq!(image_of(json!({ "image": b64(&at_limit) }).to_string().as_bytes()).map(|i| i.bytes.len()), Ok(IMAGE_MAX_BYTES));
    }

    /// Every refusal says why, the video's in the words the platform owes.
    #[test]
    fn refusals_say_why() {
        assert_eq!(Refusal::VideoOff.message(), "video steps are off until they run on Cloudflare");
        assert!(Refusal::Path.message().contains(".jpg"));
        assert!(AnswerFault::TooLarge.message().contains(&IMAGE_MAX_BYTES.to_string()));
    }
}
