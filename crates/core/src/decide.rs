//! Typed decisions (docs/api.md, Decisions): a job's `ai.decide` step and
//! an agent's `POST /api/models/v1/decide`, on Workers AI's Clef family.
//! Fragment's names are its own: the provider's System One wire says
//! `state` for `input`, `noul` for `predicate` and `criteria` for
//! `options` and `levels`, and apps never see those.
//!
//! The models (developers.cloudflare.com/workers-ai/models/clef,
//! clef-flash and clef-omni, their schemas and the pricing page, read
//! 2026-10-14): `clef-flash` (the default, a 24,576-token context, $0.038
//! per million input tokens), `clef` (65,536 tokens, $0.24) and
//! `clef-omni` (64,000 tokens, $0.15). All three read images (PNG, JPEG,
//! WebP); only clef-omni hears audio (WAV, MP3) and watches video (MP4,
//! WebM). Media is billed as input tokens, and output never: a call's cost
//! is its reported `usage.input_tokens` at its model's input price.
//!
//! The bound a call reserves is its model's context window. Clef scores
//! every question in one pass over one context: "Media tokens count toward
//! the model's context window, together with the questions. If they exceed
//! the context window, the request fails. Otherwise, the text `state` is
//! truncated to fit the remaining space." So no call's input passes it. A
//! usage reported past it is charged all the same (the ledger prices what
//! happened), and one missing is charged the reservation.
//!
//! Media is a fragment's own blob by its SHA-256 (`{sha256, type}`, as
//! `fragment.blob(file)` and a chat's attachments name one), which the
//! cell reads, or bytes inline (`{data}`, a base64 `data:` URL). A job's
//! inline media is small (its args travel with the run); an agent's media
//! is all inline (it has no fragment's blob store), bounded by its route's
//! body. Every limit is checked before anything is reserved.
//!
//! The request is read by hand from its JSON (`bound`), not derived: each
//! way it is wrong is a refusal of its own, and the cell's Wasm carries no
//! deserializer per shape (xtask's `CELL_WASM_MAX_BYTES`).

use serde_json::{json, Map, Value};

use crate::price::Usage;

/// The family, by the name a request gives it (`model`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Model {
    ClefFlash,
    Clef,
    ClefOmni,
}

pub const MODELS: [Model; 3] = [Model::ClefFlash, Model::Clef, Model::ClefOmni];
/// What a request naming no model runs: the cheapest, for the many small
/// decisions an app or an agent makes.
pub const DEFAULT_MODEL: Model = Model::ClefFlash;

impl Model {
    /// The name a request gives it, and its System One selector (`model`).
    pub const fn name(self) -> &'static str {
        match self {
            Model::ClefFlash => "clef-flash",
            Model::Clef => "clef",
            Model::ClefOmni => "clef-omni",
        }
    }

    /// Its Workers AI catalog id: the price book's row, and the route's path.
    pub const fn id(self) -> &'static str {
        match self {
            Model::ClefFlash => "@cf/cloudflare/clef-flash",
            Model::Clef => "@cf/cloudflare/clef",
            Model::ClefOmni => "@cf/cloudflare/clef-omni",
        }
    }

    pub fn named(name: &str) -> Option<Model> {
        MODELS.into_iter().find(|m| m.name() == name)
    }

    pub fn of_id(id: &str) -> Option<Model> {
        MODELS.into_iter().find(|m| m.id() == id)
    }

    /// Its context window, in tokens: the most one call's input counts
    /// (the module's doc), so what a call reserves.
    pub const fn context_tokens(self) -> u64 {
        match self {
            Model::ClefFlash => 24_576,
            Model::Clef => 65_536,
            Model::ClefOmni => 64_000,
        }
    }

    /// Its list price, micro-dollars per million input tokens (the price
    /// book's rows: crate::price::DEFAULT_MODELS).
    pub const fn input_price(self) -> i64 {
        match self {
            Model::ClefFlash => 38_000,
            Model::Clef => 240_000,
            Model::ClefOmni => 150_000,
        }
    }

    /// Whether it reads media of `kind`: images all, audio and video
    /// clef-omni alone.
    pub fn takes(self, kind: Kind) -> bool {
        kind == Kind::Image || self == Model::ClefOmni
    }

    /// The most bytes a request's input and questions encode to, without
    /// its media: under its context (a byte-level token is at least a
    /// byte), with room left for media and the model's own framing.
    pub const fn request_max_bytes(self) -> usize {
        match self {
            Model::ClefFlash => 16 * 1024,
            Model::Clef | Model::ClefOmni => 48 * 1024,
        }
    }
}

const _: () = {
    let mut i = 0;
    while i < MODELS.len() {
        let m = MODELS[i];
        assert!((m.request_max_bytes() as u64) < m.context_tokens(), "a request's text leaves room in its model's context");
        i += 1;
    }
};

/// A media item's kind, as Clef's input takes it (`images`, `audio`, `videos`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Image,
    Audio,
    Video,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Image => "image",
            Kind::Audio => "audio",
            Kind::Video => "video",
        }
    }

    /// One item's most bytes (decoded): Clef's own for an image (4 MiB)
    /// and audio (8 MiB); for a video ours, under Clef's 16 MiB, so that
    /// every item at its most fits `MEDIA_TOTAL_MAX_BYTES`.
    pub const fn max_bytes(self) -> u64 {
        match self {
            Kind::Image => 4 * MIB,
            Kind::Audio | Kind::Video => 8 * MIB,
        }
    }

    /// The field of Clef's input that carries it.
    fn field(self) -> &'static str {
        match self {
            Kind::Image => "images",
            Kind::Audio => "audio",
            Kind::Video => "videos",
        }
    }
}

const MIB: u64 = 1024 * 1024;
pub const QUESTIONS_MAX: usize = 64;
pub const ID_MAX_BYTES: usize = 100;
pub const OPTIONS_MAX: usize = 255;
pub const LEVELS_MAX: usize = 10;
/// Media items in one decision, and videos among them (Clef's: at most 4
/// images, 4 audio clips, 2 videos).
pub const MEDIA_MAX: usize = 4;
pub const VIDEOS_MAX: usize = 2;
/// All of one decision's media, decoded. Its base64 and the request's
/// text stay under Clef's 13 MiB request body, and a step's or a route's
/// copies of it well under a Worker's 128 MB.
pub const MEDIA_TOTAL_MAX_BYTES: u64 = 8 * MIB;
/// A job's inline media, all of it, as its `data:` URLs' bytes: a run's
/// step args travel with it (the Workflow), so large media is a blob.
pub const JOB_INLINE_MAX_BYTES: usize = 256 * 1024;
/// Clef's whole request body (its schema's `images` description).
pub const CLEF_BODY_MAX_BYTES: u64 = 13 * MIB;
const _: () = assert!(
    MEDIA_TOTAL_MAX_BYTES.div_ceil(3) * 4 + 48 * 1024 + 4096 < CLEF_BODY_MAX_BYTES,
    "a decision's media as base64, with its text and Clef's framing, fits Clef's request body"
);
const _: () = assert!(Kind::Image.max_bytes() <= MEDIA_TOTAL_MAX_BYTES && Kind::Video.max_bytes() <= MEDIA_TOTAL_MAX_BYTES);

/// The media types it takes, each the type sent to Clef, its kind, and
/// the other names a browser or an SDK gives the same type.
const TYPES: [(&str, Kind, &[&str]); 7] = [
    ("image/png", Kind::Image, &[]),
    ("image/jpeg", Kind::Image, &[]),
    ("image/webp", Kind::Image, &[]),
    ("audio/wav", Kind::Audio, &["audio/x-wav", "audio/wave"]),
    ("audio/mpeg", Kind::Audio, &["audio/mp3"]),
    ("video/mp4", Kind::Video, &[]),
    ("video/webm", Kind::Video, &[]),
];

/// A type it takes, as sent, and its kind; parameters dropped, any case.
pub fn media_type(declared: &str) -> Option<(&'static str, Kind)> {
    let essence = declared.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
    TYPES.iter().find(|(t, _, aliases)| *t == essence || aliases.contains(&essence.as_str())).map(|(t, k, _)| (*t, *k))
}

/// The keys a request takes; any other is refused.
const REQUEST_KEYS: [&str; 4] = ["model", "input", "questions", "media"];
/// The keys a media item takes: a blob's hash and type, or inline data;
/// `size` and `name` are what `fragment.blob(file)` and a chat's
/// attachments carry beside a blob's, so an attachment passes as it is,
/// neither read.
const MEDIA_KEYS: [&str; 5] = ["sha256", "type", "data", "size", "name"];

/// Who asks: a job's step (its media blobs of its fragment, or small and
/// inline), or an agent on the model route (all inline).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Asker {
    Job,
    Agent,
}

/// Why a decision is refused before anything is reserved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// Not an object of `REQUEST_KEYS`.
    Request,
    Model,
    Input,
    Questions,
    Name,
    /// A question that is none of the three shapes.
    Question,
    Instructions,
    Options,
    Levels,
    TooLarge(Model),
    /// A list of media that is no list, or an item that is neither
    /// `{sha256, type}` nor `{data}`.
    MediaItem,
    /// A type it does not take (`TYPES`).
    MediaType,
    /// Audio or video for a model that does not take it.
    MediaModel(Kind, Model),
    /// More than `MEDIA_MAX` items, or `VIDEOS_MAX` videos.
    MediaCount,
    /// An item past its kind's bytes.
    ItemTooLarge(Kind),
    /// All the media past `MEDIA_TOTAL_MAX_BYTES`.
    MediaTooLarge,
    /// A job's inline media past `JOB_INLINE_MAX_BYTES`.
    InlineTooLarge,
    /// A blob on the model route: an agent sends its media inline.
    BlobOnRoute,
    /// A blob its fragment does not hold (the cell's to find).
    BlobMissing,
}

impl Refusal {
    /// Said whole, its limits written out (a test holds each to its const).
    pub fn message(self) -> &'static str {
        match self {
            Self::Request => "ai.decide takes {model?, input, questions, media?} and nothing else",
            Self::Model => "ai.decide's model is one of clef-flash, clef, clef-omni (clef-flash unless named)",
            Self::Input => "ai.decide's input is nonempty text, an object or an array",
            Self::Questions => "ai.decide takes 1 to 64 questions",
            Self::Name => "ai.decide's question ids are 1 to 100 ASCII letters, digits, '_', '.' or '-'",
            Self::Question => "ai.decide's question is {type: \"choice\", instructions, options}, {type: \"predicate\", instructions} or {type: \"score\", instructions, levels}",
            Self::Instructions => "ai.decide's instructions are nonempty text",
            Self::Options => "ai.decide's choice has 2 to 255 nonempty option ids, at most 100 bytes each, with nonempty descriptions",
            Self::Levels => "ai.decide's score has 2 to 10 nonempty level descriptions",
            Self::TooLarge(Model::ClefFlash) => "ai.decide's input and questions encode to at most 16384 bytes on clef-flash (49152 on clef and clef-omni)",
            Self::TooLarge(_) => "ai.decide's input and questions encode to at most 49152 bytes on clef and clef-omni",
            Self::MediaItem => "ai.decide's media is a list of blobs, {sha256, type}, or inline items, {data: a base64 data: URL}",
            Self::MediaType => "ai.decide's media is one of image/png, image/jpeg, image/webp, audio/wav, audio/mpeg, video/mp4, video/webm",
            Self::MediaModel(..) => "ai.decide's audio and video are heard and watched by clef-omni alone",
            Self::MediaCount => "ai.decide takes at most 4 media items, 2 of them videos",
            Self::ItemTooLarge(Kind::Image) => "ai.decide's image is at most 4 MiB",
            Self::ItemTooLarge(Kind::Audio) => "ai.decide's audio is at most 8 MiB",
            Self::ItemTooLarge(Kind::Video) => "ai.decide's video is at most 8 MiB",
            Self::MediaTooLarge => "ai.decide's media is at most 8 MiB together",
            Self::InlineTooLarge => "a job's inline media is at most 256 KiB together: upload larger media as a blob (fragment.blob(file)) and name its sha256",
            Self::BlobOnRoute => "an agent's decision carries its media inline, {data: a base64 data: URL}: the route reads no fragment's blobs",
            Self::BlobMissing => "ai.decide's media names a blob this fragment does not hold: upload it first (fragment.blob(file))",
        }
    }
}

/// Why a paid answer is none the platform passes on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnswerFault {
    Shape,
    Questions,
    Type,
    Probability,
    Choice,
    Score,
}

impl AnswerFault {
    pub fn message(self) -> &'static str {
        match self {
            Self::Shape => "ai.decide's model returned an invalid JSON answer",
            Self::Questions => "ai.decide's model returned an invalid question set",
            Self::Type => "ai.decide's model returned an invalid answer type",
            Self::Probability => "ai.decide's model returned an invalid probability distribution",
            Self::Choice => "ai.decide's model returned an invalid choice",
            Self::Score => "ai.decide's model returned an invalid score",
        }
    }
}

/// Where an item's bytes are.
#[derive(Debug, Clone, PartialEq)]
pub enum Bytes {
    /// A blob of the asking fragment, by its SHA-256: the cell reads it.
    Blob(String),
    /// Inline: its base64, checked, and its decoded size.
    Inline { base64: String, size: u64 },
}

/// One item, checked: its kind, the type Clef is sent, and its bytes.
#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub kind: Kind,
    pub media_type: &'static str,
    pub bytes: Bytes,
}

/// What a question asked, to check its answer by.
#[derive(Debug, Clone, PartialEq)]
enum Asked {
    /// The option ids, in the order the wire lists them.
    Choice(Vec<String>),
    Predicate,
    /// The levels, as asked: an answer names them back.
    Score(Value),
}

/// A decision, checked: the model, its wire input less media, what it
/// asked, and its media.
#[derive(Debug, Clone, PartialEq)]
pub struct DecisionCall {
    pub model: Model,
    /// System One's input without `images`, `audio` or `videos`.
    wire: Value,
    asked: Vec<(String, Asked)>,
    pub media: Vec<Item>,
}

fn nonempty(v: &Value) -> bool {
    v.as_str().is_some_and(|s| !s.trim().is_empty())
}

fn valid_id(id: &str) -> bool {
    (1..=ID_MAX_BYTES).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
}

/// Whether an object holds only `keys`.
fn only(o: &Map<String, Value>, keys: &[&str]) -> bool {
    o.keys().all(|k| keys.contains(&k.as_str()))
}

/// One question, checked: its System One wire and what it asked.
fn question(q: &Value) -> Result<(Value, Asked), Refusal> {
    let o = q.as_object().ok_or(Refusal::Question)?;
    let (kind, list) = match o.get("type").and_then(Value::as_str) {
        Some("choice") => ("choice", "options"),
        Some("predicate") => ("noul", ""),
        Some("score") => ("score", "levels"),
        _ => return Err(Refusal::Question),
    };
    let keys = ["type", "instructions", list];
    if !only(o, &keys[..if list.is_empty() { 2 } else { 3 }]) || !o.contains_key("instructions") || (!list.is_empty() && !o.contains_key(list)) {
        return Err(Refusal::Question);
    }
    let instructions = &o["instructions"];
    if !nonempty(instructions) {
        return Err(Refusal::Instructions);
    }
    let asked = match kind {
        "choice" => {
            let options = o["options"].as_object().ok_or(Refusal::Options)?;
            let valid = (2..=OPTIONS_MAX).contains(&options.len()) && options.iter().all(|(k, v)| !k.trim().is_empty() && k.len() <= ID_MAX_BYTES && nonempty(v));
            if !valid {
                return Err(Refusal::Options);
            }
            Asked::Choice(options.keys().cloned().collect())
        }
        "score" => {
            let levels = o["levels"].as_array().ok_or(Refusal::Levels)?;
            if !(2..=LEVELS_MAX).contains(&levels.len()) || !levels.iter().all(nonempty) {
                return Err(Refusal::Levels);
            }
            Asked::Score(o["levels"].clone())
        }
        _ => Asked::Predicate,
    };
    let mut wire = json!({ "type": kind, "instructions": instructions });
    if !list.is_empty() {
        wire["criteria"] = o[list].clone();
    }
    Ok((wire, asked))
}

/// An inline item's type, kind and base64, from its `data:` URL.
fn inline(url: &str) -> Result<(&'static str, Kind, &str), Refusal> {
    let (head, b64) = url.split_once(',').ok_or(Refusal::MediaItem)?;
    let declared = head.strip_prefix("data:").and_then(|h| h.strip_suffix(";base64")).ok_or(Refusal::MediaItem)?;
    let (t, kind) = media_type(declared).ok_or(Refusal::MediaType)?;
    Ok((t, kind, b64))
}

/// The decoded size of standard, padded base64, or `None` when it is none
/// (checked without decoding it: no copy of the media).
fn decoded_size(b64: &str) -> Option<u64> {
    let b = b64.as_bytes();
    if b.is_empty() || !b.len().is_multiple_of(4) {
        return None;
    }
    let pad = b.iter().rev().take_while(|c| **c == b'=').count();
    let alphabet = |c: &u8| c.is_ascii_alphanumeric() || *c == b'+' || *c == b'/';
    (pad <= 2 && b[..b.len() - pad].iter().all(alphabet)).then(|| (b.len() / 4 * 3 - pad) as u64)
}

/// One media item, checked as `asker` may send it.
fn item(m: &Value, asker: Asker) -> Result<Item, Refusal> {
    let o = m.as_object().filter(|o| only(o, &MEDIA_KEYS)).ok_or(Refusal::MediaItem)?;
    let text = |k: &str| o.get(k).and_then(Value::as_str);
    match (text("sha256"), text("type"), text("data")) {
        (Some(sha), Some(declared), None) if !o.contains_key("data") => {
            if asker == Asker::Agent {
                return Err(Refusal::BlobOnRoute);
            }
            if !crate::blob::valid_sha(sha) {
                return Err(Refusal::MediaItem);
            }
            let (media_type, kind) = media_type(declared).ok_or(Refusal::MediaType)?;
            Ok(Item { kind, media_type, bytes: Bytes::Blob(sha.to_string()) })
        }
        (None, None, Some(url)) if !o.contains_key("sha256") && !o.contains_key("type") => {
            let (media_type, kind, b64) = inline(url)?;
            let size = decoded_size(b64).ok_or(Refusal::MediaItem)?;
            Ok(Item { kind, media_type, bytes: Bytes::Inline { base64: b64.to_string(), size } })
        }
        _ => Err(Refusal::MediaItem),
    }
}

/// Reads and validates a request (`{model?, input, questions, media?}`)
/// before anything is reserved, and translates it to the wire. A blob's
/// size is the cell's to read: `DecisionCall::sized` checks it.
pub fn bound(request: &Value, asker: Asker) -> Result<DecisionCall, Refusal> {
    let o = request.as_object().filter(|o| only(o, &REQUEST_KEYS)).ok_or(Refusal::Request)?;
    let model = match o.get("model") {
        None | Some(Value::Null) => DEFAULT_MODEL,
        Some(name) => name.as_str().and_then(Model::named).ok_or(Refusal::Model)?,
    };
    let input = o.get("input").unwrap_or(&Value::Null);
    let filled = match input {
        Value::String(s) => !s.trim().is_empty(),
        Value::Object(m) => !m.is_empty(),
        Value::Array(a) => !a.is_empty(),
        _ => false,
    };
    if !filled {
        return Err(Refusal::Input);
    }
    let questions = o.get("questions").and_then(Value::as_object).ok_or(Refusal::Questions)?;
    if !(1..=QUESTIONS_MAX).contains(&questions.len()) {
        return Err(Refusal::Questions);
    }
    let mut wires = Map::new();
    let mut asked = Vec::with_capacity(questions.len());
    // bounded: at most QUESTIONS_MAX
    for (id, q) in questions {
        if !valid_id(id) {
            return Err(Refusal::Name);
        }
        let (wire, a) = question(q)?;
        wires.insert(id.clone(), wire);
        asked.push((id.clone(), a));
    }
    let wire = json!({ "model": model.name(), "state": input, "questions": wires });
    if wire.to_string().len() > model.request_max_bytes() {
        return Err(Refusal::TooLarge(model));
    }
    let listed = match o.get("media") {
        None | Some(Value::Null) => &[][..],
        Some(m) => m.as_array().ok_or(Refusal::MediaItem)?.as_slice(),
    };
    if listed.len() > MEDIA_MAX {
        return Err(Refusal::MediaCount);
    }
    let mut media = Vec::with_capacity(listed.len());
    let mut inline_bytes = 0;
    // bounded: at most MEDIA_MAX items
    for m in listed {
        let i = item(m, asker)?;
        if !model.takes(i.kind) {
            return Err(Refusal::MediaModel(i.kind, model));
        }
        if let Bytes::Inline { base64, .. } = &i.bytes {
            inline_bytes += base64.len();
        }
        media.push(i);
    }
    if media.iter().filter(|i| i.kind == Kind::Video).count() > VIDEOS_MAX {
        return Err(Refusal::MediaCount);
    }
    if asker == Asker::Job && inline_bytes > JOB_INLINE_MAX_BYTES {
        return Err(Refusal::InlineTooLarge);
    }
    let call = DecisionCall { model, wire, asked, media };
    // inline items are sized now, blobs once the cell reads theirs
    call.sized(&[])?;
    assert_eq!(call.asked.len(), questions.len(), "each question asked once");
    Ok(call)
}

fn probability(v: &Value) -> Result<f64, AnswerFault> {
    v.as_f64().filter(|p| p.is_finite() && (0.0..=1.0).contains(p)).ok_or(AnswerFault::Probability)
}

/// A complete distribution over exactly `keys`, summing to 1: each key's
/// probability, in their order.
fn distribution(v: &Value, keys: &[String]) -> Result<Vec<f64>, AnswerFault> {
    let values = v.as_object().filter(|o| o.len() == keys.len()).ok_or(AnswerFault::Probability)?;
    let ps = keys.iter().map(|k| values.get(k).map_or(Err(AnswerFault::Probability), probability)).collect::<Result<Vec<f64>, _>>()?;
    if (ps.iter().sum::<f64>() - 1.0).abs() > 0.001 {
        return Err(AnswerFault::Probability);
    }
    Ok(ps)
}

/// A distribution as an answer names it: each key's probability.
fn named(keys: &[String], ps: &[f64]) -> Value {
    let mut m = Map::new();
    // bounded: at most OPTIONS_MAX keys
    for (k, p) in keys.iter().zip(ps) {
        m.insert(k.clone(), json!(p));
    }
    Value::Object(m)
}

impl DecisionCall {
    /// The blobs it names, in order: the cell reads their sizes, then
    /// their bytes.
    pub fn blobs(&self) -> Vec<&str> {
        self.media
            .iter()
            .filter_map(|i| match &i.bytes {
                Bytes::Blob(sha) => Some(sha.as_str()),
                Bytes::Inline { .. } => None,
            })
            .collect()
    }

    /// Checks the blobs' sizes (in `blobs()` order, as the store holds
    /// them; none before they are read): each within its kind's, and every
    /// item's together within `MEDIA_TOTAL_MAX_BYTES`.
    pub fn sized(&self, blob_sizes: &[u64]) -> Result<(), Refusal> {
        let mut sizes = blob_sizes.iter();
        let mut total = 0u64;
        // bounded: at most MEDIA_MAX items
        for item in &self.media {
            let size = match &item.bytes {
                Bytes::Inline { size, .. } => *size,
                Bytes::Blob(_) => match sizes.next() {
                    Some(size) => *size,
                    None => continue,
                },
            };
            if size > item.kind.max_bytes() {
                return Err(Refusal::ItemTooLarge(item.kind));
            }
            total += size;
        }
        assert!(sizes.next().is_none(), "a size for each blob, no more");
        if total > MEDIA_TOTAL_MAX_BYTES {
            return Err(Refusal::MediaTooLarge);
        }
        Ok(())
    }

    /// System One's input: the wire, and each item as a `data:` URL in its
    /// kind's field, in the order they came; `blobs` the bytes of
    /// `blobs()`, sized and in order.
    pub fn input(&self, blobs: Vec<Vec<u8>>) -> Value {
        use base64::Engine;
        assert_eq!(blobs.len(), self.blobs().len(), "the bytes of each blob it names");
        let mut wire = self.wire.clone();
        let fields = wire.as_object_mut().expect("the wire is an object");
        let mut blobs = blobs.into_iter();
        // bounded: at most MEDIA_MAX items
        for item in &self.media {
            let url = match &item.bytes {
                Bytes::Inline { base64, .. } => format!("data:{};base64,{base64}", item.media_type),
                Bytes::Blob(_) => format!("data:{};base64,{}", item.media_type, base64::engine::general_purpose::STANDARD.encode(blobs.next().expect("asserted above"))),
            };
            let field = fields.entry(item.kind.field()).or_insert_with(|| json!([]));
            field.as_array_mut().expect("a media field is a list").push(Value::String(url));
        }
        wire
    }

    /// What it reserves: its model's whole context window (the module's doc).
    pub fn worst(&self) -> Usage {
        tokens(self.model, self.model.context_tokens())
    }

    /// Its input tokens, media and questions included; output is never
    /// billed. `None` when missing or malformed: the cell charges the
    /// reservation.
    pub fn usage(&self, response: &Value) -> Option<Usage> {
        response["usage"]["input_tokens"].as_u64().map(|n| tokens(self.model, n)).filter(|u| u.validate().is_ok())
    }

    /// Only complete, type-correct answers leave the platform: `{model,
    /// answers, usage}`, each answer in Fragment's names.
    pub fn answer(&self, response: &Value) -> Result<Value, AnswerFault> {
        let raw = response["answers"].as_object().ok_or(AnswerFault::Shape)?;
        if raw.len() != self.asked.len() {
            return Err(AnswerFault::Questions);
        }
        let mut answers = Map::new();
        // bounded: at most QUESTIONS_MAX
        for (id, asked) in &self.asked {
            let a = raw.get(id).ok_or(AnswerFault::Questions)?;
            let kind = match asked {
                Asked::Predicate => "noul",
                Asked::Choice(_) => "choice",
                Asked::Score(_) => "score",
            };
            if a["type"] != kind {
                return Err(AnswerFault::Type);
            }
            let answer = match asked {
                Asked::Predicate => json!({ "type": "predicate", "probability": probability(&a["noul"])? }),
                Asked::Choice(options) => {
                    let ps = distribution(&a["probabilities"], options)?;
                    let at = a["choice"].as_str().and_then(|c| options.iter().position(|o| o == c)).ok_or(AnswerFault::Choice)?;
                    if ps.iter().any(|p| *p > ps[at] + 0.001) {
                        return Err(AnswerFault::Choice);
                    }
                    json!({ "type": "choice", "choice": options[at], "probabilities": named(options, &ps), "confidence": probability(&a["confidence"])? })
                }
                Asked::Score(levels) => {
                    let keys: Vec<String> = (0..levels.as_array().map_or(0, Vec::len)).map(|i| i.to_string()).collect();
                    let ps = distribution(&a["probabilities"], &keys)?;
                    let expected: f64 = ps.iter().enumerate().map(|(i, p)| i as f64 * p).sum();
                    let top = (keys.len() - 1) as f64;
                    let score = a["score"].as_f64().filter(|s| s.is_finite() && (0.0..=top).contains(s) && (*s - expected).abs() <= 0.01).ok_or(AnswerFault::Score)?;
                    json!({ "type": "score", "score": score, "levels": levels, "probabilities": named(&keys, &ps), "confidence": probability(&a["confidence"])? })
                }
            };
            answers.insert(id.clone(), answer);
        }
        assert_eq!(answers.len(), self.asked.len(), "an answer for each question");
        Ok(json!({ "model": self.model.id(), "answers": answers, "usage": response["usage"] }))
    }
}

fn tokens(model: Model, input: u64) -> Usage {
    Usage::Tokens { model: model.id().into(), input, cached_input: 0, cache_write: 0, output: 0 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::price::PriceBook;
    use base64::Engine;

    fn request() -> Value {
        json!({ "input": "tallow potato chips", "questions": {
            "aisle": { "type": "choice", "instructions": "Where is the finished product?", "options": { "snacks": "Chips and crackers", "produce": "Fresh vegetables" } },
            "snack": { "type": "predicate", "instructions": "Is it a snack?" },
            "urgency": { "type": "score", "instructions": "How soon to buy?", "levels": ["Later", "Now"] }
        } })
    }

    fn response() -> Value {
        json!({ "model": "clef-flash", "answers": {
            "aisle": { "type": "choice", "choice": "snacks", "probabilities": { "snacks": 0.9, "produce": 0.1 }, "confidence": 0.8 },
            "snack": { "type": "noul", "noul": 0.95 },
            "urgency": { "type": "score", "score": 0.75, "legend": { "0": "Later", "1": "Now" }, "probabilities": { "0": 0.25, "1": 0.75 }, "confidence": 0.5 }
        }, "usage": { "input_tokens": 300, "output_tokens": 40 } })
    }

    fn b64(bytes: &[u8]) -> String {
        base64::engine::general_purpose::STANDARD.encode(bytes)
    }

    fn blob(sha: char, t: &str) -> Value {
        json!({ "sha256": sha.to_string().repeat(64), "type": t })
    }

    fn data(t: &str, bytes: usize) -> Value {
        json!({ "data": format!("data:{t};base64,{}", b64(&vec![7u8; bytes])) })
    }

    fn with(model: Option<&str>, media: Vec<Value>) -> Value {
        let mut r = request();
        if let Some(m) = model {
            r["model"] = json!(m);
        }
        r["media"] = json!(media);
        r
    }

    /// Goal: the neutral contract maps to System One's wire on the default
    /// model, its answers come back in Fragment's names, and only input is
    /// billed. Method: one question of each type, through bound and answer.
    #[test]
    fn maps_the_contract_and_prices_only_input() {
        let c = bound(&request(), Asker::Job).unwrap();
        assert_eq!(c.model, Model::ClefFlash, "clef-flash unless named");
        let input = c.input(vec![]);
        assert_eq!(input["model"], "clef-flash");
        assert_eq!(input["state"], "tallow potato chips");
        assert_eq!(input["questions"]["snack"], json!({ "type": "noul", "instructions": "Is it a snack?" }));
        assert_eq!(input["questions"]["aisle"]["criteria"]["snacks"], "Chips and crackers");
        assert_eq!(input["questions"]["urgency"]["criteria"], json!(["Later", "Now"]));
        assert!(input.get("images").is_none() && input.get("audio").is_none(), "no media, no media fields");
        let r = c.answer(&response()).unwrap();
        assert_eq!(r["model"], "@cf/cloudflare/clef-flash");
        assert_eq!(r["answers"]["aisle"], json!({ "type": "choice", "choice": "snacks", "probabilities": { "snacks": 0.9, "produce": 0.1 }, "confidence": 0.8 }));
        assert_eq!(r["answers"]["snack"], json!({ "type": "predicate", "probability": 0.95 }));
        assert_eq!(r["answers"]["urgency"]["score"], 0.75);
        assert_eq!(r["answers"]["urgency"]["levels"], json!(["Later", "Now"]));
        assert_eq!(r["answers"]["urgency"]["probabilities"], json!({ "0": 0.25, "1": 0.75 }));
        assert_eq!(c.usage(&response()), Some(tokens(Model::ClefFlash, 300)));
        assert_eq!(c.usage(&json!({})), None);
        assert_eq!(c.usage(&json!({ "usage": { "input_tokens": -1 } })), None);
        assert_eq!(c.usage(&json!({ "usage": { "input_tokens": "300" } })), None);
        let mut nulls = request();
        nulls["model"] = Value::Null;
        nulls["media"] = Value::Null;
        assert_eq!(bound(&nulls, Asker::Job).map(|c| c.model), Ok(Model::ClefFlash), "a null model or media is none named");
    }

    /// Goal: each model is selectable by name, runs on its catalog id, and
    /// reserves its whole context. Method: each name, and names that are
    /// none of them (a catalog id, a tier, another type).
    #[test]
    fn the_family_is_selected_by_name() {
        for (name, id, context) in [("clef-flash", "@cf/cloudflare/clef-flash", 24_576), ("clef", "@cf/cloudflare/clef", 65_536), ("clef-omni", "@cf/cloudflare/clef-omni", 64_000)] {
            let c = bound(&with(Some(name), vec![]), Asker::Job).unwrap();
            assert_eq!((c.model.name(), c.model.id(), c.model.context_tokens()), (name, id, context));
            assert_eq!(c.input(vec![])["model"], name, "System One's selector is the name");
            assert_eq!(c.worst(), tokens(c.model, context));
            assert_eq!(Model::of_id(id), Some(c.model));
            let r = c.answer(&response()).unwrap();
            assert_eq!(r["model"], id, "the answer names the model that answered");
        }
        for bad in ["@cf/cloudflare/clef", "cheap", "Clef", "clef-omni ", ""] {
            assert_eq!(bound(&with(Some(bad), vec![]), Asker::Job).err(), Some(Refusal::Model), "{bad:?}");
        }
        let mut number = request();
        number["model"] = json!(1);
        assert_eq!(bound(&number, Asker::Job).err(), Some(Refusal::Model), "a model is a name");
        assert!(Refusal::Model.message().contains("clef-flash, clef, clef-omni"));
    }

    /// Goal: the price book prices each model's input at its list price,
    /// output never, and a reservation is its context's. Method: the
    /// default book on 300 tokens and on each context, at its fee and
    /// margin (×1.05 ×1.5, rounded up).
    #[test]
    fn each_model_is_priced_by_its_input_alone() {
        let book = PriceBook::defaults();
        let charge = |m: Model, input: u64| book.price(&tokens(m, input)).unwrap().charge;
        assert_eq!(charge(Model::ClefFlash, 300), 18, "ceil($0.038/M × 300 × 1.575 = 17.955 µ$)");
        assert_eq!(charge(Model::Clef, 300), 114, "ceil($0.24/M × 300 × 1.575 = 113.4 µ$)");
        assert_eq!(charge(Model::ClefOmni, 300), 71, "ceil($0.15/M × 300 × 1.575 = 70.875 µ$)");
        let with_output = Usage::Tokens { model: Model::Clef.id().into(), input: 300, cached_input: 0, cache_write: 0, output: 1_000_000 };
        assert_eq!(book.price(&with_output).unwrap().charge, 114, "output is free");
        for (m, worst) in [(Model::ClefFlash, 1_471), (Model::Clef, 24_773), (Model::ClefOmni, 15_120)] {
            assert_eq!(charge(m, m.context_tokens()), worst, "{} reserves its context", m.name());
            let row = book.models.iter().find(|r| r.model == m.id()).expect("a row for each model");
            assert_eq!(row.price.input, m.input_price());
            assert_eq!(row.price.output, 0);
        }
    }

    /// Goal: a request outside the contract is refused, typed, before
    /// anything is reserved. Method: each bound at and past its edge, and
    /// each shape that is not one.
    #[test]
    fn invalid_requests_are_refused_before_payment() {
        let bad = |change: &dyn Fn(&mut Value), why| {
            let mut r = request();
            change(&mut r);
            assert_eq!(bound(&r, Asker::Job).err(), Some(why), "{r}");
        };
        assert_eq!(bound(&json!("chips"), Asker::Job).err(), Some(Refusal::Request));
        bad(&|r| r["temperature"] = json!(0), Refusal::Request);
        bad(&|r| r["state"] = json!("chips"), Refusal::Request);
        bad(&|r| r["input"] = Value::Null, Refusal::Input);
        bad(&|r| r["input"] = json!("  "), Refusal::Input);
        bad(&|r| r["input"] = json!({}), Refusal::Input);
        bad(&|r| r["input"] = json!(7), Refusal::Input);
        bad(&|r| r["questions"] = json!({}), Refusal::Questions);
        bad(&|r| r["questions"] = json!([]), Refusal::Questions);
        bad(&|r| r["questions"]["bad name"] = json!({ "type": "predicate", "instructions": "x" }), Refusal::Name);
        bad(&|r| r["questions"]["x".repeat(ID_MAX_BYTES + 1)] = json!({ "type": "predicate", "instructions": "x" }), Refusal::Name);
        bad(&|r| r["questions"]["x"] = json!({ "type": "rank", "instructions": "x" }), Refusal::Question);
        bad(&|r| r["questions"]["x"] = json!({ "type": "noul", "instructions": "x" }), Refusal::Question);
        bad(&|r| r["questions"]["x"] = json!({ "type": "predicate", "instructions": "x", "options": {} }), Refusal::Question);
        bad(&|r| r["questions"]["x"] = json!({ "type": "choice", "instructions": "x" }), Refusal::Question);
        bad(&|r| r["questions"]["x"] = json!({ "instructions": "x" }), Refusal::Question);
        bad(&|r| r["questions"]["x"] = json!("x"), Refusal::Question);
        bad(&|r| r["questions"]["x"] = json!({ "type": "predicate", "instructions": "" }), Refusal::Instructions);
        bad(&|r| r["questions"]["x"] = json!({ "type": "predicate", "instructions": { "q": "x" } }), Refusal::Instructions);
        bad(&|r| r["questions"]["x"] = json!({ "type": "choice", "instructions": "x", "options": {} }), Refusal::Options);
        bad(&|r| r["questions"]["x"] = json!({ "type": "choice", "instructions": "x", "options": { "a": "A", "b": " " } }), Refusal::Options);
        bad(&|r| r["questions"]["x"] = json!({ "type": "choice", "instructions": "x", "options": ["a", "b"] }), Refusal::Options);
        bad(&|r| r["questions"]["x"] = json!({ "type": "score", "instructions": "x", "levels": ["x"] }), Refusal::Levels);
        bad(&|r| r["questions"]["x"] = json!({ "type": "score", "instructions": "x", "levels": vec!["x"; LEVELS_MAX + 1] }), Refusal::Levels);
        bad(&|r| r["questions"]["x"] = json!({ "type": "score", "instructions": "x", "levels": ["x", 2] }), Refusal::Levels);
        bad(&|r| r["input"] = json!("x".repeat(Model::ClefFlash.request_max_bytes())), Refusal::TooLarge(Model::ClefFlash));
        bad(&|r| r["questions"] = Value::Object((0..=QUESTIONS_MAX).map(|i| (i.to_string(), json!({ "type": "predicate", "instructions": "x" }))).collect()), Refusal::Questions);
        let mut long = request();
        long["input"] = json!("x".repeat(Model::ClefFlash.request_max_bytes()));
        long["model"] = json!("clef");
        assert!(bound(&long, Asker::Job).is_ok(), "clef and clef-omni read more text than flash");
    }

    /// Goal: images go to every model, audio and video to clef-omni alone,
    /// each as a data: URL in its kind's field, in the order they came,
    /// and a blob's bytes are the cell's to supply. Method: a blob and an
    /// inline item of each kind, then the wrong model for each.
    #[test]
    fn media_goes_to_the_models_that_take_it() {
        for model in [None, Some("clef"), Some("clef-omni")] {
            let c = bound(&with(model, vec![blob('a', "image/png"), data("image/jpeg", 30)]), Asker::Job).unwrap();
            assert_eq!(c.blobs(), vec!["a".repeat(64)]);
            let input = c.input(vec![b"PNG!".to_vec()]);
            assert_eq!(input["images"], json!([format!("data:image/png;base64,{}", b64(b"PNG!")), format!("data:image/jpeg;base64,{}", b64(&[7; 30]))]));
        }
        let omni = bound(&with(Some("clef-omni"), vec![blob('a', "audio/x-wav"), data("video/webm", 9), blob('b', "VIDEO/MP4; codecs=avc1"), data("audio/mp3", 3)]), Asker::Job).unwrap();
        let input = omni.input(vec![b"RIFF".to_vec(), b"ftyp".to_vec()]);
        assert_eq!(input["audio"], json!([format!("data:audio/wav;base64,{}", b64(b"RIFF")), format!("data:audio/mpeg;base64,{}", b64(&[7; 3]))]), "aliases are sent as Clef's types");
        assert_eq!(input["videos"], json!([format!("data:video/webm;base64,{}", b64(&[7; 9])), format!("data:video/mp4;base64,{}", b64(b"ftyp"))]));
        assert!(input.get("images").is_none());
        for (model, item, kind) in [(None, blob('a', "audio/wav"), Kind::Audio), (Some("clef"), data("video/mp4", 4), Kind::Video), (Some("clef-flash"), data("audio/mpeg", 4), Kind::Audio)] {
            let refused = bound(&with(model, vec![item]), Asker::Job).err();
            let m = Model::named(model.unwrap_or("clef-flash")).unwrap();
            assert_eq!(refused, Some(Refusal::MediaModel(kind, m)));
            assert!(refused.unwrap().message().contains("clef-omni alone"));
        }
        // an attachment as a chat's records carry it passes as it is
        let attachment = json!({ "sha256": "c".repeat(64), "size": 12, "type": "image/webp", "name": "cat.webp" });
        assert!(bound(&with(None, vec![attachment]), Asker::Job).is_ok());
    }

    /// Goal: media outside its bounds is refused, typed, before anything is
    /// reserved or read whole. Method: each bound at and past its edge.
    #[test]
    fn media_limits_hold() {
        let refused = |model: Option<&str>, media: Vec<Value>, asker| bound(&with(model, media), asker).err();
        let omni = Some("clef-omni");
        assert_eq!(refused(None, vec![blob('a', "image/gif")], Asker::Job), Some(Refusal::MediaType));
        assert_eq!(refused(omni, vec![blob('a', "audio/webm")], Asker::Job), Some(Refusal::MediaType), "a browser's voice memo is no type Clef hears");
        assert_eq!(refused(None, vec![data("text/html", 4)], Asker::Job), Some(Refusal::MediaType));
        assert!(refused(None, vec![blob('a', "image/png"); MEDIA_MAX], Asker::Job).is_none());
        assert_eq!(refused(None, vec![blob('a', "image/png"); MEDIA_MAX + 1], Asker::Job), Some(Refusal::MediaCount));
        assert_eq!(refused(omni, vec![blob('a', "video/mp4"); VIDEOS_MAX + 1], Asker::Job), Some(Refusal::MediaCount));
        let mut listless = request();
        listless["media"] = blob('a', "image/png");
        assert_eq!(bound(&listless, Asker::Job).err(), Some(Refusal::MediaItem), "media is a list");
        for item in [
            json!({}),
            json!("a".repeat(64)),
            json!({ "sha256": "a".repeat(64) }),
            json!({ "sha256": "A".repeat(64), "type": "image/png" }),
            json!({ "sha256": "a".repeat(64), "type": "image/png", "data": "data:image/png;base64,QUJD" }),
            json!({ "type": "image/png", "data": "data:image/png;base64,QUJD" }),
            json!({ "url": "https://example.com/cat.png" }),
            json!({ "data": "https://example.com/cat.png" }),
            json!({ "data": "data:image/png,QUJD" }),
            json!({ "data": "data:image/png;base64,QUJ" }),
            json!({ "data": "data:image/png;base64,QU JD" }),
            json!({ "data": "data:image/png;base64,Q===" }),
            json!({ "data": "data:image/png;base64," }),
        ] {
            assert_eq!(refused(None, vec![item.clone()], Asker::Job), Some(Refusal::MediaItem), "{item}");
        }
        assert_eq!(decoded_size("QUJD"), Some(3));
        assert_eq!(decoded_size("QUI="), Some(2));
        assert_eq!(decoded_size("QQ=="), Some(1));
        // a job's inline media is small; an agent's is bounded by its route's body
        let small = (JOB_INLINE_MAX_BYTES - 64) / 4 * 3;
        assert!(refused(None, vec![data("image/png", small)], Asker::Job).is_none());
        assert_eq!(refused(None, vec![data("image/png", small + 3 * 1024)], Asker::Job), Some(Refusal::InlineTooLarge));
        assert!(refused(None, vec![data("image/png", small + 3 * 1024)], Asker::Agent).is_none());
        assert_eq!(refused(None, vec![blob('a', "image/png")], Asker::Agent), Some(Refusal::BlobOnRoute));
        // each kind's bytes, inline at once, a blob's once read
        let image = Kind::Image.max_bytes() as usize;
        assert!(refused(None, vec![data("image/png", image)], Asker::Agent).is_none());
        assert_eq!(refused(None, vec![data("image/png", image + 1)], Asker::Agent), Some(Refusal::ItemTooLarge(Kind::Image)));
        let c = bound(&with(omni, vec![blob('a', "image/png"), blob('b', "audio/wav"), data("audio/wav", 1024)]), Asker::Job).unwrap();
        assert_eq!(c.sized(&[4 * MIB, 4 * MIB - 1024]), Ok(()));
        assert_eq!(c.sized(&[4 * MIB + 1, 1]), Err(Refusal::ItemTooLarge(Kind::Image)));
        assert_eq!(c.sized(&[1, 8 * MIB + 1]), Err(Refusal::ItemTooLarge(Kind::Audio)));
        assert_eq!(c.sized(&[4 * MIB, 4 * MIB - 1023]), Err(Refusal::MediaTooLarge), "one byte past the total");
        assert_eq!(refused(omni, vec![data("audio/wav", 6 * MIB as usize), data("video/mp4", 2 * MIB as usize + 1)], Asker::Agent), Some(Refusal::MediaTooLarge), "inline items alone are checked at once");
    }

    /// Goal: each refusal says the limit it holds, as the consts set it
    /// (its words are written out: no formatting in the cell's Wasm).
    /// Method: each message against its consts.
    #[test]
    fn refusals_say_their_limits() {
        let says = |r: Refusal, parts: &[String]| {
            for p in parts {
                assert!(r.message().contains(p.as_str()), "{r:?}: {:?} lacks {p}", r.message());
            }
        };
        says(Refusal::Model, &MODELS.map(|m| m.name().to_string()));
        says(Refusal::Model, &[format!("{} unless named", DEFAULT_MODEL.name())]);
        says(Refusal::Questions, &[format!("1 to {QUESTIONS_MAX}")]);
        says(Refusal::Name, &[format!("1 to {ID_MAX_BYTES}")]);
        says(Refusal::Options, &[format!("2 to {OPTIONS_MAX}"), format!("at most {ID_MAX_BYTES} bytes")]);
        says(Refusal::Levels, &[format!("2 to {LEVELS_MAX}")]);
        for m in MODELS {
            says(Refusal::TooLarge(m), &[format!("at most {} bytes on", m.request_max_bytes()), m.name().to_string()]);
            for kind in [Kind::Audio, Kind::Video] {
                says(Refusal::MediaModel(kind, m), &[kind.as_str().to_string(), Model::ClefOmni.name().to_string()]);
            }
        }
        says(Refusal::MediaType, &TYPES.map(|(t, _, _)| t.to_string()));
        says(Refusal::MediaCount, &[format!("at most {MEDIA_MAX} media items, {VIDEOS_MAX} of them videos")]);
        for kind in [Kind::Image, Kind::Audio, Kind::Video] {
            says(Refusal::ItemTooLarge(kind), &[format!("{} is at most {} MiB", kind.as_str(), kind.max_bytes() / MIB)]);
        }
        says(Refusal::MediaTooLarge, &[format!("at most {} MiB", MEDIA_TOTAL_MAX_BYTES / MIB)]);
        says(Refusal::InlineTooLarge, &[format!("at most {} KiB", JOB_INLINE_MAX_BYTES / 1024)]);
    }

    /// Goal: a paid answer that does not answer what was asked, in range
    /// and consistent, never leaves the platform. Method: each fault.
    #[test]
    fn malformed_paid_answers_are_never_exposed() {
        let c = bound(&request(), Asker::Job).unwrap();
        let bad = |change: fn(&mut Value), why| {
            let mut r = response();
            change(&mut r);
            assert_eq!(c.answer(&r), Err(why));
        };
        bad(|r| r["answers"] = Value::Null, AnswerFault::Shape);
        bad(|r| { r["answers"].as_object_mut().unwrap().remove("snack"); }, AnswerFault::Questions);
        bad(|r| r["answers"]["extra"] = json!({ "type": "noul", "noul": 0.5 }), AnswerFault::Questions);
        bad(|r| { let a = r["answers"].as_object_mut().unwrap(); let s = a.remove("snack").unwrap(); a.insert("other".into(), s); }, AnswerFault::Questions);
        bad(|r| r["answers"]["snack"]["type"] = json!("choice"), AnswerFault::Type);
        bad(|r| r["answers"]["snack"]["noul"] = json!(1.5), AnswerFault::Probability);
        bad(|r| r["answers"]["aisle"]["choice"] = json!("dairy"), AnswerFault::Choice);
        bad(|r| r["answers"]["aisle"]["choice"] = json!("produce"), AnswerFault::Choice);
        bad(|r| r["answers"]["aisle"]["probabilities"]["snacks"] = json!(0.5), AnswerFault::Probability);
        bad(|r| r["answers"]["aisle"]["probabilities"]["extra"] = json!(0.0), AnswerFault::Probability);
        bad(|r| { r["answers"]["aisle"]["probabilities"] = json!({ "snacks": 0.9, "dairy": 0.1 }); }, AnswerFault::Probability);
        bad(|r| r["answers"]["aisle"]["confidence"] = json!(-0.1), AnswerFault::Probability);
        bad(|r| r["answers"]["urgency"]["score"] = json!(2), AnswerFault::Score);
        bad(|r| r["answers"]["urgency"]["score"] = json!(0.1), AnswerFault::Score);
    }
}
