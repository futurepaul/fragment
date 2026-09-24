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

use std::time::Duration;

use base64::Engine;
use fragment_core::{blob, budget};
use fragment_proto::ErrorCode;
use serde_json::{json, Value};
use worker::*;

use crate::error::CellError;
use crate::ledger;
use crate::files::FileWrite;
use crate::fragment::FragmentCell;
use crate::jobs::{permanent, StepFail};
use crate::ops::JOB_ID_PREFIX;

pub const IMAGE_MODEL: &str = "google/gemini-3.1-flash-lite-image";
pub const VIDEO_MODEL: &str = "minimax/hailuo-3-max";
/// A generated file the platform stores (a video is a blob).
const MEDIA_MAX_BYTES: usize = 64 * 1024 * 1024;
const CALL_TIMEOUT: Duration = Duration::from_secs(120);
const KEY_SECRET: &str = "OPENROUTER_API_KEY";

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
    fn step_ref(&self, run: &Value, index: i64) -> Result<String, StepFail> {
        let retry = |e: CellError| StepFail::Retry(e.message);
        Ok(format!("{}@{}/run/{}/step/{index}", self.name().map_err(retry)?, self.must("created_at").map_err(retry)?, run["id"].as_i64().unwrap_or(0)))
    }

    /// Who pays for step `index` of `run`, reserving its worst case when it
    /// is a paid step.
    async fn payer(&self, run: &Value, index: i64, kind: &str, args: &Value) -> Result<Paying, StepFail> {
        if let Some(own) = self.open_secret(KEY_SECRET).await.map_err(|e| StepFail::Retry(e.message))? {
            let own = String::from_utf8(own).map_err(|_| permanent(format!("{KEY_SECRET} is not text")))?;
            return Ok(Paying::Payer(Payer::Own(own.trim().to_string())));
        }
        let owner = self.must("owner").map_err(|e| StepFail::Retry(e.message))?;
        let org = ledger::org_of(&owner).ok_or_else(|| permanent("the fragment's owner has no billing org"))?;
        let Some(amount) = budget::reservation(kind, args) else {
            let v = ledger::ask(&self.env, &org, Method::Post, "/key", Some(&json!({}))).await.map_err(ledger_fail)?;
            let key = v["key"].as_str().ok_or_else(|| StepFail::Retry("the ledger answered no key".into()))?.to_string();
            return Ok(Paying::Payer(Payer::Org { org, key, reference: None }));
        };
        let reference = self.step_ref(run, index)?;
        let principal = run["principal"].as_str().unwrap_or("").to_string();
        let agent = self
            .rows("SELECT principal FROM members WHERE principal = ? AND kind = 'agent'", vec![principal.as_str().into()])
            .map_err(|e| StepFail::Retry(e.message))?
            .first()
            .map(|_| principal.clone());
        let model = match kind {
            "ai.image" => args["model"].as_str().unwrap_or(IMAGE_MODEL),
            "ai.video.start" => args["model"].as_str().unwrap_or(VIDEO_MODEL),
            _ => args["model"].as_str().unwrap_or(""),
        };
        let body = json!({
            "ref": reference, "kind": kind, "model": model, "amount": amount, "fragment": self.name().map_err(|e| StepFail::Retry(e.message))?,
            "run": run["id"], "principal": principal, "agent": agent,
        });
        let v = ledger::ask(&self.env, &org, Method::Post, "/reserve", Some(&body)).await.map_err(ledger_fail)?;
        if v["replay"] == true {
            return Ok(Paying::Replay(v["result"].clone()));
        }
        let key = v["key"].as_str().ok_or_else(|| StepFail::Retry("the ledger answered no key".into()))?.to_string();
        Ok(Paying::Payer(Payer::Org { org, key, reference: Some(reference) }))
    }

    /// A paid step's answer: settled to its cost (a video's comes with its
    /// last poll), and the cost recorded on its run.
    async fn settle(&self, run: &Value, payer: &Payer, cost_usd: Option<f64>, result: &Value, video: Option<&str>) -> Result<(), StepFail> {
        let Payer::Org { org, reference: Some(reference), .. } = payer else { return Ok(()) };
        let cost = match video {
            Some(_) => None,
            None => Some(budget::micros(cost_usd.unwrap_or(0.0))),
        };
        let body = json!({ "ref": reference, "cost": cost, "result": result, "video": video });
        let mut tries = 0;
        loop {
            match ledger::ask(&self.env, org, Method::Post, "/settle", Some(&body)).await {
                Ok(_) => break,
                Err(e) if tries < 2 && e.code != ErrorCode::NotFound => tries += 1,
                Err(e) => return Err(StepFail::Retry(format!("settling the step's cost: {}", e.message))),
            }
        }
        let _ = self.exec(
            "INSERT OR IGNORE INTO spend (ref, run, micros, at, video) VALUES (?, ?, ?, ?, ?)",
            vec![
                reference.as_str().into(),
                SqlStorageValue::Integer(run["id"].as_i64().unwrap_or(0)),
                SqlStorageValue::Integer(cost.unwrap_or(0)),
                SqlStorageValue::Integer(crate::js::now_ms()),
                video.map_or(SqlStorageValue::Null, |v| v.into()),
            ],
        );
        Ok(())
    }

    /// A reservation a failed step no longer needs.
    async fn release(&self, payer: &Payer) {
        if let Payer::Org { org, reference: Some(reference), .. } = payer {
            let _ = ledger::ask(&self.env, org, Method::Post, "/release", Some(&json!({ "ref": reference }))).await;
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
        let mut resp = crate::cs::fetch(req, CALL_TIMEOUT).await.map_err(|e| StepFail::Retry(format!("OpenRouter: {}", e.message)))?;
        let status = resp.status_code();
        let bytes = resp.bytes().await.map_err(|e| StepFail::Retry(format!("OpenRouter: {e}")))?;
        Ok((status, bytes))
    }

    fn api(&self, path: &str) -> String {
        format!("{}/api/v1/{path}", self.cfg.openrouter_url)
    }

    /// Writes generated bytes to `path` on `main` once per step: in git, or
    /// as a blob and its pointer when 1 MiB or more.
    async fn store_media(&self, run: &Value, index: i64, path: &str, bytes: Vec<u8>) -> Result<Value, StepFail> {
        let retry = |e: CellError| StepFail::Retry(e.message);
        let sha = blob::sha256_hex(&bytes);
        let size = bytes.len() as u64;
        let content = if bytes.len() >= blob::BLOB_MIN_BYTES {
            self.put_blob_bytes(&sha, bytes).await.map_err(retry)?;
            blob::pointer(&sha, size).into_bytes()
        } else {
            bytes
        };
        let run_id = run["id"].as_i64().unwrap_or(0);
        let key = format!("{JOB_ID_PREFIX}{run_id}:{index}");
        let message = format!("{} run {run_id}: generated {path}", run["op"].as_str().unwrap_or(""));
        let writes = [FileWrite { path: path.to_string(), bytes: Some(content) }];
        let depth = run["depth"].as_u64().unwrap_or(0) as u32;
        self.commit_files(&key, &writes, &Default::default(), &message, run["principal"].as_str().unwrap_or(""), depth).await.map_err(retry)?;
        Ok(json!({ "path": path, "size": size, "sha256": sha }))
    }

    /// `job.ai.*` steps, paid by whoever pays (above).
    pub(crate) async fn step_ai(&self, run: &Value, index: i64, kind: &str, args: &Value) -> Result<Value, StepFail> {
        if !matches!(kind, "ai.text" | "ai.image" | "ai.video.start" | "ai.video.poll" | "ai.video.save") {
            return Err(permanent(format!("unknown step kind {kind:?}")));
        }
        let payer = match self.payer(run, index, kind, args).await? {
            Paying::Replay(result) => return Ok(result),
            Paying::Payer(p) => p,
        };
        let answer = self.ai_call(run, index, kind, args, &payer).await;
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

    /// Settles a video's cost when its last poll reports it.
    async fn video_done(&self, payer: &Payer, id: &str, cost_usd: Option<f64>) -> Result<(), StepFail> {
        let Payer::Org { org, .. } = payer else { return Ok(()) };
        let cost = budget::micros(cost_usd.unwrap_or(0.0));
        ledger::ask(&self.env, org, Method::Post, "/settle-video", Some(&json!({ "video": id, "cost": cost }))).await.map_err(ledger_fail)?;
        let _ = self.exec("UPDATE spend SET micros = ? WHERE video = ?", vec![SqlStorageValue::Integer(cost), id.into()]);
        Ok(())
    }

    /// One `job.ai.*` step's call: its result, the cost OpenRouter reported,
    /// and the video it started (whose cost comes later).
    async fn ai_call(&self, run: &Value, index: i64, kind: &str, args: &Value, payer: &Payer) -> Result<(Value, Option<f64>, Option<String>), StepFail> {
        let key = payer.key();
        match kind {
            "ai.text" => {
                let model = args["model"].as_str().ok_or_else(|| permanent("ai.text needs a model (an OpenRouter model id)"))?;
                let messages = match (&args["messages"], args["prompt"].as_str()) {
                    (Value::Array(m), _) => Value::Array(m.clone()),
                    (_, Some(p)) => json!([{ "role": "user", "content": p }]),
                    _ => return Err(permanent("ai.text needs messages or a prompt")),
                };
                let mut body = json!({ "model": model, "messages": messages });
                // OpenRouter's reasoning control, as the job gave it (a
                // reasoning model can spend a small cap thinking)
                if args["reasoning"].is_object() {
                    body["reasoning"] = args["reasoning"].clone();
                }
                if let Some(n) = args["max_tokens"].as_u64() {
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
            "ai.image" => {
                let path = args["path"].as_str().unwrap_or("");
                let mut body = json!({ "model": args["model"].as_str().unwrap_or(IMAGE_MODEL), "prompt": args["prompt"] });
                if let Some(a) = args["aspect_ratio"].as_str() {
                    body["aspect_ratio"] = json!(a);
                }
                let (status, bytes) = self.openrouter(key, Method::Post, &self.api("images"), Some(&body)).await?;
                if status != 200 {
                    return Err(failure(status, &bytes));
                }
                let v: Value = serde_json::from_slice(&bytes).map_err(|e| StepFail::Retry(format!("OpenRouter images: {e}")))?;
                let b64 = v["data"][0]["b64_json"].as_str().ok_or_else(|| StepFail::Retry("OpenRouter images: no image in the answer".into()))?;
                let image = base64::engine::general_purpose::STANDARD.decode(b64).map_err(|e| StepFail::Retry(format!("OpenRouter images: {e}")))?;
                if image.len() > MEDIA_MAX_BYTES {
                    return Err(permanent(format!("the image is over {MEDIA_MAX_BYTES} bytes")));
                }
                let mut out = self.store_media(run, index, path, image).await?;
                out["mediaType"] = v["data"][0]["media_type"].clone();
                Ok((out, v["usage"]["cost"].as_f64(), None))
            }
            "ai.video.start" => {
                let mut body = json!({ "model": args["model"].as_str().unwrap_or(VIDEO_MODEL), "prompt": args["prompt"] });
                for k in ["duration", "resolution", "aspect_ratio"] {
                    if !args[k].is_null() {
                        body[k] = args[k].clone();
                    }
                }
                let (status, bytes) = self.openrouter(key, Method::Post, &self.api("videos"), Some(&body)).await?;
                if !matches!(status, 200 | 202) {
                    return Err(failure(status, &bytes));
                }
                let v: Value = serde_json::from_slice(&bytes).map_err(|e| StepFail::Retry(format!("OpenRouter videos: {e}")))?;
                let id = v["id"].as_str().map(str::to_string);
                Ok((json!({ "id": v["id"] }), None, id))
            }
            "ai.video.poll" => {
                let id = args["id"].as_str().ok_or_else(|| permanent("ai.video.poll needs the job id"))?;
                let (status, bytes) = self.openrouter(key, Method::Get, &self.api(&format!("videos/{id}")), None).await?;
                if status != 200 {
                    return Err(failure(status, &bytes));
                }
                let v: Value = serde_json::from_slice(&bytes).map_err(|e| StepFail::Retry(format!("OpenRouter videos: {e}")))?;
                if matches!(v["status"].as_str(), Some("completed" | "failed")) {
                    self.video_done(payer, id, v["usage"]["cost"].as_f64()).await?;
                }
                Ok((json!({ "status": v["status"], "error": v["error"], "urls": v["unsigned_urls"], "usage": v["usage"] }), None, None))
            }
            "ai.video.save" => {
                let (id, path) = (args["id"].as_str().unwrap_or(""), args["path"].as_str().unwrap_or(""));
                let url = match args["url"].as_str() {
                    Some(u) if u.starts_with(&self.cfg.openrouter_url) => u.to_string(),
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
            other => Err(permanent(format!("unknown step kind {other:?}"))),
        }
    }
}
