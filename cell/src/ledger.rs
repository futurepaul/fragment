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
//! Inner routes (fragments and the router reach them through `ask`): each
//! is a POST of a `Route`'s request type, answered with its `Route::Answer`,
//! so both ends are the same Rust types.

use fragment_core::budget::{self, Month};
use fragment_core::npub;
use fragment_proto::{BudgetView, ErrorCode, UsageRow, UsageState, UsageUnit};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use worker::*;

use crate::config::Config;
use crate::error::{CellError, CellResult};
use crate::{js, keys};

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
/// A top-up at a time, and a month's usage rows listed at most (in
/// `/usage`; `/status` lists the newest few).
const TOPUP_MAX: i64 = 1_000 * budget::USD;
const USAGE_PAGE: i64 = 1_000;
const STATUS_USAGE: i64 = 20;

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
    /// The isolate's settings (config.rs: built once per isolate).
    cfg: &'static Config,
    /// One mint (or limit change) of the org's key at a time.
    keying: futures_util::lock::Mutex<()>,
}

impl DurableObject for LedgerCell {
    fn new(state: State, env: Env) -> Self {
        state.storage().sql().exec(SCHEMA, None).expect("the Ledger schema applies");
        let cfg = Config::from_env(&env);
        LedgerCell { state, env, cfg, keying: futures_util::lock::Mutex::new(()) }
    }

    async fn fetch(&self, req: Request) -> Result<Response> {
        match self.route(req).await {
            Ok(v) => Response::from_json(&v),
            Err(e) => e.response(),
        }
    }
}

/// One of the ledger's routes: the request a caller sends and the answer
/// it gets back.
pub trait Route: Serialize + DeserializeOwned {
    const PATH: &'static str;
    type Answer: Serialize + DeserializeOwned;
}

/// A paid step's worst case, held before the step runs.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reserve {
    /// The step's source reference (ai.rs, `step_ref`).
    #[serde(rename = "ref")]
    pub reference: String,
    /// The step's kind (`ai.text`, …).
    pub kind: String,
    pub model: Option<String>,
    pub amount: i64,
    pub fragment: String,
    pub run: i64,
    /// Who started the run (it may be a visitor; the owner pays).
    pub principal: String,
    /// The principal, when it is an agent member.
    pub agent: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "answer", rename_all = "snake_case")]
pub enum Reserved {
    /// Held (now, or by this step's earlier try): run it with the org's key.
    Held { key: String },
    /// The step settled before (a retry or a replay): its stored result,
    /// not a second charge.
    Replay { result: Value },
}

impl Route for Reserve {
    const PATH: &'static str = "/reserve";
    type Answer = Reserved;
}

/// A span of a computer's (computer.rs), held before the computer is: as
/// `reserve` holds a paid step, refused past the month, but without
/// `reserve`'s OpenRouter key (a computer's awake time needs none, on a
/// fleet with no AI too). It is then settled (`settle`) once the computer
/// was held, or given back (`release`) when it never was; the same
/// reference again holds nothing more.
#[derive(Serialize, Deserialize)]
#[serde(transparent)]
pub struct Hold(pub Reserve);

impl Route for Hold {
    const PATH: &'static str = "/hold";
    type Answer = ();
}

/// The org's OpenRouter key, for a step that costs nothing (a video's polls).
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Key {}

#[derive(Serialize, Deserialize)]
pub struct KeyAnswer {
    pub key: String,
}

impl Route for Key {
    const PATH: &'static str = "/key";
    type Answer = KeyAnswer;
}

/// A paid step's end: its cost (`None`: charged its reservation) and result.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settle {
    #[serde(rename = "ref")]
    pub reference: String,
    pub cost: Option<i64>,
    pub result: Value,
    /// A video's OpenRouter id: its cost comes with its last poll (`SettleVideo`).
    pub video: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum Settlement {
    Now { cost: i64 },
    /// A retry of a step that settled: the cost it settled at.
    Before { cost: i64 },
    /// A video, still holding its reservation until its cost comes.
    Waiting { video: String },
}

impl Settlement {
    /// What the step has been charged so far.
    pub fn charged(&self) -> i64 {
        match self {
            Settlement::Now { cost } | Settlement::Before { cost } => *cost,
            Settlement::Waiting { .. } => 0,
        }
    }
}

impl Route for Settle {
    const PATH: &'static str = "/settle";
    type Answer = Settlement;
}

/// A video's cost, from its last poll (`None`: charged its reservation).
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SettleVideo {
    pub video: String,
    pub cost: Option<i64>,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum VideoSettlement {
    Now { cost: i64 },
    /// A poll step run again: the cost it settled at.
    Before { cost: i64 },
    /// Nothing holds for this video (its held run gave the reservation back).
    NoReservation,
}

impl Route for SettleVideo {
    const PATH: &'static str = "/settle-video";
    type Answer = VideoSettlement;
}

/// A reservation a step no longer needs.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Release {
    #[serde(rename = "ref")]
    pub reference: String,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum ReleaseAnswer {
    Released,
    /// It settled meanwhile: its cost stands.
    Settled { cost: i64 },
    /// Nothing is reserved under it (released before, or never reserved).
    Gone,
}

impl Route for Release {
    const PATH: &'static str = "/release";
    type Answer = ReleaseAnswer;
}

/// This month, with its newest usage.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Status {}

impl Route for Status {
    const PATH: &'static str = "/status";
    type Answer = BudgetView;
}

/// A month (`YYYY-MM`, the router checks it; default this one) with every usage row.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Usage {
    pub period: Option<String>,
}

impl Route for Usage {
    const PATH: &'static str = "/usage";
    type Answer = BudgetView;
}

/// More allowance this month, by one of the fleet's operators.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TopUp {
    pub micros: i64,
    /// The operator's identity.
    pub by: String,
}

impl Route for TopUp {
    const PATH: &'static str = "/top-up";
    type Answer = BudgetView;
}

/// Dev fleets only: move this ledger's clock (to cross a month).
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SetClock {
    pub offset_ms: i64,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Clock {
    pub offset_ms: i64,
    pub period: String,
}

impl Route for SetClock {
    const PATH: &'static str = "/test";
    type Answer = Clock;
}

fn decode<R: Route>(body: &[u8]) -> CellResult<R> {
    serde_json::from_slice(body).map_err(|e| CellError::invalid(format!("{}: {e}", R::PATH)))
}

/// A route's answer, typed by its route.
fn reply<R: Route>(answer: CellResult<R::Answer>) -> CellResult<Value> {
    serde_json::to_value(answer?).map_err(|e| CellError::host(e.to_string()))
}

impl LedgerCell {
    fn rows(&self, q: &str, binds: Vec<SqlStorageValue>) -> CellResult<Vec<Value>> {
        Ok(self.state.storage().sql().exec(q, binds)?.to_array::<Value>()?)
    }

    fn exec(&self, q: &str, binds: Vec<SqlStorageValue>) -> CellResult<()> {
        self.state.storage().sql().exec(q, binds)?;
        Ok(())
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

    /// A month's allowance, spent, and reserved, in one statement (each sum
    /// an index range of its own table).
    fn month(&self, period: &str) -> CellResult<Month> {
        let rows = self.rows(
            "SELECT (SELECT COALESCE(SUM(micros), 0) FROM topups WHERE period = ?) AS topped,
                    (SELECT COALESCE(SUM(cost), 0) FROM usage WHERE period = ? AND state = 'settled') AS spent,
                    (SELECT COALESCE(SUM(reserved), 0) FROM usage WHERE period = ? AND state = 'reserved') AS reserved",
            vec![period.into(), period.into(), period.into()],
        )?;
        let row = rows.first().expect("a SELECT of sums answers one row");
        let sum = |k: &str| row[k].as_i64().unwrap_or_else(|| panic!("a COALESCE(SUM(…), 0) answers an integer ({k})"));
        Ok(Month { allowance: self.cfg.budget_micros + sum("topped"), spent: sum("spent"), reserved: sum("reserved") })
    }

    /// Holds a step's reservation; answers the stored result instead when
    /// the step settled before.
    async fn reserve(&self, b: Reserve) -> CellResult<Reserved> {
        if b.amount <= 0 || b.reference.is_empty() || b.reference.len() > 512 {
            return Err(CellError::invalid("a reservation names its step and a positive amount"));
        }
        let prior = self.rows("SELECT state, result FROM usage WHERE ref = ?", vec![b.reference.as_str().into()])?;
        match prior.first().map(|row| (row, row["state"].as_str().and_then(UsageState::parse).expect("usage.state is one the ledger wrote"))) {
            // the same step again: its result, not another charge
            Some((row, UsageState::Settled)) => {
                let result = row["result"].as_str().map_or(Value::Null, |r| serde_json::from_str(r).expect("usage.result is JSON the ledger stored"));
                return Ok(Reserved::Replay { result });
            }
            // a retry of a step still holding its reservation
            Some((_, UsageState::Reserved)) => {}
            None => self.hold(&b)?,
        }
        match self.key().await {
            Ok(key) => Ok(Reserved::Held { key }),
            Err(e) => {
                // no key, no step: the reservation goes back
                self.exec("DELETE FROM usage WHERE ref = ? AND state = 'reserved'", vec![b.reference.as_str().into()])?;
                Err(e)
            }
        }
    }

    /// A new reservation, if the month can cover it.
    fn hold(&self, b: &Reserve) -> CellResult<()> {
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
                SqlStorageValue::Integer(b.run),
                b.principal.as_str().into(),
                opt(&b.agent),
                SqlStorageValue::Integer(b.amount),
                SqlStorageValue::Integer(now),
            ],
        )
    }

    /// A settled row's cost: every settle writes one.
    fn settled_cost(row: &Value) -> i64 {
        row["cost"].as_i64().expect("a settled usage row has its cost")
    }

    /// The org's OpenRouter key, minted the first time with the month's
    /// allowance as its monthly limit, and its limit kept to the allowance.
    async fn key(&self) -> CellResult<String> {
        // the management key is the node's: KEYS makes these calls for a Ledger only
        let no_ai = || {
            CellError::invalid("set the fragment's OPENROUTER_API_KEY secret to use OpenRouter (`fragment secret set`): this fleet pays for no AI")
        };
        let _one = self.keying.lock().await;
        let org = self.org()?;
        // Read under the lock, not handed in by a reservation: one that
        // waited here behind a top-up's key change would set its limit back.
        let period = budget::period_of(self.now()?);
        let allowance = self.month(&period)?.allowance;
        let usd = allowance as f64 / budget::USD as f64;
        if let (Some(sealed), Some(hash)) = (self.meta("or_key")?, self.meta("or_hash")?) {
            let set = format!("{period}:{allowance}");
            if self.meta("or_limit")?.as_deref() != Some(set.as_str()) {
                let (status, answer) = keys::openrouter_keys(&self.env, "PATCH", Some(&hash), Some(&json!({ "limit": usd }))).await?.ok_or_else(no_ai)?;
                if status != 200 {
                    return Err(CellError::new(ErrorCode::UpstreamFailed, format!("OpenRouter would not change the key's limit ({status}): {answer}")));
                }
                self.set_meta("or_limit", &set)?;
            }
            let opened = keys::open(&self.env, &sealed, &org).await.map_err(|e| CellError::host(format!("the org's key: {}", e.message)))?;
            if let Some(fresh) = opened.resealed {
                self.set_meta("or_key", &fresh)?;
            }
            return String::from_utf8(opened.plaintext).map_err(|_| CellError::host("the org's key is not text"));
        }
        let body = json!({ "name": format!("fragment {org}"), "limit": usd, "limit_reset": "monthly", "include_byok_in_limit": false });
        let (status, answer) = keys::openrouter_keys(&self.env, "POST", None, Some(&body)).await?.ok_or_else(no_ai)?;
        let (Some(key), Some(hash)) = (answer["key"].as_str(), answer["data"]["hash"].as_str()) else {
            return Err(CellError::new(ErrorCode::UpstreamFailed, format!("OpenRouter would not mint a key ({status})")));
        };
        let sealed = keys::seal(&self.env, key.as_bytes()).await?;
        self.set_meta("or_key", &sealed)?;
        self.set_meta("or_hash", hash)?;
        self.set_meta("or_limit", &format!("{period}:{allowance}"))?;
        Ok(key.to_string())
    }

    fn hold_span(&self, Hold(b): Hold) -> CellResult<()> {
        if b.amount <= 0 || b.reference.is_empty() || b.reference.len() > 512 {
            return Err(CellError::invalid("a hold names its span and a positive amount"));
        }
        if self.rows("SELECT state FROM usage WHERE ref = ?", vec![b.reference.as_str().into()])?.is_empty() {
            self.hold(&b)?;
        }
        Ok(())
    }

    fn settle(&self, b: Settle) -> CellResult<Settlement> {
        let rows = self.rows("SELECT state, reserved, cost FROM usage WHERE ref = ?", vec![b.reference.as_str().into()])?;
        let Some(row) = rows.first() else { return Err(CellError::new(ErrorCode::NotFound, "no such reservation")) };
        if row["state"].as_str().and_then(UsageState::parse).expect("usage.state is one the ledger wrote") == UsageState::Settled {
            return Ok(Settlement::Before { cost: Self::settled_cost(row) });
        }
        let reserved = row["reserved"].as_i64().expect("usage.reserved is INTEGER");
        let result = SqlStorageValue::from(b.result.to_string());
        match (b.cost, b.video) {
            // a video's cost comes with its last poll
            (None, Some(video)) => {
                self.exec("UPDATE usage SET video = ?, result = ? WHERE ref = ?", vec![video.as_str().into(), result, b.reference.as_str().into()])?;
                Ok(Settlement::Waiting { video })
            }
            (cost, _) => {
                // a step whose answer named no cost is charged its worst
                // case: the money path fails closed, never at zero
                let cost = cost.map_or(reserved, |c| c.max(0));
                self.exec(
                    "UPDATE usage SET state = 'settled', cost = ?, result = ?, settled_at = ? WHERE ref = ? AND state = 'reserved'",
                    vec![SqlStorageValue::Integer(cost), result, SqlStorageValue::Integer(self.now()?), b.reference.as_str().into()],
                )?;
                Ok(Settlement::Now { cost })
            }
        }
    }

    fn settle_video(&self, b: SettleVideo) -> CellResult<VideoSettlement> {
        // without a cost, the video is charged its reservation (it fails closed)
        let cost = b.cost.map_or(SqlStorageValue::Null, |c| SqlStorageValue::Integer(c.max(0)));
        let settled = self.rows(
            "UPDATE usage SET state = 'settled', cost = COALESCE(?, reserved), settled_at = ? WHERE video = ? AND state = 'reserved' RETURNING cost",
            vec![cost, SqlStorageValue::Integer(self.now()?), b.video.as_str().into()],
        )?;
        if let Some(row) = settled.first() {
            return Ok(VideoSettlement::Now { cost: Self::settled_cost(row) });
        }
        // settled before (a poll step run again): the cost it settled at
        let before = self.rows("SELECT cost FROM usage WHERE video = ? AND state = 'settled'", vec![b.video.as_str().into()])?;
        Ok(before.first().map_or(VideoSettlement::NoReservation, |r| VideoSettlement::Before { cost: Self::settled_cost(r) }))
    }

    fn release(&self, b: Release) -> CellResult<ReleaseAnswer> {
        let released = self.rows("DELETE FROM usage WHERE ref = ? AND state = 'reserved' RETURNING ref", vec![b.reference.as_str().into()])?;
        if !released.is_empty() {
            return Ok(ReleaseAnswer::Released);
        }
        let settled = self.rows("SELECT cost FROM usage WHERE ref = ? AND state = 'settled'", vec![b.reference.as_str().into()])?;
        Ok(settled.first().map_or(ReleaseAnswer::Gone, |r| ReleaseAnswer::Settled { cost: Self::settled_cost(r) }))
    }

    async fn top_up(&self, t: TopUp) -> CellResult<BudgetView> {
        if !(1..=TOPUP_MAX).contains(&t.micros) {
            return Err(CellError::invalid(format!("a top-up is 1 to {TOPUP_MAX} micro-dollars")));
        }
        let now = self.now()?;
        self.exec(
            "INSERT INTO topups (period, micros, by_whom, at) VALUES (?, ?, ?, ?)",
            vec![budget::period_of(now).into(), SqlStorageValue::Integer(t.micros), t.by.as_str().into(), SqlStorageValue::Integer(now)],
        )?;
        // the key's limit follows (now if there is one, else when it is minted)
        if self.meta("or_key")?.is_some() {
            self.key().await?;
        }
        self.view(&budget::period_of(now), STATUS_USAGE)
    }

    fn view(&self, period: &str, limit: i64) -> CellResult<BudgetView> {
        let cfg = self.cfg;
        let org = self.org()?;
        let month = self.month(period)?;
        let usage = self
            .rows(
                "SELECT * FROM usage WHERE period = ? ORDER BY created_at DESC, ref LIMIT ?",
                vec![period.into(), SqlStorageValue::Integer(limit)],
            )?
            .iter()
            .map(|r| {
                // only the ledger writes usage: a NOT NULL column that is missing is corruption
                let text = |k: &str| r[k].as_str().unwrap_or_else(|| panic!("usage.{k} is TEXT NOT NULL")).to_string();
                let state = r["state"].as_str().and_then(UsageState::parse).expect("usage.state is one the ledger wrote");
                let quantity = match state {
                    UsageState::Settled => r["cost"].as_i64().expect("a settled usage row has its cost"),
                    UsageState::Reserved => r["reserved"].as_i64().expect("usage.reserved is INTEGER"),
                };
                UsageRow {
                    source_ref: text("ref"),
                    agent: r["agent"].as_str().map(str::to_string),
                    billing_org: org.clone(),
                    period: text("period"),
                    unit: UsageUnit::UsdMicro,
                    quantity,
                    state,
                    kind: text("kind"),
                    model: r["model"].as_str().map(str::to_string),
                    fragment: text("fragment"),
                    principal: text("principal"),
                    at: r["created_at"].as_i64().expect("usage.created_at is INTEGER NOT NULL"),
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
        let org = req.headers().get(ORG_HEADER)?.filter(|o| valid_org(o)).ok_or_else(|| CellError::host("a ledger call names its org"))?;
        match self.meta("org")? {
            None => self.set_meta("org", &org)?,
            Some(o) if o != org => return Err(CellError::host("this ledger is another org's")),
            Some(_) => {}
        }
        let path = req.path();
        if req.method() != Method::Post {
            return Err(CellError::new(ErrorCode::NotFound, format!("no route {} {path}", req.method().as_ref())));
        }
        let body = req.bytes().await?;
        match path.as_str() {
            Reserve::PATH => reply::<Reserve>(self.reserve(decode(&body)?).await),
            Key::PATH => {
                let Key {} = decode(&body)?;
                reply::<Key>(self.key().await.map(|key| KeyAnswer { key }))
            }
            Settle::PATH => reply::<Settle>(self.settle(decode(&body)?)),
            Hold::PATH => reply::<Hold>(self.hold_span(decode(&body)?)),
            SettleVideo::PATH => reply::<SettleVideo>(self.settle_video(decode(&body)?)),
            Release::PATH => reply::<Release>(self.release(decode(&body)?)),
            Status::PATH => {
                let Status {} = decode(&body)?;
                reply::<Status>(self.view(&budget::period_of(self.now()?), STATUS_USAGE))
            }
            Usage::PATH => {
                let usage: Usage = decode(&body)?;
                let period = match usage.period {
                    Some(p) => p,
                    None => budget::period_of(self.now()?),
                };
                reply::<Usage>(self.view(&period, USAGE_PAGE))
            }
            TopUp::PATH => reply::<TopUp>(self.top_up(decode(&body)?).await),
            SetClock::PATH if self.cfg.test_hooks => {
                let clock: SetClock = decode(&body)?;
                self.set_meta("clock_offset", &clock.offset_ms.to_string())?;
                reply::<SetClock>(Ok(Clock { offset_ms: clock.offset_ms, period: budget::period_of(self.now()?) }))
            }
            p => Err(CellError::new(ErrorCode::NotFound, format!("no route {p}"))),
        }
    }
}

/// The org a ledger call is for; only platform code sets it.
pub const ORG_HEADER: &str = "x-fragment-org";

/// Asks an org's ledger (from a fragment or the router). Its refusals pass
/// through; not reaching it is a failure for now (5xx).
pub async fn ask<R: Route>(env: &Env, org: &str, request: &R) -> CellResult<R::Answer> {
    let headers = Headers::new();
    headers.set(ORG_HEADER, org)?;
    headers.set("content-type", "application/json")?;
    let body = serde_json::to_string(request).map_err(|e| CellError::host(format!("a ledger request: {e}")))?;
    let mut init = RequestInit::new();
    init.with_method(Method::Post).with_headers(headers).with_body(Some(body.into()));
    let req = Request::new_with_init(&format!("https://ledger.internal{}", R::PATH), &init)?;
    let mut resp = env.durable_object("LEDGER")?.get_by_name(org)?.fetch_with_request(req).await?;
    let status = resp.status_code();
    let bytes = resp.bytes().await?;
    if status == 200 {
        return serde_json::from_slice(&bytes).map_err(|e| CellError::host(format!("the ledger's answer to {}: {e}", R::PATH)));
    }
    match serde_json::from_slice::<fragment_proto::ErrorBody>(&bytes) {
        Ok(e) => Err(CellError::new(e.error, e.message)),
        Err(_) => Err(CellError::host(format!("the ledger answered {status}"))),
    }
}
