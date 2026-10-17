//! A person's export of everything that is theirs (docs/api.md, Export;
//! the archive's layout, limits and tar are `fragment_core::export`'s).
//!
//!   POST /api/export                 start one (again while it builds: the same one)
//!   GET  /api/export                 its view
//!   GET  /api/export/{id}/download   its archive, to its person alone
//!
//! **Who.** Only the person, signed in as themselves: their session, or a
//! key of their own. An agent is refused, whomever it acts for, and so is
//! anything that is not a person (an operator's key no one holds names no
//! identity). There is no operator's export. An export and its archive are
//! kept in the person's own list (`Principal`), so a request of anyone
//! else's finds none (404).
//!
//! **What it reads, and how.** As its person, through the routes their
//! own requests take: each fragment's (`/api/f/{name}/…`, handed to the
//! fragment as the router hands a signed request: the fragment decides the
//! role, and meters the request to its owner), their computer's (its
//! owner's view, and its save through its own Durable Object, which checks
//! its owner), and their list. It reads nothing around them, so a part
//! sealed at rest later is read as its owner reads it.
//!
//! **Where it runs.** In the person's list, on its alarm, a slice of
//! `SLICE_MS` at a time, its place kept in the list's storage between
//! slices: the job (`exports.job`), the archive's bytes not uploaded yet
//! (`export_tail`, under a part), the parts uploaded (`export_parts`). A
//! crash, a deploy or a restart loses at most the slice it cut, which runs
//! again. The archive is an R2 multipart upload
//! (`exports/<npub>/<id>/archive.tar`), written front to back in parts of
//! `PART_BYTES`. An entry whose size is known only at its end (a channel's
//! records, a table's rows, a transcript) is held to `PIECE_BYTES`, then
//! put aside in pieces (`…/spool/<n>`) and copied in once its size is
//! known; a stored source (a computer's save, a blob) is copied a piece
//! at a time. So a build holds at most a part, a piece and a read at once.

use std::collections::{BTreeMap, VecDeque};

use fragment_core::export::{self as core, limits, Begun, Start, Tail};
use fragment_proto::export::{download_path, valid_export_id, ExportError, ExportFailure, ExportProgress, ExportState, ExportStatus, ExportView};
use fragment_proto::{ErrorCode, FragmentKind, FragmentList, Identity, IdentityKind, Role};
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use serde_json::{json, Value};
use worker::wasm_bindgen::JsValue;
use worker::*;

use crate::config::Config;
use crate::error::{CellError, CellResult};
use crate::fragment::{json_response, Caller, FragmentCell};
use crate::principal::PrincipalCell;
use crate::registry::calls::{self, By};
use crate::routed::{Routed, Signed};
use crate::{ask_registry, js, read_body};

/// A request to start or read one carries no body.
const BODY_MAX_BYTES: usize = 1024;

// ---- the router's routes ----

/// `/api/export…`.
pub(crate) async fn route(mut req: Request, env: &Env, cfg: &Config, url: &Url, rest: &[&str]) -> CellResult<Response> {
    match (req.method(), rest) {
        (Method::Post, []) => {
            let body = read_body(&mut req, BODY_MAX_BYTES).await?;
            let who = person(crate::signer(env, &req, url, &body).await?)?;
            if !body.is_empty() && body.iter().any(|b| !b.is_ascii_whitespace()) && serde_json::from_slice::<Value>(&body).ok() != Some(json!({})) {
                return Err(CellError::invalid("POST /api/export takes no body"));
            }
            principal(env, &who.id, Method::Post, "/export/start", Some(json!({ "person": who.id }).to_string()), None).await
        }
        (Method::Get, []) => {
            let who = person(crate::signer(env, &req, url, &[]).await?)?;
            principal(env, &who.id, Method::Get, "/export/status", None, None).await
        }
        (Method::Get | Method::Head, [id, "download"]) => {
            let who = downloader(env, cfg, &req, url).await?;
            if !valid_export_id(id) {
                return Err(CellError::new(ErrorCode::NotFound, "no such export"));
            }
            let range = req.headers().get("range")?;
            let method = req.method();
            principal(env, &who.id, method, &format!("/export/download?id={id}"), None, range.as_deref()).await
        }
        (m, _) => Err(CellError::new(ErrorCode::NotFound, format!("no route {} {}", m.as_ref(), url.path()))),
    }
}

/// Only a person exports, as themselves: an agent is refused whomever it
/// acts for (a `for` never reaches here: `signer` refuses it).
fn person(who: Signed) -> CellResult<Signed> {
    match who.kind {
        IdentityKind::Person => Ok(who),
        IdentityKind::Agent => Err(CellError::new(ErrorCode::Forbidden, "an export is its person's own: an agent can't start, read or download one")),
    }
}

/// Who downloads: a signed request of the person's (the CLI), the shell's
/// own fetch, or the person's click on the platform's own page: a
/// navigation the platform's page started (`Sec-Fetch-Site: same-origin`,
/// `Sec-Fetch-Mode: navigate`), with their platform session. No other page
/// downloads it with their cookie, a fragment's (one site with the
/// platform) included.
async fn downloader(env: &Env, cfg: &Config, req: &Request, url: &Url) -> CellResult<Signed> {
    if req.headers().get("authorization")?.is_some() || req.headers().get(crate::SHELL_HEADER)?.is_some() {
        return person(crate::signer(env, req, url, &[]).await?);
    }
    let header = |k: &str| req.headers().get(k).map(Option::unwrap_or_default);
    let own_page = cfg.is_platform_host(url.host_str().unwrap_or_default()) && header("sec-fetch-site")? == "same-origin" && header("sec-fetch-mode")? == "navigate";
    let token = if own_page { crate::auth::platform_session_token(req, url)? } else { None };
    let token = token.ok_or_else(|| CellError::new(ErrorCode::Unauthenticated, "sign in: an export downloads from Settings, or signed (`fragment export`)"))?;
    let live = ask_registry(env, &calls::Session { token, fragment: None, frame: false }).await?;
    person(Signed::new(live.identity, None))
}

/// One of the person's list's export routes, answered as it answers.
async fn principal(env: &Env, person: &str, method: Method, path: &str, body: Option<String>, range: Option<&str>) -> CellResult<Response> {
    let headers = Headers::new();
    if let Some(r) = range {
        headers.set("range", r)?;
    }
    let mut init = RequestInit::new();
    init.with_method(method).with_headers(headers);
    if let Some(b) = body {
        init.with_body(Some(b.into()));
    }
    let req = Request::new_with_init(&format!("https://principal.internal{path}"), &init)?;
    Ok(env.durable_object("PRINCIPAL")?.get_by_name(person)?.fetch_with_request(req).await?)
}

// ---- the fragment's side: its app's database, for its owner ----

impl FragmentCell {
    /// `GET /api/f/{name}/database` (its owner, never an agent): the app's
    /// database, as its platform code reads it in the facet (`__dump`,
    /// platform.mjs). With no `table`, its schema: each table's, index's,
    /// view's and trigger's statement, and the tables whose rows it holds
    /// (a virtual table's, not its shadow tables'). With one, a page of its
    /// rows from `offset`: at most `TABLE_PAGE` rows and about
    /// `TABLE_PAGE_MAX_BYTES` of their JSON, `next` the offset after them
    /// (`null` at the end). Without app code there is no database (404
    /// `no_code`). The platform's own table, its operation ledger, is left
    /// out.
    pub(crate) async fn database(&self, caller: &Caller, table: Option<String>, offset: Option<String>) -> CellResult<Response> {
        self.require(caller, false, Role::Owner)?;
        let facet = self.facet()?;
        let schema: Dumped = dumped(&facet.call("__dump", &[Value::Null, json!(0), json!(0), json!(0)]).await?)?;
        let entries = schema.schema.ok_or_else(|| app_dump("its schema"))?;
        let tables: Vec<String> = entries.iter().filter(|e| e["type"] == "table").filter_map(|e| e["name"].as_str().map(str::to_string)).collect();
        let Some(table) = table else {
            return json_response(&json!({ "schema": entries, "tables": tables }));
        };
        if !tables.contains(&table) {
            return Err(CellError::new(ErrorCode::NotFound, format!("the app's database has no table {table:?}")));
        }
        let offset: u64 = offset.as_deref().unwrap_or("0").parse().map_err(|_| CellError::invalid("offset is a row count"))?;
        let page: Dumped = dumped(&facet.call("__dump", &[json!(table), json!(offset), json!(limits::TABLE_PAGE), json!(limits::TABLE_PAGE_MAX_BYTES)]).await?)?;
        let rows = page.rows.ok_or_else(|| app_dump("a page of rows"))?;
        // the app's realm answered it: held to what its platform code answers
        let bytes: usize = rows.iter().map(|r| r.get().len()).sum();
        if rows.len() > limits::TABLE_PAGE || (rows.len() > 1 && bytes > 2 * limits::TABLE_PAGE_MAX_BYTES) {
            return Err(app_dump("a page past its bounds"));
        }
        let next = page.more.then(|| offset + rows.len() as u64);
        json_response(&json!({ "table": table, "offset": offset, "rows": rows, "next": next }))
    }
}

/// What `__dump` answers: the schema, or a page of rows.
#[derive(Deserialize)]
struct Dumped {
    /// Each statement: `{type, name, table, sql}`.
    #[serde(default)]
    schema: Option<Vec<Value>>,
    #[serde(default)]
    rows: Option<Vec<Box<RawValue>>>,
    #[serde(default)]
    more: bool,
}

fn dumped(v: &Value) -> CellResult<Dumped> {
    let text = v.as_str().ok_or_else(|| app_dump("not JSON text"))?;
    #[derive(Deserialize)]
    struct Envelope {
        result: Dumped,
    }
    serde_json::from_str::<Envelope>(text).map(|e| e.result).map_err(|e| app_dump(&e.to_string()))
}

fn app_dump(why: &str) -> CellError {
    CellError::new(ErrorCode::AppFailed, format!("the app facet's __dump answered what its platform code never does: {why}"))
}

// ---- the person's list: the export itself ----

/// Why a slice stopped short of its work: a failure that ends the export
/// (a limit, a source gone), or one that may pass (tried again later).
enum Stop {
    Fail(ExportError),
    Retry(String),
}

fn fail(code: ExportFailure, message: impl Into<String>) -> Stop {
    Stop::Fail(ExportError { code, message: message.into() })
}

/// A failure of the platform's: tried again when it may pass (a 5xx, a
/// 429, an outage), else it ends the export, saying so.
impl From<CellError> for Stop {
    fn from(e: CellError) -> Stop {
        if e.code.status() >= 500 || e.code == ErrorCode::RateLimited {
            Stop::Retry(e.message)
        } else {
            fail(ExportFailure::Unavailable, format!("{:?}: {}", e.code, e.message))
        }
    }
}

impl From<worker::Error> for Stop {
    fn from(e: worker::Error) -> Stop {
        Stop::Retry(e.to_string())
    }
}

type Step<T = ()> = Result<T, Stop>;

/// An export's row, as its routes read it.
#[derive(Deserialize)]
struct Row {
    id: String,
    person: String,
    state: String,
    started_at: i64,
    ready_at: Option<i64>,
    expires_at: Option<i64>,
    size: Option<i64>,
    error: Option<String>,
    job: Option<String>,
    upload: Option<String>,
    requests: i64,
    downloads: i64,
    gone_at: Option<i64>,

}

const ROW: &str = "SELECT id, person, state, started_at, ready_at, expires_at, size, error, job, upload, requests, downloads, gone_at FROM exports";

/// Where an export's objects are: its archive, and the pieces put aside.
fn prefix(person: &str, id: &str) -> String {
    format!("exports/{person}/{id}/")
}

fn archive_key(person: &str, id: &str) -> String {
    format!("{}archive.tar", prefix(person, id))
}

fn spool_key(person: &str, id: &str, n: u32) -> String {
    format!("{}spool/{n}", prefix(person, id))
}

/// A build's place: what is planned, where its next bytes come from, and
/// what it has written. Kept between slices as JSON (`exports.job`).
#[derive(Serialize, Deserialize)]
struct Job {
    person: String,
    made_at: i64,
    plan: Option<Plan>,
    at: At,
    /// Bytes to copy into the archive before `at` goes on.
    #[serde(default)]
    pending: VecDeque<Copy>,
    /// The entry being written whose size is known only at its end.
    #[serde(default)]
    open: Option<Open>,
    /// Parts uploaded (`export_parts`), and the archive's bytes so far.
    parts: u32,
    written: u64,
    entries: u32,
    /// Pieces put aside, numbered from 0.
    spooled: u32,
    done: u32,
    total: u32,
    /// R2 operations it made (metered as requests).
    requests: u64,
    attempt: u32,
    wait_until: i64,
    #[serde(default)]
    frag: Option<Frag>,
    #[serde(default)]
    comp: Option<Comp>,
}

#[derive(Serialize, Deserialize)]
struct Plan {
    email: Option<String>,
    agents: Vec<String>,
    fragments: Vec<Item>,
    shared: Vec<core::Shared>,
    computers: Vec<String>,
}

/// A fragment the archive holds, as the list that names it says.
#[derive(Clone, Serialize, Deserialize)]
struct Item {
    name: String,
    kind: FragmentKind,
    title: Option<String>,
    owner: String,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum At {
    Plan,
    Owners { next: usize },
    Readme,
    Manifest,
    Fragment { i: usize, step: FragStep },
    Computer { i: usize, step: CompStep },
    End,
    Complete,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum FragStep {
    Facts,
    Files { after: Option<String> },
    Schema,
    Tables { table: usize, offset: u64 },
    Channels { channel: usize, after: i64 },
    Transcript { chat: i64, work: i64 },
    Blobs { after: String },
    Meta,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum CompStep {
    Facts,
    Records { record: usize },
    Meta,
}

/// Bytes of a source to copy in, `done` of `size` so far, then `pad` zeros.
#[derive(Clone, Serialize, Deserialize)]
struct Copy {
    from: Source,
    size: u64,
    done: u64,
    pad: u32,
    /// They count toward the fragment's limit.
    fragment: bool,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Source {
    Spool { n: u32 },
    Blob { fragment: String, sha: String },
    Save { computer: String, record: String },
}

/// An entry of unknown size: the pieces of it put aside (number, size).
#[derive(Clone, Serialize, Deserialize)]
struct Open {
    path: String,
    pieces: Vec<(u32, u64)>,
    fragment: bool,
}

/// The fragment being exported: what its meta will say, who its people
/// are, what it has, and its part of the archive so far.
#[derive(Serialize, Deserialize)]
struct Frag {
    /// Its `meta.json`, a `core::FragmentMeta` once its facts are read, kept
    /// as JSON: the job is read back each slice, and a typed reading of the
    /// meta costs the cell's Wasm more than its budget leaves.
    meta: Value,
    names: BTreeMap<String, String>,
    /// Its channels, and the newest record of each as its facts were read.
    channels: Vec<(String, i64)>,
    tables: Option<Vec<String>>,
    schema: Option<String>,
    bytes: u64,
    /// Records or rows the open entry holds.
    counted: u64,
    blobs: usize,
}

#[derive(Serialize, Deserialize)]
struct Comp {
    id: String,
    view: Value,
    save: Option<core::SaveExported>,
    skipped: Vec<core::Skipped>,
}

impl Job {
    fn new(person: &str, now: i64) -> Job {
        Job {
            person: person.to_string(),
            made_at: now,
            plan: None,
            at: At::Plan,
            pending: VecDeque::new(),
            open: None,
            parts: 0,
            written: 0,
            entries: 0,
            spooled: 0,
            done: 0,
            total: 1,
            requests: 0,
            attempt: 0,
            wait_until: 0,
            frag: None,
            comp: None,
        }
    }

    fn plan(&self) -> &Plan {
        self.plan.as_ref().expect("a build past its plan has one")
    }

    fn frag(&mut self) -> &mut Frag {
        self.frag.as_mut().expect("a fragment's step has its facts")
    }

    /// Where the build goes after the fragment `i`, or the computer `c`.
    fn after_fragment(&self, i: usize) -> At {
        if i + 1 < self.plan().fragments.len() {
            At::Fragment { i: i + 1, step: FragStep::Facts }
        } else if !self.plan().computers.is_empty() {
            At::Computer { i: 0, step: CompStep::Facts }
        } else {
            At::End
        }
    }
}

/// One slice's working state: the job, and what is held in memory.
struct Run {
    id: String,
    key: String,
    job: Job,
    tail: Tail,
    /// The open entry's bytes not put aside yet.
    piece: Vec<u8>,
    /// Parts this slice uploaded (number, etag).
    new_parts: Vec<(u32, String)>,
    /// The upload's id once made.
    upload: Option<String>,
    /// The fragment's files as this slice listed them.
    files: Option<(usize, std::rc::Rc<Vec<Value>>)>,
    /// The most of an open entry held before it is put aside
    /// (`PIECE_BYTES`; smaller on a test fleet's lever, so a small export
    /// puts its entries aside in many pieces, as a large one does).
    piece_bytes: usize,
    /// The most the archive holds (`ARCHIVE_MAX_BYTES`; smaller on a test
    /// fleet's lever, so a small export meets its limit).
    archive_max: u64,
}

/// How a slice ended.
enum Sliced {
    /// Its time (or its pace) is up: its place is kept, and the next slice goes on.
    Paused,
    /// The archive's last bytes are written.
    Finished,
}

impl PrincipalCell {
    fn rows_of<T: serde::de::DeserializeOwned>(&self, q: &str, binds: Vec<SqlStorageValue>) -> CellResult<Vec<T>> {
        Ok(self.sql().exec(q, binds)?.to_array::<T>()?)
    }

    fn run_sql(&self, q: &str, binds: Vec<SqlStorageValue>) -> CellResult<()> {
        self.sql().exec(q, binds)?;
        Ok(())
    }

    /// The export routes (`/export/…`, the router's).
    pub(crate) async fn export_route(&self, mut req: Request) -> CellResult<Response> {
        let url = req.url()?;
        let query = |k: &str| url.query_pairs().find(|(q, _)| q == k).map(|(_, v)| v.into_owned());
        match (req.method(), url.path()) {
            (Method::Post, "/export/start") => {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct StartAsk {
                    person: String,
                }
                let b: StartAsk = serde_json::from_slice(&req.bytes().await?).map_err(|e| CellError::invalid(format!("body: {e}")))?;
                assert!(fragment_core::npub::is_identity(&b.person), "the router names its person");
                let view = self.start_export(&b.person).await?;
                Ok(Response::from_json(&view)?)
            }
            (Method::Get, "/export/status") => Ok(Response::from_json(&ExportStatus { export: self.newest()?.map(|r| view(&r)).transpose()? })?),
            (method @ (Method::Get | Method::Head), "/export/download") => {
                let id = query("id").unwrap_or_default();
                let range = req.headers().get("range")?;
                self.download(&id, method == Method::Head, range.as_deref()).await
            }
            (m, p) => Err(CellError::new(ErrorCode::NotFound, format!("no route {} {p}", m.as_ref()))),
        }
    }

    fn newest(&self) -> CellResult<Option<Row>> {
        Ok(self.rows_of::<Row>(&format!("{ROW} ORDER BY started_at DESC, id DESC LIMIT 1"), vec![])?.into_iter().next())
    }

    fn row(&self, id: &str) -> CellResult<Option<Row>> {
        Ok(self.rows_of::<Row>(&format!("{ROW} WHERE id = ?"), vec![id.into()])?.into_iter().next())
    }

    /// A press of "Export everything": the one building (a replay), a
    /// refusal past the day's count (429), or a new one, which replaces
    /// the last (its archive is deleted).
    async fn start_export(&self, person: &str) -> CellResult<ExportView> {
        let now = js::now_ms();
        let begun: Vec<Begun> = self.rows_of::<Row>(ROW, vec![])?.into_iter().map(|b| Begun { building: b.state == "building", id: b.id, started_at: b.started_at }).collect();
        let id = match core::start(&begun, now) {
            Start::Replay(id) => id,
            Start::Refused { next_at } => {
                return Err(CellError::new(
                    ErrorCode::RateLimited,
                    format!("you started {} exports in the last 24 hours, the most a day allows; the next may start at {}", limits::PER_DAY, core::readable(next_at)),
                ))
            }
            Start::New => {
                let id = js::random_hex::<8>();
                let job = serde_json::to_string(&Job::new(person, now)).map_err(|e| CellError::host(e.to_string()))?;
                // the one before ends: its archive is deleted on the alarm
                self.run_sql("UPDATE exports SET state = 'expired' WHERE state = 'ready'", vec![])?;
                self.run_sql(
                    "INSERT INTO exports (id, person, state, started_at, job) VALUES (?, ?, 'building', ?, ?)",
                    vec![id.as_str().into(), person.into(), SqlStorageValue::Integer(now), job.into()],
                )?;
                console_log!("{}", json!({ "export": id, "started": person }));
                self.state.storage().set_alarm(ScheduledTime::new(js_sys::Date::new(&JsValue::from_f64(now as f64)))).await?;
                id
            }
        };
        let row = self.row(&id)?.ok_or_else(|| CellError::host("an export just started or found has its row"))?;
        view(&row)
    }

    /// The archive, to its person (this list's), while it is ready; a
    /// download counts as a request, metered with its storage.
    async fn download(&self, id: &str, head: bool, range: Option<&str>) -> CellResult<Response> {
        let row = self.row(id)?.filter(|r| r.state == "ready" && r.expires_at.is_some_and(|at| at > js::now_ms()));
        let Some(row) = row else { return Err(CellError::new(ErrorCode::NotFound, "no such export, or it is not ready")) };
        let key = archive_key(&row.person, &row.id);
        let name = format!("fragment-export-{}.tar", &core::rfc3339(row.started_at)[..10]);
        let h = Headers::new();
        h.set("content-type", "application/x-tar")?;
        h.set("content-disposition", &format!("attachment; filename=\"{name}\""))?;
        h.set("cache-control", "private, no-store")?;
        h.set("x-content-type-options", "nosniff")?;
        h.set("accept-ranges", "bytes")?;
        self.run_sql("UPDATE exports SET downloads = downloads + 1 WHERE id = ?", vec![id.into()])?;
        if head {
            h.set("content-length", &row.size.unwrap_or(0).to_string())?;
            return Ok(Response::empty()?.with_headers(h));
        }
        let b = js::blob_get(&self.env, &key, range).await?.ok_or_else(|| CellError::host(format!("export {id}'s archive is missing")))?;
        let status = match b.range {
            Some((offset, length)) => {
                h.set("content-range", &format!("bytes {offset}-{}/{}", offset + length.max(1) - 1, b.size))?;
                h.set("content-length", &length.to_string())?;
                206
            }
            None => {
                h.set("content-length", &b.size.to_string())?;
                200
            }
        };
        Ok(Response::from_body(ResponseBody::Stream(b.body))?.with_status(status).with_headers(h))
    }

    /// The test fleets' levers (`/api/test/export`): `pace {steps | null}`
    /// lets the person's build take at most `steps` more steps, then wait
    /// (a second at a time) until it is let go (`null`): a build stopped
    /// part way, as a crash or a restart finds one. `piece {steps: bytes |
    /// null}` holds an open entry `bytes` at a time before putting it aside
    /// (1 KiB to `PIECE_BYTES`; `null`, `PIECE_BYTES`), so a small export
    /// puts its entries aside in many pieces, as a large one does. `cap
    /// {steps: bytes | null}` makes the archive's limit `bytes` (`null`,
    /// `ARCHIVE_MAX_BYTES`), so a small export meets it.
    pub(crate) fn export_lever(&self, body: &[u8]) -> CellResult<Value> {
        assert!(self.test_hooks, "a list's levers answer on fleets with them only");
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Lever {
            op: String,
            #[serde(default)]
            steps: Option<u32>,
        }
        let l: Lever = serde_json::from_slice(body).map_err(|e| CellError::invalid(format!("body: {e}")))?;
        let name = match l.op.as_str() {
            "pace" => "pace",
            "piece" if l.steps.is_none_or(|n| (1024..=limits::PIECE_BYTES as u32).contains(&n)) => "piece",
            "piece" => return Err(CellError::invalid(format!("a piece is 1024 to {} bytes", limits::PIECE_BYTES))),
            "cap" => "cap",
            other => return Err(CellError::new(ErrorCode::NotFound, format!("no export lever {other}: pace, piece, or cap"))),
        };
        match l.steps {
            Some(n) => self.run_sql("INSERT INTO export_levers (name, value) VALUES (?, ?) ON CONFLICT (name) DO UPDATE SET value = excluded.value", vec![name.into(), SqlStorageValue::Integer(i64::from(n))])?,
            None => self.run_sql("DELETE FROM export_levers WHERE name = ?", vec![name.into()])?,
        }
        Ok(json!({ name: l.steps }))
    }

    /// A test lever's value, when it is set (never on a fleet without levers).
    fn lever(&self, name: &str) -> CellResult<Option<u32>> {
        if !self.test_hooks {
            return Ok(None);
        }
        let rows = self.rows_of::<Value>("SELECT value FROM export_levers WHERE name = ?", vec![name.into()])?;
        Ok(rows.first().and_then(|p| p["value"].as_i64()).map(|v| v.clamp(0, i64::from(u32::MAX)) as u32))
    }

    fn pace(&self) -> CellResult<Option<u32>> {
        self.lever("pace")
    }

    fn set_pace(&self, left: Option<u32>) -> CellResult<()> {
        match left {
            Some(n) => self.run_sql("UPDATE export_levers SET value = ? WHERE name = 'pace'", vec![SqlStorageValue::Integer(i64::from(n))]),
            None => Ok(()),
        }
    }

    /// A wipe of its person (wipe.rs, `lists`): every export ends, its
    /// upload aborted, and everything under the person's exports' prefix
    /// deleted, before the list empties (and again on each wipe call after,
    /// for anything a slice cut short wrote late). Answers `{exports,
    /// more}`: `more`, objects are left (call again).
    pub(crate) async fn wipe_exports(&self, body: &[u8]) -> CellResult<Value> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Ask {
            person: String,
        }
        let ask: Ask = serde_json::from_slice(body).map_err(|e| CellError::invalid(format!("wipe/exports: {e}")))?;
        if !fragment_core::npub::is_identity(&ask.person) {
            return Err(CellError::invalid("wipe/exports names its person"));
        }
        let rows: Vec<Row> = if self.wiped.get() { vec![] } else { self.rows_of(ROW, vec![])? };
        for r in rows.iter().filter(|r| r.person == ask.person) {
            if let Some(upload) = &r.upload {
                js::upload_abort(&self.env, &archive_key(&r.person, &r.id), upload).await?;
            }
        }
        let (deleted, more) = js::blob_delete_under(&self.env, &format!("exports/{}/", ask.person), 10).await?;
        if !more && !self.wiped.get() {
            self.run_sql("UPDATE exports SET state = 'expired', job = NULL, upload = NULL, gone_at = COALESCE(gone_at, ?), metered = 1", vec![SqlStorageValue::Integer(js::now_ms())])?;
        }
        Ok(json!({ "exports": rows.len(), "objects": deleted, "more": more }))
    }

    /// The alarm: ready archives past their days expire, ended exports'
    /// objects go, their usage is metered, and the one building takes a
    /// slice; then the alarm is set for whichever is due next. A failure
    /// is logged and its part tried again at the next alarm.
    pub(crate) async fn export_alarm(&self) {
        if self.wiped.get() {
            return;
        }
        let now = js::now_ms();
        if let Err(e) = self.run_sql("UPDATE exports SET state = 'expired' WHERE state = 'ready' AND expires_at <= ?", vec![SqlStorageValue::Integer(now)]) {
            console_error!("{}", json!({ "export": "expire", "error": e.message }));
        }
        if let Err(e) = self.clean(now).await {
            console_error!("{}", json!({ "export": "clean", "error": e.message }));
        }
        match self.newest() {
            Ok(Some(r)) if r.state == "building" => {
                if let Err(e) = self.slice(&r).await {
                    console_error!("{}", json!({ "export": r.id, "slice": "failed to keep its place", "error": e.message }));
                }
            }
            Ok(_) => {}
            Err(e) => console_error!("{}", json!({ "export": "read", "error": e.message })),
        }
        if let Err(e) = self.arm_export().await {
            console_error!("{}", json!({ "export": "alarm", "error": e.message }));
        }
    }

    /// Sets the alarm for what is due next, read from the rows as they are
    /// now, with nothing awaited between the reading and the setting (so a
    /// press that came during the slice is not set aside): the build's next
    /// slice (at once; its retry's wait; a second while a test paces it), a
    /// ready archive's expiry, or another try at an ended one's cleanup.
    async fn arm_export(&self) -> CellResult<()> {
        let now = js::now_ms();
        let mut due: Vec<i64> = vec![];
        for r in self.rows_of::<Row>(ROW, vec![])? {
            match r.state.as_str() {
                "building" => {
                    let wait = r.job.as_deref().and_then(|j| serde_json::from_str::<Job>(j).ok()).map_or(0, |j| j.wait_until);
                    let paced = self.pace()? == Some(0);
                    due.push(wait.max(if paced { now + 1000 } else { now }));
                }
                "ready" => due.push(r.expires_at.unwrap_or(now)),
                _ if r.gone_at.is_none() => due.push(now + 30_000),
                _ => {}
            }
        }
        let unmetered: Vec<Value> = self.rows_of("SELECT id FROM exports WHERE gone_at IS NOT NULL AND metered = 0", vec![])?;
        if !unmetered.is_empty() {
            due.push(now + 30_000);
        }
        match due.into_iter().min() {
            Some(at) => self.state.storage().set_alarm(ScheduledTime::new(js_sys::Date::new(&JsValue::from_f64(at.max(now) as f64)))).await?,
            None => self.state.storage().delete_alarm().await?,
        }
        Ok(())
    }

    /// Ended exports (failed, or expired) lose their objects (ten pages of
    /// them an alarm), then are metered; rows past the newest `KEPT_ROWS`
    /// that are done go. What is left is tried at the next alarm.
    async fn clean(&self, now: i64) -> CellResult<()> {
        let ended: Vec<Row> = self.rows_of(&format!("{ROW} WHERE state IN ('failed', 'expired') AND (gone_at IS NULL OR metered = 0)"), vec![])?;
        for r in ended {
            if r.gone_at.is_none() {
                if let Some(upload) = &r.upload {
                    js::upload_abort(&self.env, &archive_key(&r.person, &r.id), upload).await?;
                }
                let (_, more) = js::blob_delete_under(&self.env, &prefix(&r.person, &r.id), 10).await?;
                if more {
                    continue;
                }
                self.run_sql("UPDATE exports SET gone_at = ?, upload = NULL WHERE id = ?", vec![SqlStorageValue::Integer(now), r.id.as_str().into()])?;
            }
            if self.meter(&r, r.gone_at.unwrap_or(now)).await {
                self.run_sql("UPDATE exports SET metered = 1 WHERE id = ?", vec![r.id.as_str().into()])?;
            }
        }
        self.run_sql(
            "DELETE FROM exports WHERE gone_at IS NOT NULL AND metered = 1 AND id NOT IN (SELECT id FROM exports ORDER BY started_at DESC LIMIT ?)",
            vec![SqlStorageValue::Integer(limits::KEPT_ROWS as i64)],
        )
    }

    /// Its usage, once, to its person's ledger (decision 24): its archive's
    /// storage while it was kept (byte-hours) and its requests (its R2
    /// operations and its downloads). Answers whether the ledger took it
    /// (a guest's refuses it: billed nothing).
    async fn meter(&self, r: &Row, gone_at: i64) -> bool {
        use fragment_core::ledger::{Meter, MeterRow};
        use fragment_core::price::{StorageClass, Usage};
        let hours_ms = r.ready_at.map_or(0, |ready| (gone_at - ready).max(0));
        let byte_hours = (r.size.unwrap_or(0).max(0) as u128 * hours_ms as u128).div_ceil(3_600_000) as u64;
        let mut rows = vec![MeterRow {
            reference: format!("export:{}:requests", r.id),
            usage: Usage::Requests { count: (r.requests + r.downloads).max(1) as u64 },
            fragment: None,
            agent: None,
            computer: None,
            at_ms: gone_at,
        }];
        if byte_hours > 0 {
            rows.push(MeterRow { reference: format!("export:{}:storage", r.id), usage: Usage::Storage { class: StorageClass::R2, byte_hours }, fragment: None, agent: None, computer: None, at_ms: gone_at });
        }
        let meter = Meter { batch: format!("export:{}", r.id), rows };
        match crate::ledger::ask(&self.env, &r.person, &meter).await {
            Ok(m) => {
                for refused in &m.refused {
                    console_error!("{}", json!({ "export": r.id, "meter": "refused", "why": refused.why }));
                }
                true
            }
            Err(e) if e.refused.is_some() => {
                console_log!("{}", json!({ "export": r.id, "meter": "refused", "why": e.message }));
                true
            }
            Err(e) => {
                console_error!("{}", json!({ "export": r.id, "meter": "failed", "error": e.message }));
                false
            }
        }
    }

    /// One slice of the export building: from its place, until its time is
    /// up, then its place kept (`arm_export` sets when the next is due).
    async fn slice(&self, r: &Row) -> CellResult<()> {
        let now = js::now_ms();
        let job: Job = serde_json::from_str(r.job.as_deref().unwrap_or("null")).map_err(|e| CellError::host(format!("export {}'s job: {e}", r.id)))?;
        if job.wait_until > now || self.pace()? == Some(0) {
            return Ok(());
        }
        if now - r.started_at > limits::BUILD_MAX_MS {
            return self.end_failed(r, ExportError { code: ExportFailure::TimedOut, message: format!("it did not finish within {} hours", limits::BUILD_MAX_MS / 3_600_000) }).await;
        }
        // a finish cut short after its upload completed: the archive is there
        if let (Some(_), Some(size)) = (&r.upload, js::blob_head(&self.env, &archive_key(&r.person, &r.id)).await?) {
            console_log!("{}", json!({ "export": r.id, "finished": "before", "size": size }));
            return self.ready(r, &job, size).await;
        }
        let mut kept: Vec<u8> = vec![];
        for row in self.sql().exec("SELECT bytes FROM export_tail WHERE export = ? ORDER BY n", vec![r.id.as_str().into()])?.raw() {
            match row?.into_iter().next() {
                Some(SqlStorageValue::Blob(b)) => kept.extend(b),
                other => return Err(CellError::host(format!("export {}'s tail holds {other:?}", r.id))),
            }
        }
        let piece_bytes = self.lever("piece")?.map_or(limits::PIECE_BYTES, |n| n as usize);
        let archive_max = self.lever("cap")?.map_or(limits::ARCHIVE_MAX_BYTES, u64::from);
        let mut run = Run { id: r.id.clone(), key: archive_key(&r.person, &r.id), tail: Tail::new(limits::PART_BYTES, kept), job, piece: vec![], new_parts: vec![], upload: r.upload.clone(), files: None, piece_bytes, archive_max };
        let mut pace = self.pace()?;
        let outcome = self.steps(&mut run, now + limits::SLICE_MS, &mut pace).await;
        let outcome = match outcome {
            Ok(Sliced::Finished) => self.finish(r, run).await,
            Ok(Sliced::Paused) => self.keep_place(&mut run).await,
            Err(stop) => Err(stop),
        };
        self.set_pace(pace)?;
        match outcome {
            Ok(()) => Ok(()),
            Err(Stop::Fail(e)) => self.end_failed(r, e).await,
            Err(Stop::Retry(why)) => {
                // the slice is lost: its place is the last one kept
                let mut kept: Job = serde_json::from_str(r.job.as_deref().unwrap_or("null")).map_err(|e| CellError::host(e.to_string()))?;
                kept.attempt += 1;
                console_error!("{}", json!({ "export": r.id, "retry": kept.attempt, "why": why }));
                if kept.attempt >= limits::ATTEMPTS_MAX {
                    return self.end_failed(r, ExportError { code: ExportFailure::Unavailable, message: format!("a part kept failing ({} tries): {why}", kept.attempt) }).await;
                }
                kept.wait_until = js::now_ms() + core::retry_ms(kept.attempt);
                let text = serde_json::to_string(&kept).map_err(|e| CellError::host(e.to_string()))?;
                self.run_sql("UPDATE exports SET job = ? WHERE id = ?", vec![text.into(), r.id.as_str().into()])
            }
        }
    }

    /// The export stopped, saying why: nothing of it is kept (the alarm
    /// aborts its upload and deletes what it put aside).
    async fn end_failed(&self, r: &Row, e: ExportError) -> CellResult<()> {
        console_log!("{}", json!({ "export": r.id, "failed": e.code, "message": e.message }));
        let error = serde_json::to_string(&e).map_err(|e| CellError::host(e.to_string()))?;
        let requests = r.job.as_deref().and_then(|j| serde_json::from_str::<Job>(j).ok()).map_or(0, |j| j.requests);
        self.run_sql("UPDATE exports SET state = 'failed', error = ?, job = NULL, requests = ? WHERE id = ?", vec![error.into(), SqlStorageValue::Integer(requests as i64), r.id.as_str().into()])?;
        self.run_sql("DELETE FROM export_tail WHERE export = ?", vec![r.id.as_str().into()])?;
        self.run_sql("DELETE FROM export_parts WHERE export = ?", vec![r.id.as_str().into()])?;
        self.run_sql("DELETE FROM export_blobs WHERE export = ?", vec![r.id.as_str().into()])
    }

    /// Keeps the slice's place: the open entry's bytes put aside, then, in
    /// one transaction, the job, the tail, and the parts uploaded.
    async fn keep_place(&self, run: &mut Run) -> Step {
        // a wipe emptied the list under the slice: it writes nothing back
        if self.wiped.get() {
            return Err(fail(ExportFailure::Unavailable, "its person was wiped"));
        }
        if run.job.open.is_some() {
            self.spool(run).await?;
        }
        let id = run.id.clone();
        let job = serde_json::to_string(&run.job).map_err(|e| Stop::Retry(e.to_string()))?;
        let tail = run.tail.bytes().to_vec();
        let parts = std::mem::take(&mut run.new_parts);
        let (sql, written, requests) = (self.sql(), run.job.written, run.job.requests);
        // the blobs of the fragments done: kept with the place, so a slice
        // lost after a fragment's end finds them again
        let current = match &run.job.at {
            At::Fragment { i, .. } => run.job.plan().fragments.get(*i).map(|f| f.name.clone()).unwrap_or_default(),
            _ => String::new(),
        };
        js::transaction_sync(&self.raw, move || {
            sql.exec("UPDATE exports SET job = ?, size = ?, requests = ? WHERE id = ?", vec![job.into(), SqlStorageValue::Integer(written as i64), SqlStorageValue::Integer(requests as i64), id.as_str().into()])?;
            sql.exec("DELETE FROM export_blobs WHERE export = ? AND fragment <> ?", vec![id.as_str().into(), current.as_str().into()])?;
            sql.exec("DELETE FROM export_tail WHERE export = ?", vec![id.as_str().into()])?;
            for (n, chunk) in tail.chunks(1024 * 1024).enumerate() {
                sql.exec("INSERT INTO export_tail (export, n, bytes) VALUES (?, ?, ?)", vec![id.as_str().into(), SqlStorageValue::Integer(n as i64), SqlStorageValue::Blob(chunk.to_vec())])?;
            }
            for (n, etag) in &parts {
                sql.exec("INSERT INTO export_parts (export, part, etag) VALUES (?, ?, ?) ON CONFLICT (export, part) DO UPDATE SET etag = excluded.etag", vec![id.as_str().into(), SqlStorageValue::Integer(i64::from(*n)), etag.as_str().into()])?;
            }
            Ok(())
        })
        .map_err(Stop::from)
    }

    /// The archive is whole: its last part, the upload completed (or found
    /// completed, a finish cut short before), the export ready, what it put
    /// aside deleted, and its person mailed a link to Settings.
    async fn finish(&self, r: &Row, mut run: Run) -> Step {
        if self.wiped.get() {
            return Err(fail(ExportFailure::Unavailable, "its person was wiped"));
        }
        let last = std::mem::take(&mut run.tail).into_bytes();
        if !last.is_empty() || run.job.parts == 0 {
            self.upload_part(&mut run, last).await?;
        }
        let kept = self.rows_of::<Value>("SELECT part, etag FROM export_parts WHERE export = ?", vec![run.id.as_str().into()])?;
        let mut parts: BTreeMap<u32, String> = kept.iter().filter_map(|p| Some((p["part"].as_u64()? as u32, p["etag"].as_str()?.to_string()))).collect();
        parts.extend(run.new_parts.iter().cloned());
        let parts: Vec<(u16, String)> = parts.into_iter().filter(|(n, _)| *n <= run.job.parts).map(|(n, e)| (n as u16, e)).collect();
        if parts.len() as u32 != run.job.parts {
            return Err(fail(ExportFailure::Unavailable, format!("{} of its {} parts are named", parts.len(), run.job.parts)));
        }
        let upload = run.upload.clone().ok_or_else(|| Stop::Retry("the archive's upload has no id".into()))?;
        let size = match js::upload_complete(&self.env, &run.key, &upload, &parts).await {
            Ok(size) => size,
            // completed by a finish cut short: the archive is there
            Err(e) => match js::blob_head(&self.env, &run.key).await? {
                Some(size) => size,
                None => return Err(Stop::Retry(e.message)),
            },
        };
        if size != run.job.written {
            return Err(fail(ExportFailure::Unavailable, format!("the archive stored is {size} bytes, not the {} written", run.job.written)));
        }
        console_log!("{}", json!({ "export": run.id, "entries": run.job.entries, "parts": run.job.parts }));
        self.ready(r, &run.job, size).await.map_err(Stop::from)
    }

    /// The export is ready: kept for `KEPT_MS`, what it put aside deleted,
    /// and its person mailed a link to Settings (never to the archive).
    async fn ready(&self, r: &Row, job: &Job, size: u64) -> CellResult<()> {
        let now = js::now_ms();
        let expires = now + limits::KEPT_MS;
        let requests = job.requests + 1;
        let id = r.id.clone();
        let sql = self.sql();
        js::transaction_sync(&self.raw, move || {
            sql.exec(
                "UPDATE exports SET state = 'ready', ready_at = ?, expires_at = ?, size = ?, job = NULL, upload = NULL, requests = ? WHERE id = ?",
                vec![SqlStorageValue::Integer(now), SqlStorageValue::Integer(expires), SqlStorageValue::Integer(size as i64), SqlStorageValue::Integer(requests as i64), id.as_str().into()],
            )?;
            for table in ["export_tail", "export_parts", "export_blobs"] {
                sql.exec(&format!("DELETE FROM {table} WHERE export = ?"), vec![id.as_str().into()])?;
            }
            Ok(())
        })?;
        console_log!("{}", json!({ "export": r.id, "ready": size }));
        if let Err(e) = js::blob_delete_under(&self.env, &format!("{}spool/", prefix(&r.person, &r.id)), 10).await {
            console_error!("{}", json!({ "export": r.id, "spool": "left, for its expiry", "error": e.message }));
        }
        if let Some(to) = job.plan.as_ref().and_then(|p| p.email.clone()) {
            let cfg = Config::from_env(&self.env);
            let mail = crate::mail::Mail {
                to,
                subject: format!("Your {} export is ready", fragment_core::mail::PRODUCT),
                text: format!(
                    "Your export of everything that is yours is ready: {} in one file.\n\nDownload it from Settings, under Your data:\n{}/settings\n\nIt is kept until {}, then deleted.\n",
                    bytes_text(size),
                    cfg.platform(),
                    core::readable(expires)
                ),
            };
            if let Err(e) = crate::mail::send(&self.env, cfg, &mail).await {
                console_error!("{}", json!({ "export": r.id, "mail": "not sent", "error": e.message }));
            }
        }
        Ok(())
    }

    /// The build's steps, from its place, until its time (or its pace) is up.
    async fn steps(&self, run: &mut Run, deadline: i64, pace: &mut Option<u32>) -> Step<Sliced> {
        // bounded: each step does a bounded piece of work, and the slice's time ends it
        loop {
            if js::now_ms() > deadline || *pace == Some(0) {
                return Ok(Sliced::Paused);
            }
            if let Some(p) = pace.as_mut() {
                *p -= 1;
            }
            if !run.job.pending.is_empty() {
                self.copy_piece(run).await?;
                continue;
            }
            let at = run.job.at.clone();
            match at {
                At::Plan => self.plan(run).await?,
                At::Owners { next } => self.owners(run, next, deadline).await?,
                At::Readme => {
                    self.entry(run, &core::readme_path(), core::README.as_bytes(), false).await?;
                    run.job.at = At::Manifest;
                }
                At::Manifest => {
                    let m = self.manifest(&run.job, &run.id);
                    self.entry(run, &core::manifest_path(), &core::json_file(&m), false).await?;
                    run.job.done += 1;
                    run.job.at = match (run.job.plan().fragments.is_empty(), run.job.plan().computers.is_empty()) {
                        (false, _) => At::Fragment { i: 0, step: FragStep::Facts },
                        (true, false) => At::Computer { i: 0, step: CompStep::Facts },
                        (true, true) => At::End,
                    };
                }
                At::Fragment { i, step } => self.fragment_step(run, i, step, deadline).await?,
                At::Computer { i, step } => self.computer_step(run, i, step).await?,
                At::End => {
                    self.put(run, &core::END, false).await?;
                    run.job.at = At::Complete;
                }
                At::Complete => return Ok(Sliced::Finished),
            }
        }
    }

    // ---- writing the archive ----

    /// Bytes onto the archive: whole parts upload as they fill. Past the
    /// archive's limit, or the fragment's (`fragment`), it fails.
    async fn put(&self, run: &mut Run, bytes: &[u8], fragment: bool) -> Step {
        let n = bytes.len() as u64;
        if run.job.written + n > run.archive_max {
            return Err(fail(ExportFailure::TooLarge, format!("the archive would pass {}, the most an export holds", bytes_text(run.archive_max))));
        }
        if fragment {
            let f = run.job.frag();
            f.bytes += n;
            if f.bytes > limits::FRAGMENT_MAX_BYTES {
                let name = f.meta["name"].as_str().unwrap_or("").to_string();
                return Err(fail(ExportFailure::TooLarge, format!("{name} would pass {}, the most one fragment's part of an export holds", bytes_text(limits::FRAGMENT_MAX_BYTES))));
            }
        }
        run.job.written += n;
        for part in run.tail.push(bytes) {
            self.upload_part(run, part).await?;
        }
        Ok(())
    }

    /// The next part. The upload is made at the first, its id kept at once
    /// (so a slice cut short leaves none no one can abort).
    async fn upload_part(&self, run: &mut Run, part: Vec<u8>) -> Step {
        let upload = match &run.upload {
            Some(u) => u.clone(),
            None => {
                let u = js::upload_start(&self.env, &run.key, "application/x-tar").await?;
                self.run_sql("UPDATE exports SET upload = ? WHERE id = ?", vec![u.as_str().into(), run.id.as_str().into()])?;
                run.upload = Some(u.clone());
                u
            }
        };
        run.job.parts += 1;
        if run.job.parts as usize > 10_000 {
            return Err(fail(ExportFailure::TooLarge, "the archive would pass R2's 10,000 parts"));
        }
        let etag = js::upload_part(&self.env, &run.key, &upload, run.job.parts as u16, part).await?;
        run.new_parts.push((run.job.parts, etag));
        run.job.requests += 1;
        Ok(())
    }

    async fn header(&self, run: &mut Run, path: &str, size: u64, fragment: bool) -> Step {
        run.job.entries += 1;
        if run.job.entries > limits::ENTRIES_MAX {
            return Err(fail(ExportFailure::TooMany, format!("the archive would pass {} entries", limits::ENTRIES_MAX)));
        }
        let h = core::header(path, size, (run.job.made_at / 1000) as u64);
        self.put(run, &h, fragment).await
    }

    /// An entry whose bytes are at hand.
    async fn entry(&self, run: &mut Run, path: &str, bytes: &[u8], fragment: bool) -> Step {
        self.header(run, path, bytes.len() as u64, fragment).await?;
        self.put(run, bytes, fragment).await?;
        self.put(run, &vec![0; core::padding(bytes.len() as u64)], fragment).await
    }

    /// An entry of a stored source: its header now, its bytes copied next.
    async fn stored(&self, run: &mut Run, path: &str, from: Source, size: u64, fragment: bool) -> Step {
        self.header(run, path, size, fragment).await?;
        match size {
            0 => Ok(()),
            _ => {
                run.job.pending.push_back(Copy { from, size, done: 0, pad: core::padding(size) as u32, fragment });
                Ok(())
            }
        }
    }

    /// Opens an entry whose size is known only at its end.
    fn open(&self, run: &mut Run, path: String, fragment: bool) {
        assert!(run.job.open.is_none() && run.piece.is_empty(), "one entry is open at a time");
        run.job.open = Some(Open { path, pieces: vec![], fragment });
    }

    /// Bytes of the open entry: held to a piece, then put aside.
    async fn more(&self, run: &mut Run, bytes: &[u8]) -> Step {
        run.piece.extend_from_slice(bytes);
        let open = run.job.open.as_ref().expect("bytes go to an open entry");
        let held: u64 = open.pieces.iter().map(|(_, s)| s).sum::<u64>() + run.piece.len() as u64;
        if run.job.written + held > run.archive_max {
            return Err(fail(ExportFailure::TooLarge, format!("the archive would pass {}, the most an export holds", bytes_text(run.archive_max))));
        }
        if open.fragment && run.job.frag.as_ref().is_some_and(|f| f.bytes + held > limits::FRAGMENT_MAX_BYTES) {
            let name = run.job.frag().meta["name"].as_str().unwrap_or("").to_string();
            return Err(fail(ExportFailure::TooLarge, format!("{name} would pass {}, the most one fragment's part of an export holds", bytes_text(limits::FRAGMENT_MAX_BYTES))));
        }
        if run.piece.len() >= run.piece_bytes {
            self.spool(run).await?;
        }
        Ok(())
    }

    /// Puts the open entry's piece aside in R2.
    async fn spool(&self, run: &mut Run) -> Step {
        if run.piece.is_empty() {
            return Ok(());
        }
        let n = run.job.spooled;
        let piece = std::mem::take(&mut run.piece);
        let size = piece.len() as u64;
        js::blob_put_bytes(&self.env, &spool_key(&run.job.person, &run.id, n), &piece).await?;
        run.job.spooled += 1;
        run.job.requests += 1;
        run.job.open.as_mut().expect("a piece is an open entry's").pieces.push((n, size));
        Ok(())
    }

    /// Closes the open entry: written at once when it is one piece, else
    /// its header now and its pieces copied in next.
    async fn close(&self, run: &mut Run) -> Step<u64> {
        if run.job.open.as_ref().is_some_and(|o| o.pieces.is_empty()) {
            let open = run.job.open.take().expect("an open entry");
            let piece = std::mem::take(&mut run.piece);
            self.entry(run, &open.path, &piece, open.fragment).await?;
            return Ok(piece.len() as u64);
        }
        self.spool(run).await?;
        let open = run.job.open.take().expect("an open entry");
        let total: u64 = open.pieces.iter().map(|(_, s)| s).sum();
        self.header(run, &open.path, total, open.fragment).await?;
        let last = open.pieces.len() - 1;
        for (i, (n, size)) in open.pieces.iter().enumerate() {
            let pad = if i == last { core::padding(total) as u32 } else { 0 };
            run.job.pending.push_back(Copy { from: Source::Spool { n: *n }, size: *size, done: 0, pad, fragment: open.fragment });
        }
        Ok(total)
    }

    /// One piece of the first source to copy in.
    async fn copy_piece(&self, run: &mut Run) -> Step {
        let c = run.job.pending.front().cloned().expect("a copy pending");
        let length = (c.size - c.done).min(limits::PIECE_BYTES as u64);
        assert!(length > 0, "a copy pending has bytes left");
        let bytes = match &c.from {
            Source::Spool { n } => {
                run.job.requests += 1;
                js::blob_range(&self.env, &spool_key(&run.job.person, &run.id, *n), c.done, length).await?.ok_or_else(|| Stop::Retry(format!("piece {n} put aside is missing")))?
            }
            Source::Blob { fragment, sha } => {
                let range = format!("bytes={}-{}", c.done, c.done + length - 1);
                let mut resp = self.as_person(&run.job.person, fragment, Method::Get, &format!("blobs/{sha}"), "", Some(&range)).await?;
                let whole = resp.headers().get("content-range")?.and_then(|r| r.rsplit('/').next().and_then(|t| t.parse::<u64>().ok()));
                match resp.status_code() {
                    206 if whole == Some(c.size) => resp.bytes().await?,
                    200 if c.done == 0 && length == c.size => resp.bytes().await?,
                    404 => return Err(fail(ExportFailure::SourceChanged, format!("{fragment}'s blob {sha} went away as it was copied"))),
                    s if s >= 500 || s == 429 => return Err(Stop::Retry(format!("{fragment}'s blob {sha} answered {s}"))),
                    s => return Err(fail(ExportFailure::SourceChanged, format!("{fragment}'s blob {sha} answered {s} for its bytes {range}"))),
                }
            }
            Source::Save { computer, record } => {
                let body = json!({ "owner": run.job.person, "record": record, "offset": c.done, "length": length });
                let req = crate::routed::internal_request("computer/export-read", &body.to_string())?;
                let mut resp = self.env.durable_object("COMPUTER")?.get_by_name(computer)?.fetch_with_request(req).await?;
                match resp.status_code() {
                    200 => resp.bytes().await?,
                    404 => return Err(fail(ExportFailure::SourceChanged, format!("its computer's save went away as it was copied ({}): export again", resp.text().await.unwrap_or_default()))),
                    s => return Err(Stop::Retry(format!("its computer's save answered {s}"))),
                }
            }
        };
        if bytes.len() as u64 != length {
            return Err(Stop::Retry(format!("a read of {length} bytes answered {}", bytes.len())));
        }
        self.put(run, &bytes, c.fragment).await?;
        let front = run.job.pending.front_mut().expect("the copy read");
        front.done += length;
        if front.done == front.size {
            let pad = front.pad as usize;
            run.job.pending.pop_front();
            self.put(run, &vec![0; pad], c.fragment).await?;
        }
        Ok(())
    }

    // ---- reading as the person ----

    /// A fragment's route (`/api/f/{name}/{path}{query}`), asked as the
    /// person, as the router hands a request they signed: the fragment
    /// decides what they may read.
    async fn as_person(&self, person: &str, name: &str, method: Method, path: &str, query: &str, range: Option<&str>) -> CellResult<Response> {
        let cfg = Config::from_env(&self.env);
        let identity = Identity { id: person.to_string(), kind: IdentityKind::Person, owner: None, held: None, fenced: false };
        let (inner, url) = match path.strip_prefix("serve/") {
            // a site route, on the fragment's own origin
            Some(site) => (format!("/serve/{site}"), format!("{}{site}{query}", cfg.canonical(&Url::parse(&cfg.platform()).map_err(|e| CellError::host(e.to_string()))?, name))),
            None => (format!("/api/{path}"), format!("{}/api/f/{name}/{path}{query}", cfg.platform())),
        };
        let url = Url::parse(&url).map_err(|e| CellError::host(format!("{url}: {e}")))?;
        let routed = Routed { name: name.to_string(), url, signed: Some(Signed::new(identity, None)), credential: None };
        let headers = Headers::new();
        routed.to_headers(&headers)?;
        if let Some(r) = range {
            headers.set("range", r)?;
        }
        let mut init = RequestInit::new();
        init.with_method(method).with_headers(headers);
        let req = Request::new_with_init(&format!("https://fragment.internal{inner}"), &init)?;
        Ok(self.env.durable_object("FRAGMENT")?.get_by_name(name)?.fetch_with_request(req).await?)
    }

    /// A fragment's route as the person: its answer's bytes, or its refusal
    /// (a 4xx: the step decides); a failure that may pass is the slice's.
    /// (Untyped, so the cell's Wasm holds one reader, not one a type.)
    async fn read_bytes(&self, person: &str, name: &str, path: &str, query: &str) -> Step<Result<Vec<u8>, CellError>> {
        let mut resp = self.as_person(person, name, Method::Get, path, query, None).await?;
        let status = resp.status_code();
        let bytes = resp.bytes().await?;
        if status == 200 {
            return Ok(Ok(bytes));
        }
        let refusal: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        let e = match (refusal["error"].as_str().and_then(|c| serde_json::from_value::<ErrorCode>(json!(c)).ok()), refusal["message"].as_str()) {
            (Some(code), Some(message)) => CellError::new(code, message),
            _ => CellError::host(format!("{name}'s {path} answered {status}")),
        };
        if status >= 500 || status == 429 {
            return Err(Stop::Retry(e.message));
        }
        Ok(Err(e))
    }

    /// `read_bytes`, its answer JSON.
    async fn read_json(&self, person: &str, name: &str, path: &str, query: &str) -> Step<Result<Value, CellError>> {
        match self.read_bytes(person, name, path, query).await? {
            Ok(bytes) => serde_json::from_slice(&bytes).map(Ok).map_err(|e| Stop::Retry(format!("{name}'s {path}: {e}"))),
            Err(e) => Ok(Err(e)),
        }
    }

    // ---- the plan ----

    /// Whose it is and what it holds: the person and their agents (the
    /// registry, which refuses a person a wipe of whom runs), the fragments
    /// on their list and their agents' lists, and their computer.
    async fn plan(&self, run: &mut Run) -> Step {
        let person = run.job.person.clone();
        let me = match ask_registry(&self.env, &calls::View { identity: None, by: By::Identity(person.clone()) }).await {
            Ok(me) => me,
            Err(e) if e.code.status() < 500 => return Err(fail(ExportFailure::Unavailable, format!("its person cannot be read: {}", e.message))),
            Err(e) => return Err(e.into()),
        };
        let mut fragments: Vec<Item> = vec![];
        let mut shared: Vec<core::Shared> = vec![];
        let mut lists: Vec<(String, FragmentList)> = vec![(person.clone(), self.own_list()?)];
        for agent in &me.agents {
            let req = Request::new("https://principal.internal/list", Method::Get)?;
            let list: FragmentList = self.env.durable_object("PRINCIPAL")?.get_by_name(agent)?.fetch_with_request(req).await?.json().await?;
            lists.push((agent.clone(), list));
        }
        for (whose, list) in &lists {
            for f in &list.fragments {
                if f.role == Role::Owner && !fragments.iter().any(|x| x.name == f.name) {
                    fragments.push(Item { name: f.name.clone(), kind: f.kind, title: f.title.clone(), owner: whose.clone() });
                }
            }
        }
        for (whose, list) in &lists {
            for f in list.fragments.iter().filter(|f| f.role != Role::Owner) {
                if !fragments.iter().any(|x| x.name == f.name) && !shared.iter().any(|s| s.name == f.name && s.member == *whose) {
                    shared.push(core::Shared { name: f.name.clone(), kind: f.kind, title: f.title.clone(), owner: None, role: f.role, member: whose.clone() });
                }
            }
        }
        if fragments.len() > limits::FRAGMENTS_MAX {
            return Err(fail(ExportFailure::TooMany, format!("{} fragments are theirs; an export holds at most {}", fragments.len(), limits::FRAGMENTS_MAX)));
        }
        if shared.len() > limits::SHARED_MAX {
            return Err(fail(ExportFailure::TooMany, format!("{} fragments are shared with them; an export names at most {}", shared.len(), limits::SHARED_MAX)));
        }
        let computer = fragment_core::computer::default_computer_of(&person);
        let computers = match crate::computer::ask(&self.env, &computer, "computer/view", &json!({})).await {
            Ok(v) if v["owner"] == person.as_str() => vec![computer],
            Ok(_) => vec![],
            Err(e) if e.code == ErrorCode::NotFound => vec![],
            Err(e) => return Err(e.into()),
        };
        run.job.total = 1 + fragments.len() as u32 + computers.len() as u32;
        run.job.at = if shared.is_empty() { At::Readme } else { At::Owners { next: 0 } };
        run.job.plan = Some(Plan { email: me.email, agents: me.agents, fragments, shared, computers });
        Ok(())
    }

    /// This list's own rows (the person's), as `GET /api/fragments` lists them.
    fn own_list(&self) -> CellResult<FragmentList> {
        let rows: Vec<Value> = self.rows_of("SELECT fragment, role, face FROM memberships WHERE role IS NOT NULL ORDER BY fragment", vec![])?;
        let fragments = rows
            .into_iter()
            .filter_map(|r| {
                let face: Value = r["face"].as_str().and_then(|f| serde_json::from_str(f).ok()).unwrap_or(Value::Null);
                Some(fragment_proto::ListedFragment {
                    name: r["fragment"].as_str()?.to_string(),
                    role: Role::parse(r["role"].as_str()?)?,
                    kind: serde_json::from_value(face["kind"].clone()).unwrap_or_default(),
                    title: face["title"].as_str().map(str::to_string),
                    picture: face["picture"].as_str().map(str::to_string),
                    agents: vec![],
                    preview: None,
                    preview_at: None,
                    unread: 0,
                    sharing: None,
                    archived: false,
                    owned: false,
                })
            })
            .collect();
        Ok(FragmentList { fragments })
    }

    /// Each shared fragment's owner, from its status as the person reads it.
    async fn owners(&self, run: &mut Run, next: usize, deadline: i64) -> Step {
        let person = run.job.person.clone();
        let count = run.job.plan().shared.len();
        let mut i = next;
        while i < count && js::now_ms() <= deadline {
            let name = run.job.plan().shared[i].name.clone();
            let owner = match self.read_json(&person, &name, "status", "").await? {
                Ok(s) => s["owner"].as_str().map(str::to_string),
                Err(_) => None,
            };
            run.job.plan.as_mut().expect("planned").shared[i].owner = owner;
            i += 1;
        }
        run.job.at = if i >= count { At::Readme } else { At::Owners { next: i } };
        Ok(())
    }

    fn manifest(&self, job: &Job, id: &str) -> core::Manifest {
        let cfg = Config::from_env(&self.env);
        let plan = job.plan();
        core::Manifest {
            format: core::FORMAT.into(),
            version: core::VERSION,
            deployment: core::Deployment { id: cfg.deploy_id.clone(), platform: cfg.platform() },
            made_at: job.made_at,
            made: core::rfc3339(job.made_at),
            export: id.to_string(),
            person: core::Person { npub: job.person.clone(), email: plan.email.clone() },
            agents: plan.agents.clone(),
            fragments: plan.fragments.iter().map(|f| core::Listed { name: f.name.clone(), kind: f.kind, title: f.title.clone(), owner: f.owner.clone(), path: core::within(&core::fragment_dir(&f.name), core::ROOT) }).collect(),
            computers: plan.computers.iter().map(|c| core::ComputerListed { id: c.clone(), path: core::within(&core::computer_dir(c), core::ROOT) }).collect(),
            shared_with_me: plan.shared.clone(),
        }
    }

    // ---- a fragment ----

    async fn fragment_step(&self, run: &mut Run, i: usize, step: FragStep, deadline: i64) -> Step {
        let item = run.job.plan().fragments[i].clone();
        let person = run.job.person.clone();
        let name = item.name.clone();
        match step {
            FragStep::Facts => {
                run.job.frag = Some(self.facts(&person, &item).await?);
                let gone = run.job.frag().meta["npub"].is_null();
                run.job.at = At::Fragment { i, step: if gone { FragStep::Meta } else { FragStep::Files { after: None } } };
            }
            FragStep::Files { after } => {
                if run.files.as_ref().is_none_or(|(at, _)| *at != i) {
                    let mut listing = match self.read_json(&person, &name, "files", "").await? {
                        Ok(l) => l,
                        Err(e) => {
                            skip(run, "files", &e);
                            run.job.at = At::Fragment { i, step: FragStep::Schema };
                            return Ok(());
                        }
                    };
                    let files = match listing["files"].take() {
                        Value::Array(files) => files,
                        _ => return Err(Stop::Retry(format!("{name}'s files listed no files"))),
                    };
                    let own: Vec<Value> = files.into_iter().filter(|f| f["release"] != true).collect();
                    if own.len() > limits::FILES_MAX {
                        return Err(fail(ExportFailure::TooMany, format!("{name} holds {} files; an export takes at most {} a fragment", own.len(), limits::FILES_MAX)));
                    }
                    run.job.frag().meta["files"]["ref"] = json!(listing["ref"].as_str());
                    run.files = Some((i, std::rc::Rc::new(own)));
                }
                let files = run.files.as_ref().map(|(_, f)| std::rc::Rc::clone(f)).unwrap_or_default();
                let start = after;
                for f in files.iter().filter(|f| start.as_deref().is_none_or(|a| f["path"].as_str().unwrap_or("") > a)) {
                    let path = f["path"].as_str().unwrap_or("").to_string();
                    let after = Some(path.clone());
                    if !fragment_proto::valid_repo_path(&path) {
                        let why = CellError::invalid(format!("{path:?} is no path a file is written at"));
                        skip(run, &format!("files/{path}"), &why);
                        continue;
                    }
                    let size = f["size"].as_u64().unwrap_or(0);
                    if f["blob"] == true {
                        let sha = f["sha256"].as_str().unwrap_or("").to_string();
                        if !fragment_core::blob::valid_sha(&sha) {
                            return Err(Stop::Retry(format!("{name}'s listing names {path} a blob without its hash")));
                        }
                        count_file(run, size);
                        self.stored(run, &core::file_path(&name, &path), Source::Blob { fragment: name.clone(), sha }, size, true).await?;
                        run.job.at = At::Fragment { i, step: FragStep::Files { after } };
                        return Ok(());
                    }
                    let q = format!("?path={}", url::form_urlencoded::byte_serialize(path.as_bytes()).collect::<String>());
                    let mut resp = self.as_person(&person, &name, Method::Get, "file", &q, None).await?;
                    match resp.status_code() {
                        200 => {}
                        404 => continue,
                        s if s >= 500 || s == 429 => return Err(Stop::Retry(format!("{name}'s {path} answered {s}"))),
                        s => {
                            skip(run, &format!("files/{path}"), &CellError::new(ErrorCode::Forbidden, format!("answered {s}")));
                            continue;
                        }
                    }
                    let bytes = resp.bytes().await?;
                    if bytes.len() >= fragment_core::blob::BLOB_MIN_BYTES {
                        // main moved under the listing: list it again
                        run.files = None;
                        return Err(Stop::Retry(format!("{name}'s {path} became a blob as it was read")));
                    }
                    count_file(run, bytes.len() as u64);
                    self.entry(run, &core::file_path(&name, &path), &bytes, true).await?;
                    if js::now_ms() > deadline {
                        run.job.at = At::Fragment { i, step: FragStep::Files { after } };
                        return Ok(());
                    }
                }
                run.job.at = At::Fragment { i, step: FragStep::Schema };
            }
            FragStep::Schema => {
                if let Some(schema) = run.job.frag().schema.clone().filter(|s| !s.is_empty()) {
                    self.entry(run, &core::schema_path(&name), schema.as_bytes(), true).await?;
                }
                if run.job.frag().tables.is_some() && run.job.frag().meta["database"].is_null() {
                    run.job.frag().meta["database"] = json!([]);
                }
                let next = if run.job.frag().tables.is_some() { FragStep::Tables { table: 0, offset: 0 } } else { FragStep::Channels { channel: 0, after: 0 } };
                run.job.at = At::Fragment { i, step: next };
            }
            FragStep::Tables { table, offset } => {
                let tables = run.job.frag().tables.clone().unwrap_or_default();
                let Some(t) = tables.get(table).cloned() else {
                    run.job.at = At::Fragment { i, step: FragStep::Channels { channel: 0, after: 0 } };
                    return Ok(());
                };
                if run.job.open.is_none() {
                    self.open(run, core::table_path(&name, &t), true);
                    run.job.frag().counted = 0;
                }
                #[derive(Deserialize)]
                struct Page {
                    rows: Vec<Box<RawValue>>,
                    next: Option<u64>,
                }
                let q = format!("?table={}&offset={offset}", url::form_urlencoded::byte_serialize(t.as_bytes()).collect::<String>());
                let page = match self.read_bytes(&person, &name, "database", &q).await? {
                    Ok(bytes) => serde_json::from_slice::<Page>(&bytes).map_err(|e| Stop::Retry(format!("{name}'s {t} rows: {e}")))?,
                    Err(e) => {
                        skip(run, &format!("database/{t}"), &e);
                        Page { rows: vec![], next: None }
                    }
                };
                let mut text = Vec::with_capacity(page.rows.iter().map(|r| r.get().len() + 1).sum());
                for r in &page.rows {
                    text.extend_from_slice(r.get().as_bytes());
                    text.push(b'\n');
                }
                run.job.frag().counted += page.rows.len() as u64;
                self.more(run, &text).await?;
                match page.next {
                    Some(n) if n > offset => run.job.at = At::Fragment { i, step: FragStep::Tables { table, offset: n } },
                    _ => {
                        self.close(run).await?;
                        let counted = run.job.frag().counted;
                        note(run, "database", core::Counted { name: t.clone(), count: counted, file: core::within(&core::table_path(&name, &t), &core::fragment_dir(&name)) });
                        run.job.at = At::Fragment { i, step: FragStep::Tables { table: table + 1, offset: 0 } };
                    }
                }
            }
            FragStep::Channels { channel, after } => {
                let channels = run.job.frag().channels.clone();
                let Some((c, newest)) = channels.get(channel).cloned() else {
                    let chat = item.kind == FragmentKind::Chat && channels.iter().any(|(c, _)| c == "chat");
                    run.job.at = At::Fragment { i, step: if chat { FragStep::Transcript { chat: 0, work: 0 } } else { FragStep::Blobs { after: String::new() } } };
                    return Ok(());
                };
                if run.job.open.is_none() {
                    self.open(run, core::channel_path(&name, &c), true);
                    run.job.frag().counted = 0;
                }
                let page = if after < newest { self.page(run, &person, &name, &c, after).await? } else { vec![] };
                let mut text = Vec::new();
                let mut last = after;
                for r in page.iter().filter(|r| r.seq <= newest) {
                    text.extend_from_slice(&serde_json::to_vec(r).map_err(|e| Stop::Retry(e.to_string()))?);
                    text.push(b'\n');
                    last = r.seq;
                    self.attached(run, &name, r.body.get())?;
                    run.job.frag().counted += 1;
                }
                self.more(run, &text).await?;
                if page.len() < limits::CHANNEL_PAGE || last >= newest {
                    self.close(run).await?;
                    let counted = run.job.frag().counted;
                    note(run, "channels", core::Counted { name: c.clone(), count: counted, file: core::within(&core::channel_path(&name, &c), &core::fragment_dir(&name)) });
                    run.job.at = At::Fragment { i, step: FragStep::Channels { channel: channel + 1, after: 0 } };
                } else {
                    run.job.at = At::Fragment { i, step: FragStep::Channels { channel, after: last } };
                }
            }
            FragStep::Transcript { chat, work } => {
                let newest = |c: &str, f: &Frag| f.channels.iter().find(|(n, _)| n == c).map_or(0, |(_, s)| *s);
                let (chat_max, work_max) = (newest("chat", run.job.frag()), newest("work", run.job.frag()));
                if run.job.open.is_none() {
                    self.open(run, core::transcript_path(&name), true);
                    let head = core::transcript_head(&name, item.title.as_deref(), run.job.made_at);
                    self.more(run, head.as_bytes()).await?;
                }
                let chats = if chat < chat_max { self.page(run, &person, &name, "chat", chat).await? } else { vec![] };
                let works = if work < work_max { self.page(run, &person, &name, "work", work).await? } else { vec![] };
                let chat_end = chats.len() < limits::CHANNEL_PAGE || chats.last().is_some_and(|r| r.seq >= chat_max);
                let work_end = works.len() < limits::CHANNEL_PAGE || works.last().is_some_and(|r| r.seq >= work_max);
                let parse = |rs: &[Record], max: i64| -> Vec<(i64, i64, String, Value)> { rs.iter().filter(|r| r.seq <= max).map(|r| (r.seq, r.at, r.principal.clone(), serde_json::from_str(r.body.get()).unwrap_or(Value::Null))).collect() };
                let (a, b) = (parse(&chats, chat_max), parse(&works, work_max));
                let names = core::Names(run.job.frag().names.clone());
                let (mut ai, mut bi, mut out) = (0, 0, String::new());
                let (mut chat_at, mut work_at) = (chat, work);
                // A record goes in only once nothing unread of the other
                // channel can come before it: past the end of one page whose
                // channel holds more, the next step reads on from both
                // cursors. Bounded: each round takes one record of the pages.
                loop {
                    let x = a.get(ai).map(|r| core::Said { channel: "chat", seq: r.0, at: r.1, principal: &r.2, body: &r.3 });
                    let y = b.get(bi).map(|r| core::Said { channel: "work", seq: r.0, at: r.1, principal: &r.2, body: &r.3 });
                    let said = match (x, y) {
                        (Some(x), Some(y)) if x.order() <= y.order() => x,
                        (Some(_), Some(y)) => y,
                        (Some(x), None) if work_end => x,
                        (None, Some(y)) if chat_end => y,
                        _ => break,
                    };
                    out.extend(core::transcript_part(&said, &names));
                    if said.channel == "chat" {
                        chat_at = said.seq;
                        ai += 1;
                    } else {
                        work_at = said.seq;
                        bi += 1;
                    }
                }
                self.more(run, out.as_bytes()).await?;
                if chat_end && work_end && ai == a.len() && bi == b.len() {
                    self.close(run).await?;
                    run.job.frag().meta["transcript"] = json!(core::within(&core::transcript_path(&name), &core::fragment_dir(&name)));
                    run.job.at = At::Fragment { i, step: FragStep::Blobs { after: String::new() } };
                } else {
                    run.job.at = At::Fragment { i, step: FragStep::Transcript { chat: chat_at, work: work_at } };
                }
            }
            FragStep::Blobs { after } => {
                let next = self.rows_of::<Value>("SELECT sha FROM export_blobs WHERE export = ? AND fragment = ? AND sha > ? ORDER BY sha LIMIT 1", vec![run.id.as_str().into(), name.as_str().into(), after.as_str().into()])?;
                let Some(sha) = next.first().and_then(|r| r["sha"].as_str()).map(str::to_string) else {
                    run.job.at = At::Fragment { i, step: FragStep::Meta };
                    return Ok(());
                };
                let resp = self.as_person(&person, &name, Method::Head, &format!("blobs/{sha}"), "", None).await?;
                let size = resp.headers().get("content-length")?.and_then(|l| l.parse::<u64>().ok());
                match (resp.status_code(), size) {
                    (200, Some(size)) => {
                        note(run, "blobs", core::BlobCounted { sha256: sha.clone(), size });
                        self.stored(run, &core::blob_path(&name, &sha), Source::Blob { fragment: name.clone(), sha: sha.clone() }, size, true).await?;
                    }
                    (s, _) if s >= 500 || s == 429 => return Err(Stop::Retry(format!("{name}'s blob {sha} answered {s}"))),
                    (s, _) => skip(run, &format!("blobs/{sha}"), &CellError::new(ErrorCode::NotFound, format!("a record attaches it, and it answered {s} (gone since)"))),
                }
                run.job.at = At::Fragment { i, step: FragStep::Blobs { after: sha } };
            }
            FragStep::Meta => {
                let meta = run.job.frag().meta.clone();
                self.entry(run, &core::meta_path(&name), &core::json_file(&meta), true).await?;
                run.job.done += 1;
                run.job.frag = None;
                run.files = None;
                run.job.at = run.job.after_fragment(i);
            }
        }
        Ok(())
    }

    /// What a fragment is, as its owner reads it (status, members, invites,
    /// secrets' names, channels, its app's schema), and its people's names
    /// for its transcript. A fragment gone since the plan is named so.
    async fn facts(&self, person: &str, item: &Item) -> Step<Frag> {
        let name = item.name.as_str();
        let mut meta = core::FragmentMeta {
            name: name.to_string(),
            kind: item.kind,
            title: item.title.clone(),
            template: None,
            npub: None,
            owner: Some(item.owner.clone()),
            visibility: None,
            share_link: false,
            inbox_token: false,
            pins: None,
            code: None,
            code_error: None,
            members: vec![],
            invites: vec![],
            secrets: vec![],
            files: core::FilesCounted::default(),
            database: None,
            channels: vec![],
            blobs: vec![],
            transcript: None,
            skipped: vec![],
        };
        let mut frag = Frag { meta: Value::Null, names: BTreeMap::new(), channels: vec![], tables: None, schema: None, bytes: 0, counted: 0, blobs: 0 };
        let status = match self.read_json(person, name, "status", "").await? {
            Ok(s) => s,
            Err(e) => {
                meta.skipped.push(core::Skipped { part: "the fragment".into(), why: format!("{}: it is gone, or no longer theirs, since the export began", e.message) });
                frag.meta = json!(meta);
                return Ok(frag);
            }
        };
        let text = |v: &Value| v.as_str().map(str::to_string);
        let visibility = status["visibility"].as_str().and_then(fragment_proto::Visibility::parse);
        meta.npub = text(&status["npub"]);
        meta.owner = text(&status["owner"]);
        meta.visibility = visibility;
        meta.share_link = status["viewToken"].is_string() && visibility != Some(fragment_proto::Visibility::Members);
        meta.inbox_token = status["inboxToken"].is_string();
        meta.pins = Some(fragment_proto::Pins { main: text(&status["pins"]["main"]), live: text(&status["pins"]["live"]) });
        meta.code = text(&status["code"]["id"]);
        meta.code_error = text(&status["code"]["error"]);
        meta.template = meta.code.as_deref().and_then(|id| id.strip_prefix("blessed:")).map(|t| t.split('@').next().unwrap_or(t).to_string());
        if meta.npub.is_none() {
            return Err(Stop::Retry(format!("{name}'s status names no npub")));
        }
        let members: Vec<Value> = match self.read_json(person, name, "members", "").await? {
            Ok(mut m) => match m["members"].take() {
                Value::Array(list) => list,
                _ => vec![],
            },
            Err(e) => {
                skip_meta(&mut meta, "members", &e);
                vec![]
            }
        };
        // who they are, as its owner's share sheet and access view show them
        // (share.rs `profiles`): a person by their email, with their agents,
        // who reach what the person does (a share is a person's and their
        // agents'); an agent by its name (its label) and its title, and its
        // owner's email. A fragment its person reads only through an agent
        // of theirs names no one.
        let owned = meta.owner.as_deref() == Some(person);
        let ids: Vec<String> = members.iter().flat_map(|m| [m["principal"].as_str(), m["owner"].as_str()]).flatten().map(str::to_string).collect();
        let named = if owned { crate::share::profiles(&self.env, ids).await } else { BTreeMap::new() };
        let email = |id: Option<&str>| id.and_then(|id| named.get(id)).and_then(|p| p.email.clone());
        for mut m in members {
            let principal = m["principal"].as_str().unwrap_or("").to_string();
            let p = named.get(&principal);
            let mail = email(Some(&principal));
            let (shown, title) = (p.and_then(|p| p.name.clone()), p.and_then(|p| p.title.clone()));
            if let Some(n) = mail.clone().or(title.clone()).or(shown.clone()) {
                frag.names.insert(principal.clone(), n);
            }
            m["email"] = json!(mail);
            m["name"] = json!(shown);
            if m["kind"] == "agent" {
                m["title"] = json!(title);
                m["ownerEmail"] = json!(email(m["owner"].as_str()));
            } else {
                let agents: Vec<Value> = p.map(|p| p.agents.iter().map(|a| json!({ "principal": a.id, "name": a.name, "title": a.title })).collect()).unwrap_or_default();
                m["agents"] = json!(agents);
            }
            meta.members.push(m);
        }
        match self.read_json(person, name, "invites", "").await? {
            Ok(mut l) => meta.invites = if let Value::Array(list) = l["invites"].take() { list } else { vec![] },
            Err(e) => skip_meta(&mut meta, "invites", &e),
        }
        match self.read_json(person, name, "secrets", "").await? {
            Ok(n) => meta.secrets = n["secrets"].as_array().into_iter().flatten().filter_map(|s| s["name"].as_str().map(str::to_string)).collect(),
            Err(e) => skip_meta(&mut meta, "secrets", &e),
        }
        match self.read_json(person, name, "channels", "").await? {
            Ok(c) => {
                let listed = c["channels"].as_array().into_iter().flatten();
                frag.channels = listed.filter_map(|c| Some((c["name"].as_str()?.to_string(), c["seq"].as_i64().unwrap_or(0)))).filter(|(n, _)| fragment_proto::valid_channel_name(n)).collect();
            }
            Err(e) => skip_meta(&mut meta, "channels", &e),
        }
        match self.read_json(person, name, "database", "").await? {
            Ok(s) => {
                let tables: Vec<String> = s["tables"].as_array().into_iter().flatten().filter_map(|t| t.as_str().map(str::to_string)).collect();
                if tables.len() > limits::TABLES_MAX {
                    return Err(fail(ExportFailure::TooMany, format!("{name}'s app has {} tables; an export takes at most {}", tables.len(), limits::TABLES_MAX)));
                }
                let mut sql = String::new();
                for stmt in s["schema"].as_array().into_iter().flatten().filter_map(|e| e["sql"].as_str()) {
                    sql.push_str(stmt.trim_end_matches(';'));
                    sql.push_str(";\n");
                }
                frag.schema = Some(sql);
                frag.tables = Some(tables);
            }
            Err(e) if e.code == ErrorCode::NoCode => {}
            Err(e) => skip_meta(&mut meta, "database", &e),
        }
        frag.meta = json!(meta);
        Ok(frag)
    }

    /// A page of a channel's records after `after`, as the person reads it.
    async fn page(&self, run: &mut Run, person: &str, name: &str, channel: &str, after: i64) -> Step<Vec<Record>> {
        #[derive(Deserialize)]
        struct Page {
            records: Vec<Record>,
        }
        match self.read_bytes(person, name, &format!("channels/{channel}"), &format!("?after={after}&limit={}", limits::CHANNEL_PAGE)).await? {
            Ok(bytes) => serde_json::from_slice::<Page>(&bytes).map(|p| p.records).map_err(|e| Stop::Retry(format!("{name}'s {channel} records: {e}"))),
            Err(e) => {
                skip(run, &format!("channels/{channel}"), &e);
                Ok(vec![])
            }
        }
    }

    /// The blobs a record's body attaches (`attachments[].sha256`), kept to
    /// export after the fragment's channels.
    fn attached(&self, run: &mut Run, name: &str, body: &str) -> Step {
        #[derive(Deserialize)]
        struct Body {
            #[serde(default)]
            attachments: Vec<Value>,
        }
        let Ok(b) = serde_json::from_str::<Body>(body) else { return Ok(()) };
        for sha in b.attachments.iter().filter_map(|a| a["sha256"].as_str()).filter(|s| fragment_core::blob::valid_sha(s)) {
            let added = self.rows_of::<Value>("INSERT INTO export_blobs (export, fragment, sha) VALUES (?, ?, ?) ON CONFLICT DO NOTHING RETURNING sha", vec![run.id.as_str().into(), name.into(), sha.into()])?;
            if !added.is_empty() {
                let f = run.job.frag();
                f.blobs += 1;
                if f.blobs > limits::BLOBS_MAX {
                    return Err(fail(ExportFailure::TooMany, format!("{name}'s records attach more than {} files; an export takes at most that many a fragment", limits::BLOBS_MAX)));
                }
            }
        }
        Ok(())
    }

    // ---- a computer ----

    async fn computer_step(&self, run: &mut Run, i: usize, step: CompStep) -> Step {
        let id = run.job.plan().computers[i].clone();
        let person = run.job.person.clone();
        match step {
            CompStep::Facts => {
                let view = crate::computer::ask(&self.env, &id, "computer/view", &json!({})).await?;
                let mut skipped = vec![];
                let save = match crate::computer::ask(&self.env, &id, "computer/export", &json!({ "owner": person })).await {
                    Ok(v) if v["save"].is_object() => {
                        let s = &v["save"];
                        let records: Vec<Value> = s["records"].as_array().cloned().unwrap_or_default();
                        Some(core::SaveExported {
                            number: s["number"].as_u64().unwrap_or(0),
                            id: s["id"].as_str().unwrap_or("").to_string(),
                            at: s["at"].as_i64().unwrap_or(0),
                            generation: s["generation"].as_u64().unwrap_or(0),
                            held: s["held"] == true,
                            records: records
                                .into_iter()
                                .map(|mut r| {
                                    let dir = r["dir"].as_str().unwrap_or("/data").to_string();
                                    r["file"] = json!(core::within(&core::save_path(&id, &dir), &core::computer_dir(&id)));
                                    r
                                })
                                .collect(),
                        })
                    }
                    Ok(_) => {
                        skipped.push(core::Skipped { part: "save".into(), why: "it was never saved".into() });
                        None
                    }
                    Err(e) => return Err(e.into()),
                };
                run.job.comp = Some(Comp { id: id.clone(), view, save, skipped });
                run.job.at = At::Computer { i, step: CompStep::Records { record: 0 } };
            }
            CompStep::Records { record } => {
                let rec = run.job.comp.as_ref().and_then(|c| c.save.as_ref()).and_then(|s| s.records.get(record)).cloned();
                let Some(rec) = rec else {
                    run.job.at = At::Computer { i, step: CompStep::Meta };
                    return Ok(());
                };
                let (dir, size, rid) = (rec["dir"].as_str().unwrap_or("/data").to_string(), rec["size"].as_u64().unwrap_or(0), rec["id"].as_str().unwrap_or("").to_string());
                self.stored(run, &core::save_path(&id, &dir), Source::Save { computer: id.clone(), record: rid }, size, false).await?;
                run.job.at = At::Computer { i, step: CompStep::Records { record: record + 1 } };
            }
            CompStep::Meta => {
                let comp = run.job.comp.take().expect("a computer's facts before its meta");
                let meta = core::ComputerMeta { view: comp.view, save: comp.save, skipped: comp.skipped };
                self.entry(run, &core::computer_meta_path(&id), &core::json_file(&meta), false).await?;
                run.job.done += 1;
                run.job.at = if i + 1 < run.job.plan().computers.len() { At::Computer { i: i + 1, step: CompStep::Facts } } else { At::End };
            }
        }
        Ok(())
    }
}

/// A channel's record as the export reads it, its body as it was stored;
/// a line of its `.jsonl`, in this order.
#[derive(Serialize, Deserialize)]
struct Record {
    seq: i64,
    at: i64,
    principal: String,
    kind: String,
    body: Box<RawValue>,
}

fn count_file(run: &mut Run, size: u64) {
    let f = &mut run.job.frag().meta["files"];
    f["count"] = json!(f["count"].as_u64().unwrap_or(0) + 1);
    f["bytes"] = json!(f["bytes"].as_u64().unwrap_or(0) + size);
}

/// Onto one of the lists of the current fragment's meta.
fn note(run: &mut Run, list: &str, entry: impl Serialize) {
    let Value::Array(entries) = &mut run.job.frag().meta[list] else {
        panic!("a fragment's meta holds {list} as a list");
    };
    entries.push(json!(entry));
}

/// A part of the current fragment its person may not read, or that is
/// not there, named in its meta.
fn skip(run: &mut Run, part: &str, why: &CellError) {
    note(run, "skipped", core::Skipped { part: part.to_string(), why: format!("{:?}: {}", why.code, why.message) });
}

fn skip_meta(meta: &mut core::FragmentMeta, part: &str, why: &CellError) {
    meta.skipped.push(core::Skipped { part: part.to_string(), why: format!("{:?}: {}", why.code, why.message) });
}

/// An export's view, from its row (and, while it builds, its job).
fn view(r: &Row) -> CellResult<ExportView> {
    let state = match r.state.as_str() {
        "building" => ExportState::Building,
        "ready" => ExportState::Ready,
        "failed" => ExportState::Failed,
        "expired" => ExportState::Expired,
        other => return Err(CellError::host(format!("export {} is {other:?}", r.id))),
    };
    let job: Option<Job> = r.job.as_deref().and_then(|j| serde_json::from_str(j).ok());
    let progress = match (&job, state) {
        (Some(j), _) => ExportProgress { done: j.done, total: j.total, bytes: j.written },
        (None, ExportState::Ready) => ExportProgress { done: 1, total: 1, bytes: r.size.unwrap_or(0) as u64 },
        (None, _) => ExportProgress::default(),
    };
    let error = r.error.as_deref().map(serde_json::from_str::<ExportError>).transpose().map_err(|e| CellError::host(format!("export {}'s error: {e}", r.id)))?;
    let ready = state == ExportState::Ready;
    Ok(ExportView {
        id: r.id.clone(),
        state,
        started_at: r.started_at,
        progress,
        size: if ready { r.size.map(|s| s as u64) } else { None },
        ready_at: r.ready_at,
        expires_at: r.expires_at,
        error,
        download: ready.then(|| download_path(&r.id)),
    })
}

/// Bytes as a person reads them: `3.2 MB`.
fn bytes_text(n: u64) -> String {
    match n {
        n if n >= 1_000_000_000 => format!("{:.1} GB", n as f64 / 1e9),
        n if n >= 1_000_000 => format!("{:.1} MB", n as f64 / 1e6),
        n if n >= 1_000 => format!("{:.1} KB", n as f64 / 1e3),
        n => format!("{n} bytes"),
    }
}
