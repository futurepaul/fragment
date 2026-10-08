//! A job's text step on its payer's own provider (ai.rs's `step_text`, when
//! the payer chose one for the step's role: providers/mod.rs). The same
//! step as on Fragment's models, its answer the same shape (`{text,
//! message, finish_reason, model, tier, usage}`, and `provider`), streamed
//! when it names a draft, kept beside the step so a step tried again never
//! calls again; but nothing is reserved or charged: what it used is
//! counted on the payer's computer.

use fragment_core::models::{self as bounds, Stream};
use fragment_core::providers::{self as own, Role};
use fragment_core::steps::AiText;
use fragment_proto::{limits, ErrorCode, Tier, Visibility};
use futures_util::StreamExt;
use serde_json::{json, Value};
use worker::*;

use super::{Chosen, Own};
use crate::error::CellError;
use crate::fragment::{FragmentCell, MetaKey};
use crate::jobs::{permanent, RunRow, StepFail};

/// A streamed step's drafts go at most this often (ai.rs's pace).
const DRAFT_EVERY_MS: i64 = 250;

/// Where a streamed step's drafts go, and who drafts them.
pub(crate) struct DraftTo<'a> {
    pub channel: &'a str,
    pub turn: &'a str,
    pub principal: &'a str,
}

/// The step an own provider's call is for: `key` keeps what it answered
/// beside the step (`paid`, settled: nothing is reserved, so nothing
/// settles), `run` is its run, `payer` whose computer counts it, `tier` the
/// one it named, and `draft` where its text so far goes; `t0` when this
/// try began, `tries` and `began` the step's tries and its first's start
/// (ai.rs `try_begins`), for its `timing`.
pub(crate) struct StepAt<'a> {
    pub key: &'a str,
    pub run: i64,
    pub payer: &'a str,
    pub tier: Tier,
    pub draft: Option<DraftTo<'a>>,
    pub t0: i64,
    pub tries: u32,
    pub began: i64,
}

/// What a text step of `t`, naming `tier`, runs on: its payer's choice for
/// its role (the one it names, else its tier's), when `own_spend` (the run
/// is the payer's or their agent's). A role out of shape, or a choice whose
/// provider is no longer connected, fails the step for good; a computer
/// that did not answer, for now.
pub(crate) async fn chosen_for_step(env: &Env, payer: &str, t: &AiText, tier: Tier, own_spend: bool) -> Result<Chosen, StepFail> {
    let role = Role::of_step(t.role.as_deref(), tier).map_err(|u| permanent(u.message()))?;
    super::chosen(env, payer, role, tier, own_spend).await.map_err(|e| match e.code {
        ErrorCode::NotConnected | ErrorCode::InvalidRequest => permanent(e.message),
        _ => StepFail::Retry(e.message),
    })
}

/// A vendor's failure as the step's: one that may pass is tried again.
fn failed(f: &own::Failure) -> StepFail {
    let message = format!("the model ({}) failed: {}", f.kind, f.message.chars().take(500).collect::<String>());
    if f.passing {
        StepFail::Retry(message)
    } else {
        permanent(message)
    }
}

impl FragmentCell {
    /// Whether `run`'s steps spend its payer's own choice: a run of the
    /// fragment's owner or an agent of theirs (one the fragment's cap does
    /// not apply to: `capped` false), or the fragment's own (its triggers'
    /// runs, its cron's, the runs its jobs call: run as its key) while it is
    /// its owner's alone (`members`, no member but its owner and their own
    /// agents: `access::owner_lent`'s test), so no one else started it.
    pub(crate) fn spends_own(&self, run: &RunRow, capped: bool) -> Result<bool, StepFail> {
        if !capped {
            return Ok(true);
        }
        let retry = |e: CellError| StepFail::Retry(e.message);
        if run.principal != self.own_key().map_err(retry)? {
            return Ok(false);
        }
        let owner = self.must(MetaKey::Owner).map_err(retry)?;
        let others = self
            .count_of("SELECT COUNT(*) AS n FROM members WHERE principal != ? AND (owner IS NULL OR owner != ?)", vec![owner.as_str().into(), owner.as_str().into()])
            .map_err(retry)?;
        Ok(self.facts().map_err(retry)?.visibility == Visibility::Members && others == 0)
    }

    /// One draft of the text so far: its length, when it went (none past a
    /// record's size, or past the fragment's pace).
    fn own_draft(&self, d: &DraftTo<'_>, text: &str) -> Option<usize> {
        (text.len() <= limits::RECORD_BODY_MAX_BYTES && self.broadcast_draft(d.channel, d.principal, d.turn, Some(text), None)).then_some(text.len())
    }

    /// The step's call on `own` (the module's doc), for the step `at`.
    pub(crate) async fn step_text_own(&self, at: StepAt<'_>, t: &AiText, own: Own, body: Value) -> Result<Value, StepFail> {
        let StepAt { key, run, payer, tier, draft, t0, tries, began } = at;
        let role = Role::of_step(t.role.as_deref(), tier).map_err(|u| permanent(u.message()))?;
        let bounded = bounds::bound_hinted(own::OWN, body, true).map_err(|why| permanent(why.message()))?;
        // the page sees the call begin: thinking, until its words come (as on Fragment's models)
        if let Some(d) = &draft {
            self.broadcast_draft(d.channel, d.principal, d.turn, Some(""), Some(0));
        }
        let opened = super::open(&self.env, &own, &bounded.input).await.map_err(|e| match e.code {
            ErrorCode::InvalidRequest => permanent(e.message),
            _ => StepFail::Retry(e.message),
        })?;
        let mut resp = match opened {
            Ok(r) => r,
            Err((_, f)) => return Err(failed(&f)),
        };
        let mut bytes = resp.stream().map_err(|e| StepFail::Retry(format!("the model's stream: {e}")))?;
        let mut tr = own::translator(own.vendor, &own.model);
        let mut folded = Stream::answering();
        let (mut sent, mut sent_at) = (0usize, None::<i64>);
        let mut out = vec![];
        let mut first_ms = None::<i64>;
        // bounded by the model's answer: at most the cap's tokens
        while let Some(chunk) = bytes.next().await {
            let chunk = chunk.map_err(|e| StepFail::Retry(format!("the model's stream broke: {e}")))?;
            first_ms.get_or_insert_with(|| crate::js::now_ms() - t0);
            tr.push(&chunk, &mut out);
            folded.push(&out, None);
            out.clear();
            if let Some(f) = tr.failure() {
                return Err(failed(f));
            }
            if let Some(d) = &draft {
                let text = &folded.answer().expect("an answering stream keeps its answer").content;
                let now = crate::js::now_ms();
                if text.len() > sent && sent_at.is_none_or(|at| now - at >= DRAFT_EVERY_MS) {
                    sent = self.own_draft(d, text).unwrap_or(sent);
                    sent_at = Some(now);
                }
            }
            if tr.done() {
                break;
            }
        }
        tr.finish(&mut out);
        folded.push(&out, None);
        folded.finish(None);
        if let Some(f) = tr.failure() {
            return Err(failed(f));
        }
        let answer = folded.answer().cloned().expect("an answering stream keeps its answer");
        if answer.finish_reason.is_none() {
            return Err(StepFail::Retry("the model's stream ended before its answer did".into()));
        }
        if let Some(d) = &draft {
            if answer.content.len() > sent {
                self.own_draft(d, &answer.content);
            }
        }
        let counted = tr.counted();
        super::record(&self.env, payer, &own, role, counted).await;
        // its timing, in any text step's shape (ai.rs; docs/optchat.md, "Latency"), its provider named
        let now = crate::js::now_ms();
        let timing = json!({
            "first_ms": first_ms, "ms": now - t0, "model": own.model, "calls": 1, "thought": 0,
            "tries": tries, "since_ms": now - began, "at": now, "provider": own.vendor,
        });
        let result = json!({
            "text": answer.content, "message": own::message_of(&answer, tr.thinking_blocks()), "finish_reason": answer.finish_reason,
            "model": own.model, "tier": tier, "provider": own.vendor, "usage": counted.map(|c| c.openai()), "timing": timing,
        });
        // kept, and settled: nothing was reserved, so nothing settles it
        self.exec(
            "INSERT INTO paid (key, run, result, usage, settled, at) VALUES (?, ?, ?, NULL, 1, ?) ON CONFLICT (key) DO NOTHING",
            vec![key.into(), SqlStorageValue::Integer(run), result.to_string().into(), SqlStorageValue::Integer(crate::js::now_ms())],
        )
        .map_err(|e: CellError| StepFail::Retry(e.message))?;
        Ok(result)
    }
}
