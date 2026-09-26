//! The `Computer` cell: one per fragment whose live `fragment.json`
//! declares `"computer": {}` (docs/computers.md), named by the fragment,
//! holding its Sprite's life. Its Sprite is `KEYS`'s to reach (the fleet's
//! token stays in the node, and a cell reaches only the Sprite named for
//! it: crates/native `sprite_of`).
//!
//! - A deploy that declares it makes the Sprite, which installs the
//!   released CLI with the one-line install and pairs itself as a computer
//!   its fragment's owner owns (`fragment login --pair`, with a single-use
//!   token from the registry on its stdin: its key never leaves it). The
//!   router makes the new computer an editor of the fragment and says so
//!   here (`Paired`).
//! - It is held awake (the Sprite's Tasks API, `sprite-env`) while
//!   something needs it (`Wake`: a page viewer; later, a job step), and
//!   let sleep `FRAGMENT_COMPUTER_IDLE_S` after the last viewer leaves.
//!   Each tick it is awake is charged before it is held, at list price
//!   (`budget::computer`), to its owner; so is its disk asleep, when it
//!   next wakes. A tick that does not fit the budget lets it sleep.
//! - A deploy that drops the block lets it sleep and keeps it (cold);
//!   `fragment computers rm` destroys it. Nothing destroys it on its own.
//! - A job's command (`job.computer.exec`) wakes it as a page does, then
//!   runs detached on it, journaled on its disk (fragment_core::computer).
//!
//! Every step is the alarm's, from the row below: a failed one is retried
//! with backoff, and its fragment's `events` says why.

use std::time::Duration;

use fragment_core::computer::{self as exec, ExecState};
use fragment_core::steps::ComputerExec;
use fragment_core::budget;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use worker::*;

use crate::config::Config;
use crate::error::{CellError, CellResult};
use crate::fragment::{json_response, FragmentCell, MetaKey};
use crate::jobs::{permanent, RunRow, StepFail};
use crate::ledger::{self, Charge, Reserve};
use crate::registry::calls;
use crate::{js, keys};

/// The marker of a computer's calls into its fragment (`computer/…`).
pub(crate) const HEADER: &str = "x-fragment-computer";
/// The longest wait before a failed step is tried again.
const RETRY_MAX_MS: i64 = 3_600_000;
/// A job's command waits this long for its computer to wake (its first
/// tick paid), looking this often, before its step is tried again.
const WAKE_WAIT_MS: i64 = 20_000;
const WAKE_LOOK_MS: u64 = 250;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS computer (
  one INTEGER PRIMARY KEY CHECK (one = 1), fragment TEXT NOT NULL, owner TEXT NOT NULL, declared INTEGER NOT NULL,
  phase TEXT NOT NULL, awake_until INTEGER NOT NULL, held INTEGER NOT NULL, billed_to INTEGER NOT NULL,
  asleep_since INTEGER, disk_bytes INTEGER NOT NULL, tries INTEGER NOT NULL);";

/// What is asked of a computer.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum Ask {
    /// A deploy of `fragment` (its owner's) declares a computer, or no longer does.
    Declare { fragment: String, owner: String, declared: bool },
    /// Something needs it awake until then (ms).
    Wake { until: i64 },
    /// Its Sprite paired as its computer, now its fragment's editor.
    Paired,
    /// Its owner removed it: the Sprite goes.
    Destroy,
    /// A job's command, `id` its run's and step's: started once.
    Exec { id: String, exec: ComputerExec },
    /// That command's state, waiting on the computer up to `POLL_WAIT_S`.
    ExecPoll { id: String },
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
enum Phase {
    /// No paired Sprite yet: the alarm makes and boots it.
    Boot,
    Ready,
    Destroy,
    Gone,
}

#[derive(Serialize, Deserialize)]
struct Row {
    fragment: String,
    owner: String,
    #[serde(deserialize_with = "flag")]
    declared: bool,
    phase: Phase,
    awake_until: i64,
    #[serde(deserialize_with = "flag")]
    held: bool,
    billed_to: i64,
    asleep_since: Option<i64>,
    disk_bytes: i64,
    tries: i64,
}

/// SQLite's 0 or 1, as a flag.
fn flag<'de, D: serde::Deserializer<'de>>(d: D) -> Result<bool, D::Error> {
    Ok(i64::deserialize(d)? != 0)
}

fn upstream(m: String) -> CellError {
    CellError::new(fragment_proto::ErrorCode::UpstreamFailed, m)
}

/// The Sprite's Tasks API, which holds it awake (`sprite-env`, on it).
fn tasks<'a>(method: &'a str, body: &'a str) -> Vec<&'a str> {
    let mut argv = vec!["sprite-env", "curl", "-s", "-X", method, "-H", "content-type: application/json", "/v1/tasks/fragment"];
    if !body.is_empty() {
        argv.extend(["-d", body]);
    }
    argv
}

/// Asks the computer of `fragment`; its refusal keeps its code.
pub(crate) async fn ask(env: &Env, fragment: &str, what: &Ask) -> CellResult<Value> {
    let body = serde_json::to_string(what).map_err(|e| CellError::host(e.to_string()))?;
    let mut init = RequestInit::new();
    init.with_method(Method::Post).with_body(Some(body.into()));
    let req = Request::new_with_init("https://computer.internal/", &init)?;
    let mut resp = env.durable_object("COMPUTER")?.get_by_name(fragment)?.fetch_with_request(req).await?;
    let (status, bytes) = (resp.status_code(), resp.bytes().await?);
    match (status, serde_json::from_slice::<fragment_proto::ErrorBody>(&bytes)) {
        (200, _) => serde_json::from_slice(&bytes).map_err(|e| CellError::host(format!("{fragment}'s computer answered: {e}"))),
        (_, Ok(e)) => Err(CellError::new(e.error, e.message)),
        (s, Err(_)) => Err(CellError::host(format!("{fragment}'s computer answered {s}"))),
    }
}

/// The end of a Sprite's answer, for an error.
fn tail(out: &str) -> &str {
    &out[out.char_indices().rev().nth(400).map_or(0, |(i, _)| i)..]
}

/// The last number a command printed (`du -sk`'s KiB): 0 when it printed none.
fn last_number(out: &str) -> i64 {
    let line = out.lines().rev().find(|l| l.trim_start().starts_with(|c: char| c.is_ascii_digit())).unwrap_or("");
    line.split_whitespace().next().and_then(|n| n.parse().ok()).unwrap_or(0)
}

#[durable_object]
pub struct ComputerCell {
    state: State,
    env: Env,
    cfg: &'static Config,
}

impl DurableObject for ComputerCell {
    fn new(state: State, env: Env) -> Self {
        state.storage().sql().exec(SCHEMA, None).expect("the Computer schema applies");
        let cfg = Config::from_env(&env);
        ComputerCell { state, env, cfg }
    }

    async fn fetch(&self, mut req: Request) -> Result<Response> {
        let asked = match serde_json::from_slice::<Ask>(&req.bytes().await?) {
            Ok(Ask::Exec { id, exec }) => self.exec(&id, &exec).await,
            Ok(Ask::ExecPoll { id }) => self.exec_poll(&id).await,
            Ok(a) => self.asked(a).await.map(|()| Value::Null),
            Err(e) => Err(CellError::invalid(format!("body: {e}"))),
        };
        match asked {
            Ok(v) => Response::from_json(&v),
            Err(e) => e.response(),
        }
    }

    async fn alarm(&self) -> Result<Response> {
        if let Err(e) = self.step().await {
            let Some(mut c) = self.row().ok().flatten() else { return Response::ok("") };
            c.tries += 1;
            let wait = (self.cfg.computer_tick_ms << c.tries.min(12)).min(RETRY_MAX_MS);
            let _ = self.save(&c);
            self.tell(&c, "computer.failed", &format!("{} (again in {} s)", e.message, wait / 1000)).await;
            let _ = self.arm(wait).await;
        }
        Response::ok("")
    }
}

impl ComputerCell {
    fn sql(&self) -> SqlStorage {
        self.state.storage().sql()
    }

    fn row(&self) -> CellResult<Option<Row>> {
        Ok(self.sql().exec("SELECT * FROM computer", None)?.to_array::<Row>()?.pop())
    }

    fn save(&self, c: &Row) -> CellResult<()> {
        let v = serde_json::to_value(c).map_err(|e| CellError::host(e.to_string()))?;
        let cols = ["fragment", "owner", "declared", "phase", "awake_until", "held", "billed_to", "asleep_since", "disk_bytes", "tries"];
        let binds = cols.iter().map(|k| match &v[*k] {
            Value::Bool(b) => SqlStorageValue::Integer(i64::from(*b)),
            Value::Number(n) => SqlStorageValue::Integer(n.as_i64().expect("the row's numbers are integers")),
            Value::String(s) => s.as_str().into(),
            _ => SqlStorageValue::Null,
        });
        let q = format!("INSERT OR REPLACE INTO computer (one, {}) VALUES (1, {})", cols.join(", "), vec!["?"; cols.len()].join(", "));
        self.sql().exec(&q, binds.collect::<Vec<_>>())?;
        Ok(())
    }

    async fn arm(&self, in_ms: i64) -> CellResult<()> {
        let at = js::now_ms() + in_ms;
        Ok(self.state.storage().set_alarm(ScheduledTime::new(js_sys::Date::new(&worker::wasm_bindgen::JsValue::from_f64(at as f64)))).await?)
    }

    /// Its Sprite's name, as `KEYS` names it (crates/native `sprite_of`).
    fn sprite(&self) -> String {
        format!("fragment-{}", &self.state.id().to_string()[..24])
    }

    async fn asked(&self, what: Ask) -> CellResult<()> {
        let now = js::now_ms();
        let mut c = match (self.row()?, what) {
            (None, Ask::Declare { fragment, owner, declared }) => {
                let phase = if declared { Phase::Boot } else { Phase::Gone };
                Row { fragment, owner, declared, phase, awake_until: 0, held: false, billed_to: 0, asleep_since: None, disk_bytes: 0, tries: 0 }
            }
            // nothing was ever declared here: nothing to wake, pair, or destroy
            (None, _) => return Ok(()),
            (Some(mut c), Ask::Declare { declared, .. }) => {
                c.declared = declared;
                if declared && c.phase == Phase::Gone {
                    (c.phase, c.tries) = (Phase::Boot, 0);
                }
                c
            }
            (Some(mut c), Ask::Wake { until }) => {
                c.awake_until = c.awake_until.max(until);
                c
            }
            (Some(mut c), Ask::Paired) if c.phase == Phase::Boot => {
                (c.phase, c.tries, c.asleep_since) = (Phase::Ready, 0, Some(now));
                c
            }
            (Some(c), Ask::Paired) => c,
            (Some(mut c), Ask::Destroy) => {
                if c.phase != Phase::Gone {
                    c.phase = Phase::Destroy;
                }
                c
            }
            (_, Ask::Exec { .. } | Ask::ExecPoll { .. }) => unreachable!("a job's command is asked of `exec`"),
        };
        let due = match c.phase {
            Phase::Boot | Phase::Destroy => true,
            Phase::Ready => c.held != (c.declared && now < c.awake_until),
            Phase::Gone => false,
        };
        c.tries = if due { 0 } else { c.tries };
        self.save(&c)?;
        if due {
            self.arm(0).await?;
        }
        Ok(())
    }

    /// The alarm's one step, from the row.
    async fn step(&self) -> CellResult<()> {
        let Some(mut c) = self.row()? else { return Ok(()) };
        match c.phase {
            Phase::Boot if c.declared => self.boot(&mut c).await,
            Phase::Ready => self.keep(&mut c).await,
            Phase::Destroy => {
                let (status, body) = keys::sprites(&self.env, "delete", &[], "").await?;
                if !matches!(status, 200 | 204 | 404) {
                    return Err(upstream(format!("Sprites would not delete {} ({status}): {body}", self.sprite())));
                }
                (c.phase, c.held, c.tries) = (Phase::Gone, false, 0);
                self.save(&c)?;
                self.tell(&c, "computer.destroyed", &format!("{} is destroyed", self.sprite())).await;
                Ok(())
            }
            Phase::Boot | Phase::Gone => Ok(()),
        }
    }

    /// Charges `ms` of it, awake or asleep, under `reference` (a retry of
    /// the same span is not charged twice).
    async fn charge(&self, c: &Row, reference: String, ms: i64, awake: bool) -> CellResult<()> {
        let org = ledger::org_of(&c.owner).ok_or_else(|| CellError::host("a computer's owner is an identity"))?;
        let kind = if awake { "computer.awake" } else { "computer.asleep" };
        let reserve = Reserve {
            reference,
            kind: kind.into(),
            model: None,
            amount: budget::computer(ms, c.disk_bytes, awake),
            fragment: c.fragment.clone(),
            run: 0,
            principal: c.owner.clone(),
            agent: None,
        };
        ledger::ask(&self.env, &org, &Charge(reserve)).await.map(|_| ())
    }

    /// The next tick awake is charged, then held (the Sprite's own Tasks
    /// API: a hold outlives a missed tick, never two).
    async fn hold(&self, c: &mut Row) -> CellResult<()> {
        let tick = self.cfg.computer_tick_ms;
        self.charge(c, format!("{}@{}", self.sprite(), c.billed_to), tick, true).await?;
        let expire = json!({ "expire": 2 * tick / 1000 }).to_string();
        let (status, body) = keys::sprites(&self.env, "exec", &tasks("PUT", &expire), "").await?;
        if status != 200 {
            return Err(upstream(format!("{} would not stay awake ({status}): {body}", self.sprite())));
        }
        c.billed_to += tick;
        Ok(())
    }

    /// Makes the Sprite (or finds it made), then boots it: the release's
    /// one-line install, and a pairing with a token only it is handed.
    async fn boot(&self, c: &mut Row) -> CellResult<()> {
        c.billed_to = js::now_ms();
        let tick = self.cfg.computer_tick_ms;
        self.charge(c, format!("{}@{}", self.sprite(), c.billed_to), tick, true).await?;
        let (status, body) = keys::sprites(&self.env, "get", &[], "").await?;
        if status == 404 {
            let (status, body) = keys::sprites(&self.env, "create", &[], "").await?;
            if !matches!(status, 200 | 201) {
                return Err(upstream(format!("Sprites would not make {} ({status}): {body}", self.sprite())));
            }
        } else if status != 200 {
            return Err(upstream(format!("Sprites would not say whether {} is made ({status}): {body}", self.sprite())));
        }
        let minted = crate::ask_registry(&self.env, &calls::MintPairing { owner: c.owner.clone(), name: c.fragment.clone() }).await?;
        let (release, host) = (&self.cfg.cli_release_url, self.cfg.computer_platform()?);
        assert!(!release.contains('\'') && !host.contains('\''), "configured URLs hold no quote");
        let script = format!(
            "set -e; mkdir -p \"$HOME/.local/bin\"; curl -fsSL '{release}/fragment-'\"$(uname -s)-$(uname -m)\"'.tar.gz' | tar -xzf - -C \"$HOME/.local/bin\"; \
             \"$HOME/.local/bin/fragment\" --host '{host}' login --pair; du -sk \"$HOME\""
        );
        let (status, out) = keys::sprites(&self.env, "exec", &["sh", "-c", &script], &minted.token).await?;
        // the pairing says so itself (`Paired`), before the command ends
        let Some(mut now) = self.row()?.filter(|r| r.phase == Phase::Ready) else {
            let tail = out.char_indices().rev().nth(400).map_or(0, |(i, _)| i);
            return Err(upstream(format!("{} did not pair ({status}): {}", self.sprite(), &out[tail..])));
        };
        (now.billed_to, now.disk_bytes) = (c.billed_to + tick, last_number(&out) * 1024);
        self.save(&now)?;
        self.tell(&now, "computer.ready", &format!("{} is {}'s computer, an editor here", self.sprite(), now.fragment)).await;
        self.arm(0).await
    }

    /// Awake while something needs it: a viewer on its fragment's page
    /// keeps it `FRAGMENT_COMPUTER_IDLE_S` more.
    async fn keep(&self, c: &mut Row) -> CellResult<()> {
        let now = js::now_ms();
        if c.declared && self.viewers(c).await? > 0 {
            c.awake_until = c.awake_until.max(now + self.cfg.computer_idle_ms);
        }
        if c.declared && now < c.awake_until {
            let woke = !c.held;
            if woke {
                // its disk asleep, since it slept, then awake from now
                if let Some(since) = c.asleep_since {
                    self.charge(c, format!("{}@asleep-{since}", self.sprite()), now - since, false).await?;
                }
                (c.asleep_since, c.billed_to) = (None, now);
            }
            match self.hold(c).await {
                Ok(()) => {}
                Err(e) if e.code == fragment_proto::ErrorCode::BudgetUsedUp => {
                    self.tell(c, "computer.budget", &e.message).await;
                    c.awake_until = 0;
                    return self.release(c, now).await;
                }
                Err(e) => return Err(e),
            }
            if woke {
                let (_, out) = keys::sprites(&self.env, "exec", &["sh", "-c", "du -sk \"$HOME\""], "").await?;
                c.disk_bytes = last_number(&out) * 1024;
            }
            (c.held, c.tries) = (true, 0);
            self.save(c)?;
            return self.arm(self.cfg.computer_tick_ms).await;
        }
        self.release(c, now).await
    }

    async fn release(&self, c: &mut Row, now: i64) -> CellResult<()> {
        if c.held {
            keys::sprites(&self.env, "exec", &tasks("DELETE", ""), "").await?;
            (c.held, c.asleep_since) = (false, Some(now));
        }
        self.save(c)
    }

    /// How many pages of its fragment are open (its live sockets).
    async fn viewers(&self, c: &Row) -> CellResult<i64> {
        let req = crate::routed::internal_request("computer/viewers", "{}")?;
        let v: Value = self.env.durable_object("FRAGMENT")?.get_by_name(&c.fragment)?.fetch_with_request(req).await?.json().await?;
        v["viewers"].as_i64().ok_or_else(|| CellError::host("the fragment answered no viewers"))
    }

    /// Awake for a job's command until `until`, as for a page, and held:
    /// it waits a while for the alarm to hold it (its first tick paid).
    /// Refused for good when it is not declared, or its owner's month cannot
    /// pay a tick; one still waking or booting is tried again.
    async fn awake(&self, until: i64) -> CellResult<()> {
        let refused = |m: &str| CellError::invalid(format!("job.computer.exec: {m}"));
        let (t0, mut asked) = (js::now_ms(), false);
        loop {
            let Some(mut c) = self.row()?.filter(|c| c.declared) else {
                return Err(refused("the fragment declares no computer: add \"computer\": {} to its fragment.json and deploy"));
            };
            if matches!(c.phase, Phase::Destroy | Phase::Gone) {
                return Err(refused("its computer was removed (`fragment computers rm`); the next deploy that declares one makes a new one"));
            }
            if c.awake_until < until {
                // asked again: a budget's refusal clears it (`keep`), and so
                // can the alarm's save of a row it read before the ask
                if asked && c.awake_until == 0 && !self.tick_fits(&c).await? {
                    return Err(CellError::new(
                        fragment_proto::ErrorCode::BudgetUsedUp,
                        "job.computer.exec: its owner's budget cannot hold the computer awake this month (a top-up, or next month, lets it run)",
                    ));
                }
                c.awake_until = until;
                self.save(&c)?;
                if c.phase == Phase::Ready && !c.held {
                    self.arm(0).await?;
                }
            }
            asked = true;
            if c.phase == Phase::Ready && c.held {
                return Ok(());
            }
            if js::now_ms() - t0 > WAKE_WAIT_MS {
                let what = if c.phase == Phase::Boot { "is still being made" } else { "did not wake in time" };
                return Err(upstream(format!("job.computer.exec: its computer {what}")));
            }
            Delay::from(Duration::from_millis(WAKE_LOOK_MS)).await;
        }
    }

    /// Whether its owner's month has room for a tick awake.
    async fn tick_fits(&self, c: &Row) -> CellResult<bool> {
        let org = ledger::org_of(&c.owner).ok_or_else(|| CellError::host("a computer's owner is an identity"))?;
        let month = ledger::ask(&self.env, &org, &ledger::Status {}).await?;
        Ok(month.remaining_micros >= budget::computer(self.cfg.computer_tick_ms, c.disk_bytes, true))
    }

    /// `job.computer.exec`'s start: the computer awake, then the command
    /// started on it, once (`exec::START`).
    async fn exec(&self, id: &str, e: &ComputerExec) -> CellResult<Value> {
        exec::check(e).map_err(CellError::invalid)?;
        self.awake(js::now_ms() + self.cfg.computer_idle_ms).await?;
        let script = exec::job_script(e, &self.cfg.computer_platform()?);
        let timeout = exec::timeout_s(e).to_string();
        let (status, out) = keys::sprites(&self.env, "exec", &["bash", "-c", exec::START, "fragment-exec", id, &timeout], &script).await?;
        match (status, exec::state(&out)) {
            (200, Some(ExecState::Started)) => Ok(json!({ "exec": id })),
            _ => Err(upstream(format!("{} did not start the command ({status}): {}", self.sprite(), tail(&out)))),
        }
    }

    /// `job.computer.exec`'s poll: awake, waiting on the computer for the
    /// command to end, then its result, read in chunks.
    async fn exec_poll(&self, id: &str) -> CellResult<Value> {
        // awake through the wait, then the idle wait from its end, as after a page
        self.awake(js::now_ms() + 1000 * (exec::POLL_WAIT_S as i64 + 10)).await?;
        let wait = exec::POLL_WAIT_S.to_string();
        let (status, out) = keys::sprites(&self.env, "exec", &["bash", "-c", exec::POLL, "fragment-exec", id, &wait], "").await?;
        self.asked(Ask::Wake { until: js::now_ms() + self.cfg.computer_idle_ms }).await?;
        let (code, out_n, err_n, truncated) = match (status, exec::state(&out)) {
            (200, Some(ExecState::Running)) => return Ok(json!({ "done": false })),
            (200, Some(ExecState::Done { code, out, err, truncated })) => (code, out, err, truncated),
            (200, Some(ExecState::Interrupted)) => {
                return Err(CellError::invalid("job.computer.exec: its computer stopped before the command ended, which does not run again"));
            }
            (200, Some(ExecState::Lost)) => return Err(CellError::invalid(format!("job.computer.exec: no command {id} on the computer (it was made again since)"))),
            _ => return Err(upstream(format!("{} did not say how the command is ({status}): {}", self.sprite(), tail(&out)))),
        };
        let (stdout, stderr) = (self.exec_read(id, "stdout", out_n).await?, self.exec_read(id, "stderr", err_n).await?);
        Ok(json!({ "done": true, "result": exec::result(code, &stdout, &stderr, truncated) }))
    }

    /// The first `n` bytes of a command's stream (at most what it keeps).
    async fn exec_read(&self, id: &str, stream: &str, n: usize) -> CellResult<Vec<u8>> {
        let n = n.min(exec::OUTPUT_MAX_BYTES);
        let mut bytes = Vec::with_capacity(n);
        while bytes.len() < n {
            let (at, len) = (bytes.len().to_string(), exec::CHUNK_BYTES.min(n - bytes.len()).to_string());
            let (status, out) = keys::sprites(&self.env, "exec", &["bash", "-c", exec::READ, "fragment-exec", id, stream, &at, &len], "").await?;
            match exec::answer(&out).filter(|b| status == 200 && !b.is_empty()) {
                Some(chunk) => bytes.extend(chunk),
                None => return Err(upstream(format!("{} did not read the command's {stream} ({status}): {}", self.sprite(), tail(&out)))),
            }
        }
        Ok(bytes)
    }

    /// An entry in its fragment's `events`; one that does not land is logged.
    async fn tell(&self, c: &Row, kind: &str, summary: &str) {
        let body = json!({ "kind": kind, "summary": summary, "sprite": self.sprite() }).to_string();
        let sent = async {
            let req = crate::routed::internal_request("computer/event", &body)?;
            self.env.durable_object("FRAGMENT")?.get_by_name(&c.fragment)?.fetch_with_request(req).await?;
            Ok::<(), CellError>(())
        };
        if let Err(e) = sent.await {
            console_error!("{}: {kind} ({summary}) did not reach its events: {}", c.fragment, e.message);
        }
    }
}

/// Its computer's refusal as a step's failure: one it will give again is
/// for good (the job sees it); any other may pass (the step is retried).
fn exec_fail(e: CellError) -> StepFail {
    use fragment_proto::ErrorCode::*;
    match e.code {
        InvalidRequest | BudgetUsedUp | Forbidden | NotFound => permanent(e.message),
        _ => StepFail::Retry(e.message),
    }
}

/// A fragment's side: its computer is told what live declares, woken for
/// its pages, runs its jobs' commands, and is answered.
impl FragmentCell {
    /// Live's `computer` block, installed: its computer is to be told (a
    /// fragment that never declared one tells nothing, and has no computer).
    pub(crate) fn want_computer(&self, declared: bool) -> CellResult<()> {
        if declared || self.meta(MetaKey::ComputerDeclared)?.is_some() {
            self.set_meta(MetaKey::ComputerPending, if declared { "1" } else { "0" })?;
        }
        Ok(())
    }

    /// Tells its computer what live declares; one that did not hear is
    /// told again by the alarm.
    pub(crate) async fn tell_computer(&self) {
        let told = async {
            let Some(pending) = self.meta(MetaKey::ComputerPending)? else { return Ok(()) };
            let (name, declared) = (self.name()?, pending == "1");
            ask(&self.env, &name, &Ask::Declare { fragment: name.clone(), owner: self.must(MetaKey::Owner)?, declared }).await?;
            match declared {
                true => self.set_meta(MetaKey::ComputerDeclared, "1")?,
                false => self.del_meta(MetaKey::ComputerDeclared)?,
            }
            // a deploy meanwhile has something newer to tell
            if self.meta(MetaKey::ComputerPending)?.as_deref() == Some(pending.as_str()) {
                self.del_meta(MetaKey::ComputerPending)?;
            }
            Ok::<(), CellError>(())
        };
        if let Err(e) = told.await {
            self.event("computer.failed", &format!("its computer did not hear the deploy: {}", e.message), json!({ "code": e.code }));
        }
    }

    /// That its live commit declares a computer, for a job's command; one a
    /// deploy just declared, which its computer has not heard, is waited for.
    fn job_computer(&self) -> Result<(), StepFail> {
        let [declared, pending] = self.metas([MetaKey::ComputerDeclared, MetaKey::ComputerPending]).map_err(|e| StepFail::Retry(e.message))?;
        match (declared, pending.as_deref()) {
            (Some(_), None | Some("1")) => Ok(()),
            (None, Some("1")) => Err(StepFail::Retry("its computer has not heard the deploy that declares it yet".into())),
            _ => Err(permanent("job.computer.exec needs the fragment's own computer: add \"computer\": {} to its fragment.json and deploy")),
        }
    }

    /// `job.computer.exec`'s start: its command on the fragment's computer,
    /// named by the run and this step, not the attempt: a retried or
    /// replayed step reattaches to it, and it never runs twice.
    pub(crate) async fn step_computer_exec(&self, run: &RunRow, index: u32, e: ComputerExec) -> Result<Value, StepFail> {
        exec::check(&e).map_err(permanent)?;
        self.job_computer()?;
        let id = hex::encode(<sha2::Sha256 as sha2::Digest>::digest(self.step_ref(run, index)?.as_bytes()))[..24].to_string();
        let name = self.name().map_err(|e| StepFail::Retry(e.message))?;
        ask(&self.env, &name, &Ask::Exec { id, exec: e }).await.map_err(exec_fail)
    }

    /// `job.computer.exec`'s poll: `{done: false}`, or `{done: true, result}`.
    pub(crate) async fn step_computer_poll(&self, id: &str) -> Result<Value, StepFail> {
        if id.len() != 24 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(permanent(format!("{id:?} is not a command's id")));
        }
        self.job_computer()?;
        let name = self.name().map_err(|e| StepFail::Retry(e.message))?;
        ask(&self.env, &name, &Ask::ExecPoll { id: id.to_string() }).await.map_err(exec_fail)
    }

    /// A page opened: its computer, if it has one, is awake for it.
    pub(crate) async fn viewed(&self) {
        if !matches!(self.meta(MetaKey::ComputerDeclared), Ok(Some(_))) {
            return;
        }
        let until = js::now_ms() + self.cfg.computer_idle_ms;
        let woke = async { ask(&self.env, &self.name()?, &Ask::Wake { until }).await };
        if let Err(e) = woke.await {
            console_error!("a page opened, and its computer did not hear: {}", e.message);
        }
    }

    /// Its computer's calls: how many pages are open, and its events.
    pub(crate) fn computer_asks(&self, what: &str, body: &Value) -> CellResult<Response> {
        match (what, body["kind"].as_str(), body["summary"].as_str()) {
            ("viewers", _, _) => json_response(&json!({ "viewers": self.viewers() })),
            ("event", Some(kind), Some(summary)) if kind.starts_with("computer.") => {
                self.event(kind, summary, json!({ "sprite": body["sprite"] }));
                json_response(&json!({ "ok": true }))
            }
            _ => Err(CellError::invalid(format!("computer/{what} is viewers, or event {{kind, summary}}"))),
        }
    }
}
