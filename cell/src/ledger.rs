//! The `Ledger` cell: one per billing org, fragment's stand-in for
//! finite.computer's Core billing (FIN-10; docs/finite-integration.md).
//! For now every billing org is a person's own personal org, named
//! `org:` + the hex of their identity (a fragment's owner pays for what
//! it spends, whoever started the run: ROADMAP decision 14).
//!
//! A paid step reserves its worst case here before it runs and settles to
//! the reported cost after, so a month's spend stays within its allowance
//! (the budget plus that month's top-ups); a step that does not fit is
//! refused with `budget_used_up`. Each step is one usage row, keyed by its
//! source reference, so a retried or replayed step is never paid twice: a
//! settled one answers its stored result. The org's own OpenRouter key,
//! minted with the fleet's management key, carries the allowance as its
//! limit: OpenRouter itself stops the org at it.
//!
//! Inner routes (fragments and the router reach them):
//!
//!   POST /reserve      {ref, kind, model?, amount, fragment, run, principal, agent?}
//!                      → {reserved, key} | {replay, result} | 402 budget_used_up
//!   POST /key          → {key}: for steps that cost nothing (a video's polls)
//!   POST /settle       {ref, cost?, result?, video?}   (a video waits for its cost)
//!   POST /settle-video {video, cost}
//!   POST /release      {ref}
//!   GET  /status       → the month (BudgetView) with the newest usage
//!   GET  /usage?period=
//!   POST /top-up       {micros, by}
//!   POST /test         {offsetMs}: dev fleets move this ledger's clock

use fragment_core::budget::{self, Month};
use fragment_core::{npub, secrets};
use fragment_proto::{BudgetView, ErrorCode, UsageRow};
use serde::Deserialize;
use serde_json::{json, Value};
use worker::*;

use crate::config::Config;
use crate::error::{CellError, CellResult};
use crate::js;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS topups (
  id INTEGER PRIMARY KEY AUTOINCREMENT, period TEXT NOT NULL, micros INTEGER NOT NULL, by_whom TEXT NOT NULL, at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS usage (
  ref TEXT PRIMARY KEY, period TEXT NOT NULL, kind TEXT NOT NULL, model TEXT, fragment TEXT NOT NULL, run INTEGER,
  principal TEXT NOT NULL, agent TEXT, video TEXT, reserved INTEGER NOT NULL, cost INTEGER, state TEXT NOT NULL,
  result TEXT, created_at INTEGER NOT NULL, settled_at INTEGER);
CREATE INDEX IF NOT EXISTS usage_period ON usage (period, state);
CREATE INDEX IF NOT EXISTS usage_video ON usage (video) WHERE video IS NOT NULL;
";
const KEY_CALL: std::time::Duration = std::time::Duration::from_secs(30);
/// A top-up at a time, and a month's usage rows listed at most.
const TOPUP_MAX: i64 = 1_000 * budget::USD;
const USAGE_PAGE: i64 = 1_000;

/// A person's personal billing org.
pub fn org_of(identity: &str) -> Option<String> {
    npub::is_identity(identity).then(|| format!("org:{}", &identity[npub::ID_PREFIX.len()..]))
}

pub fn valid_org(org: &str) -> bool {
    org.strip_prefix("org:").is_some_and(|h| npub::is_identity(&format!("{}{h}", npub::ID_PREFIX)))
}

#[durable_object]
pub struct LedgerCell {
    state: State,
    env: Env,
    /// One mint (or limit change) of the org's key at a time.
    keying: futures_util::lock::Mutex<()>,
}

impl DurableObject for LedgerCell {
    fn new(state: State, env: Env) -> Self {
        state.storage().sql().exec(SCHEMA, None).expect("the Ledger schema applies");
        LedgerCell { state, env, keying: futures_util::lock::Mutex::new(()) }
    }

    async fn fetch(&self, req: Request) -> Result<Response> {
        match self.route(req).await {
            Ok(v) => Response::from_json(&v),
            Err(e) => e.response(),
        }
    }
}

#[derive(Deserialize)]
struct Reserve {
    #[serde(rename = "ref")]
    reference: String,
    kind: String,
    model: Option<String>,
    amount: i64,
    fragment: String,
    run: Option<i64>,
    principal: String,
    agent: Option<String>,
}

#[derive(Deserialize)]
struct Settle {
    #[serde(rename = "ref")]
    reference: String,
    cost: Option<i64>,
    result: Option<Value>,
    video: Option<String>,
}

fn from<T: serde::de::DeserializeOwned>(v: Value) -> CellResult<T> {
    serde_json::from_value(v).map_err(|e| CellError::invalid(format!("body: {e}")))
}

impl LedgerCell {
    fn rows(&self, q: &str, binds: Vec<SqlStorageValue>) -> CellResult<Vec<Value>> {
        Ok(self.state.storage().sql().exec(q, binds)?.to_array::<Value>()?)
    }

    fn exec(&self, q: &str, binds: Vec<SqlStorageValue>) -> CellResult<()> {
        self.state.storage().sql().exec(q, binds)?;
        Ok(())
    }

    fn sum(&self, q: &str, binds: Vec<SqlStorageValue>) -> CellResult<i64> {
        Ok(self.rows(q, binds)?.first().and_then(|r| r["n"].as_i64()).unwrap_or(0))
    }

    fn meta(&self, k: &str) -> CellResult<Option<String>> {
        Ok(self.rows("SELECT value FROM meta WHERE key = ?", vec![k.into()])?.first().and_then(|r| r["value"].as_str().map(str::to_string)))
    }

    fn set_meta(&self, k: &str, v: &str) -> CellResult<()> {
        self.exec("INSERT INTO meta (key, value) VALUES (?, ?) ON CONFLICT (key) DO UPDATE SET value = excluded.value", vec![k.into(), v.into()])
    }

    fn org(&self) -> CellResult<String> {
        self.meta("org")?.ok_or_else(|| CellError::host("the ledger does not know its org"))
    }

    /// Now, as this ledger sees it (a dev fleet may move its clock).
    fn now(&self) -> CellResult<i64> {
        let offset: i64 = self.meta("clock_offset")?.and_then(|v| v.parse().ok()).unwrap_or(0);
        Ok(js::now_ms() + offset)
    }

    fn month(&self, period: &str) -> CellResult<Month> {
        let cfg = Config::from_env(&self.env);
        let topped = self.sum("SELECT COALESCE(SUM(micros), 0) AS n FROM topups WHERE period = ?", vec![period.into()])?;
        Ok(Month {
            allowance: cfg.budget_micros + topped,
            spent: self.sum("SELECT COALESCE(SUM(cost), 0) AS n FROM usage WHERE period = ? AND state = 'settled'", vec![period.into()])?,
            reserved: self.sum("SELECT COALESCE(SUM(reserved), 0) AS n FROM usage WHERE period = ? AND state = 'reserved'", vec![period.into()])?,
        })
    }

    fn reserve(&self, b: &Reserve) -> CellResult<Option<Value>> {
        if b.amount <= 0 || b.reference.is_empty() || b.reference.len() > 512 {
            return Err(CellError::invalid("a reservation names its step and a positive amount"));
        }
        if let Some(row) = self.rows("SELECT state, result FROM usage WHERE ref = ?", vec![b.reference.as_str().into()])?.first() {
            return match row["state"].as_str() {
                // the same step again: its result, not another charge
                Some("settled") => Ok(Some(json!({ "replay": true, "result": row["result"].as_str().and_then(|r| serde_json::from_str::<Value>(r).ok()) }))),
                // a retry of a step still holding its reservation
                _ => Ok(None),
            };
        }
        let now = self.now()?;
        let period = budget::period_of(now);
        let month = self.month(&period)?;
        if !month.fits(b.amount) {
            return Err(CellError::new(
                ErrorCode::BudgetUsedUp,
                format!(
                    "budget used up: {} of {} spent in {period} ({} left; this step reserves up to {})",
                    budget::dollars(month.spent + month.reserved),
                    budget::dollars(month.allowance),
                    budget::dollars(month.remaining().max(0)),
                    budget::dollars(b.amount)
                ),
            ));
        }
        let opt = |v: &Option<String>| v.as_deref().map_or(SqlStorageValue::Null, |s| s.into());
        self.exec(
            "INSERT INTO usage (ref, period, kind, model, fragment, run, principal, agent, reserved, state, created_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, 'reserved', ?)",
            vec![
                b.reference.as_str().into(),
                period.into(),
                b.kind.as_str().into(),
                opt(&b.model),
                b.fragment.as_str().into(),
                b.run.map_or(SqlStorageValue::Null, SqlStorageValue::Integer),
                b.principal.as_str().into(),
                opt(&b.agent),
                SqlStorageValue::Integer(b.amount),
                SqlStorageValue::Integer(now),
            ],
        )?;
        Ok(None)
    }

    /// The org's OpenRouter key, minted the first time with the month's
    /// allowance as its monthly limit, and its limit kept to the allowance.
    async fn key(&self) -> CellResult<String> {
        let cfg = Config::from_env(&self.env);
        let management = cfg.openrouter_management.clone().ok_or_else(|| {
            CellError::new(ErrorCode::HostFailed, "this fleet pays for no AI (no OpenRouter management key): set the fragment's own OPENROUTER_API_KEY secret")
        })?;
        let _one = self.keying.lock().await;
        let org = self.org()?;
        let period = budget::period_of(self.now()?);
        let allowance = self.month(&period)?.allowance;
        let usd = allowance as f64 / budget::USD as f64;
        let hosts = cfg.host_secrets()?;
        if let (Some(sealed), Some(hash)) = (self.meta("or_key")?, self.meta("or_hash")?) {
            let set = format!("{period}:{allowance}");
            if self.meta("or_limit")?.as_deref() != Some(set.as_str()) {
                let (status, answer) = openrouter(&cfg.openrouter_url, &management, Method::Patch, &format!("keys/{hash}"), &json!({ "limit": usd })).await?;
                if status != 200 {
                    return Err(CellError::new(ErrorCode::UpstreamFailed, format!("OpenRouter would not change the key's limit ({status}): {answer}")));
                }
                self.set_meta("or_limit", &set)?;
            }
            let opened = secrets::open(&hosts, &org, &sealed).map_err(|e| CellError::host(format!("the org's key: {e}")))?;
            return String::from_utf8(opened.plaintext).map_err(|_| CellError::host("the org's key is not text"));
        }
        let body = json!({ "name": format!("fragment {org}"), "limit": usd, "limit_reset": "monthly", "include_byok_in_limit": false });
        let (status, answer) = openrouter(&cfg.openrouter_url, &management, Method::Post, "keys", &body).await?;
        let (Some(key), Some(hash)) = (answer["key"].as_str(), answer["data"]["hash"].as_str()) else {
            return Err(CellError::new(ErrorCode::UpstreamFailed, format!("OpenRouter would not mint a key ({status})")));
        };
        let sealed = secrets::seal(hosts[0], &org, key.as_bytes(), js::random_bytes()).map_err(|e| CellError::host(e.to_string()))?;
        self.set_meta("or_key", &sealed)?;
        self.set_meta("or_hash", hash)?;
        self.set_meta("or_limit", &format!("{period}:{allowance}"))?;
        Ok(key.to_string())
    }

    fn settle(&self, b: Settle) -> CellResult<Value> {
        let rows = self.rows("SELECT state FROM usage WHERE ref = ?", vec![b.reference.as_str().into()])?;
        match rows.first().and_then(|r| r["state"].as_str()) {
            None => return Err(CellError::new(ErrorCode::NotFound, "no such reservation")),
            Some("settled") => return Ok(json!({ "settled": false })),
            Some(_) => {}
        }
        let result = b.result.map_or(SqlStorageValue::Null, |r| r.to_string().into());
        match (b.cost, &b.video) {
            // a video's cost comes with its last poll
            (None, Some(video)) => {
                self.exec("UPDATE usage SET video = ?, result = ? WHERE ref = ?", vec![video.as_str().into(), result, b.reference.as_str().into()])?;
                Ok(json!({ "settled": false, "waiting": video }))
            }
            (cost, _) => {
                let cost = cost.unwrap_or(0).max(0);
                self.exec(
                    "UPDATE usage SET state = 'settled', cost = ?, result = ?, settled_at = ? WHERE ref = ? AND state = 'reserved'",
                    vec![SqlStorageValue::Integer(cost), result, SqlStorageValue::Integer(self.now()?), b.reference.as_str().into()],
                )?;
                Ok(json!({ "settled": true, "cost": cost }))
            }
        }
    }

    fn view(&self, period: &str, limit: i64) -> CellResult<BudgetView> {
        let cfg = Config::from_env(&self.env);
        let org = self.org()?;
        let month = self.month(period)?;
        let usage = self
            .rows(
                "SELECT * FROM usage WHERE period = ? ORDER BY created_at DESC, ref LIMIT ?",
                vec![period.into(), SqlStorageValue::Integer(limit)],
            )?
            .iter()
            .map(|r| {
                let settled = r["state"] == "settled";
                UsageRow {
                    source_ref: r["ref"].as_str().unwrap_or("").to_string(),
                    agent: r["agent"].as_str().map(str::to_string),
                    billing_org: org.clone(),
                    period: r["period"].as_str().unwrap_or("").to_string(),
                    unit: "usd_micro".into(),
                    quantity: if settled { r["cost"].as_i64().unwrap_or(0) } else { r["reserved"].as_i64().unwrap_or(0) },
                    state: r["state"].as_str().unwrap_or("").to_string(),
                    kind: r["kind"].as_str().unwrap_or("").to_string(),
                    model: r["model"].as_str().map(str::to_string),
                    fragment: r["fragment"].as_str().unwrap_or("").to_string(),
                    principal: r["principal"].as_str().unwrap_or("").to_string(),
                    at: r["created_at"].as_i64().unwrap_or(0),
                }
            })
            .collect();
        Ok(BudgetView {
            billing_org: org,
            period: period.to_string(),
            budget_micros: cfg.budget_micros,
            topped_up_micros: month.allowance - cfg.budget_micros,
            allowance_micros: month.allowance,
            spent_micros: month.spent,
            reserved_micros: month.reserved,
            remaining_micros: month.remaining(),
            warn: month.warn(),
            usage,
        })
    }

    async fn route(&self, mut req: Request) -> CellResult<Value> {
        let url = req.url()?;
        let org = req.headers().get(ORG_HEADER)?.filter(|o| valid_org(o)).ok_or_else(|| CellError::host("a ledger call names its org"))?;
        match self.meta("org")? {
            None => self.set_meta("org", &org)?,
            Some(o) if o != org => return Err(CellError::host("this ledger is another org's")),
            Some(_) => {}
        }
        let body: Value = if req.method() == Method::Get {
            Value::Null
        } else {
            let bytes = req.bytes().await?;
            if bytes.is_empty() { json!({}) } else { serde_json::from_slice(&bytes).map_err(|e| CellError::invalid(format!("body: {e}")))? }
        };
        let to = |v: BudgetView| serde_json::to_value(v).map_err(|e| CellError::host(e.to_string()));
        match (req.method(), url.path()) {
            (Method::Post, "/reserve") => {
                let b: Reserve = from(body)?;
                if let Some(replay) = self.reserve(&b)? {
                    return Ok(replay);
                }
                match self.key().await {
                    Ok(key) => Ok(json!({ "reserved": true, "key": key })),
                    Err(e) => {
                        // no key, no step: the reservation goes back
                        self.exec("DELETE FROM usage WHERE ref = ? AND state = 'reserved'", vec![b.reference.as_str().into()])?;
                        Err(e)
                    }
                }
            }
            (Method::Post, "/key") => Ok(json!({ "key": self.key().await? })),
            (Method::Post, "/settle") => self.settle(from(body)?),
            (Method::Post, "/settle-video") => {
                let video = body["video"].as_str().ok_or_else(|| CellError::invalid("name the video"))?;
                let cost = body["cost"].as_i64().unwrap_or(0).max(0);
                let settled = self.rows(
                    "UPDATE usage SET state = 'settled', cost = ?, settled_at = ? WHERE video = ? AND state = 'reserved' RETURNING ref",
                    vec![SqlStorageValue::Integer(cost), SqlStorageValue::Integer(self.now()?), video.into()],
                )?;
                Ok(json!({ "settled": !settled.is_empty(), "cost": cost }))
            }
            (Method::Post, "/release") => {
                let reference = body["ref"].as_str().ok_or_else(|| CellError::invalid("name the step"))?;
                self.exec("DELETE FROM usage WHERE ref = ? AND state = 'reserved' AND video IS NULL", vec![reference.into()])?;
                Ok(json!({ "released": true }))
            }
            (Method::Get, "/status") => to(self.view(&budget::period_of(self.now()?), 20)?),
            (Method::Get, "/usage") => {
                let period = url.query_pairs().find(|(k, _)| k == "period").map(|(_, v)| v.into_owned()).unwrap_or(budget::period_of(self.now()?));
                to(self.view(&period, USAGE_PAGE)?)
            }
            (Method::Post, "/top-up") => {
                let micros = body["micros"].as_i64().filter(|m| (1..=TOPUP_MAX).contains(m)).ok_or_else(|| CellError::invalid(format!("a top-up is 1 to {} micro-dollars", TOPUP_MAX)))?;
                let by = body["by"].as_str().unwrap_or("");
                let now = self.now()?;
                self.exec(
                    "INSERT INTO topups (period, micros, by_whom, at) VALUES (?, ?, ?, ?)",
                    vec![budget::period_of(now).into(), SqlStorageValue::Integer(micros), by.into(), SqlStorageValue::Integer(now)],
                )?;
                // the key's limit follows (now if there is one, else when it is minted)
                if self.meta("or_key")?.is_some() {
                    self.key().await?;
                }
                to(self.view(&budget::period_of(now), 20)?)
            }
            (Method::Post, "/test") => {
                if !Config::from_env(&self.env).test_hooks {
                    return Err(CellError::new(ErrorCode::NotFound, "no route /test"));
                }
                let offset = body["offsetMs"].as_i64().ok_or_else(|| CellError::invalid("offsetMs"))?;
                self.set_meta("clock_offset", &offset.to_string())?;
                Ok(json!({ "offsetMs": offset, "period": budget::period_of(self.now()?) }))
            }
            (m, p) => Err(CellError::new(ErrorCode::NotFound, format!("no route {} {p}", m.as_ref()))),
        }
    }
}

/// The org a ledger call is for; only platform code sets it.
pub const ORG_HEADER: &str = "x-fragment-org";

/// One call to OpenRouter's key API with the management key.
async fn openrouter(base: &str, management: &str, method: Method, path: &str, body: &Value) -> CellResult<(u16, Value)> {
    let headers = Headers::new();
    headers.set("authorization", &format!("Bearer {management}"))?;
    headers.set("content-type", "application/json")?;
    let mut init = RequestInit::new();
    init.with_method(method).with_headers(headers).with_body(Some(body.to_string().into()));
    let req = Request::new_with_init(&format!("{base}/api/v1/{path}"), &init)?;
    let mut resp = crate::cs::fetch(req, KEY_CALL).await?;
    let status = resp.status_code();
    let answer: Value = resp.json().await.unwrap_or(Value::Null);
    Ok((status, answer))
}

/// Asks an org's ledger (from a fragment or the router). Its refusals pass
/// through; not reaching it is a failure for now (5xx).
pub async fn ask(env: &Env, org: &str, method: Method, path: &str, body: Option<&Value>) -> CellResult<Value> {
    let headers = Headers::new();
    headers.set(ORG_HEADER, org)?;
    headers.set("content-type", "application/json")?;
    let mut init = RequestInit::new();
    init.with_method(method).with_headers(headers);
    if let Some(b) = body {
        init.with_body(Some(b.to_string().into()));
    }
    let req = Request::new_with_init(&format!("https://ledger.internal{path}"), &init)?;
    let mut resp = env.durable_object("LEDGER")?.get_by_name(org)?.fetch_with_request(req).await?;
    let status = resp.status_code();
    let bytes = resp.bytes().await?;
    if status == 200 {
        return serde_json::from_slice(&bytes).map_err(|e| CellError::host(format!("the ledger's answer: {e}")));
    }
    match serde_json::from_slice::<fragment_proto::ErrorBody>(&bytes) {
        Ok(e) => Err(CellError::new(e.error, e.message)),
        Err(_) => Err(CellError::host(format!("the ledger answered {status}"))),
    }
}
