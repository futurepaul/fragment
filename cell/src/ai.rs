//! Platform AI (phase 2 slice F): OpenRouter text, images, and video as a
//! job's steps. Generated images and videos are files: written to `main`
//! (a blob when 1 MiB or more), so they sync to folders like anything else.
//!
//! Who pays (ROADMAP decision 14; phase 4 slice C): a fragment with its
//! own `OPENROUTER_API_KEY` secret pays with it, unmetered. Otherwise its
//! owner's billing org pays: a paid step reserves its worst case in the
//! org's ledger (`ledger.rs`), runs with the org's own OpenRouter key, and
//! settles to the cost OpenRouter reports; a step the month cannot cover
//! fails with "budget used up" (the run is held, and replays after a
//! top-up). A step the ledger already settled answers its stored result:
//! a replayed run is not paid twice. Keys are opened here, at the egress
//! point, and never reach the app.
//!
//! A step's cost goes on its run in `spend`. A video's row keeps its
//! OpenRouter id (`video`) while it waits for the cost its last poll
//! reports; settling clears it. A run held with a video still waiting
//! gives that reservation back: nothing polls the video any more.

use std::time::Duration;

use base64::Engine;
use fragment_core::budget::VideoEnd;
use fragment_core::steps::Step;
use fragment_core::{blob, budget};
use fragment_proto::ErrorCode;
use serde_json::{json, Value};
use worker::*;

use crate::cs::FetchError;
use crate::error::CellError;
use crate::ledger::{self, ReleaseAnswer, Reserved, VideoSettlement};
use crate::files::FileWrite;
use crate::fragment::{FragmentCell, MetaKey};
use crate::jobs::{permanent, RunRow, StepFail};
use crate::ops::JOB_ID_PREFIX;

pub const IMAGE_MODEL: &str = "google/gemini-3.1-flash-lite-image";
pub const VIDEO_MODEL: &str = "minimax/hailuo-3-max";
/// A generated file the platform stores (a video is a blob).
const MEDIA_MAX_BYTES: usize = 64 * 1024 * 1024;
const CALL_TIMEOUT: Duration = Duration::from_secs(120);
const KEY_SECRET: &str = "OPENROUTER_API_KEY";
/// Held runs' waiting videos released per pass.
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

/// Who pays for a step, and with which key.
enum Payer {
    /// The fragment's own key: not metered.
    Own(String),
    /// The owner's billing org, with its key; `reference` is the step's
    /// reservation when the step is paid.
    Org { org: String, key: String, reference: Option<String> },
}

enum Paying {
    Payer(Payer),
    /// The ledger settled this step before: its result.
    Replay(Value),
}

impl Payer {
    fn key(&self) -> &str {
        match self {
            Payer::Own(k) | Payer::Org { key: k, .. } => k,
        }
    }
}

/// The model a paid step runs on: the step's own, or the platform's
/// default for its kind.
fn model_of(step: &Step) -> Option<&str> {
    match step {
        Step::AiText(t) => Some(&t.model),
        Step::AiImage(i) => Some(i.model.as_deref().unwrap_or(IMAGE_MODEL)),
        Step::AiVideoStart(v) => Some(v.model.as_deref().unwrap_or(VIDEO_MODEL)),
        _ => None,
    }
}

/// A ledger refusal as a step's failure: a month that cannot cover it is
/// for good (the run is held), a ledger that did not answer is for now.
fn ledger_fail(e: CellError) -> StepFail {
    match e.code {
        ErrorCode::BudgetUsedUp | ErrorCode::InvalidRequest | ErrorCode::NotFound => permanent(e.message),
        _ => StepFail::Retry(format!("the budget ledger: {}", e.message)),
    }
}

impl FragmentCell {
    /// This step's source reference: unique to the fragment's life, the
    /// run, and the step's place in it (the same on a retry and a replay).
    fn step_ref(&self, run: &RunRow, index: u32) -> Result<String, StepFail> {
        let retry = |e: CellError| StepFail::Retry(e.message);
        Ok(format!("{}@{}/run/{}/step/{index}", self.name().map_err(retry)?, self.must(MetaKey::CreatedAt).map_err(retry)?, run.id))
    }

    /// Who pays for step `index` of `run`, reserving its worst case when it
    /// is a paid step.
    async fn payer(&self, run: &RunRow, index: u32, step: &Step) -> Result<Paying, StepFail> {
        if let Some(own) = self.open_secret(KEY_SECRET).await.map_err(|e| StepFail::Retry(e.message))? {
            let own = String::from_utf8(own).map_err(|_| permanent(format!("{KEY_SECRET} is not text")))?;
            return Ok(Paying::Payer(Payer::Own(own.trim().to_string())));
        }
        let owner = self.must(MetaKey::Owner).map_err(|e| StepFail::Retry(e.message))?;
        let org = ledger::org_of(&owner).ok_or_else(|| permanent("the fragment's owner has no billing org"))?;
        let Some(amount) = budget::reservation(step) else {
            let key = ledger::ask(&self.env, &org, &ledger::Key {}).await.map_err(ledger_fail)?.key;
            return Ok(Paying::Payer(Payer::Org { org, key, reference: None }));
        };
        let reference = self.step_ref(run, index)?;
        let principal = run.principal.clone();
        let agent = self
            .rows("SELECT principal FROM members WHERE principal = ? AND kind = 'agent'", vec![principal.as_str().into()])
            .map_err(|e| StepFail::Retry(e.message))?
            .first()
            .map(|_| principal.clone());
        let reserve = ledger::Reserve {
            reference: reference.clone(),
            kind: step.kind().to_string(),
            model: model_of(step).map(str::to_string),
            amount,
            fragment: self.name().map_err(|e| StepFail::Retry(e.message))?,
            run: run.id,
            principal,
            agent,
        };
        match ledger::ask(&self.env, &org, &reserve).await.map_err(ledger_fail)? {
            Reserved::Replay { result } => Ok(Paying::Replay(result)),
            Reserved::Held { key } => Ok(Paying::Payer(Payer::Org { org, key, reference: Some(reference) })),
        }
    }

    /// A paid step's answer: settled to its cost (a video's comes with its
    /// last poll; any other step that reported none is charged its
    /// reservation), and the cost recorded on its run.
    async fn settle(&self, run: &RunRow, payer: &Payer, cost_usd: Option<f64>, result: &Value, video: Option<&str>) -> Result<(), StepFail> {
        let Payer::Org { org, reference: Some(reference), .. } = payer else { return Ok(()) };
        let cost = match video {
            Some(_) => None,
            None => budget::charge(cost_usd, None),
        };
        if video.is_none() && cost.is_none() {
            self.event("ai.cost-missing", &format!("{reference}: OpenRouter reported no cost; the step is charged its reservation"), json!({ "ref": reference }));
        }
        let settle = ledger::Settle { reference: reference.clone(), cost, result: result.clone(), video: video.map(str::to_string) };
        let mut tries = 0;
        let settlement = loop {
            match ledger::ask(&self.env, org, &settle).await {
                Ok(s) => break s,
                Err(e) if tries < 2 && e.code != ErrorCode::NotFound => tries += 1,
                Err(e) => return Err(StepFail::Retry(format!("settling the step's cost: {}", e.message))),
            }
        };
        // what the ledger charged (nothing yet for a video waiting on its cost)
        let charged = settlement.charged();
        let _ = self.exec(
            "INSERT INTO spend (ref, run, micros, at, video) VALUES (?, ?, ?, ?, ?)
             ON CONFLICT (ref) DO UPDATE SET micros = excluded.micros, video = excluded.video",
            vec![
                reference.as_str().into(),
                SqlStorageValue::Integer(run.id),
                SqlStorageValue::Integer(charged),
                SqlStorageValue::Integer(crate::js::now_ms()),
                video.map_or(SqlStorageValue::Null, |v| v.into()),
            ],
        );
        Ok(())
    }

    /// A reservation a failed step no longer needs.
    async fn release(&self, payer: &Payer) {
        if let Payer::Org { org, reference: Some(reference), .. } = payer {
            let _ = ledger::ask(&self.env, org, &ledger::Release { reference: reference.clone() }).await;
        }
    }

    /// Held runs' videos still waiting for their cost: nothing polls them
    /// any more, so their reservations go back (a replay starts them
    /// again). A video that settled meanwhile keeps its cost. From a job
    /// that just failed, and from the alarm for runs held any other way.
    pub(crate) async fn release_held_videos(&self) {
        let Ok(rows) = self.rows(
            "SELECT ref FROM spend WHERE video IS NOT NULL AND run IN (SELECT id FROM runs WHERE status = 'held') LIMIT ?",
            vec![SqlStorageValue::Integer(RELEASE_BATCH)],
        ) else {
            return;
        };
        if rows.is_empty() {
            return;
        }
        let Some(org) = self.must(MetaKey::Owner).ok().and_then(|o| ledger::org_of(&o)) else { return };
        for row in rows {
            let reference = row["ref"].as_str().expect("spend.ref is TEXT");
            let Ok(released) = ledger::ask(&self.env, &org, &ledger::Release { reference: reference.to_string() }).await else { continue };
            let _ = match released {
                ReleaseAnswer::Settled { cost } => self.exec("UPDATE spend SET micros = ?, video = NULL WHERE ref = ?", vec![SqlStorageValue::Integer(cost), reference.into()]),
                ReleaseAnswer::Released | ReleaseAnswer::Gone => self.exec("DELETE FROM spend WHERE ref = ?", vec![reference.into()]),
            };
            if matches!(released, ReleaseAnswer::Released) {
                self.event("ai.video-released", &format!("{reference}: its run was held before the video's cost came; the reservation goes back"), json!({ "ref": reference }));
            }
        }
    }

    /// One OpenRouter call, with the payer's key.
    async fn openrouter(&self, key: &str, method: Method, url: &str, body: Option<&Value>) -> Result<(u16, Vec<u8>), StepFail> {
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
        let mut resp = crate::cs::fetch(req, CALL_TIMEOUT).await.map_err(|e| match e {
            FetchError::Refused(m) => permanent(format!("OpenRouter: {m}")),
            FetchError::Failed(m) => StepFail::Retry(format!("OpenRouter: {m}")),
        })?;
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
        let retry = |e: CellError| StepFail::Retry(e.message);
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

    /// `job.ai.*` steps, paid by whoever pays (above).
    pub(crate) async fn step_ai(&self, run: &RunRow, index: u32, step: &Step) -> Result<Value, StepFail> {
        let payer = match self.payer(run, index, step).await? {
            Paying::Replay(result) => return Ok(result),
            Paying::Payer(p) => p,
        };
        let answer = self.ai_call(run, index, step, &payer).await;
        match answer {
            Ok((result, cost, video)) => {
                self.settle(run, &payer, cost, &result, video.as_deref()).await?;
                Ok(result)
            }
            Err(StepFail::Permanent(m)) => {
                self.release(&payer).await;
                Err(StepFail::Permanent(m))
            }
            // the step runs again, on the reservation it holds
            Err(retry) => Err(retry),
        }
    }

    /// Settles a video once its poll says it ended: completed, at the cost
    /// reported (its reservation when none is); not delivered, at nothing
    /// unless a cost is reported.
    async fn video_done(&self, payer: &Payer, id: &str, status: &str, end: VideoEnd, cost_usd: Option<f64>) -> Result<(), StepFail> {
        let Payer::Org { org, .. } = payer else { return Ok(()) };
        let cost = budget::charge(cost_usd, Some(end));
        match (end, cost) {
            (VideoEnd::Completed, None) => {
                self.event("ai.cost-missing", &format!("video {id}: OpenRouter reported no cost; it is charged its reservation"), json!({ "video": id }))
            }
            (VideoEnd::Undelivered, _) => self.event(
                "ai.video-undelivered",
                &format!("video {id} {status}; charged {}", budget::dollars(cost.unwrap_or(0))),
                json!({ "video": id, "status": status }),
            ),
            (VideoEnd::Completed, Some(_)) => {}
        }
        match ledger::ask(&self.env, org, &ledger::SettleVideo { video: id.to_string(), cost }).await.map_err(ledger_fail)? {
            VideoSettlement::Now { cost: charged } | VideoSettlement::Before { cost: charged } => {
                let _ = self.exec("UPDATE spend SET micros = ?, video = NULL WHERE video = ?", vec![SqlStorageValue::Integer(charged), id.into()]);
            }
            // its reservation went back when its run was held
            VideoSettlement::NoReservation => {}
        }
        Ok(())
    }

    /// One `job.ai.*` step's call: its result, the cost OpenRouter reported,
    /// and the video it started (whose cost comes later).
    async fn ai_call(&self, run: &RunRow, index: u32, step: &Step, payer: &Payer) -> Result<(Value, Option<f64>, Option<String>), StepFail> {
        let key = payer.key();
        match step {
            Step::AiText(t) => {
                let messages = match (&t.messages, &t.prompt) {
                    (Some(m), _) => Value::Array(m.clone()),
                    (None, Some(p)) => json!([{ "role": "user", "content": p }]),
                    (None, None) => return Err(permanent("ai.text needs messages or a prompt")),
                };
                let mut body = json!({ "model": t.model, "messages": messages });
                // OpenRouter's reasoning control, as the job gave it (a
                // reasoning model can spend a small cap thinking)
                if let Some(reasoning) = &t.reasoning {
                    body["reasoning"] = Value::Object(reasoning.clone());
                }
                if let Some(n) = t.max_tokens {
                    body["max_tokens"] = json!(n);
                }
                let (status, bytes) = self.openrouter(key, Method::Post, &self.api("chat/completions"), Some(&body)).await?;
                let v: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
                if status != 200 || v.get("error").is_some() {
                    return Err(failure(if status == 200 { 502 } else { status }, &bytes));
                }
                let text = v["choices"][0]["message"]["content"].as_str().unwrap_or("").to_string();
                Ok((json!({ "text": text, "model": v["model"], "usage": v["usage"] }), v["usage"]["cost"].as_f64(), None))
            }
            Step::AiImage(image) => {
                let mut body = json!({ "model": model_of(step), "prompt": image.prompt });
                if let Some(a) = &image.aspect_ratio {
                    body["aspect_ratio"] = json!(a);
                }
                let (status, bytes) = self.openrouter(key, Method::Post, &self.api("images"), Some(&body)).await?;
                if status != 200 {
                    return Err(failure(status, &bytes));
                }
                let v: Value = serde_json::from_slice(&bytes).map_err(|e| StepFail::Retry(format!("OpenRouter images: {e}")))?;
                let b64 = v["data"][0]["b64_json"].as_str().ok_or_else(|| StepFail::Retry("OpenRouter images: no image in the answer".into()))?;
                let decoded = base64::engine::general_purpose::STANDARD.decode(b64).map_err(|e| StepFail::Retry(format!("OpenRouter images: {e}")))?;
                if decoded.len() > MEDIA_MAX_BYTES {
                    return Err(permanent(format!("the image is over {MEDIA_MAX_BYTES} bytes")));
                }
                let mut out = self.store_media(run, index, &image.path, decoded).await?;
                out["mediaType"] = v["data"][0]["media_type"].clone();
                Ok((out, v["usage"]["cost"].as_f64(), None))
            }
            Step::AiVideoStart(video) => {
                let mut body = json!({ "model": model_of(step), "prompt": video.prompt });
                if let Some(d) = video.duration {
                    body["duration"] = json!(d);
                }
                if let Some(r) = &video.resolution {
                    body["resolution"] = json!(r);
                }
                if let Some(a) = &video.aspect_ratio {
                    body["aspect_ratio"] = json!(a);
                }
                let (status, bytes) = self.openrouter(key, Method::Post, &self.api("videos"), Some(&body)).await?;
                if !matches!(status, 200 | 202) {
                    return Err(failure(status, &bytes));
                }
                let v: Value = serde_json::from_slice(&bytes).map_err(|e| StepFail::Retry(format!("OpenRouter videos: {e}")))?;
                let id = v["id"].as_str().map(str::to_string);
                Ok((json!({ "id": v["id"] }), None, id))
            }
            Step::AiVideoPoll { id } => {
                let (status, bytes) = self.openrouter(key, Method::Get, &self.api(&format!("videos/{id}")), None).await?;
                if status != 200 {
                    return Err(failure(status, &bytes));
                }
                let v: Value = serde_json::from_slice(&bytes).map_err(|e| StepFail::Retry(format!("OpenRouter videos: {e}")))?;
                let status = v["status"].as_str().unwrap_or("");
                let end = budget::video_end(status);
                if let Some(end) = end {
                    self.video_done(payer, id, status, end, v["usage"]["cost"].as_f64()).await?;
                }
                let answer = json!({ "status": v["status"], "ended": end.is_some(), "error": v["error"], "urls": v["unsigned_urls"], "usage": v["usage"] });
                Ok((answer, None, None))
            }
            Step::AiVideoSave { id, path, url } => {
                let url = match url {
                    Some(u) if u.starts_with(&self.cfg.openrouter_url) => u.clone(),
                    Some(u) => return Err(permanent(format!("the video is at {u}, outside OpenRouter"))),
                    None => self.api(&format!("videos/{id}/content?index=0")),
                };
                let (status, video) = self.openrouter(key, Method::Get, &url, None).await?;
                if status != 200 {
                    return Err(failure(status, &video));
                }
                if video.len() > MEDIA_MAX_BYTES {
                    return Err(permanent(format!("the video is over {MEDIA_MAX_BYTES} bytes")));
                }
                Ok((self.store_media(run, index, path, video).await?, None, None))
            }
            other => unreachable!("only AI steps are performed here, not {}", other.kind()),
        }
    }
}
