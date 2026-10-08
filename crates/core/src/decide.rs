//! A job's decision step (docs/api.md, AI): `job.ai.decide` is Clef,
//! Workers AI's decision model, called through the deployment's AI Gateway
//! as text is (cell/src/models.rs), and metered on the payer's ledger in
//! its input tokens (its output is free). It takes a state and typed
//! questions, and answers each with probabilities (its catalog's schemas,
//! `developers.cloudflare.com/workers-ai/models/clef/schema-{input,output}.json`,
//! read 2026-10-07). A step is checked against them before it reserves.
//! An agent asks the same on the decision route (`POST
//! /api/models/v1/decide`), its body a step's input (`route_body`).

use serde_json::Value;

use crate::price::Usage;
use crate::steps::{AiDecide, Clef, Question};

/// The catalog ids, by the size the input names.
pub const CLEF_MODEL: &str = "@cf/cloudflare/clef";
pub const CLEF_FLASH_MODEL: &str = "@cf/cloudflare/clef-flash";
/// Both models' context window: no call's input passes it (a long state is
/// cut to fit), so it bounds a call's worst case.
pub const CONTEXT_TOKENS: u64 = 65_536;
/// The input's bounds, from its schema.
pub const QUESTIONS_MAX: usize = 64;
pub const QUESTION_ID_MAX_BYTES: usize = 100;
pub const OPTIONS_MIN: usize = 2;
pub const OPTIONS_MAX: usize = 255;
pub const LEVELS_MIN: usize = 2;
pub const LEVELS_MAX: usize = 10;
pub const IMAGES_MAX: usize = 4;
/// The image types it reads, as `data:` URLs (it takes no remote URL).
pub const IMAGE_TYPES: [&str; 3] = ["image/png", "image/jpeg", "image/webp"];

/// Why a decision step is refused before it reserves anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// A state that is neither text nor an object or array.
    State,
    /// No questions, or more than `QUESTIONS_MAX`.
    Questions,
    /// An id that is not 1 to `QUESTION_ID_MAX_BYTES` of letters, digits, `_`, `.`, `-`.
    QuestionId,
    /// Instructions that are not text nor an object or array, or are empty.
    Instructions,
    /// A choice outside `OPTIONS_MIN` to `OPTIONS_MAX` options, or one with an empty id.
    Options,
    /// A score outside `LEVELS_MIN` to `LEVELS_MAX` levels.
    Levels,
    /// More than `IMAGES_MAX` images, or one that is no `data:` URL of `IMAGE_TYPES`.
    Images,
}

impl Refusal {
    /// The refusal for people (and the job, which may catch it).
    pub fn message(self) -> String {
        match self {
            Refusal::State => "ai.decide's state is text, or JSON (an object or an array)".into(),
            Refusal::Questions => format!("ai.decide asks 1 to {QUESTIONS_MAX} questions"),
            Refusal::QuestionId => format!("a question's id is 1 to {QUESTION_ID_MAX_BYTES} letters, digits, '_', '.' or '-'"),
            Refusal::Instructions => "a question's instructions are text, or JSON holding it, and not empty".into(),
            Refusal::Options => format!("a choice has {OPTIONS_MIN} to {OPTIONS_MAX} options, each with an id"),
            Refusal::Levels => format!("a score has {LEVELS_MIN} to {LEVELS_MAX} levels"),
            Refusal::Images => format!("ai.decide takes at most {IMAGES_MAX} images, each a data: URL of a PNG, a JPEG or a WebP"),
        }
    }
}

/// The size the decision route calls when its body names none: the
/// cheaper, for an agent's many small decisions.
pub const ROUTE_DEFAULT_MODEL: Clef = Clef::Flash;

/// Why the decision route refuses a body before `decide_call` reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BodyRefusal {
    /// Not a JSON object.
    NotAnObject,
    /// A `model` that is neither `clef` nor `clef-flash`.
    Model,
    /// A field missing, unknown, or of the wrong type: serde's words.
    Shape(String),
}

impl BodyRefusal {
    pub fn message(&self) -> String {
        match self {
            BodyRefusal::NotAnObject => "a decision is a JSON object: {model?, state, questions, images?}".into(),
            BodyRefusal::Model => "a decision's model is clef or clef-flash (the default)".into(),
            BodyRefusal::Shape(why) => format!("a decision: {why}"),
        }
    }
}

/// The decision route's body, as a step: a job's `ai.decide` input, its
/// `model` `ROUTE_DEFAULT_MODEL` unless it names one. `decide_call`
/// checks it next, as it checks a step.
pub fn route_body(body: Value) -> Result<AiDecide, BodyRefusal> {
    let Value::Object(mut fields) = body else { return Err(BodyRefusal::NotAnObject) };
    match fields.get("model") {
        None | Some(Value::Null) => {
            fields.insert("model".into(), serde_json::to_value(ROUTE_DEFAULT_MODEL).expect("a size serializes"));
        }
        Some(m) if serde_json::from_value::<Clef>(m.clone()).is_ok() => {}
        Some(_) => return Err(BodyRefusal::Model),
    }
    serde_json::from_value(Value::Object(fields)).map_err(|e| BodyRefusal::Shape(e.to_string()))
}

/// Why Clef's answer is none the platform passes on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnswerFault {
    /// Not JSON with an `answers` object holding every question's id.
    NoAnswers,
}

impl AnswerFault {
    pub fn message(self) -> String {
        match self {
            AnswerFault::NoAnswers => "the model's answer does not answer every question".into(),
        }
    }
}

/// A decision step, checked: the catalog model and its input.
#[derive(Debug, Clone, PartialEq)]
pub struct DecideCall {
    pub model: &'static str,
    pub input: Value,
}

/// The catalog id of a size.
pub fn model_of(clef: Clef) -> &'static str {
    match clef {
        Clef::Clef => CLEF_MODEL,
        Clef::Flash => CLEF_FLASH_MODEL,
    }
}

fn text_or_json(v: &Value) -> bool {
    match v {
        Value::String(s) => !s.trim().is_empty(),
        Value::Object(o) => !o.is_empty(),
        Value::Array(a) => !a.is_empty(),
        _ => false,
    }
}

fn valid_id(id: &str) -> bool {
    (1..=QUESTION_ID_MAX_BYTES).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
}

fn valid_image(url: &str) -> bool {
    let Some((head, data)) = url.split_once(',') else { return false };
    let Some(kind) = head.strip_prefix("data:").and_then(|h| h.strip_suffix(";base64")) else { return false };
    IMAGE_TYPES.contains(&kind.to_ascii_lowercase().as_str()) && !data.is_empty()
}

/// The call a decision step makes, or why it is refused. Its size is the
/// model route's to bound (`MODEL_BODY_MAX_BYTES`), as any call's.
pub fn decide_call(step: &AiDecide) -> Result<DecideCall, Refusal> {
    if !matches!(step.state, Value::String(_) | Value::Object(_) | Value::Array(_)) {
        return Err(Refusal::State);
    }
    if !(1..=QUESTIONS_MAX).contains(&step.questions.len()) {
        return Err(Refusal::Questions);
    }
    for (id, q) in &step.questions {
        if !valid_id(id) {
            return Err(Refusal::QuestionId);
        }
        let instructions = match q {
            Question::Noul { instructions, .. } | Question::Choice { instructions, .. } | Question::Score { instructions, .. } => instructions,
        };
        if !text_or_json(instructions) {
            return Err(Refusal::Instructions);
        }
        match q {
            Question::Noul { .. } => {}
            Question::Choice { criteria, .. } => {
                if !(OPTIONS_MIN..=OPTIONS_MAX).contains(&criteria.len()) || criteria.keys().any(String::is_empty) {
                    return Err(Refusal::Options);
                }
            }
            Question::Score { criteria, .. } => {
                if !(LEVELS_MIN..=LEVELS_MAX).contains(&criteria.len()) {
                    return Err(Refusal::Levels);
                }
            }
        }
    }
    let images = step.images.as_deref().unwrap_or_default();
    if images.len() > IMAGES_MAX || !images.iter().all(|u| valid_image(u)) {
        return Err(Refusal::Images);
    }
    let input = serde_json::to_value(step).expect("a decision step serializes");
    Ok(DecideCall { model: model_of(step.model), input })
}

impl DecideCall {
    /// What the call reserves: every byte of its input a token, up to the
    /// context window; its output is free.
    pub fn worst(&self, body_bytes: usize) -> Usage {
        Usage::Tokens { model: self.model.to_string(), input: (body_bytes as u64).min(CONTEXT_TOKENS), cached_input: 0, cache_write: 0, output: 0 }
    }

    /// What the call cost, from its answer's `usage` (raw, or inside
    /// `result`; `None` when it does not read: the ledger charges the
    /// reservation).
    pub fn usage(&self, answer: &Value) -> Option<Usage> {
        let u = answer.get("usage").or_else(|| answer["result"].get("usage"))?;
        Some(Usage::Tokens { model: self.model.to_string(), input: u["input_tokens"].as_u64()?, cached_input: 0, cache_write: 0, output: u["output_tokens"].as_u64()? })
    }
}

/// The answers in Clef's answer, one per question asked: its catalog's
/// shape, as the binding answers it raw, or inside `result`, as Workers
/// AI's REST API wraps it.
pub fn answers_of(step: &AiDecide, answer: &Value) -> Result<Value, AnswerFault> {
    let answers = answer.get("answers").or_else(|| answer["result"].get("answers")).and_then(Value::as_object).ok_or(AnswerFault::NoAnswers)?;
    if !step.questions.keys().all(|id| answers.get(id).is_some_and(Value::is_object)) {
        return Err(AnswerFault::NoAnswers);
    }
    Ok(Value::Object(answers.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::price::PriceBook;
    use crate::steps::Step;
    use serde_json::json;

    fn decide(args: Value) -> AiDecide {
        match Step::from_parts("ai.decide", args) {
            Ok(Step::AiDecide(d)) => d,
            other => panic!("{other:?}"),
        }
    }

    fn noul(instructions: &str) -> Value {
        json!({ "type": "noul", "instructions": instructions })
    }

    /// Goal: a step's input is the catalog's, its model the size it named.
    /// Method: a step of each question type, through to the input.
    #[test]
    fn a_decision_is_clefs_input() {
        let args = json!({
            "model": "clef-flash", "state": "Checkout fails for everyone.",
            "questions": {
                "urgent": noul("Is this urgent?"),
                "team": { "type": "choice", "instructions": "Which team?", "criteria": { "billing": "Payments", "technical": null } },
                "severity": { "type": "score", "instructions": "How severe?", "criteria": ["None", "Minor", "Major"] },
            },
        });
        let call = decide_call(&decide(args.clone())).unwrap();
        assert_eq!((call.model, call.input.clone()), (CLEF_FLASH_MODEL, args), "the input is what the job asked, its model the size");
        let big = decide_call(&decide(json!({ "model": "clef", "state": { "rows": [1] }, "questions": { "q": noul("?") } }))).unwrap();
        assert_eq!(big.model, CLEF_MODEL);
    }

    /// Goal: the decision route's body is a step's input, `clef-flash`
    /// unless it names a size, and a body that is not one is refused,
    /// typed, before anything reads it further. Method: a body naming no
    /// model, a null one and each size, then an unknown model, a body that
    /// is no object, a field missing and one unknown, through to the call.
    #[test]
    fn the_routes_body_is_a_steps_input() {
        let ask = json!({ "state": "The invoice is overdue.", "questions": { "late": noul("Is the invoice late?") } });
        let step = route_body(ask.clone()).unwrap();
        let mut asked = ask.clone();
        asked["model"] = json!("clef-flash");
        assert_eq!(step, decide(asked.clone()), "no model: the flash size");
        assert_eq!(decide_call(&step).unwrap(), DecideCall { model: CLEF_FLASH_MODEL, input: asked }, "the call a step with the same input makes");
        let mut nulled = ask.clone();
        nulled["model"] = Value::Null;
        assert_eq!(route_body(nulled).unwrap().model, Clef::Flash);
        for (named, size) in [("clef", Clef::Clef), ("clef-flash", Clef::Flash)] {
            let mut sized = ask.clone();
            sized["model"] = json!(named);
            assert_eq!(route_body(sized).unwrap().model, size);
        }
        for model in [json!("@cf/cloudflare/clef"), json!("cheap"), json!(1), json!(["clef"])] {
            let mut other = ask.clone();
            other["model"] = model.clone();
            assert_eq!(route_body(other), Err(BodyRefusal::Model), "{model}");
        }
        assert_eq!(route_body(json!("decide")), Err(BodyRefusal::NotAnObject));
        assert_eq!(route_body(json!([ask])), Err(BodyRefusal::NotAnObject));
        let shape = |body: Value| match route_body(body) {
            Err(BodyRefusal::Shape(why)) => why,
            other => panic!("{other:?}"),
        };
        assert!(shape(json!({ "state": "s" })).contains("questions"), "no questions");
        assert!(shape(json!({ "state": "s", "questions": { "q": noul("?") }, "stream": true })).contains("stream"), "a field Clef does not take");
        assert!(shape(json!({ "state": "s", "questions": { "q": { "type": "rank", "instructions": "?" } } })).contains("rank"));
        let empty = route_body(json!({ "state": "s", "questions": {} })).unwrap();
        assert_eq!(decide_call(&empty), Err(Refusal::Questions), "the step's bounds hold after");
        assert!(BodyRefusal::Model.message().contains("clef-flash"));
    }

    /// Goal: a step outside the catalog's bounds is refused, typed, before
    /// it reserves. Method: each bound at and past its edge.
    #[test]
    fn a_decision_out_of_bounds_is_refused() {
        let base = |questions: Value| json!({ "model": "clef", "state": "s", "questions": questions });
        let check = |args: Value| decide_call(&decide(args));
        assert_eq!(check(json!({ "model": "clef", "state": 7, "questions": { "q": noul("?") } })), Err(Refusal::State));
        assert_eq!(check(json!({ "model": "clef", "state": null, "questions": { "q": noul("?") } })), Err(Refusal::State));
        assert_eq!(check(base(json!({}))), Err(Refusal::Questions));
        let many = |n: usize| Value::Object((0..n).map(|i| (format!("q{i}"), noul("?"))).collect());
        assert!(check(base(many(QUESTIONS_MAX))).is_ok());
        assert_eq!(check(base(many(QUESTIONS_MAX + 1))), Err(Refusal::Questions));
        assert_eq!(check(base(json!({ "a b": noul("?") }))), Err(Refusal::QuestionId));
        assert_eq!(check(base(json!({ "x".repeat(QUESTION_ID_MAX_BYTES + 1): noul("?") }))), Err(Refusal::QuestionId));
        assert!(check(base(json!({ "topic.t_1-a": noul("?") }))).is_ok());
        assert_eq!(check(base(json!({ "q": noul(" ") }))), Err(Refusal::Instructions));
        assert_eq!(check(base(json!({ "q": { "type": "noul", "instructions": 3 } }))), Err(Refusal::Instructions));
        assert!(check(base(json!({ "q": { "type": "noul", "instructions": { "question": "?", "data": [1] } } }))).is_ok());
        let choice = |n: usize| json!({ "q": { "type": "choice", "instructions": "?", "criteria": Value::Object((0..n).map(|i| (format!("o{i}"), Value::Null)).collect()) } });
        assert_eq!(check(base(choice(1))), Err(Refusal::Options));
        assert!(check(base(choice(OPTIONS_MAX))).is_ok());
        assert_eq!(check(base(choice(OPTIONS_MAX + 1))), Err(Refusal::Options));
        assert_eq!(check(base(json!({ "q": { "type": "choice", "instructions": "?", "criteria": { "": "x", "b": "y" } } }))), Err(Refusal::Options));
        let score = |n: usize| json!({ "q": { "type": "score", "instructions": "?", "criteria": vec!["level"; n] } });
        assert_eq!(check(base(score(1))), Err(Refusal::Levels));
        assert!(check(base(score(LEVELS_MAX))).is_ok());
        assert_eq!(check(base(score(LEVELS_MAX + 1))), Err(Refusal::Levels));
        let images = |urls: Vec<&str>| json!({ "model": "clef", "state": "s", "questions": { "q": noul("?") }, "images": urls });
        assert!(check(images(vec!["data:image/webp;base64,UklGR"; IMAGES_MAX])).is_ok());
        assert_eq!(check(images(vec!["data:image/png;base64,iVBO"; IMAGES_MAX + 1])), Err(Refusal::Images));
        assert_eq!(check(images(vec!["https://example.com/a.png"])), Err(Refusal::Images), "no remote URL");
        assert_eq!(check(images(vec!["data:image/gif;base64,R0lG"])), Err(Refusal::Images));
        assert_eq!(check(images(vec!["data:image/png;base64,"])), Err(Refusal::Images));
        assert!(Refusal::Questions.message().contains("1 to 64"));
    }

    /// Goal: a decision costs its input tokens at Clef's price, its output
    /// free, and its worst case bounds it. Method: the book's rows, a
    /// reported usage, and the worst case of a large input.
    #[test]
    fn a_decision_costs_its_input_tokens() {
        let call = decide_call(&decide(json!({ "model": "clef-flash", "state": "s", "questions": { "q": noul("?") } }))).unwrap();
        let used = call.usage(&json!({ "model": "clef-flash", "answers": {}, "usage": { "input_tokens": 1_000_000, "output_tokens": 40 } })).unwrap();
        let book = PriceBook::defaults();
        assert_eq!(book.price(&used).unwrap().list, 90_000, "$0.09 per million input tokens, the output free");
        let clef = DecideCall { model: CLEF_MODEL, input: Value::Null };
        assert_eq!(book.price(&clef.usage(&json!({ "usage": { "input_tokens": 1_000_000, "output_tokens": 9 } })).unwrap()).unwrap().list, 240_000, "$0.24 per million");
        assert_eq!(call.usage(&json!({ "usage": { "input_tokens": 3 } })), None, "a usage that does not read: the reservation");
        assert_eq!(call.worst(500), Usage::Tokens { model: CLEF_FLASH_MODEL.into(), input: 500, cached_input: 0, cache_write: 0, output: 0 });
        assert_eq!(call.worst(6 * 1024 * 1024), Usage::Tokens { model: CLEF_FLASH_MODEL.into(), input: CONTEXT_TOKENS, cached_input: 0, cache_write: 0, output: 0 }, "no input passes the window");
        assert!(book.price(&call.worst(6 * 1024 * 1024)).unwrap().charge >= book.price(&used).unwrap().charge / 1_000_000 * CONTEXT_TOKENS as i64);
    }

    /// Goal: the answers pass on when every question has one. Method: the
    /// binding's raw answer, the REST API's wrapped one, and answers missing.
    #[test]
    fn the_answers_read_from_clefs_answer() {
        let step = decide(json!({ "model": "clef", "state": "s", "questions": { "a": noul("?"), "b": noul("?") } }));
        let answers = json!({ "a": { "type": "noul", "noul": 0.9 }, "b": { "type": "noul", "noul": 0.1 } });
        assert_eq!(answers_of(&step, &json!({ "model": "clef", "answers": answers, "usage": {} })), Ok(answers.clone()));
        assert_eq!(answers_of(&step, &json!({ "result": { "answers": answers }, "success": true })), Ok(answers));
        assert_eq!(answers_of(&step, &json!({ "answers": { "a": { "type": "noul", "noul": 0.9 } } })), Err(AnswerFault::NoAnswers));
        assert_eq!(answers_of(&step, &json!({ "response": "yes" })), Err(AnswerFault::NoAnswers));
    }
}
