//! The model route's transcriptions (decision 9: a voice memo is one the
//! agent transcribes itself; docs/api.md, Models): OpenAI's `POST
//! /v1/audio/transcriptions` shape, a `multipart/form-data` body
//! (crate::multipart) with the audio as `file` and `model` the route's
//! name `whisper`, run on Workers AI's Whisper through the deployment's AI
//! Gateway as a chat call is (cell/src/models.rs), and metered in the
//! neurons Workers AI prices it at, per minute of audio
//! (`price::Usage::Neurons`). Pure: the cell reads the body, makes the call
//! and meters it.
//!
//! A call reserves its worst case first, the audio's bytes read as
//! `WORST_BYTES_PER_SECOND` (`worst`), and settles at the length Whisper
//! reports (`usage_of`), above the reservation too (docs/ledger.md). What
//! reaches the model is the audio and, when the client sent them, its
//! language and prompt (`bound`); the client's answer is OpenAI's shape,
//! `{"text": …}` or the text alone (`answer`).

use base64::Engine;
use serde_json::{json, Value};

use crate::multipart::Part;
use crate::price::Usage;

/// The route's name for transcription: no tier, as `vision` is none.
pub const WHISPER: &str = "whisper";
/// The model, by its Workers AI catalog id
/// (developers.cloudflare.com/workers-ai/models/whisper-large-v3-turbo/,
/// read 2026-10-07): `{audio: <base64>, task, language?, initial_prompt?}`
/// in, `{text, transcription_info: {duration, …}, …}` out.
pub const TRANSCRIBE_MODEL: &str = "@cf/openai/whisper-large-v3-turbo";
/// Its price (Workers AI's pricing page, read 2026-10-07): 46.63 neurons
/// per audio minute, which at $0.011 per thousand neurons
/// (`price::DEFAULT_NEURONS`) is its listed $0.0005 a minute. Here in
/// thousandths of a neuron, the ledger's unit, as FLUX's tiles and steps
/// are (crate::media): the book prices neurons, so no row of its own.
pub const MILLI_NEURONS_PER_MINUTE: u64 = 46_630;
/// The largest audio a call takes (Paul, 2026-10-07): an hour of a voice
/// note at 24 kbps, five minutes of 16 kHz WAV. Its base64 (about 13.3
/// MiB) and the answer stay well within a Worker's memory.
pub const AUDIO_MAX_BYTES: usize = 10 * 1024 * 1024;
/// The whole form: the audio and its other fields.
pub const BODY_MAX_BYTES: usize = AUDIO_MAX_BYTES + 64 * 1024;
/// The reservation reads the audio as this many bytes a second (16 kbps,
/// below any voice note's), so 10 MiB holds about 87 minutes. Audio
/// encoded tighter settles above it, charged in full.
pub const WORST_BYTES_PER_SECOND: u64 = 2_000;
/// A prompt (Whisper's `initial_prompt`), at most this many characters:
/// OpenAI reads 224 tokens of one.
pub const PROMPT_MAX_CHARS: usize = 1_024;
/// An answer's audio is at most a day long: a duration past it is not
/// Whisper's, and settles at the reservation.
pub const DURATION_MAX_SECONDS: f64 = 86_400.0;

/// Why a transcription is refused before anything is reserved or sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// `model` is not `whisper`.
    Model,
    /// No `file`, or an empty one.
    NoAudio,
    /// The audio is over `AUDIO_MAX_BYTES` (413).
    TooLarge(usize),
    /// A `response_format` other than `json` or `text`.
    Format,
    /// A `language` that is no ISO-639 code.
    Language,
    /// A `prompt` over `PROMPT_MAX_CHARS`, or not UTF-8.
    Prompt,
    /// A field that is not UTF-8 text.
    Field(String),
}

impl Refusal {
    pub fn message(&self) -> String {
        match self {
            Refusal::Model => format!("a transcription's model is {WHISPER:?}"),
            Refusal::NoAudio => "a transcription carries its audio as `file`".into(),
            Refusal::TooLarge(n) => format!("the audio is {n} bytes; a transcription takes at most {AUDIO_MAX_BYTES}"),
            Refusal::Format => "response_format is json or text".into(),
            Refusal::Language => "language is an ISO-639-1 code, as en".into(),
            Refusal::Prompt => format!("prompt is at most {PROMPT_MAX_CHARS} characters"),
            Refusal::Field(name) => format!("{name} is text"),
        }
    }
}

/// How the client reads its answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// `{"text": …}`, OpenAI's default.
    Json,
    /// The text alone, `text/plain`.
    Text,
}

/// A transcription bounded: the model's input, the audio's size (what
/// the reservation reads), and how the client reads the answer.
#[derive(Debug, Clone, PartialEq)]
pub struct Bounded {
    pub input: Value,
    pub audio_bytes: usize,
    pub format: Format,
}

fn text<'a>(p: &'a Part<'_>) -> Result<&'a str, Refusal> {
    std::str::from_utf8(p.data).map_err(|_| Refusal::Field(p.name.clone()))
}

/// A form's fields, bounded: its audio as Whisper's input, its language
/// and prompt when it named them, nothing else (OpenAI's `temperature`
/// and `timestamp_granularities` have no Whisper input, and are let go).
pub fn bound(parts: &[Part<'_>]) -> Result<Bounded, Refusal> {
    let field = |name: &str| parts.iter().find(|p| p.name == name);
    match field("model").map(text).transpose()? {
        Some(m) if m.trim() == WHISPER => {}
        _ => return Err(Refusal::Model),
    }
    let audio = field("file").map(|p| p.data).filter(|d| !d.is_empty()).ok_or(Refusal::NoAudio)?;
    if audio.len() > AUDIO_MAX_BYTES {
        return Err(Refusal::TooLarge(audio.len()));
    }
    let format = match field("response_format").map(text).transpose()?.map(str::trim) {
        None | Some("") | Some("json") => Format::Json,
        Some("text") => Format::Text,
        Some(_) => return Err(Refusal::Format),
    };
    let mut input = json!({ "audio": base64::engine::general_purpose::STANDARD.encode(audio), "task": "transcribe" });
    if let Some(l) = field("language").map(text).transpose()?.map(str::trim).filter(|l| !l.is_empty()) {
        if !(2..=3).contains(&l.len()) || !l.bytes().all(|b| b.is_ascii_lowercase()) {
            return Err(Refusal::Language);
        }
        input["language"] = json!(l);
    }
    if let Some(p) = field("prompt").map(text).transpose().map_err(|_| Refusal::Prompt)?.filter(|p| !p.trim().is_empty()) {
        if p.chars().count() > PROMPT_MAX_CHARS {
            return Err(Refusal::Prompt);
        }
        input["initial_prompt"] = json!(p);
    }
    Ok(Bounded { input, audio_bytes: audio.len(), format })
}

/// `seconds` of audio in the ledger's unit, a part of a thousandth of a
/// neuron counted whole.
fn neurons(millis: u64) -> Usage {
    // at most a day of audio: far under QUANTITY_MAX
    Usage::Neurons { milli: (millis * MILLI_NEURONS_PER_MINUTE).div_ceil(60_000) }
}

impl Bounded {
    /// What the call reserves: its audio's bytes as seconds at
    /// `WORST_BYTES_PER_SECOND`, at least one.
    pub fn worst(&self) -> Usage {
        let seconds = (self.audio_bytes as u64).div_ceil(WORST_BYTES_PER_SECOND).max(1);
        neurons(seconds * 1_000)
    }
}

// Whisper's answer, as the AI binding gives it (the hosted lane, on
// e2e.finite.place, 2026-10-08): its catalog's output schema, unwrapped
// (`text`, `transcription_info`, `segments`, `vtt`, `word_count`), and a
// `usage` the schema does not name. Read as it is: an answer in any other
// shape (inside `result`, as Workers AI's REST API wraps one) has no text
// and no length, so it is answered 502 and settled at its reservation.

/// What a call cost, from Whisper's answer: its audio's length
/// (`transcription_info.duration`, seconds, the catalog's own field;
/// measured hosted, 2 s of audio settled at 1,555 thousandths of a neuron).
/// Its undocumented `usage` is not read: what it holds is shown by
/// `shape` until it is known. An answer that does not say, or says what no
/// audio is, is `None`, which the ledger settles at the reservation: the
/// money path fails closed.
pub fn usage_of(answer: &Value) -> Option<Usage> {
    let seconds = answer["transcription_info"]["duration"].as_f64()?;
    if !seconds.is_finite() || !(0.0..=DURATION_MAX_SECONDS).contains(&seconds) {
        return None;
    }
    Some(neurons((seconds * 1_000.0).ceil() as u64))
}

/// What a transcription's answer says of Whisper's (the route's
/// `x-fragment-answer-shape`), never its words: its top-level keys, and its
/// `usage` when that is a small object of numbers, booleans and short
/// strings (else `null`), so a hosted run shows what Workers AI meters it
/// by.
pub fn shape(whisper: &Value) -> Value {
    let keys: Vec<&str> = whisper.as_object().map(|o| o.keys().map(String::as_str).collect()).unwrap_or_default();
    let scalar = |v: &Value| v.is_number() || v.is_boolean() || v.as_str().is_some_and(|s| s.len() <= 32);
    let usage = match whisper.get("usage") {
        Some(Value::Object(u)) if u.len() <= 8 && u.values().all(scalar) => Value::Object(u.clone()),
        _ => Value::Null,
    };
    json!({ "keys": keys, "usage": usage })
}

/// The client's answer from Whisper's: its content type and body, or
/// `None` when the answer carries no text.
pub fn answer(format: Format, whisper: &Value) -> Option<(&'static str, Vec<u8>)> {
    let text = whisper["text"].as_str()?.trim();
    Some(match format {
        Format::Json => ("application/json", json!({ "text": text }).to_string().into_bytes()),
        Format::Text => ("text/plain; charset=utf-8", text.as_bytes().to_vec()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::price::PriceBook;

    fn part<'a>(name: &str, data: &'a [u8]) -> Part<'a> {
        Part { name: name.into(), filename: (name == "file").then(|| "memo.ogg".into()), content_type: None, data }
    }

    /// Valid: an SDK's form is Whisper's input, the audio as base64 and the
    /// language and prompt it named; json unless it asked for text.
    #[test]
    fn a_form_is_whispers_input() {
        let audio = b"OggS\x00voice".to_vec();
        let b = bound(&[part("model", b"whisper"), part("file", &audio), part("response_format", b"json")]).unwrap();
        assert_eq!(b.input, json!({ "audio": base64::engine::general_purpose::STANDARD.encode(&audio), "task": "transcribe" }), "no language: Whisper detects it");
        assert_eq!((b.audio_bytes, b.format), (audio.len(), Format::Json));
        let b = bound(&[part("model", b" whisper "), part("file", &audio), part("language", b"de"), part("prompt", b"Paul, fragment"), part("response_format", b"text"), part("temperature", b"0")]).unwrap();
        assert_eq!((b.input["language"].clone(), b.input["initial_prompt"].clone(), b.format), (json!("de"), json!("Paul, fragment"), Format::Text));
        assert!(b.input.get("temperature").is_none(), "what Whisper has no input for is let go");
        let b = bound(&[part("model", b"whisper"), part("file", &audio), part("language", b""), part("prompt", b"  ")]).unwrap();
        assert!(b.input.get("language").is_none() && b.input.get("initial_prompt").is_none(), "empty is none");
    }

    /// Invalid: what is no transcription the route takes is refused before
    /// anything is reserved, saying why.
    #[test]
    fn a_form_out_of_shape_is_refused() {
        let a = b"audio".as_slice();
        assert_eq!(bound(&[part("file", a)]), Err(Refusal::Model));
        assert_eq!(bound(&[part("model", b"whisper-1"), part("file", a)]), Err(Refusal::Model), "OpenAI's own model is no name of ours");
        assert_eq!(bound(&[part("model", b"medium"), part("file", a)]), Err(Refusal::Model), "a tier transcribes nothing");
        assert_eq!(bound(&[part("model", b"whisper")]), Err(Refusal::NoAudio));
        assert_eq!(bound(&[part("model", b"whisper"), part("file", b"")]), Err(Refusal::NoAudio));
        let big = vec![0u8; AUDIO_MAX_BYTES + 1];
        assert_eq!(bound(&[part("model", b"whisper"), part("file", &big)]), Err(Refusal::TooLarge(AUDIO_MAX_BYTES + 1)));
        assert!(bound(&[part("model", b"whisper"), part("file", &big[..AUDIO_MAX_BYTES])]).is_ok(), "the bound itself is taken");
        for f in ["srt", "vtt", "verbose_json", "JSON"] {
            assert_eq!(bound(&[part("model", b"whisper"), part("file", a), part("response_format", f.as_bytes())]), Err(Refusal::Format), "{f}");
        }
        for l in ["english", "EN", "e", "en-US"] {
            assert_eq!(bound(&[part("model", b"whisper"), part("file", a), part("language", l.as_bytes())]), Err(Refusal::Language), "{l}");
        }
        let long = "p".repeat(PROMPT_MAX_CHARS + 1);
        assert_eq!(bound(&[part("model", b"whisper"), part("file", a), part("prompt", long.as_bytes())]), Err(Refusal::Prompt));
        assert_eq!(bound(&[part("model", b"\xff"), part("file", a)]), Err(Refusal::Field("model".into())));
        assert!(Refusal::TooLarge(11).message().contains("at most 10485760"));
    }

    /// Goal: a call reserves its audio's bytes at 16 kbps and settles at
    /// the length Whisper heard, at Workers AI's 46.63 neurons a minute, so
    /// a minute is its listed $0.0005 (before the fee and margin). Method:
    /// the book's price of a minute, a reservation, and answers that do and
    /// do not say how long.
    #[test]
    fn a_minute_is_workers_ais_price() {
        let book = PriceBook::defaults();
        let minute = usage_of(&json!({ "text": "hi", "transcription_info": { "duration": 60.0 } })).unwrap();
        assert_eq!(minute, Usage::Neurons { milli: 46_630 });
        let priced = book.price(&minute).unwrap();
        assert_eq!(priced.list, 513, "46.63 neurons at $0.011 a thousand: $0.000513 (512.93 µ$, rounded up)");
        assert_eq!(usage_of(&json!({ "transcription_info": { "duration": 1.5 } })), Some(Usage::Neurons { milli: 1_166 }), "a part counted whole");
        assert_eq!(usage_of(&json!({ "transcription_info": { "duration": 0.0 } })), Some(Usage::Neurons { milli: 0 }));
        for bad in [json!({}), json!({ "transcription_info": { "duration": "60" } }), json!({ "transcription_info": { "duration": -1.0 } }), json!({ "transcription_info": { "duration": 90_000.0 } })] {
            assert_eq!(usage_of(&bad), None, "{bad}");
        }
        let b = Bounded { input: json!({}), audio_bytes: AUDIO_MAX_BYTES, format: Format::Json };
        // 10 MiB at 2,000 bytes a second: 5,243 s, about 87 minutes
        assert_eq!(b.worst(), Usage::Neurons { milli: (5_243u64 * 1_000 * 46_630).div_ceil(60_000) });
        let worst = book.price(&b.worst()).unwrap().charge;
        assert!((65_000..75_000).contains(&worst), "about $0.07 held for the largest, the fee and margin in: {worst}");
        assert_eq!(Bounded { audio_bytes: 1, ..b }.worst(), Usage::Neurons { milli: 778 }, "at least a second");
    }

    /// The client's answer: OpenAI's json, or the text alone; an answer of
    /// Whisper's with no text is none.
    #[test]
    fn the_answer_is_openais_shape() {
        let w = json!({ "text": " hello there ", "word_count": 2, "transcription_info": { "duration": 1.2 }, "vtt": "WEBVTT" });
        assert_eq!(answer(Format::Json, &w), Some(("application/json", br#"{"text":"hello there"}"#.to_vec())));
        assert_eq!(answer(Format::Text, &w), Some(("text/plain; charset=utf-8", b"hello there".to_vec())));
        assert_eq!(answer(Format::Json, &json!({ "transcription_info": {} })), None);
        // an answer in another shape (the REST API's wrapping) has no text
        // and no length: answered 502, settled at its reservation
        let wrapped = json!({ "result": w, "success": true });
        assert_eq!((answer(Format::Text, &wrapped), usage_of(&wrapped)), (None, None));
    }

    /// The shape a transcription's answer names: the keys, and a `usage` of
    /// scalars as it came, never the words or anything that could carry
    /// them. Method: the hosted answer's keys, with a usage shown and ones
    /// not.
    #[test]
    fn the_shape_shows_keys_and_a_scalar_usage() {
        let hosted = json!({ "text": "Thank you.", "transcription_info": { "duration": 2.0 }, "segments": [], "vtt": "WEBVTT", "word_count": 2, "usage": { "type": "duration", "seconds": 2 } });
        assert_eq!(shape(&hosted), json!({ "keys": ["segments", "text", "transcription_info", "usage", "vtt", "word_count"], "usage": { "type": "duration", "seconds": 2 } }));
        assert!(!shape(&hosted).to_string().contains("Thank you"), "never its words");
        for not_shown in [json!({ "usage": { "said": "x".repeat(33) } }), json!({ "usage": { "nested": { "a": 1 } } }), json!({ "usage": [1, 2] }), json!({})] {
            assert_eq!(shape(&not_shown)["usage"], Value::Null, "{not_shown}");
        }
    }
}
