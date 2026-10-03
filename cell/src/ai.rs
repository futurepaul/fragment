//! AI steps (a job's `ai.*`; docs/api.md, AI). Text goes through the
//! platform's model route by tier (models.rs); images and videos go to
//! OpenRouter with the deployment's own key until phase 7 (media.rs; the
//! debt ledger). Generated images and videos are files written to `main`
//! (a blob when 1 MiB or more), so they sync to folders like anything else.
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
//! whose retries ran out, or of a run that ended. A video's cost comes
//! with the poll that sees it end; its reservation is held until then.

use std::time::Duration;

use base64::Engine;
use fragment_core::ledger::{Release, Released, Reserve, Reserved, Settle, Spend};
use fragment_core::media::{self, VideoEnd};
use fragment_core::models as bounds;
use fragment_core::price::Usage;
use fragment_core::steps::{AiText, Step};
use fragment_core::{blob, npub};
use serde::Deserialize;
use serde_json::{json, Value};
use worker::*;

use crate::error::CellError;
use crate::files::FileWrite;
use crate::fragment::{FragmentCell, MetaKey};
use crate::jobs::{permanent, RunRow, StepFail};
use crate::ledger::{self, LedgerError};
use crate::ops::JOB_ID_PREFIX;

pub const IMAGE_MODEL: &str = "google/gemini-3.1-flash-lite-image";
pub const VIDEO_MODEL: &str = "minimax/hailuo-3-max";
/// A generated file the platform stores (a video is a blob).
const MEDIA_MAX_BYTES: usize = 64 * 1024 * 1024;
const CALL_TIMEOUT: Duration = Duration::from_secs(120);
/// Holds released per pass, of runs that ended.
const RELEASE_BATCH: i64 = 25;

/// A failed OpenRouter answer: passing (429, 5xx) or lasting.
fn failure(status: u16, body: &[u8]) -> StepFail {
    let v: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
    let message = v["error"]["message"].as_str().map(str::to_string).unwrap_or_else(|| String::from_utf8_lossy(body).chars().take(200).collect());
    match status {
        429 | 500..=599 => StepFail::Retry(format!("OpenRouter answered {status}: {message}")),
        402 => permanent(format!("OpenRouter: out of credits ({message})")),
        _ => permanent(format!("OpenRouter answered {status}: {message}")),
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

/// A media step on a deployment with no OpenRouter key fails before it
/// reserves anything.
fn no_key(env: &Env) -> Result<(), StepFail> {
    match crate::keys::openrouter_key(env) {
        Some(_) => Ok(()),
        None => Err(permanent("this deployment has no OPENROUTER_API_KEY: image and video steps are off")),
    }
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
            "INSERT INTO charges (ref, run, micros, held, video, at) VALUES (?, ?, 0, 1, NULL, ?) ON CONFLICT (ref) DO NOTHING",
            vec![p.reference.as_str().into(), SqlStorageValue::Integer(p.run), SqlStorageValue::Integer(crate::js::now_ms())],
        )
        .map_err(retry)
    }

    /// Settles a held call from what it reported (`None`: its worst case),
    /// and records the charge on the run. Once: a settle again answers the
    /// same.
    async fn settle(&self, owner: &str, reference: &str, usage: Option<Usage>) -> Result<i64, StepFail> {
        let settled = ledger::ask(&self.env, owner, &Settle { reference: reference.to_string(), usage }).await.map_err(ledger_fail)?;
        self.exec("UPDATE charges SET micros = ?, held = 0, video = NULL WHERE ref = ?", vec![SqlStorageValue::Integer(settled.charge), reference.into()]).map_err(retry)?;
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
            Ok(Released::Settled { charge }) => self.exec("UPDATE charges SET micros = ?, held = 0, video = NULL WHERE ref = ?", vec![SqlStorageValue::Integer(charge), reference.into()]),
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

    /// Holds of runs that ended (held, or done with a video still waiting
    /// on its cost): nothing will settle them, so their reservations go
    /// back. A replay reserves again. From a job that ended, and from the
    /// alarm for runs ended any other way.
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

    /// One OpenRouter call, with the deployment's key.
    async fn openrouter(&self, method: Method, url: &str, body: Option<&Value>) -> Result<(u16, Vec<u8>), StepFail> {
        let key = crate::keys::openrouter_key(&self.env).ok_or_else(|| permanent("this deployment has no OPENROUTER_API_KEY: image and video steps are off"))?;
        let headers = Headers::new();
        headers.set("authorization", &format!("Bearer {key}")).map_err(|e| permanent(e.to_string()))?;
        headers.set("x-openrouter-title", "fragment").map_err(|e| permanent(e.to_string()))?;
        let mut init = RequestInit::new();
        init.with_method(method).with_headers(headers);
        if let Some(b) = body {
            init.headers.set("content-type", "application/json").map_err(|e| permanent(e.to_string()))?;
            init.with_body(Some(b.to_string().into()));
        }
        let req = Request::new_with_init(url, &init).map_err(|e| permanent(e.to_string()))?;
        let mut resp = crate::cs::fetch(req, CALL_TIMEOUT).await.map_err(|e| StepFail::Retry(format!("OpenRouter: {}", e.0)))?;
        let status = resp.status_code();
        let bytes = resp.bytes().await.map_err(|e| StepFail::Retry(format!("OpenRouter: {e}")))?;
        Ok((status, bytes))
    }

    fn api(&self, path: &str) -> String {
        format!("{}/api/v1/{path}", self.cfg.openrouter_url)
    }

    /// Writes generated bytes to `path` on `main` once per step: in git, or
    /// as a blob and its pointer when 1 MiB or more.
    async fn store_media(&self, run: &RunRow, index: u32, path: &str, bytes: Vec<u8>) -> Result<Value, StepFail> {
        let sha = blob::sha256_hex(&bytes);
        let size = bytes.len() as u64;
        let content = if bytes.len() >= blob::BLOB_MIN_BYTES {
            self.put_blob_bytes(&sha, bytes).await.map_err(retry)?;
            blob::pointer(&sha, size).into_bytes()
        } else {
            bytes
        };
        let key = format!("{JOB_ID_PREFIX}{}:{index}", run.id);
        let message = format!("{} run {}: generated {path}", run.op, run.id);
        let writes = [FileWrite { path: path.to_string(), bytes: Some(content) }];
        self.commit_files(&key, &writes, &Default::default(), &message, &run.principal, run.depth).await.map_err(retry)?;
        Ok(json!({ "path": path, "size": size, "sha256": sha }))
    }

    /// A kept image's bytes, from the blob store where it was put before
    /// it was kept.
    async fn kept_bytes(&self, sha: &str) -> Result<Vec<u8>, StepFail> {
        let key = self.blob_key(sha).map_err(retry)?;
        let found = crate::js::blob_get(self.env.as_ref(), &key, None).await.map_err(retry)?;
        let body = found.ok_or_else(|| permanent(format!("the image this step bought ({sha}) is gone from the blob store: replay the run to make it again")))?;
        let mut resp = Response::from_body(ResponseBody::Stream(body.body)).map_err(|e| retry(e.into()))?;
        resp.bytes().await.map_err(|e| retry(e.into()))
    }

    /// `job.ai.*` steps (the module's doc).
    pub(crate) async fn step_ai(&self, run: &RunRow, index: u32, step: &Step) -> Result<Value, StepFail> {
        match step {
            Step::AiText(t) => self.step_text(run, index, t).await,
            Step::AiImage(image) => {
                let p = self.paying(run, index)?;
                if let Some(kept) = self.kept(&p.key)? {
                    self.settle_kept(&p, &kept).await?;
                    let sha = kept.result["sha256"].as_str().ok_or_else(|| retry(CellError::host("a kept image names its bytes")))?.to_string();
                    let mut out = self.store_media(run, index, &image.path, self.kept_bytes(&sha).await?).await?;
                    out["mediaType"] = kept.result["mediaType"].clone();
                    return Ok(out);
                }
                no_key(&self.env)?;
                self.reserve(&p, media::worst(step).expect("an image has a worst case")).await?;
                let model = image.model.as_deref().unwrap_or(IMAGE_MODEL);
                let mut body = json!({ "model": model, "prompt": image.prompt });
                if let Some(a) = &image.aspect_ratio {
                    body["aspect_ratio"] = json!(a);
                }
                let (status, bytes) = match self.openrouter(Method::Post, &self.api("images"), Some(&body)).await {
                    Ok(answer) => answer,
                    Err(failed) => return Err(self.unpaid(&p, failed).await),
                };
                if status != 200 {
                    return Err(self.unpaid(&p, failure(status, &bytes)).await);
                }
                let v: Value = serde_json::from_slice(&bytes).map_err(|e| StepFail::Retry(format!("OpenRouter images: {e}")))?;
                let usage = self.reported(&p, v["usage"]["cost"].as_f64());
                let decoded = v["data"][0]["b64_json"].as_str().and_then(|b64| base64::engine::general_purpose::STANDARD.decode(b64).ok());
                let Some(decoded) = decoded.filter(|d| d.len() <= MEDIA_MAX_BYTES) else {
                    // it was billed: charged, then refused
                    self.settle(&p.owner, &p.reference, usage).await?;
                    return Err(permanent(format!("OpenRouter's image is missing or over {MEDIA_MAX_BYTES} bytes")));
                };
                let sha = blob::sha256_hex(&decoded);
                // the bytes first, then the note of them: a step tried again finds both
                self.put_blob_bytes(&sha, decoded.clone()).await.map_err(retry)?;
                let kept = json!({ "sha256": sha, "size": decoded.len(), "mediaType": v["data"][0]["media_type"] });
                self.keep(&p, &kept, usage.as_ref())?;
                self.settle_kept(&p, &Kept { result: kept.clone(), usage, settled: false }).await?;
                // test fleets: a failure after the paid call (bug 2's), which the retry survives
                self.test_countdown(MetaKey::TestFailAfterPaid, "the step failed after its paid call").map_err(retry)?;
                let mut out = self.store_media(run, index, &image.path, decoded).await?;
                out["mediaType"] = kept["mediaType"].clone();
                Ok(out)
            }
            Step::AiVideoStart(video) => {
                let p = self.paying(run, index)?;
                if let Some(kept) = self.kept(&p.key)? {
                    // its cost comes with the poll that sees it end
                    return Ok(kept.result);
                }
                no_key(&self.env)?;
                self.reserve(&p, media::worst(step).expect("a video has a worst case")).await?;
                let mut body = json!({ "model": video.model.as_deref().unwrap_or(VIDEO_MODEL), "prompt": video.prompt });
                if let Some(d) = video.duration {
                    body["duration"] = json!(d);
                }
                if let Some(r) = &video.resolution {
                    body["resolution"] = json!(r);
                }
                if let Some(a) = &video.aspect_ratio {
                    body["aspect_ratio"] = json!(a);
                }
                let (status, bytes) = match self.openrouter(Method::Post, &self.api("videos"), Some(&body)).await {
                    Ok(answer) => answer,
                    Err(failed) => return Err(self.unpaid(&p, failed).await),
                };
                if !matches!(status, 200 | 202) {
                    return Err(self.unpaid(&p, failure(status, &bytes)).await);
                }
                let v: Value = serde_json::from_slice(&bytes).map_err(|e| StepFail::Retry(format!("OpenRouter videos: {e}")))?;
                let id = v["id"].as_str().ok_or_else(|| StepFail::Retry("OpenRouter videos: no id in the answer".into()))?.to_string();
                let started = json!({ "id": id });
                self.keep(&p, &started, None)?;
                self.exec("UPDATE charges SET video = ? WHERE ref = ?", vec![id.as_str().into(), p.reference.as_str().into()]).map_err(retry)?;
                Ok(started)
            }
            Step::AiVideoPoll { id } => {
                let (status, bytes) = self.openrouter(Method::Get, &self.api(&format!("videos/{id}")), None).await?;
                if status != 200 {
                    return Err(failure(status, &bytes));
                }
                let v: Value = serde_json::from_slice(&bytes).map_err(|e| StepFail::Retry(format!("OpenRouter videos: {e}")))?;
                let status = v["status"].as_str().unwrap_or("");
                let end = media::video_end(status);
                if let Some(end) = end {
                    self.video_done(id, status, end, v["usage"]["cost"].as_f64()).await?;
                }
                Ok(json!({ "status": v["status"], "ended": end.is_some(), "error": v["error"], "urls": v["unsigned_urls"], "usage": v["usage"] }))
            }
            Step::AiVideoSave { id, path, url } => {
                let url = match url {
                    Some(u) if u.starts_with(&self.cfg.openrouter_url) => u.clone(),
                    Some(u) => return Err(permanent(format!("the video is at {u}, outside OpenRouter"))),
                    None => self.api(&format!("videos/{id}/content?index=0")),
                };
                let (status, video) = self.openrouter(Method::Get, &url, None).await?;
                if status != 200 {
                    return Err(failure(status, &video));
                }
                if video.len() > MEDIA_MAX_BYTES {
                    return Err(permanent(format!("the video is over {MEDIA_MAX_BYTES} bytes")));
                }
                self.store_media(run, index, path, video).await
            }
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

    /// What OpenRouter reported a call cost, as the usage the ledger
    /// meters; none reported is settled at the reservation, and says so.
    fn reported(&self, p: &Paying, cost_usd: Option<f64>) -> Option<Usage> {
        let usage = media::billed(cost_usd);
        if usage.is_none() {
            self.event("ai.cost-missing", &format!("{}: OpenRouter reported no cost; the step is charged its reservation", p.reference), json!({ "ref": p.reference }));
        }
        usage
    }

    /// `job.ai.text`: the model route's call, unstreamed, kept, then settled.
    async fn step_text(&self, run: &RunRow, index: u32, t: &AiText) -> Result<Value, StepFail> {
        let p = self.paying(run, index)?;
        if let Some(kept) = self.kept(&p.key)? {
            self.settle_kept(&p, &kept).await?;
            return Ok(kept.result);
        }
        let messages = match (&t.messages, &t.prompt) {
            (Some(m), _) => Value::Array(m.clone()),
            (None, Some(prompt)) => json!([{ "role": "user", "content": prompt }]),
            (None, None) => return Err(permanent("ai.text needs messages or a prompt")),
        };
        let tier = bounds::tier_named(t.model.as_deref()).map_err(|why| permanent(why.message()))?;
        let mut body = json!({ "messages": messages });
        if let Some(n) = t.max_tokens {
            body["max_tokens"] = json!(n);
        }
        if let Some(effort) = &t.reasoning_effort {
            body["reasoning_effort"] = json!(effort);
        }
        let body_bytes = body.to_string().len();
        let bounded = bounds::bound(tier, body, false).map_err(|why| permanent(why.message()))?;
        self.reserve(&p, bounded.worst(body_bytes)).await?;
        let (status, bytes, log_id) = crate::models::call(&self.env, &bounded, &p.owner, p.agent.as_deref()).await.map_err(retry)?;
        if status != 200 {
            let message = String::from_utf8_lossy(&bytes).chars().take(500).collect::<String>();
            let failed = match status {
                429 | 500..=599 => StepFail::Retry(format!("the model answered {status}: {message}")),
                _ => permanent(format!("the model answered {status}: {message}")),
            };
            return Err(self.unpaid(&p, failed).await);
        }
        let v: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        let usage = bounds::usage_of(bounded.model, &v["usage"]);
        if usage.is_none() {
            self.event("ai.cost-missing", &format!("{}: the model reported no usage; the step is charged its reservation", p.reference), json!({ "ref": p.reference, "logId": log_id }));
        }
        let text = v["choices"][0]["message"]["content"].as_str().unwrap_or("").to_string();
        let result = json!({ "text": text, "model": bounded.model, "tier": tier, "usage": v["usage"] });
        self.keep(&p, &result, usage.as_ref())?;
        self.settle_kept(&p, &Kept { result: result.clone(), usage, settled: false }).await?;
        self.test_countdown(MetaKey::TestFailAfterPaid, "the step failed after its paid call").map_err(retry)?;
        Ok(result)

    }

    /// Settles a video once its poll says it ended: completed, at the cost
    /// reported (its reservation when none is); not delivered, at the cost
    /// reported, else its reservation goes back. Its hold is found by its
    /// id, kept by the step that started it.
    async fn video_done(&self, id: &str, status: &str, end: VideoEnd, cost_usd: Option<f64>) -> Result<(), StepFail> {
        let rows = self.rows("SELECT ref FROM charges WHERE video = ? AND held = 1", vec![id.into()]).map_err(retry)?;
        // settled before (a poll step run again), or released with its run
        let Some(reference) = rows.first().and_then(|r| r["ref"].as_str()).map(str::to_string) else { return Ok(()) };
        let owner = self.must(MetaKey::Owner).map_err(retry)?;
        let usage = media::billed(cost_usd);
        match (end, usage) {
            (VideoEnd::Completed, None) => {
                self.event("ai.cost-missing", &format!("video {id}: OpenRouter reported no cost; it is charged its reservation"), json!({ "video": id }));
                self.settle(&owner, &reference, None).await?;
            }
            (_, Some(usage)) => {
                let charge = self.settle(&owner, &reference, Some(usage)).await?;
                if end == VideoEnd::Undelivered {
                    self.event("ai.video-undelivered", &format!("video {id} {status}; charged {}", fragment_core::price::dollars(charge)), json!({ "video": id, "status": status }));
                }
            }
            (VideoEnd::Undelivered, None) => {
                self.release(&owner, &reference).await;
                self.event("ai.video-undelivered", &format!("video {id} {status}; charged nothing"), json!({ "video": id, "status": status }));
            }
        }
        Ok(())
    }
}
