//! AI steps (a job's `ai.*`; docs/api.md, AI). Text goes through the
//! platform's model route by tier (models.rs), streamed when it names a
//! draft, its text so far that app channel's draft (the fragment's own, as
//! `PUT …/draft` puts one). A decision is Clef on Workers AI, priced in its
//! input tokens (fragment_core::decide); an image is FLUX.1
//! [schnell] on Workers AI, on the model route's transport (the AI binding
//! through the deployment's gateway), priced in neurons by its tiles and
//! steps (fragment_core::media). A generated image is a file written to
//! `main` (a blob when 1 MiB or more), so it syncs to folders like anything
//! else. Video steps are refused until they run on Cloudflare (the debt
//! ledger).
//!
//! Who pays (docs/cloudflare-v1.md, decision 26): every paid step bills the
//! fragment's owner, on their ledger, capped when the run's principal is
//! not that owner nor an agent of theirs (a run does not record whom an
//! agent asked for, so an agent of the owner's counts as the owner).
//!
//! A paid step reserves its worst case under its reference
//! (`step:<f>@<life>/run/<run>/attempt/<a>/step/<i>`: a replay is a new
//! attempt, so a step released before may reserve again), makes its call,
//! keeps what it bought beside the step (`paid`, by the step's key
//! `<f>@<life>/run/<run>/step/<i>`, which a replay's attempt finds too),
//! and then settles. A step tried again after its call answered reuses what
//! was kept and never calls the vendor again (bug 2): a settle that did not
//! land lands from the kept usage, and a commit that did not finish runs
//! from the kept bytes. A step that fails for good before its call used
//! anything releases its reservation (bug 3), as does any hold of a step
//! whose retries ran out, or of a run that ended.

use fragment_core::ledger::{Release, Released, Reserve, Reserved, Settle, Spend};
use fragment_core::models::{self as bounds, Answer, Bounded, Stream, MODEL_BODY_MAX_BYTES};
use fragment_core::price::Usage;
use fragment_core::steps::{AiDecide, AiText, Draft, Step};
use fragment_core::{blob, decide, media, npub};
use fragment_proto::limits;
use futures_util::StreamExt;
use serde::Deserialize;
use serde_json::{json, Value};
use worker::*;

use crate::error::CellError;
use crate::files::FileWrite;
use crate::fragment::{FragmentCell, MetaKey};
use crate::jobs::{permanent, RunRow, StepFail};
use crate::ledger::{self, LedgerError};
use crate::ops::JOB_ID_PREFIX;

/// Holds released per pass, of runs that ended.
const RELEASE_BATCH: i64 = 25;
/// A streamed text step's drafts go at most this often: 4 a second.
const DRAFT_EVERY_MS: i64 = 250;

/// A model's refusal: passing (429, 5xx) or lasting.
fn model_failure(status: u16, body: &[u8]) -> StepFail {
    let message = String::from_utf8_lossy(body).chars().take(500).collect::<String>();
    match status {
        429 | 500..=599 => StepFail::Retry(format!("the model answered {status}: {message}")),
        _ => permanent(format!("the model answered {status}: {message}")),
    }
}

/// A ledger's answer as a step's failure: a refusal is for good (no
/// credit, a cap, a guest: the run is held, and replays after a top-up or
/// next month), a ledger that did not answer is for now.
fn ledger_fail(e: LedgerError) -> StepFail {
    match e.refused {
        Some(_) => permanent(e.message),
        None => StepFail::Retry(format!("the owner's ledger: {}", e.message)),
    }
}

fn retry(e: CellError) -> StepFail {
    StepFail::Retry(e.message)
}

/// A paid step's place on its payer's ledger.
struct Paying {
    /// The fragment's owner, who pays.
    owner: String,
    fragment: String,
    /// The reservation's reference: this attempt's.
    reference: String,
    /// What the step bought is kept by this, the same in every attempt.
    key: String,
    run: i64,
    /// The run's principal, when it is an agent (for the record).
    agent: Option<String>,
    /// The fragment's cap applies (decision 26).
    capped: bool,
}

/// What a paid step bought, kept beside it.
struct Kept {
    result: Value,
    usage: Option<Usage>,
    settled: bool,
}

#[derive(Deserialize)]
struct KeptRow {
    result: String,
    usage: Option<String>,
    settled: i64,
}

impl FragmentCell {
    /// This step's key: unique to the fragment's life, the run, and the
    /// step's place in it (the same on a retry and a replay).
    pub(crate) fn step_ref(&self, run: &RunRow, index: u32) -> Result<String, StepFail> {
        Ok(format!("{}@{}/run/{}/step/{index}", self.name().map_err(retry)?, self.must(MetaKey::CreatedAt).map_err(retry)?, run.id))
    }

    /// Who pays for step `index` of `run`, and where on their ledger.
    fn paying(&self, run: &RunRow, index: u32) -> Result<Paying, StepFail> {
        let [owner, created] = self.metas([MetaKey::Owner, MetaKey::CreatedAt]).map_err(retry)?;
        let (owner, created) = (owner.ok_or_else(|| retry(crate::fragment::missing(MetaKey::Owner)))?, created.ok_or_else(|| retry(crate::fragment::missing(MetaKey::CreatedAt)))?);
        let fragment = self.name().map_err(retry)?;
        #[derive(Deserialize)]
        struct Agent {
            owner: Option<String>,
        }
        let agents: Vec<Agent> =
            self.typed("SELECT owner FROM members WHERE principal = ? AND kind = 'agent'", vec![run.principal.as_str().into()]).map_err(retry)?;
        let by_owner = run.principal == owner || agents.first().is_some_and(|a| a.owner.as_deref() == Some(owner.as_str()));
        Ok(Paying {
            reference: format!("step:{fragment}@{created}/run/{}/attempt/{}/step/{index}", run.id, run.attempt),
            key: self.step_ref(run, index)?,
            owner,
            fragment,
            run: run.id,
            agent: (!agents.is_empty() && npub::is_identity(&run.principal)).then(|| run.principal.clone()),
            capped: !by_owner,
        })
    }

    fn kept(&self, key: &str) -> Result<Option<Kept>, StepFail> {
        let rows: Vec<KeptRow> = self.typed("SELECT result, usage, settled FROM paid WHERE key = ?", vec![key.into()]).map_err(retry)?;
        let Some(row) = rows.into_iter().next() else { return Ok(None) };
        let corrupt = |e: serde_json::Error| retry(CellError::host(format!("a kept step result: {e}")));
        let result = serde_json::from_str(&row.result).map_err(corrupt)?;
        let usage = row.usage.map(|u| serde_json::from_str(&u)).transpose().map_err(corrupt)?;
        Ok(Some(Kept { result, usage, settled: row.settled != 0 }))
    }

    /// Keeps what a paid call bought, before it is settled: a step tried
    /// again finds it and never calls again.
    fn keep(&self, p: &Paying, result: &Value, usage: Option<&Usage>) -> Result<(), StepFail> {
        let usage = usage.map(|u| serde_json::to_string(u).expect("a usage serializes"));
        self.exec(
            "INSERT INTO paid (key, run, result, usage, settled, at) VALUES (?, ?, ?, ?, 0, ?) ON CONFLICT (key) DO NOTHING",
            vec![
                p.key.as_str().into(),
                SqlStorageValue::Integer(p.run),
                result.to_string().into(),
                usage.map_or(SqlStorageValue::Null, SqlStorageValue::from),
                SqlStorageValue::Integer(crate::js::now_ms()),
            ],
        )
        .map_err(retry)
    }

    /// Holds the step's worst case on the owner's ledger. A reference that
    /// answers it was settled already is a call paid whose result was not
    /// kept (the node died between the two): it is never made again.
    async fn reserve(&self, p: &Paying, worst: Usage) -> Result<(), StepFail> {
        let reserve = Reserve { reference: p.reference.clone(), spend: Spend::AiStep, worst, fragment: Some(p.fragment.clone()), agent: p.agent.clone(), capped: p.capped };
        match ledger::ask(&self.env, &p.owner, &reserve).await.map_err(ledger_fail)? {
            Reserved::Held { .. } => {}
            Reserved::Settled { .. } => return Err(permanent("this step's call was made and paid, and what it bought was lost: replay the run to make it again")),
            Reserved::Released => return Err(permanent("this step's reservation was released: replay the run to try it again")),
        }
        self.exec(
            "INSERT INTO charges (ref, run, micros, held, at) VALUES (?, ?, 0, 1, ?) ON CONFLICT (ref) DO NOTHING",
            vec![p.reference.as_str().into(), SqlStorageValue::Integer(p.run), SqlStorageValue::Integer(crate::js::now_ms())],
        )
        .map_err(retry)
    }

    /// Settles a held call from what it reported (`None`: its worst case),
    /// and records the charge on the run. Once: a settle again answers the
    /// same.
    async fn settle(&self, owner: &str, reference: &str, usage: Option<Usage>) -> Result<i64, StepFail> {
        let settled = ledger::ask(&self.env, owner, &Settle { reference: reference.to_string(), usage }).await.map_err(ledger_fail)?;
        self.exec("UPDATE charges SET micros = ?, held = 0 WHERE ref = ?", vec![SqlStorageValue::Integer(settled.charge), reference.into()]).map_err(retry)?;
        Ok(settled.charge)
    }

    /// A kept call's settle, when it has not landed.
    async fn settle_kept(&self, p: &Paying, kept: &Kept) -> Result<(), StepFail> {
        if kept.settled {
            return Ok(());
        }
        self.settle(&p.owner, &p.reference, kept.usage.clone()).await?;
        self.exec("UPDATE paid SET settled = 1 WHERE key = ?", vec![p.key.as_str().into()]).map_err(retry)
    }

    /// A held call that failed for good, having used nothing: its
    /// reservation goes back (bug 3). One the ledger answers settled keeps
    /// its charge.
    async fn release(&self, owner: &str, reference: &str) {
        let released = ledger::ask(&self.env, owner, &Release { reference: reference.to_string() }).await;
        let _ = match released {
            Ok(Released::Back) | Err(LedgerError { refused: Some(_), .. }) => self.exec("DELETE FROM charges WHERE ref = ?", vec![reference.into()]),
            Ok(Released::Settled { charge }) => self.exec("UPDATE charges SET micros = ?, held = 0 WHERE ref = ?", vec![SqlStorageValue::Integer(charge), reference.into()]),
            // the ledger did not answer: the alarm releases it later (`release_ended_holds`)
            Err(_) => Ok(()),
        };
    }

    /// The hold of step `index` of a run's attempt whose retries ran out
    /// (the job sees its error, and may go on): its reservation goes back.
    pub(crate) async fn release_step(&self, run: &RunRow, index: u32) {
        let Ok(p) = self.paying(run, index) else { return };
        let held = self.count_of("SELECT COUNT(*) AS n FROM charges WHERE ref = ? AND held = 1", vec![p.reference.as_str().into()]).unwrap_or(0);
        if held > 0 {
            self.release(&p.owner, &p.reference).await;
            self.event("ai.released", &format!("{}: its step failed for good; the reservation goes back", p.reference), json!({ "ref": p.reference }));
        }
    }

    /// Holds of runs that ended before their calls settled (a release the
    /// ledger did not answer, a run held mid-step): nothing will settle
    /// them, so their reservations go back. A replay reserves again. From a
    /// job that ended, and from the alarm for runs ended any other way.
    pub(crate) async fn release_ended_holds(&self) {
        let Ok(rows) = self.rows(
            "SELECT ref FROM charges WHERE held = 1 AND run NOT IN (SELECT id FROM runs WHERE status IN ('queued', 'running')) LIMIT ?",
            vec![SqlStorageValue::Integer(RELEASE_BATCH)],
        ) else {
            return;
        };
        let Some(owner) = rows.first().and_then(|_| self.must(MetaKey::Owner).ok()) else { return };
        for row in rows {
            let reference = row["ref"].as_str().expect("charges.ref is TEXT").to_string();
            self.release(&owner, &reference).await;
            self.event("ai.released", &format!("{reference}: its run ended before the call settled; the reservation goes back"), json!({ "ref": reference }));
        }
    }

    /// Writes generated bytes to `path` on `main` once per step: in git, or
    /// as a blob and its pointer when 1 MiB or more. The platform's own
    /// commit, past an app's write limit (256 KiB), so the two limits meet
    /// and an image between them is kept (bug 1).
    async fn store_media(&self, run: &RunRow, index: u32, path: &str, bytes: Vec<u8>) -> Result<Value, StepFail> {
        let sha = blob::sha256_hex(&bytes);
        let size = bytes.len() as u64;
        let content = if bytes.len() >= blob::BLOB_MIN_BYTES {
            self.put_blob_bytes(&sha, bytes).await.map_err(retry)?;
            blob::pointer(&sha, size).into_bytes()
        } else {
            bytes
        };
        assert!(content.len() < blob::BLOB_MIN_BYTES, "what goes to git is under a blob's size");
        let key = format!("{JOB_ID_PREFIX}{}:{index}", run.id);
        let message = format!("{} run {}: generated {path}", run.op, run.id);
        let writes = [FileWrite { path: path.to_string(), bytes: Some(content) }];
        self.commit(&key, &writes, &Default::default(), &message, &run.principal, run.depth).await.map_err(retry)?;
        Ok(json!({ "path": path, "size": size, "sha256": sha }))
    }

    /// A kept image's bytes, from the blob store where it was put before
    /// it was kept.
    async fn kept_bytes(&self, sha: &str) -> Result<Vec<u8>, StepFail> {
        let key = self.blob_key(sha).map_err(retry)?;
        let found = crate::js::blob_get(&self.env, &key, None).await.map_err(retry)?;
        let body = found.ok_or_else(|| permanent(format!("the image this step bought ({sha}) is gone from the blob store: replay the run to make it again")))?;
        let mut resp = Response::from_body(ResponseBody::Stream(body.body)).map_err(|e| retry(e.into()))?;
        resp.bytes().await.map_err(|e| retry(e.into()))
    }

    /// `job.ai.*` steps (the module's doc).
    pub(crate) async fn step_ai(&self, run: &RunRow, index: u32, step: &Step) -> Result<Value, StepFail> {
        match step {
            Step::AiText(t) => self.step_text(run, index, t).await,
            Step::AiDecide(d) => self.step_decide(run, index, d).await,
            Step::AiImage(image) => {
                let call = media::image_call(image).map_err(|why| permanent(why.message()))?;
                let p = self.paying(run, index)?;
                if let Some(kept) = self.kept(&p.key)? {
                    self.settle_kept(&p, &kept).await?;
                    let sha = kept.result["sha256"].as_str().ok_or_else(|| retry(CellError::host("a kept image names its bytes")))?.to_string();
                    let mut out = self.store_media(run, index, &image.path, self.kept_bytes(&sha).await?).await?;
                    out["mediaType"] = kept.result["mediaType"].clone();
                    return Ok(out);
                }
                self.reserve(&p, call.worst()).await?;
                let (status, bytes, log_id) = crate::models::run(&self.env, media::IMAGE_MODEL, &call.input, &p.owner, p.agent.as_deref()).await.map_err(retry)?;
                if status != 200 {
                    return Err(self.unpaid(&p, model_failure(status, &bytes)).await);
                }
                let drawn = match media::image_of(&bytes) {
                    Ok(drawn) => drawn,
                    Err(fault) => {
                        // it answered, so it was paid for: charged its reservation, then refused
                        self.settle(&p.owner, &p.reference, None).await?;
                        self.event("ai.image-refused", &format!("{}: {}; charged its reservation", p.reference, fault.message()), json!({ "ref": p.reference, "logId": log_id }));
                        return Err(permanent(format!("{}: its call is charged at its worst case", fault.message())));
                    }
                };
                let usage = call.usage(drawn.width, drawn.height);
                let sha = blob::sha256_hex(&drawn.bytes);
                // the bytes first, then the note of them: a step tried again finds both
                self.put_blob_bytes(&sha, drawn.bytes.clone()).await.map_err(retry)?;
                let kept = json!({ "sha256": sha, "size": drawn.bytes.len(), "mediaType": media::IMAGE_MEDIA_TYPE, "width": drawn.width, "height": drawn.height });
                self.keep(&p, &kept, Some(&usage))?;
                self.settle_kept(&p, &Kept { result: kept.clone(), usage: Some(usage), settled: false }).await?;
                // test fleets: a failure after the paid call (bug 2's), which the retry survives
                self.test_countdown(MetaKey::TestFailAfterPaid, "the step failed after its paid call").map_err(retry)?;
                let mut out = self.store_media(run, index, &image.path, drawn.bytes).await?;
                out["mediaType"] = kept["mediaType"].clone();
                Ok(out)
            }
            // nothing is reserved: no call is made
            Step::AiVideo {} => Err(permanent(media::Refusal::VideoOff.message())),
            other => unreachable!("only AI steps are performed here, not {}", other.kind()),
        }
    }

    /// A call refused for good, having used nothing: the reservation goes
    /// back (bug 3), and the step fails with why. One that may pass keeps
    /// its hold, and is made again.
    async fn unpaid(&self, p: &Paying, failed: StepFail) -> StepFail {
        if let StepFail::Permanent(_) = &failed {
            self.release(&p.owner, &p.reference).await;
        }
        failed
    }

    /// `job.ai.text`: the model route's call, kept, then settled; streamed
    /// when it names a draft (`streamed`). Its answer is the model's text,
    /// its message (`{role, content, tool_calls?}`, never its reasoning)
    /// and why it stopped.
    async fn step_text(&self, run: &RunRow, index: u32, t: &AiText) -> Result<Value, StepFail> {
        let p = self.paying(run, index)?;
        if let Some(kept) = self.kept(&p.key)? {
            self.settle_kept(&p, &kept).await?;
            return Ok(kept.result);
        }
        let tier = bounds::tier_named(t.model.as_deref()).map_err(|why| permanent(why.message()))?;
        let body = bounds::text_body(t).map_err(|why| permanent(why.message()))?;
        let drafting = t.draft.as_ref().map(|d| self.drafting(d)).transpose()?;
        let body_bytes = body.to_string().len();
        if body_bytes > MODEL_BODY_MAX_BYTES {
            return Err(permanent(CellError::too_large("a model call", body_bytes, MODEL_BODY_MAX_BYTES).message));
        }
        let bounded = bounds::model_of(tier).and_then(|m| bounds::bound(m, body, drafting.is_some())).map_err(|why| permanent(why.message()))?;
        self.reserve(&p, bounded.worst(body_bytes)).await?;
        let (answer, used, log_id) = match &drafting {
            Some(d) => self.streamed(&p, &bounded, d).await?,
            None => {
                let (status, bytes, log_id) = crate::models::call(&self.env, &bounded, &p.owner, p.agent.as_deref()).await.map_err(retry)?;
                if status != 200 {
                    return Err(self.unpaid(&p, model_failure(status, &bytes)).await);
                }
                let v: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
                (Answer::of_completion(&v), v["usage"].clone(), log_id)
            }
        };
        let usage = bounds::usage_of(bounded.model, &used);
        if usage.is_none() {
            self.event("ai.cost-missing", &format!("{}: the model reported no usage; the step is charged its reservation", p.reference), json!({ "ref": p.reference, "logId": log_id }));
        }
        let result = json!({ "text": answer.content, "message": answer.message(), "finish_reason": answer.finish_reason, "model": bounded.model, "tier": tier, "usage": used });
        self.keep(&p, &result, usage.as_ref())?;
        self.settle_kept(&p, &Kept { result: result.clone(), usage, settled: false }).await?;
        self.test_countdown(MetaKey::TestFailAfterPaid, "the step failed after its paid call").map_err(retry)?;
        Ok(result)
    }

    /// A text step's draft: a channel the app declares (whoever may read it
    /// sees it), drafted by the fragment itself.
    fn drafting(&self, d: &Draft) -> Result<Drafting, StepFail> {
        if self.declared_channel(&d.channel).map_err(retry)?.is_none() {
            return Err(permanent(format!("ai.text drafts to a channel fragment.json declares, and it declares no {:?}", d.channel)));
        }
        Ok(Drafting { channel: d.channel.clone(), turn: d.turn.clone(), principal: npub::display(&self.own_key().map_err(retry)?) })
    }

    /// A text step's call, streamed: its answer and usage read as they
    /// come, and its text so far put as the draft (at most every
    /// `DRAFT_EVERY_MS`, its whole text once more at the end, none past a
    /// record's size). A stream that breaks, or ends before its answer
    /// says why it stopped, is the step's to try again under the same
    /// hold, as an unstreamed answer cut short is.
    async fn streamed(&self, p: &Paying, bounded: &Bounded, d: &Drafting) -> Result<(Answer, Value, Option<String>), StepFail> {
        let (status, opened, log_id) = crate::models::call_streamed(&self.env, bounded, &p.owner, p.agent.as_deref()).await.map_err(retry)?;
        let mut bytes = match opened {
            Ok(bytes) => bytes,
            Err(refusal) => return Err(self.unpaid(p, model_failure(status, &refusal)).await),
        };
        let mut stream = Stream::answering();
        let (mut sent, mut sent_at) = (0usize, None::<i64>);
        // bounded by the model's answer: at most the tier's max_tokens
        while let Some(chunk) = bytes.next().await {
            let chunk = chunk.map_err(|e| StepFail::Retry(format!("the model's stream broke: {e}")))?;
            stream.push(&chunk, None);
            let text = &stream.answer().expect("an answering stream keeps its answer").content;
            let now = crate::js::now_ms();
            if text.len() > sent && sent_at.is_none_or(|at| now - at >= DRAFT_EVERY_MS) {
                sent = self.draft(d, text).unwrap_or(sent);
                sent_at = Some(now);
            }
            // the answer is whole at `[DONE]`: the gateway can hold the
            // connection open for minutes after it
            if stream.done() {
                break;
            }
        }
        stream.finish(None);
        let answer = stream.answer().cloned().expect("an answering stream keeps its answer");
        if answer.finish_reason.is_none() {
            return Err(StepFail::Retry("the model's stream ended before its answer did".into()));
        }
        if answer.content.len() > sent {
            self.draft(d, &answer.content);
        }
        Ok((answer, stream.usage().cloned().unwrap_or(Value::Null), log_id))
    }

    /// One draft of the text so far, to the channel's live readers: its
    /// length, when it went (none past a record's size, or past the
    /// fragment's pace).
    fn draft(&self, d: &Drafting, text: &str) -> Option<usize> {
        (text.len() <= limits::RECORD_BODY_MAX_BYTES && self.broadcast_draft(&d.channel, &d.principal, &d.turn, Some(text))).then_some(text.len())
    }

    /// `job.ai.decide`: Clef on the model route's transport, its input
    /// checked first (fragment_core::decide), kept, then settled from its
    /// input tokens.
    async fn step_decide(&self, run: &RunRow, index: u32, d: &AiDecide) -> Result<Value, StepFail> {
        let call = decide::decide_call(d).map_err(|why| permanent(why.message()))?;
        let body_bytes = call.input.to_string().len();
        if body_bytes > MODEL_BODY_MAX_BYTES {
            return Err(permanent(CellError::too_large("a model call", body_bytes, MODEL_BODY_MAX_BYTES).message));
        }
        let p = self.paying(run, index)?;
        if let Some(kept) = self.kept(&p.key)? {
            self.settle_kept(&p, &kept).await?;
            return Ok(kept.result);
        }
        self.reserve(&p, call.worst(body_bytes)).await?;
        let (status, bytes, log_id) = crate::models::run(&self.env, call.model, &call.input, &p.owner, p.agent.as_deref()).await.map_err(retry)?;
        if status != 200 {
            return Err(self.unpaid(&p, model_failure(status, &bytes)).await);
        }
        let v: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        let answers = match decide::answers_of(d, &v) {
            Ok(answers) => answers,
            Err(fault) => {
                // it answered, so it was paid for: charged its reservation, then refused
                self.settle(&p.owner, &p.reference, None).await?;
                self.event("ai.decide-refused", &format!("{}: {}; charged its reservation", p.reference, fault.message()), json!({ "ref": p.reference, "logId": log_id }));
                return Err(permanent(format!("{}: its call is charged at its worst case", fault.message())));
            }
        };
        let usage = call.usage(&v);
        if usage.is_none() {
            self.event("ai.cost-missing", &format!("{}: the model reported no usage; the step is charged its reservation", p.reference), json!({ "ref": p.reference, "logId": log_id }));
        }
        let used = v.get("usage").or_else(|| v["result"].get("usage")).cloned().unwrap_or(Value::Null);
        let result = json!({ "answers": answers, "model": call.model, "usage": used });
        self.keep(&p, &result, usage.as_ref())?;
        self.settle_kept(&p, &Kept { result: result.clone(), usage, settled: false }).await?;
        self.test_countdown(MetaKey::TestFailAfterPaid, "the step failed after its paid call").map_err(retry)?;
        Ok(result)
    }
}

/// Where a streamed text step's drafts go, and who drafts them.
struct Drafting {
    channel: String,
    turn: String,
    principal: String,
}
