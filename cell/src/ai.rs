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
//! A text step always streams, and its call is hedged (models.rs,
//! fragment_core::hedge): past its first data line's wait, a second call
//! (the same request on its tier's next model) is made under a reservation of its own
//! (`<reference>/hedge/<hex>`), the first to stream is the answer and the
//! other is aborted. The answer is kept and settled as any call's; the
//! second's hold is then charged what the cancelled call is (the answer's
//! prompt, split as the answer's was, and no output), or released when the call that was
//! not the answer failed before it began. Nothing is called twice after an
//! answer was kept: a step tried again finds it.
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
use crate::models::{Begun, Failed, SecondCall};
use crate::ops::JOB_ID_PREFIX;

/// Holds released per pass, of runs that ended.
const RELEASE_BATCH: i64 = 25;
/// Text steps whose tries this instance counts at once, at most (a step's
/// count goes when its answer is kept; the oldest go past this).
const TRIES_KEPT: usize = 256;

thread_local! {
    /// Each text step's tries on this instance, and when its first began:
    /// its answer's `timing` says them (an evicted instance counts afresh).
    static TRIES: std::cell::RefCell<std::collections::BTreeMap<String, (u32, i64)>> = const { std::cell::RefCell::new(std::collections::BTreeMap::new()) };
}

/// A text step's try begins: its number (from 1) and when its first began.
fn try_begins(key: &str, now: i64) -> (u32, i64) {
    TRIES.with(|t| {
        let mut t = t.borrow_mut();
        if t.len() >= TRIES_KEPT && !t.contains_key(key) {
            let oldest = t.iter().min_by_key(|(_, (_, at))| *at).map(|(k, _)| k.clone());
            if let Some(k) = oldest {
                t.remove(&k);
            }
        }
        let e = t.entry(key.to_string()).or_insert((0, now));
        e.0 += 1;
        *e
    })
}

/// A text step's answer was kept: its tries are counted no more.
fn tries_end(key: &str) {
    TRIES.with(|t| t.borrow_mut().remove(key));
}
/// A streamed text step's drafts go at most this often: 4 a second.
const DRAFT_EVERY_MS: i64 = 250;
/// While a model reasons with no words yet, its draft says it is thinking
/// at most this often (a page counts the seconds on its own between them).
const THINKING_EVERY_MS: i64 = 1000;

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
        // every text step streams: its first data line is what a hedge races to
        let bounded = bounds::model_of(tier).and_then(|m| bounds::bound(m, body, true)).map_err(|why| permanent(why.message()))?;
        let worst = bounded.worst(body_bytes);
        self.reserve(&p, worst.clone()).await?;
        let t0 = crate::js::now_ms();
        let (tries, began) = try_begins(&p.key, t0);
        let read = self.streamed(&p, &bounded, drafting.as_ref(), worst).await?;
        let usage = bounds::usage_of(read.model, &read.used);
        if usage.is_none() {
            self.event("ai.cost-missing", &format!("{}: the model reported no usage; the step is charged its reservation", p.reference), json!({ "ref": p.reference, "logId": read.log_id }));
        }
        // how long it took (docs/optchat.md, "Latency"): its first data line
        // and its whole answer on this try, whether it was hedged and which
        // call answered, its tries, and since its first
        let now = crate::js::now_ms();
        let won = read.second.as_ref().map(|s| if s.won { "second" } else { "first" });
        let timing = json!({ "first_ms": read.first_ms, "ms": now - t0, "hedged": won.is_some(), "won": won, "model": read.model, "passed": read.passed, "thought": read.thought, "tries": tries, "since_ms": now - began, "at": now });
        let answer = &read.answer;
        let result = json!({ "text": answer.content, "message": answer.message(), "finish_reason": answer.finish_reason, "model": read.model, "tier": tier, "usage": read.used, "timing": timing });
        tries_end(&p.key);
        self.keep(&p, &result, usage.as_ref())?;
        self.settle_kept(&p, &Kept { result: result.clone(), usage: usage.clone(), settled: false }).await?;
        self.end_hedge(&p, read.second, usage.as_ref(), body_bytes).await;
        self.test_countdown(MetaKey::TestFailAfterPaid, "the step failed after its paid call").map_err(retry)?;
        Ok(result)
    }

    /// A hedged step's second call's hold (`<reference>/hedge/<hex>`: a new
    /// one each try, as no try settles another's), held at the step's worst
    /// case and recorded on the run as the step's own is; none when the
    /// ledger refuses it or does not answer (the step waits on its first
    /// call alone).
    async fn reserve_hedge(&self, p: &Paying, worst: Usage) -> Option<String> {
        let reference = format!("{}/hedge/{}", p.reference, crate::js::random_hex::<4>());
        let reserve = Reserve { reference: reference.clone(), spend: Spend::AiStep, worst, fragment: Some(p.fragment.clone()), agent: p.agent.clone(), capped: p.capped };
        match ledger::ask(&self.env, &p.owner, &reserve).await {
            Ok(Reserved::Held { .. }) => {}
            _ => return None,
        }
        let recorded = self.exec(
            "INSERT INTO charges (ref, run, micros, held, at) VALUES (?, ?, 0, 1, ?) ON CONFLICT (ref) DO NOTHING",
            vec![reference.as_str().into(), SqlStorageValue::Integer(p.run), SqlStorageValue::Integer(crate::js::now_ms())],
        );
        if recorded.is_err() {
            self.release(&p.owner, &reference).await;
            return None;
        }
        Some(reference)
    }

    /// A hedged step's second call's hold, ended once the step's answer is
    /// kept (or it failed): charged what the cancelled call is
    /// (fragment_core::hedge::cancelled_usage), or released when the call
    /// that was not the answer failed before it began. A settle the ledger
    /// does not answer leaves it held, for `release_ended_holds`.
    async fn end_hedge(&self, p: &Paying, second: Option<SecondCall<String>>, answer: Option<&Usage>, body_bytes: usize) {
        let Some(s) = second else { return };
        if !s.cancelled {
            self.release(&p.owner, &s.hold).await;
            return;
        }
        let charged = self.settle(&p.owner, &s.hold, Some(fragment_core::hedge::cancelled_usage(s.cancelled_model, answer, body_bytes))).await;
        let summary = match &charged {
            Ok(micros) => format!("{}: the other call of its hedge was cancelled before it began, charged its prompt ({micros} micros)", s.hold),
            Err(_) => format!("{}: the other call of its hedge was cancelled; its charge did not land, and its hold goes back when the run ends", s.hold),
        };
        self.event("ai.hedged", &summary, json!({ "ref": s.hold, "won": if s.won { "second" } else { "first" }, "charged": charged.ok() }));
    }

    /// A text step's draft: a channel the app declares (whoever may read it
    /// sees it), drafted by the fragment itself.
    fn drafting(&self, d: &Draft) -> Result<Drafting, StepFail> {
        if self.declared_channel(&d.channel).map_err(retry)?.is_none() {
            return Err(permanent(format!("ai.text drafts to a channel fragment.json declares, and it declares no {:?}", d.channel)));
        }
        Ok(Drafting { channel: d.channel.clone(), turn: d.turn.clone(), principal: npub::display(&self.own_key().map_err(retry)?) })
    }

    /// A text step's call, streamed and hedged (models.rs `hedged`): its
    /// answer and usage read as they come, and with a draft, its text so far
    /// put as the draft (at most every `DRAFT_EVERY_MS`, its whole text once
    /// more at the end, none past a record's size). A stream that breaks, or
    /// ends before its answer says why it stopped, is the step's to try
    /// again under the same hold, as an unstreamed answer cut short is; its
    /// hedge's hold ends first. Read to its `[DONE]`, the connection is
    /// aborted: the gateway can hold it open for minutes after.
    async fn streamed(&self, p: &Paying, bounded: &Bounded, d: Option<&Drafting>, worst: Usage) -> Result<Read, StepFail> {
        let meta = crate::models::Metadata::of(&p.owner, p.agent.as_deref());
        let body_bytes = bounded.input.to_string().len();
        let t0 = crate::js::now_ms();
        // the page sees the call begin: thinking, until its words come
        if let Some(d) = d {
            self.put_thinking(d, 0);
        }
        let h = crate::models::hedged(&self.env, bounded, &meta, || Box::pin(self.reserve_hedge(p, worst))).await;
        let (first_ms, second) = (h.first_ms, h.second);
        let begun = match h.opened {
            Ok(b) => b,
            Err(failed) => {
                self.end_hedge(p, second, None, body_bytes).await;
                return Err(match failed {
                    Failed::Unanswered(e) => retry(e),
                    Failed::Refused { status, body, .. } => self.unpaid(p, model_failure(status, &body)).await,
                });
            }
        };
        let Begun { log_id, head, mut rest, abort, model, passed } = begun;
        let mut stream = Stream::answering();
        let (mut sent, mut sent_at) = (0usize, None::<i64>);
        // what it had thought when it last said it was thinking, and when
        let (mut thought, mut thought_at) = (0u64, t0);
        let mut next = Some(Ok(head));
        // bounded by the model's answer: at most the tier's max_tokens
        loop {
            let chunk = match next.take() {
                Some(c) => c,
                None => match rest.next().await {
                    Some(c) => c,
                    None => break,
                },
            };
            let chunk = match chunk {
                Ok(c) => c,
                Err(e) => {
                    self.end_hedge(p, second, None, body_bytes).await;
                    return Err(StepFail::Retry(format!("the model's stream broke: {e}")));
                }
            };
            stream.push(&chunk, None);
            if let Some(d) = d {
                let text = &stream.answer().expect("an answering stream keeps its answer").content;
                let now = crate::js::now_ms();
                if text.len() > sent && sent_at.is_none_or(|at| now - at >= DRAFT_EVERY_MS) {
                    sent = self.put_draft(d, text).unwrap_or(sent);
                    sent_at = Some(now);
                } else if text.is_empty() && stream.thought() > thought && now - thought_at >= THINKING_EVERY_MS {
                    // reasoning, no words yet: still thinking, this long
                    thought = stream.thought();
                    thought_at = now;
                    self.put_thinking(d, u64::try_from(now - t0).unwrap_or(0));
                }
            }
            // the answer is whole at `[DONE]`
            if stream.done() {
                break;
            }
        }
        abort.abort();
        stream.finish(None);
        let answer = stream.answer().cloned().expect("an answering stream keeps its answer");
        if answer.finish_reason.is_none() {
            self.end_hedge(p, second, None, body_bytes).await;
            return Err(StepFail::Retry("the model's stream ended before its answer did".into()));
        }
        if let Some(d) = d.filter(|_| answer.content.len() > sent) {
            self.put_draft(d, &answer.content);
        }
        Ok(Read { answer, used: stream.usage().cloned().unwrap_or(Value::Null), log_id, first_ms, thought: stream.thought(), model, passed, second })
    }

    /// One draft of the text so far, to the channel's live readers: its
    /// length, when it went (none past a record's size, or past the
    /// fragment's pace).
    fn put_draft(&self, d: &Drafting, text: &str) -> Option<usize> {
        (text.len() <= limits::RECORD_BODY_MAX_BYTES && self.broadcast_draft(&d.channel, &d.principal, &d.turn, Some(text), None)).then_some(text.len())
    }

    /// A draft with no words yet that says the model is thinking, `ms` so
    /// far (live.rs, `thinking`); one past the fragment's pace is dropped.
    fn put_thinking(&self, d: &Drafting, ms: u64) {
        self.broadcast_draft(&d.channel, &d.principal, &d.turn, Some(""), Some(ms));
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

/// A text step's call, read: its answer and the usage it reported, its
/// gateway log id, when its first data line came (ms from the call), and
/// its hedge's second call, when one was made.
struct Read {
    answer: Answer,
    used: Value,
    log_id: Option<String>,
    first_ms: i64,
    /// Characters of reasoning it streamed (none kept).
    thought: u64,
    /// The model that answered (the call's own, or its ladder's), and the
    /// rungs passed over before it.
    model: &'static str,
    passed: Vec<&'static str>,
    second: Option<SecondCall<String>>,
}

/// Where a streamed text step's drafts go, and who drafts them.
struct Drafting {
    channel: String,
    turn: String,
    principal: String,
}
