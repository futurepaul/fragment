//! The `Ledger` cell (docs/ledger.md): one Durable Object per person,
//! named by their identity (`id:<hex>`), running the usage ledger's state
//! machine (`fragment_core::ledger`) over its own SQLite. The caller
//! decides who pays a row and asks that person's ledger, which keys on
//! nothing else (docs/cloudflare-v1.md, decision 36).
//!
//! The head (plan, seat, balance, holds, price book) is kept whole as one
//! JSON row, small and bounded; what grows with use (references, batches,
//! commands, each fragment's spend by month, caps) is in tables, read and
//! written through `SqlStore`. Each route is one `transactionSync` over
//! both: the core decides before it writes, so a refusal changes nothing
//! but the month it rolled to, and a store that failed rolls the whole
//! route back. Its alarm runs `sweep` at the head's `next_sweep_ms`.
//!
//! Inner routes (platform code only, through `ask`): each is a POST of a
//! `Route`'s request (the core's types, or proto's), answered with its
//! `Route::Answer`; a refusal is `ErrorBody` with the typed `Refused`
//! beside it, so a caller can tell a read-only owner from a guest.

use std::cell::Cell;

use fragment_core::ledger::{self as core, BatchRecord, CommandRecord, Entry, Ledger, Month, Refused, Spend, Store};
use fragment_core::price::PriceBook;
use fragment_proto::ledger::{GrantCredit, LedgerStatus, Plan, SetFragmentCap, SetOverdraft, SetPlan, SetSeat};
use fragment_proto::ErrorCode;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use worker::wasm_bindgen::{JsCast, JsValue};
use worker::*;

use crate::config::Config;
use crate::error::{CellError, CellResult};
use crate::js;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS head (id INTEGER PRIMARY KEY CHECK (id = 1), ledger TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS entries (ref TEXT PRIMARY KEY, at_ms INTEGER NOT NULL, held INTEGER NOT NULL, entry TEXT NOT NULL);
CREATE INDEX IF NOT EXISTS entries_at ON entries (at_ms);
CREATE TABLE IF NOT EXISTS batches (id TEXT PRIMARY KEY, digest TEXT NOT NULL, answer TEXT NOT NULL, at_ms INTEGER NOT NULL);
CREATE INDEX IF NOT EXISTS batches_at ON batches (at_ms);
CREATE TABLE IF NOT EXISTS commands (id TEXT PRIMARY KEY, command TEXT NOT NULL, at_ms INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS spend (month INTEGER NOT NULL, fragment TEXT NOT NULL, micros INTEGER NOT NULL, PRIMARY KEY (month, fragment));
CREATE TABLE IF NOT EXISTS caps (fragment TEXT PRIMARY KEY, micros INTEGER NOT NULL);
";

/// The person a ledger call is for; only platform code sets it, and the
/// ledger keeps the first it is told and refuses another.
pub const PAYER_HEADER: &str = "x-fragment-payer";
/// The configured default plan's command id: applied once, as a new
/// person's ledger is made.
const DEFAULT_PLAN_ID: &str = "plan:default";
/// Test fleets: references a test hook lists at most.
const TEST_ENTRIES_MAX: i64 = 500;
/// The meta keys of a capped ledger's paid calls (`TestHook::PaidCalls`).
const PAID_CALLS_MAX: &str = "test_paid_calls_max";
const PAID_CALLS_USED: &str = "test_paid_calls_used";

/// The price book the deployment charges with: the core's defaults, and
/// the operator keys' prices and the book's version from the deployment's
/// configuration (decision 4: pricing lives there). Each ledger takes it
/// at its next call when its version is newer (`take_book`).
pub fn configured_book(cfg: &Config) -> PriceBook {
    let mut book = PriceBook::defaults();
    book.keys = cfg.providers.key_prices();
    book.version = book.version.max(cfg.price_book_version);
    book
}

#[durable_object]
pub struct LedgerCell {
    state: State,
    raw: JsValue,
    /// The isolate's settings (config.rs: built once per isolate).
    cfg: &'static Config,
    /// When this activation last set the alarm for (the ledger's clock):
    /// a call that needs it no later leaves it.
    armed: Cell<Option<i64>>,
}

impl DurableObject for LedgerCell {
    fn new(state: State, env: Env) -> Self {
        let raw: JsValue = state._inner().into();
        let state = State::from(raw.clone().unchecked_into::<worker_sys::DurableObjectState>());
        state.storage().sql().exec(SCHEMA, None).expect("the Ledger schema applies");
        let cfg = Config::from_env(&env);
        LedgerCell { state, raw, cfg, armed: Cell::new(None) }
    }

    async fn fetch(&self, req: Request) -> Result<Response> {
        match self.route(req).await {
            Ok(answer) => Response::from_json(&answer),
            Err(Failed::Refused(why)) => refusal(why),
            Err(Failed::Cell(e)) => e.response(),
        }
    }

    async fn alarm(&self) -> Result<Response> {
        if let Err(e) = self.swept().await {
            console_error!("{}", json!({ "event": "ledger.sweep-failed", "message": e.message }));
            // tried again within the minute, never spinning
            let at = js::now_ms() + 60_000;
            self.state.storage().set_alarm(ScheduledTime::new(js_sys::Date::new(&JsValue::from_f64(at as f64)))).await?;
        }
        Response::ok("")
    }
}

/// A refusal as its caller reads it: the platform's error body, and the
/// core's typed reason beside it.
fn refusal(why: Refused) -> Result<Response> {
    let body = json!({ "error": why.code(), "message": why.message(), "refused": why });
    Ok(Response::from_json(&body)?.with_status(why.code().status()))
}

/// How a route failed: the ledger refused (typed), or the cell did.
enum Failed {
    Refused(Refused),
    Cell(CellError),
}

impl From<CellError> for Failed {
    fn from(e: CellError) -> Failed {
        Failed::Cell(e)
    }
}

impl From<worker::Error> for Failed {
    fn from(e: worker::Error) -> Failed {
        Failed::Cell(e.into())
    }
}

/// One of the ledger's routes: the request a caller sends and the answer
/// it gets back. Both ends compile against the same types.
pub trait Route: Serialize + DeserializeOwned {
    const PATH: &'static str;
    type Answer: Serialize + DeserializeOwned;
}

/// An answer that says only that it was done (or allowed): `{}`.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Done {}

/// May this spend start (docs/ledger.md, `may_spend`)? The whole question
/// when the payer owns the fragment (or there is none); when another
/// person owns it, that owner's ledger answers `FragmentOpen` too.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MaySpend {
    pub spend: Spend,
    pub fragment: Option<String>,
    pub by_owner: bool,
}

/// Is a fragment this ledger's person owns open to spenders other than
/// its owner (under its cap) this month?
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FragmentOpen {
    pub fragment: String,
}

/// The ledger now.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Status {}

/// Fleets with levers only (`FRAGMENT_TEST_SECRET`; cell/src/levers.rs):
/// move this ledger's clock (`clock {offsetMs}`), run its sweep now
/// (`sweep`), list the references under a prefix (`entries {prefix}`),
/// read what moved its balance (`totals`), or cap its paid calls from now
/// (`paid-calls {max}`: each new reservation, a model call or an AI step,
/// counts one, and one past `max` is refused as a ledger at zero refuses
/// it; the hosted e2e's people are capped so).
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case")]
pub enum TestHook {
    #[serde(rename_all = "camelCase")]
    Clock { offset_ms: i64 },
    Sweep,
    Entries { prefix: String },
    Totals,
    PaidCalls { max: u64 },
}

impl Route for core::Reserve {
    const PATH: &'static str = "/reserve";
    type Answer = core::Reserved;
}

impl Route for core::Settle {
    const PATH: &'static str = "/settle";
    type Answer = core::Settled;
}

impl Route for core::Release {
    const PATH: &'static str = "/release";
    type Answer = core::Released;
}

impl Route for core::Meter {
    const PATH: &'static str = "/meter";
    type Answer = core::Metered;
}

impl Route for MaySpend {
    const PATH: &'static str = "/may-spend";
    type Answer = Done;
}

impl Route for FragmentOpen {
    const PATH: &'static str = "/fragment-open";
    type Answer = Done;
}

impl Route for GrantCredit {
    const PATH: &'static str = "/grant";
    type Answer = Done;
}

impl Route for SetPlan {
    const PATH: &'static str = "/plan";
    type Answer = Done;
}

impl Route for SetSeat {
    const PATH: &'static str = "/seat";
    type Answer = Done;
}

impl Route for SetOverdraft {
    const PATH: &'static str = "/overdraft";
    type Answer = Done;
}

impl Route for SetFragmentCap {
    const PATH: &'static str = "/cap";
    type Answer = Done;
}

impl Route for Status {
    const PATH: &'static str = "/status";
    type Answer = LedgerStatus;
}

impl Route for TestHook {
    const PATH: &'static str = "/test";
    type Answer = Value;
}

fn decode<R: Route>(body: &[u8]) -> CellResult<R> {
    serde_json::from_slice(body).map_err(|e| CellError::invalid(format!("{}: {e}", R::PATH)))
}

/// What grows with use, in this cell's tables (`fragment_core::ledger::Store`).
/// The trait answers values, not results, so a statement that fails marks
/// the store at fault: the route sees it before anything commits and rolls
/// the whole transaction back. A row that does not read as the core wrote
/// it is corruption, a fault too.
struct SqlStore {
    sql: SqlStorage,
    fault: std::cell::RefCell<Option<String>>,
}

impl SqlStore {
    fn new(sql: SqlStorage) -> SqlStore {
        SqlStore { sql, fault: std::cell::RefCell::new(None) }
    }

    fn fail(&self, why: String) {
        let mut fault = self.fault.borrow_mut();
        if fault.is_none() {
            *fault = Some(why);
        }
    }

    /// The first fault, as the route's failure.
    fn checked(&self) -> CellResult<()> {
        match self.fault.borrow().as_ref() {
            None => Ok(()),
            Some(why) => Err(CellError::host(format!("the ledger's store: {why}"))),
        }
    }

    fn rows<T: DeserializeOwned>(&self, q: &str, binds: Vec<SqlStorageValue>) -> Vec<T> {
        match self.sql.exec(q, binds).and_then(|c| c.to_array::<T>()) {
            Ok(rows) => rows,
            Err(e) => {
                self.fail(format!("{q}: {e}"));
                Vec::new()
            }
        }
    }

    fn exec(&self, q: &str, binds: Vec<SqlStorageValue>) {
        if let Err(e) = self.sql.exec(q, binds) {
            self.fail(format!("{q}: {e}"));
        }
    }

    fn count(&self, q: &str, binds: Vec<SqlStorageValue>) -> u64 {
        #[derive(Deserialize)]
        struct N {
            n: i64,
        }
        let n = self.rows::<N>(q, binds).first().map_or(0, |r| r.n);
        u64::try_from(n).unwrap_or_else(|_| {
            self.fail(format!("{q}: a negative count"));
            0
        })
    }

    /// A JSON column as the core's type: one that does not read is corruption.
    fn read<T: DeserializeOwned>(&self, what: &str, text: &str) -> Option<T> {
        match serde_json::from_str(text) {
            Ok(v) => Some(v),
            Err(e) => {
                self.fail(format!("{what} does not read as the ledger wrote it: {e}"));
                None
            }
        }
    }

    fn text<T: Serialize>(value: &T) -> String {
        serde_json::to_string(value).expect("the ledger's types serialize")
    }
}

#[derive(Deserialize)]
struct Json {
    value: String,
}

#[derive(Deserialize)]
struct SpendRow {
    fragment: String,
    micros: i64,
}

impl Store for SqlStore {
    fn entry(&self, reference: &str) -> Option<Entry> {
        let rows: Vec<Json> = self.rows("SELECT entry AS value FROM entries WHERE ref = ?", vec![reference.into()]);
        let entry = self.read::<Entry>("an entry", &rows.into_iter().next()?.value)?;
        if let Entry::Reservation { reserve, .. } = &entry {
            assert_eq!(reserve.reference, reference, "an entry is kept under its own reference");
        }
        Some(entry)
    }

    fn put_entry(&mut self, reference: &str, entry: Entry) {
        let held = i64::from(entry.held());
        self.exec(
            "INSERT INTO entries (ref, at_ms, held, entry) VALUES (?, ?, ?, ?)
             ON CONFLICT (ref) DO UPDATE SET at_ms = excluded.at_ms, held = excluded.held, entry = excluded.entry",
            vec![reference.into(), SqlStorageValue::Integer(entry.at_ms()), SqlStorageValue::Integer(held), SqlStore::text(&entry).into()],
        );
    }

    fn batch(&self, id: &str) -> Option<BatchRecord> {
        #[derive(Deserialize)]
        struct Row {
            digest: String,
            answer: String,
            at_ms: i64,
        }
        let row = self.rows::<Row>("SELECT digest, answer, at_ms FROM batches WHERE id = ?", vec![id.into()]).into_iter().next()?;
        Some(BatchRecord { digest: row.digest, answer: self.read("a batch's answer", &row.answer)?, at_ms: row.at_ms })
    }

    fn put_batch(&mut self, id: &str, record: BatchRecord) {
        self.exec(
            "INSERT INTO batches (id, digest, answer, at_ms) VALUES (?, ?, ?, ?)",
            vec![id.into(), record.digest.as_str().into(), SqlStore::text(&record.answer).into(), SqlStorageValue::Integer(record.at_ms)],
        );
    }

    fn command(&self, id: &str) -> Option<CommandRecord> {
        #[derive(Deserialize)]
        struct Row {
            command: String,
            at_ms: i64,
        }
        let row = self.rows::<Row>("SELECT command, at_ms FROM commands WHERE id = ?", vec![id.into()]).into_iter().next()?;
        Some(CommandRecord { command: self.read("a command", &row.command)?, at_ms: row.at_ms })
    }

    fn put_command(&mut self, id: &str, record: CommandRecord) {
        self.exec(
            "INSERT INTO commands (id, command, at_ms) VALUES (?, ?, ?)",
            vec![id.into(), SqlStore::text(&record.command).into(), SqlStorageValue::Integer(record.at_ms)],
        );
    }

    fn spent(&self, month: Month, fragment: &str) -> i64 {
        let rows: Vec<SpendRow> = self.rows("SELECT fragment, micros FROM spend WHERE month = ? AND fragment = ?", vec![SqlStorageValue::Integer(month.0.into()), fragment.into()]);
        rows.first().map_or(0, |r| r.micros)
    }

    fn add_spent(&mut self, month: Month, fragment: &str, micros: i64) {
        assert!(micros > 0, "only a charge adds to a fragment's spend");
        self.exec(
            "INSERT INTO spend (month, fragment, micros) VALUES (?, ?, ?)
             ON CONFLICT (month, fragment) DO UPDATE SET micros = micros + excluded.micros",
            vec![SqlStorageValue::Integer(month.0.into()), fragment.into(), SqlStorageValue::Integer(micros)],
        );
    }

    fn spends(&self, month: Month, limit: usize) -> Vec<(String, i64)> {
        let rows: Vec<SpendRow> = self.rows(
            "SELECT fragment, micros FROM spend WHERE month = ? ORDER BY micros DESC, fragment LIMIT ?",
            vec![SqlStorageValue::Integer(month.0.into()), SqlStorageValue::Integer(limit as i64)],
        );
        rows.into_iter().map(|r| (r.fragment, r.micros)).collect()
    }

    fn cap(&self, fragment: &str) -> Option<i64> {
        #[derive(Deserialize)]
        struct Row {
            micros: i64,
        }
        self.rows::<Row>("SELECT micros FROM caps WHERE fragment = ?", vec![fragment.into()]).first().map(|r| r.micros)
    }

    fn set_cap(&mut self, fragment: &str, micros: Option<i64>) {
        match micros {
            Some(m) => self.exec(
                "INSERT INTO caps (fragment, micros) VALUES (?, ?) ON CONFLICT (fragment) DO UPDATE SET micros = excluded.micros",
                vec![fragment.into(), SqlStorageValue::Integer(m)],
            ),
            None => self.exec("DELETE FROM caps WHERE fragment = ?", vec![fragment.into()]),
        }
    }

    fn prune(&mut self, before_ms: i64, before_month: Month) -> u64 {
        let before = SqlStorageValue::Integer(before_ms);
        // the sweep expired every hold past HOLD_MAX_MS first: one older
        // than the horizon would be a charge forgotten
        if self.count("SELECT COUNT(*) AS n FROM entries WHERE at_ms < ? AND held = 1", vec![before.clone()]) != 0 {
            self.fail("a held reservation is older than what the ledger remembers".into());
            return 0;
        }
        let forgotten = self.count(
            "SELECT (SELECT COUNT(*) FROM entries WHERE at_ms < ?) + (SELECT COUNT(*) FROM batches WHERE at_ms < ?) AS n",
            vec![before.clone(), before.clone()],
        );
        self.exec("DELETE FROM entries WHERE at_ms < ?", vec![before.clone()]);
        self.exec("DELETE FROM batches WHERE at_ms < ?", vec![before]);
        self.exec("DELETE FROM spend WHERE month < ?", vec![SqlStorageValue::Integer(before_month.0.into())]);
        forgotten
    }
}

/// The head as this cell keeps it, or a new person's: a guest with no
/// credit (`Ledger::new`), then the deployment's default plan for new
/// people (decision 25: `FRAGMENT_DEFAULT_PLAN`).
fn head(store: &mut SqlStore, cfg: &Config, now_ms: i64) -> CellResult<Ledger> {
    let rows: Vec<Json> = store.rows("SELECT ledger AS value FROM head WHERE id = 1", vec![]);
    store.checked()?;
    if let Some(row) = rows.into_iter().next() {
        let ledger: Ledger = serde_json::from_str(&row.value).map_err(|e| CellError::host(format!("the ledger's head does not read as it was written: {e}")))?;
        ledger.assert_valid();
        return Ok(ledger);
    }
    let mut ledger = Ledger::new(now_ms, configured_book(cfg));
    if cfg.default_plan != Plan::Guest {
        let set = SetPlan { id: DEFAULT_PLAN_ID.into(), plan: cfg.default_plan };
        ledger.set_plan(store, &set, now_ms).map_err(|why| CellError::host(format!("a new ledger refused its default plan: {}", why.message())))?;
    }
    Ok(ledger)
}

/// The configured book, taken when it is newer than the ledger's (under
/// its version's id, once). A configuration that changed a book's prices
/// without a new version is refused as another body, and the ledger keeps
/// charging with the one it has, saying so.
fn take_book(ledger: &mut Ledger, store: &mut SqlStore, cfg: &Config, now_ms: i64) {
    let book = configured_book(cfg);
    if book.version <= ledger.book().version {
        return;
    }
    let set = core::SetPriceBook { id: format!("book:{}", book.version), book };
    if let Err(why) = ledger.set_book(store, &set, now_ms) {
        console_error!("{}", json!({ "event": "ledger.book-refused", "id": set.id, "message": why.message() }));
    }
}

fn save(store: &SqlStore, ledger: &Ledger) {
    store.exec(
        "INSERT INTO head (id, ledger) VALUES (1, ?) ON CONFLICT (id) DO UPDATE SET ledger = excluded.ledger",
        vec![SqlStore::text(ledger).into()],
    );
}

impl LedgerCell {
    fn sql(&self) -> SqlStorage {
        self.state.storage().sql()
    }

    fn meta(&self, key: &str) -> CellResult<Option<String>> {
        let rows: Vec<Json> = self.sql().exec("SELECT value FROM meta WHERE key = ?", vec![key.into()])?.to_array()?;
        Ok(rows.into_iter().next().map(|r| r.value))
    }

    fn set_meta(&self, key: &str, value: &str) -> CellResult<()> {
        self.sql().exec("INSERT INTO meta (key, value) VALUES (?, ?) ON CONFLICT (key) DO UPDATE SET value = excluded.value", vec![key.into(), value.into()])?;
        Ok(())
    }

    /// The clock's offset a test fleet set (`TestHook::Clock`); none elsewhere.
    fn offset_ms(&self) -> CellResult<i64> {
        if !self.cfg.test_hooks {
            return Ok(0);
        }
        Ok(self.meta("clock_offset")?.and_then(|v| v.parse().ok()).unwrap_or(0))
    }

    /// Runs `op` on the head and the store in one transaction, at the
    /// ledger's now: the head is written back whatever `op` answers (a
    /// refusal changed nothing but the month it rolled to, and a newer
    /// book), and a store that failed rolls everything back. Answers `op`'s
    /// answer and when the next sweep is due (the ledger's clock).
    fn transact<T: 'static>(&self, op: impl FnOnce(&mut Ledger, &mut SqlStore, i64) -> Result<T, Refused> + 'static) -> CellResult<(Result<T, Refused>, i64)> {
        let now = js::now_ms() + self.offset_ms()?;
        let (sql, cfg) = (self.sql(), self.cfg);
        js::transaction_sync(&self.raw, move || {
            let mut store = SqlStore::new(sql);
            let mut ledger = head(&mut store, cfg, now)?;
            take_book(&mut ledger, &mut store, cfg, now);
            let answer = op(&mut ledger, &mut store, now);
            save(&store, &ledger);
            store.checked()?;
            Ok((answer, ledger.next_sweep_ms(now)))
        })
    }

    /// `transact`, then the alarm set for the next sweep when it is due
    /// sooner than this activation last set it.
    async fn mutate<T: 'static>(&self, op: impl FnOnce(&mut Ledger, &mut SqlStore, i64) -> Result<T, Refused> + 'static) -> Result<T, Failed> {
        let (answer, next) = self.transact(op)?;
        if self.armed.get().is_none_or(|armed| next < armed) {
            self.arm(next).await?;
        }
        answer.map_err(Failed::Refused)
    }

    /// Sets the alarm for `at` on the ledger's clock.
    async fn arm(&self, at: i64) -> CellResult<()> {
        let real = at - self.offset_ms()?;
        self.state.storage().set_alarm(ScheduledTime::new(js_sys::Date::new(&JsValue::from_f64(real.max(js::now_ms() + 1_000) as f64)))).await?;
        self.armed.set(Some(at));
        Ok(())
    }

    /// The alarm's work: the sweep, then the next.
    async fn swept(&self) -> CellResult<core::Swept> {
        if self.meta("payer")?.is_none() {
            // a ledger no one has called has nothing to sweep
            return Ok(core::Swept { expired: Vec::new(), forgotten: 0 });
        }
        let (swept, next) = self.transact(|ledger, store, now| Ok(ledger.sweep(store, now)))?;
        let swept = swept.expect("a sweep refuses nothing");
        if !swept.expired.is_empty() {
            console_log!("{}", json!({ "event": "ledger.holds-expired", "expired": swept.expired }));
        }
        self.arm(next).await?;
        Ok(swept)
    }

    async fn route(&self, mut req: Request) -> Result<Value, Failed> {
        let payer = req.headers().get(PAYER_HEADER)?.filter(|p| fragment_core::npub::is_identity(p)).ok_or_else(|| CellError::host("a ledger call names its payer"))?;
        match self.meta("payer")? {
            None => self.set_meta("payer", &payer)?,
            Some(p) if p != payer => return Err(CellError::host(format!("this ledger is {p}'s, not {payer}'s")).into()),
            Some(_) => {}
        }
        if req.method() != Method::Post {
            return Err(CellError::new(ErrorCode::NotFound, format!("no route {} {}", req.method().as_ref(), req.path())).into());
        }
        let path = req.path();
        let body = req.bytes().await?;
        fn reply<T: Serialize>(answer: T) -> Result<Value, Failed> {
            Ok(serde_json::to_value(answer).expect("the ledger's answers serialize"))
        }
        fn done(answer: ()) -> Done {
            let () = answer;
            Done {}
        }
        match path.as_str() {
            <core::Reserve as Route>::PATH => {
                let r: core::Reserve = decode(&body)?;
                if self.cfg.test_hooks {
                    self.paid_call(&r.reference)?;
                }
                reply(self.mutate(move |l, s, now| l.reserve(s, &r, now)).await?)
            }
            <core::Settle as Route>::PATH => {
                let r: core::Settle = decode(&body)?;
                reply(self.mutate(move |l, s, now| l.settle(s, &r, now)).await?)
            }
            <core::Release as Route>::PATH => {
                let r: core::Release = decode(&body)?;
                reply(self.mutate(move |l, s, now| l.release(s, &r, now)).await?)
            }
            <core::Meter as Route>::PATH => {
                let m: core::Meter = decode(&body)?;
                reply(self.mutate(move |l, s, now| l.meter(s, &m, now)).await?)
            }
            MaySpend::PATH => {
                let q: MaySpend = decode(&body)?;
                reply(done(self.mutate(move |l, s, now| l.may_spend(s, q.spend, q.fragment.as_deref(), q.by_owner, now)).await?))
            }
            FragmentOpen::PATH => {
                let q: FragmentOpen = decode(&body)?;
                reply(done(self.mutate(move |l, s, now| l.fragment_open(s, &q.fragment, now)).await?))
            }
            <GrantCredit as Route>::PATH => {
                let c: GrantCredit = decode(&body)?;
                reply(done(self.mutate(move |l, s, now| l.grant(s, &c, now)).await?))
            }
            <SetPlan as Route>::PATH => {
                let c: SetPlan = decode(&body)?;
                reply(done(self.mutate(move |l, s, now| l.set_plan(s, &c, now)).await?))
            }
            <SetSeat as Route>::PATH => {
                let c: SetSeat = decode(&body)?;
                reply(done(self.mutate(move |l, s, now| l.set_seat(s, &c, now)).await?))
            }
            <SetOverdraft as Route>::PATH => {
                let c: SetOverdraft = decode(&body)?;
                reply(done(self.mutate(move |l, s, now| l.set_overdraft(s, &c, now)).await?))
            }
            <SetFragmentCap as Route>::PATH => {
                let c: SetFragmentCap = decode(&body)?;
                reply(done(self.mutate(move |l, s, now| l.set_cap(s, &c, now)).await?))
            }
            Status::PATH => {
                let Status {} = decode(&body)?;
                reply(self.mutate(|l, s, now| Ok(l.status(s, now))).await?)
            }
            TestHook::PATH if self.cfg.test_hooks => self.test_hook(decode(&body)?).await,
            p => Err(CellError::new(ErrorCode::NotFound, format!("no route {p}")).into()),
        }
    }

    /// Fleets with levers: a ledger whose paid calls are capped
    /// (`TestHook::PaidCalls`) counts each new reservation, and refuses one
    /// past the cap before anything is held. A retry of one it holds (the
    /// same reference) is no new call.
    fn paid_call(&self, reference: &str) -> Result<(), Failed> {
        assert!(self.cfg.test_hooks, "only a fleet with levers caps paid calls");
        let Some(max) = self.meta(PAID_CALLS_MAX)?.and_then(|v| v.parse::<u64>().ok()) else { return Ok(()) };
        let held: Vec<Value> = self.sql().exec("SELECT 1 AS n FROM entries WHERE ref = ? LIMIT 1", vec![reference.into()])?.to_array()?;
        if !held.is_empty() {
            return Ok(());
        }
        let used = self.meta(PAID_CALLS_USED)?.and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
        if used >= max {
            return Err(CellError::new(ErrorCode::BudgetUsedUp, format!("this test person's {max} paid calls are used up (the hosted e2e's cap)")).into());
        }
        self.set_meta(PAID_CALLS_USED, &(used + 1).to_string())?;
        Ok(())
    }

    async fn test_hook(&self, hook: TestHook) -> Result<Value, Failed> {
        assert!(self.cfg.test_hooks, "test hooks answer on test fleets only");
        match hook {
            TestHook::PaidCalls { max } => {
                self.set_meta(PAID_CALLS_MAX, &max.to_string())?;
                let used = self.meta(PAID_CALLS_USED)?.and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
                Ok(json!({ "max": max, "used": used }))
            }
            TestHook::Clock { offset_ms } => {
                self.set_meta("clock_offset", &offset_ms.to_string())?;
                let status = self.mutate(|l, s, now| Ok(l.status(s, now))).await?;
                // the alarm follows the clock
                self.armed.set(None);
                Ok(json!({ "offsetMs": offset_ms, "month": status.month }))
            }
            TestHook::Sweep => Ok(serde_json::to_value(self.swept().await?).expect("a sweep serializes")),
            TestHook::Entries { prefix } => {
                #[derive(Deserialize)]
                struct Row {
                    r#ref: String,
                    entry: String,
                }
                // a prefix compared whole: a Durable Object's SQLite refuses LIKE patterns past a few dozen bytes
                let binds = vec![SqlStorageValue::Integer(prefix.len() as i64), prefix.as_str().into(), SqlStorageValue::Integer(TEST_ENTRIES_MAX)];
                let rows: Vec<Row> = self.sql().exec("SELECT ref, entry FROM entries WHERE substr(ref, 1, ?) = ? ORDER BY ref LIMIT ?", binds)?.to_array()?;
                let entries: Vec<Value> = rows
                    .into_iter()
                    .map(|r| {
                        let entry: Value = serde_json::from_str(&r.entry).expect("an entry is JSON the ledger wrote");
                        json!({ "ref": r.r#ref, "entry": entry })
                    })
                    .collect();
                Ok(json!({ "entries": entries }))
            }
            TestHook::Totals => Ok(serde_json::to_value(self.mutate(|l, _, _| Ok(l.totals())).await?).expect("totals serialize")),
        }
    }
}

/// A ledger's refusal as its caller reads it: the platform's code and
/// message, and the core's typed reason when the ledger gave one (a cell
/// failure has none).
#[derive(Debug)]
pub struct LedgerError {
    pub code: ErrorCode,
    pub message: String,
    pub refused: Option<Refused>,
}

impl From<LedgerError> for CellError {
    fn from(e: LedgerError) -> CellError {
        CellError::new(e.code, e.message)
    }
}

impl From<CellError> for LedgerError {
    fn from(e: CellError) -> LedgerError {
        LedgerError { code: e.code, message: e.message, refused: None }
    }
}

impl From<worker::Error> for LedgerError {
    fn from(e: worker::Error) -> LedgerError {
        CellError::from(e).into()
    }
}

/// Asks `payer`'s ledger one of its routes (from a fragment, the router,
/// or the queue's consumer). Not reaching it is `ledger_unavailable`'s
/// stand-in, a host failure (5xx), which a gate may treat as passing (a
/// write, a wake); a paid call never does (`retried`, then refused).
pub async fn ask<R: Route>(env: &Env, payer: &str, request: &R) -> Result<R::Answer, LedgerError> {
    assert!(fragment_core::npub::is_identity(payer), "a ledger is a person's, named by their identity");
    let headers = Headers::new();
    headers.set(PAYER_HEADER, payer)?;
    headers.set("content-type", "application/json")?;
    let body = serde_json::to_string(request).expect("a ledger request serializes");
    let mut init = RequestInit::new();
    init.with_method(Method::Post).with_headers(headers).with_body(Some(body.into()));
    let req = Request::new_with_init(&format!("https://ledger.internal{}", R::PATH), &init)?;
    let mut resp = env.durable_object("LEDGER")?.get_by_name(payer)?.fetch_with_request(req).await?;
    let status = resp.status_code();
    let bytes = resp.bytes().await?;
    if status == 200 {
        return serde_json::from_slice(&bytes).map_err(|e| CellError::host(format!("the ledger's answer to {}: {e}", R::PATH)).into());
    }
    /// A refusal's body: `ErrorBody` and, from the ledger, its reason.
    #[derive(Deserialize)]
    struct Refusal {
        error: ErrorCode,
        message: String,
        refused: Option<Refused>,
    }
    match serde_json::from_slice::<Refusal>(&bytes) {
        Ok(r) => Err(LedgerError { code: r.error, message: r.message, refused: r.refused }),
        Err(_) => Err(CellError::host(format!("the ledger answered {status}")).into()),
    }
}

/// A ledger call is asked this many times while the ledger does not
/// answer, before a paid call is given up (a reservation) or left held,
/// for the sweep to charge at its worst case (a settle, a release).
const TRIES: usize = 3;

/// `ask`, asked again while the ledger does not answer (a host failure);
/// a refusal answers at once.
pub async fn retried<R: Route>(env: &Env, payer: &str, request: &R) -> Result<R::Answer, LedgerError> {
    let mut tries = 0;
    // bounded: TRIES asks, then the last answer stands
    loop {
        tries += 1;
        match ask(env, payer, request).await {
            Err(e) if e.refused.is_none() && e.code == ErrorCode::HostFailed && tries < TRIES => continue,
            answered => return answered,
        }
    }
}

/// Holds a paid call's worst case on `payer`'s ledger before the call is
/// made (docs/ledger.md): its amount. A refusal, or a ledger that does not
/// answer, holds nothing, and the call is not made. The call then settles
/// what it used, or is released having used nothing; a settle or release
/// that does not land leaves it held, and the ledger's sweep charges its
/// worst case: the money path fails closed.
pub async fn hold(env: &Env, payer: &str, reserve: &core::Reserve) -> Result<i64, LedgerError> {
    match retried(env, payer, reserve).await? {
        core::Reserved::Held { amount } => Ok(amount),
        // a reference of the call's own was never reserved before
        other => Err(CellError::host(format!("a fresh call's reservation {} answered {other:?}", reserve.reference)).into()),
    }
}
