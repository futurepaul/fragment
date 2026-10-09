//! Typed decisions for jobs. Fragment's names are independent of the
//! provider: Clef-flash's System One wire uses `state`, `noul` and `criteria`.
//! Sources (2026-10-09): developers.cloudflare.com/workers-ai/models/clef-flash/
//! and docs.typesafe.ai/api. Input is text or JSON, no media in this contract.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::price::Usage;

pub const MODEL: &str = "@cf/cloudflare/clef-flash";
pub const QUESTIONS_MAX: usize = 64;
/// Under Clef-flash's 24,576-token context, including its prompt framing.
/// Bounding serialized bytes bounds text tokens conservatively, also for JSON.
pub const INPUT_MAX_BYTES: usize = 16 * 1024;
pub const OPTIONS_MAX: usize = 255;
pub const LEVELS_MAX: usize = 10;
/// Reserve the full model window per question. The provider may evaluate
/// questions separately; neither prompt framing nor token reuse is assumed.
pub const CONTEXT_TOKENS: u64 = 24_576;
const _: () = assert!((INPUT_MAX_BYTES as u64) < CONTEXT_TOKENS, "decision input leaves room for the model's prompt framing");

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Decide {
    pub input: Value,
    pub questions: BTreeMap<String, Question>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase", deny_unknown_fields)]
pub enum Question {
    Choice { instructions: String, options: BTreeMap<String, String> },
    Predicate { instructions: String },
    /// Ordered descriptions; the answer is a weighted, zero-based index.
    Score { instructions: String, levels: Vec<String> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    Input,
    Questions,
    Name,
    Instructions,
    Options,
    Levels,
    TooLarge,
}

impl Refusal {
    pub fn message(self) -> String {
        match self {
            Self::Input => "ai.decide's input is nonempty text, an object or an array".into(),
            Self::Questions => format!("ai.decide takes 1 to {QUESTIONS_MAX} questions"),
            Self::Name => "ai.decide's question ids are 1 to 100 ASCII letters, digits, '_', '.' or '-'".into(),
            Self::Instructions => "ai.decide's instructions are nonempty".into(),
            Self::Options => format!("ai.decide's choice has 2 to {OPTIONS_MAX} nonempty option ids, at most 100 bytes each, with nonempty descriptions"),
            Self::Levels => format!("ai.decide's score has 2 to {LEVELS_MAX} nonempty level descriptions"),
            Self::TooLarge => format!("ai.decide's encoded model request is at most {INPUT_MAX_BYTES} bytes"),
        }
    }
}

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
    pub fn message(self) -> String {
        format!("ai.decide's model returned an invalid {}", match self {
            Self::Shape => "JSON answer",
            Self::Questions => "question set",
            Self::Type => "answer type",
            Self::Probability => "probability distribution",
            Self::Choice => "choice",
            Self::Score => "score",
        })
    }
}

pub struct DecisionCall {
    pub input: Value,
    questions: BTreeMap<String, Question>,
}

fn nonempty(s: &str) -> bool { !s.trim().is_empty() }

/// Validate before reserving credit and translate to the provider's wire.
pub fn bound(step: &Decide) -> Result<DecisionCall, Refusal> {
    match &step.input {
        Value::String(s) if nonempty(s) => {},
        Value::Object(_) | Value::Array(_) => {},
        _ => return Err(Refusal::Input),
    }
    if !(1..=QUESTIONS_MAX).contains(&step.questions.len()) {
        return Err(Refusal::Questions);
    }
    let mut questions = BTreeMap::new();
    for (id, q) in &step.questions {
        if id.is_empty() || id.len() > 100 || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b)) {
            return Err(Refusal::Name);
        }
        let instructions = match q {
            Question::Choice { instructions, .. } | Question::Predicate { instructions } | Question::Score { instructions, .. } => instructions,
        };
        if !nonempty(instructions) { return Err(Refusal::Instructions); }
        let wire = match q {
            Question::Choice { options, .. } => {
                if !(2..=OPTIONS_MAX).contains(&options.len()) || options.iter().any(|(k, v)| !nonempty(k) || k.len() > 100 || !nonempty(v)) {
                    return Err(Refusal::Options);
                }
                json!({ "type": "choice", "instructions": instructions, "criteria": options })
            },
            Question::Predicate { .. } => json!({ "type": "noul", "instructions": instructions }),
            Question::Score { levels, .. } => {
                if !(2..=LEVELS_MAX).contains(&levels.len()) || levels.iter().any(|s| !nonempty(s)) {
                    return Err(Refusal::Levels);
                }
                json!({ "type": "score", "instructions": instructions, "criteria": levels })
            },
        };
        questions.insert(id, wire);
    }
    let input = json!({ "model": "clef-flash", "state": step.input, "questions": questions });
    if input.to_string().len() > INPUT_MAX_BYTES { return Err(Refusal::TooLarge); }
    Ok(DecisionCall { input, questions: step.questions.clone() })
}

fn probability(v: &Value) -> Result<f64, AnswerFault> {
    v.as_f64().filter(|p| p.is_finite() && (0.0..=1.0).contains(p)).ok_or(AnswerFault::Probability)
}

/// A complete distribution over exactly the requested alternatives.
fn distribution(v: &Value, keys: &[&str]) -> Result<BTreeMap<String, f64>, AnswerFault> {
    let values = v.as_object().ok_or(AnswerFault::Probability)?;
    if values.len() != keys.len() { return Err(AnswerFault::Probability); }
    let ps: BTreeMap<String, f64> = keys.iter().map(|k| Ok((k.to_string(), probability(&v[*k])?))).collect::<Result<_, _>>()?;
    if (ps.values().sum::<f64>() - 1.0).abs() > 0.001 { return Err(AnswerFault::Probability); }
    Ok(ps)
}

impl DecisionCall {
    pub fn worst(&self) -> Usage {
        assert!((1..=QUESTIONS_MAX).contains(&self.questions.len()), "only a bounded decision can be reserved");
        tokens(CONTEXT_TOKENS * self.questions.len() as u64)
    }

    /// Input only, including every question. Output is never billed.
    /// Missing or malformed usage is charged at the reservation by the cell.
    pub fn usage(&self, response: &Value) -> Option<Usage> {
        response["usage"]["input_tokens"].as_u64().map(tokens).filter(|u| u.validate().is_ok())
    }

    /// Only complete, type-correct answers leave the platform boundary.
    pub fn answer(&self, response: &Value) -> Result<Value, AnswerFault> {
        let raw = response["answers"].as_object().ok_or(AnswerFault::Shape)?;
        if raw.len() != self.questions.len() || !self.questions.keys().all(|k| raw.contains_key(k)) {
            return Err(AnswerFault::Questions);
        }
        let mut answers = BTreeMap::new();
        for (id, q) in &self.questions {
            let a = &raw[id];
            let kind = match q { Question::Predicate { .. } => "noul", Question::Choice { .. } => "choice", Question::Score { .. } => "score" };
            if a["type"] != kind { return Err(AnswerFault::Type); }
            let answer = match q {
                Question::Predicate { .. } => json!({ "type": "predicate", "probability": probability(&a["noul"])? }),
                Question::Choice { options, .. } => {
                    let keys: Vec<&str> = options.keys().map(String::as_str).collect();
                    let ps = distribution(&a["probabilities"], &keys)?;
                    let choice = a["choice"].as_str().filter(|c| options.contains_key(*c)).ok_or(AnswerFault::Choice)?;
                    if ps.values().any(|p| *p > ps[choice] + 0.001) { return Err(AnswerFault::Choice); }
                    json!({ "type": "choice", "choice": choice, "probabilities": ps, "confidence": probability(&a["confidence"])? })
                },
                Question::Score { levels, .. } => {
                    let keys: Vec<String> = (0..levels.len()).map(|i| i.to_string()).collect();
                    let refs: Vec<&str> = keys.iter().map(String::as_str).collect();
                    let ps = distribution(&a["probabilities"], &refs)?;
                    let expected: f64 = keys.iter().enumerate().map(|(i, k)| i as f64 * ps[k]).sum();
                    let score = a["score"].as_f64().filter(|s| s.is_finite() && *s >= 0.0 && *s <= (levels.len() - 1) as f64 && (*s - expected).abs() <= 0.01).ok_or(AnswerFault::Score)?;
                    json!({ "type": "score", "score": score, "levels": levels, "probabilities": ps, "confidence": probability(&a["confidence"])? })
                },
            };
            answers.insert(id, answer);
        }
        Ok(json!({ "model": MODEL, "answers": answers, "usage": response["usage"] }))
    }
}

fn tokens(input: u64) -> Usage {
    Usage::Tokens { model: MODEL.into(), input, cached_input: 0, cache_write: 0, output: 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> Decide {
        serde_json::from_value(json!({ "input": "tallow potato chips", "questions": {
            "aisle": { "type": "choice", "instructions": "Where is the finished product?", "options": { "snacks": "Chips and crackers", "produce": "Fresh vegetables" } },
            "snack": { "type": "predicate", "instructions": "Is it a snack?" },
            "urgency": { "type": "score", "instructions": "How soon to buy?", "levels": ["Later", "Now"] }
        } })).unwrap()
    }

    fn response() -> Value {
        json!({ "model": "clef-flash", "answers": {
            "aisle": { "type": "choice", "choice": "snacks", "probabilities": { "snacks": 0.9, "produce": 0.1 }, "confidence": 0.8 },
            "snack": { "type": "noul", "noul": 0.95 },
            "urgency": { "type": "score", "score": 0.75, "probabilities": { "0": 0.25, "1": 0.75 }, "confidence": 0.5 }
        }, "usage": { "input_tokens": 300, "output_tokens": 40 } })
    }

    #[test]
    fn maps_the_contract_and_prices_only_input() {
        let c = bound(&request()).unwrap();
        assert_eq!(c.input["state"], "tallow potato chips");
        assert_eq!(c.input["questions"]["snack"]["type"], "noul");
        assert_eq!(c.input["questions"]["aisle"]["criteria"]["snacks"], "Chips and crackers");
        let r = c.answer(&response()).unwrap();
        assert_eq!(r["answers"]["aisle"]["choice"], "snacks");
        assert_eq!(r["answers"]["snack"], json!({ "type": "predicate", "probability": 0.95 }));
        assert_eq!(r["answers"]["urgency"]["score"], 0.75);
        assert_eq!(c.usage(&response()), Some(tokens(300)));
        assert_eq!(c.worst(), tokens(CONTEXT_TOKENS * 3));
        assert_eq!(c.usage(&json!({})), None);
        assert_eq!(c.usage(&json!({ "usage": { "input_tokens": -1 } })), None);
        assert_eq!(crate::price::PriceBook::defaults().price(&tokens(300)).unwrap().charge, 18);
    }

    #[test]
    fn invalid_requests_are_refused_before_payment() {
        let bad = |change: fn(&mut Decide), why| {
            let mut r = request(); change(&mut r);
            assert_eq!(bound(&r).err(), Some(why));
        };
        bad(|r| r.input = Value::Null, Refusal::Input);
        bad(|r| r.input = json!("  "), Refusal::Input);
        bad(|r| r.questions.clear(), Refusal::Questions);
        bad(|r| { r.questions.insert("bad name".into(), Question::Predicate { instructions: "x".into() }); }, Refusal::Name);
        bad(|r| { r.questions.insert("x".into(), Question::Predicate { instructions: "".into() }); }, Refusal::Instructions);
        bad(|r| { r.questions.insert("x".into(), Question::Choice { instructions: "x".into(), options: BTreeMap::new() }); }, Refusal::Options);
        bad(|r| { r.questions.insert("x".into(), Question::Score { instructions: "x".into(), levels: vec!["x".into()] }); }, Refusal::Levels);
        bad(|r| r.input = json!("x".repeat(INPUT_MAX_BYTES)), Refusal::TooLarge);
        bad(|r| r.questions = (0..=QUESTIONS_MAX).map(|i| (i.to_string(), Question::Predicate { instructions: "x".into() })).collect(), Refusal::Questions);
        assert!(serde_json::from_value::<Decide>(json!({ "input": "x", "questions": {}, "model": "cheap" })).is_err());
        assert!(serde_json::from_value::<Question>(json!({ "type": "predicate", "instructions": "x", "options": {} })).is_err());
    }

    #[test]
    fn malformed_paid_answers_are_never_exposed() {
        let c = bound(&request()).unwrap();
        let bad = |change: fn(&mut Value), why| { let mut r = response(); change(&mut r); assert_eq!(c.answer(&r), Err(why)); };
        bad(|r| r["answers"] = Value::Null, AnswerFault::Shape);
        bad(|r| { r["answers"].as_object_mut().unwrap().remove("snack"); }, AnswerFault::Questions);
        bad(|r| r["answers"]["snack"]["type"] = json!("choice"), AnswerFault::Type);
        bad(|r| r["answers"]["snack"]["noul"] = json!(1.1), AnswerFault::Probability);
        bad(|r| r["answers"]["aisle"]["choice"] = json!("dairy"), AnswerFault::Choice);
        bad(|r| r["answers"]["aisle"]["choice"] = json!("produce"), AnswerFault::Choice);
        bad(|r| r["answers"]["aisle"]["probabilities"]["snacks"] = json!(0.5), AnswerFault::Probability);
        bad(|r| r["answers"]["aisle"]["probabilities"]["extra"] = json!(0.0), AnswerFault::Probability);
        bad(|r| r["answers"]["aisle"]["confidence"] = json!(-0.1), AnswerFault::Probability);
        bad(|r| r["answers"]["urgency"]["score"] = json!(2), AnswerFault::Score);
        bad(|r| r["answers"]["urgency"]["score"] = json!(0.1), AnswerFault::Score);
    }
}
