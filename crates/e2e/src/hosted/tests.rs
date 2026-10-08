//! The hosted lane's own tests, with no preview: its arguments, its plan
//! (each section's needs against what a preview offers), and its client
//! against a server of the tests' own that records what it is sent.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use super::*;
use crate::api::{https_cookies, Reply, Run};

const SECRET: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn strings(args: &[&str]) -> Vec<String> {
    args.iter().map(|a| a.to_string()).collect()
}

fn hosted(args: &[&str]) -> Hosted {
    parse(&strings(args)).unwrap().hosted.expect("a hosted run")
}

// ---- arguments

/// A local run's arguments are as they were; `--rehearse` keeps the
/// hosted lane's rules on the local node.
#[test]
fn a_local_run_reads_as_before() {
    assert_eq!(parse(&[]).unwrap(), Args { only: None, except: vec![], hosted: None, rehearse: None, shard: None });
    let only = parse(&strings(&["--only", "ledger,ai"])).unwrap();
    assert_eq!((only.only, only.hosted), (Some(strings(&["ledger", "ai"])), None));
    assert_eq!(parse(&strings(&["--except", "hermes"])).unwrap().except, strings(&["hermes"]));
    assert_eq!(parse(&strings(&["--rehearse"])).unwrap().rehearse, Some(MAX_PAID_CALLS_DEFAULT));
    assert_eq!(parse(&strings(&["--rehearse", "--max-paid-calls", "3", "--only", "computers"])).unwrap().rehearse, Some(3));
    for bad in [&["--only", "a", "--except", "b"][..], &["--only"], &["--max-paid-calls", "3"], &["--dry-run"], &["--zone", "finite.place"], &["--nope"], &["--only", ""]] {
        assert!(parse(&strings(bad)).is_err(), "{bad:?}");
    }
}

/// A local run takes one shard of the table's split, as CI runs it; a
/// shard is never beside `--only`, `--except`, a rehearsal, a hosted run,
/// or a split the table does not have.
#[test]
fn a_local_run_takes_one_shard() {
    let n = crate::lanes::SHARDS.len();
    let args = parse(&strings(&["--shard", &format!("2/{n}")])).unwrap();
    assert_eq!((args.shard, args.only, args.except, args.hosted, args.rehearse), (Some(2), None, vec![], None, None));
    assert_eq!(parse(&strings(&["--shard", &format!("{n}/{n}")])).unwrap().shard, Some(n as u32));
    let split = format!("1/{n}");
    for bad in [
        strings(&["--shard", &split, "--only", "auth"]),
        strings(&["--except", "auth", "--shard", &split]),
        strings(&["--shard", &split, "--shard", &split]),
        strings(&["--shard", &split, "--rehearse"]),
        strings(&["--shard", &format!("1/{}", n + 1)]),
        strings(&["--shard", &format!("{}/{n}", n + 1)]),
        strings(&["--shard", "0/4"]),
        strings(&["--shard"]),
        strings(&["--shard", "1"]),
        strings(&["--shard", "a/b"]),
        strings(&["--summary", "s.json"]),
        strings(&["--hosted", "--zone", "finite.place", "--branch", "p5", "--dry-run", "--shard", &split]),
    ] {
        assert!(parse(&bad).is_err(), "{bad:?}");
    }
}

#[test]
fn a_hosted_run_names_its_preview_and_secret() {
    let h = hosted(&["--hosted", "--zone", "finite.place", "--branch", "p5", "--secret-file", "/s/test", "--offers", "computers,models", "--only", "computers"]);
    assert_eq!(h.preview, Preview::new("finite.place", "p5"));
    assert_eq!(h.preview.platform(), "https://p5.finite.place");
    assert_eq!((h.secret_file.as_deref(), h.computers, h.models, h.max_paid_calls, h.action), (Some(Path::new("/s/test")), true, true, MAX_PAID_CALLS_DEFAULT, Action::Run));
    let dry = hosted(&["--hosted", "--zone", "finite.place", "--branch", "p5", "--dry-run"]);
    assert_eq!((dry.action, dry.secret_file, dry.computers), (Action::DryRun, None, false), "a dry run reads no secret, so needs none");
}

/// A sweep is one run's: the last that finished here (`--sweep`), or one
/// it names by its id; `--sweep-all` is every run's, and says so.
#[test]
fn a_sweep_names_its_run_or_all() {
    let base = ["--hosted", "--zone", "finite.place", "--branch", "p5", "--secret-file", "/s/test"];
    let with = |more: &[&str]| -> Vec<String> { strings(&base).into_iter().chain(strings(more)).collect() };
    let action = |more: &[&str]| parse(&with(more)).unwrap().hosted.expect("a hosted run").action;
    assert_eq!(action(&["--sweep"]), Action::Sweep(SweepOf::LastRun));
    assert_eq!(action(&["--sweep", "c58b2a"]), Action::Sweep(SweepOf::Run("c58b2a".into())));
    assert_eq!(action(&["--sweep", "000000"]), Action::Sweep(SweepOf::Run("000000".into())));
    assert_eq!(action(&["--sweep-all"]), Action::Sweep(SweepOf::All));
    let last = parse(&with(&["--sweep", "--max-paid-calls", "0"])).unwrap().hosted.unwrap();
    assert_eq!((last.action, last.max_paid_calls), (Action::Sweep(SweepOf::LastRun), 0), "a flag after --sweep is not its run");
    let named = parse(&with(&["--sweep", "c58b2a", "--max-paid-calls", "0"])).unwrap().hosted.unwrap();
    assert_eq!((named.action, named.max_paid_calls), (Action::Sweep(SweepOf::Run("c58b2a".into())), 0));
    for bad in [
        with(&["--sweep", "C58B2A"]),
        with(&["--sweep", "c58b2"]),
        with(&["--sweep", "c58b2a0"]),
        with(&["--sweep", "e2e-c58b2a-todo"]),
        with(&["--sweep", "c58b2g"]),
        with(&["--sweep", "c58b2a", "--sweep", "c58b2b"]),
        with(&["--sweep", "--sweep-all"]),
        with(&["--sweep-all", "--sweep"]),
        with(&["--sweep-all", "--sweep-all"]),
        with(&["--sweep-all", "c58b2a"]),
        with(&["--sweep-all", "--dry-run"]),
        with(&["--sweep-all", "--only", "auth"]),
        strings(&["--hosted", "--zone", "finite.place", "--branch", "p5", "--sweep-all"]),
        strings(&["--sweep-all"]),
        strings(&["--sweep", "c58b2a"]),
    ] {
        assert!(parse(&bad).is_err(), "{bad:?}");
    }
}

#[test]
fn a_hosted_run_refuses_what_it_cannot_do() {
    let base = ["--hosted", "--zone", "finite.place", "--branch", "p5", "--secret-file", "/s/test"];
    let with = |more: &[&str]| -> Vec<String> { strings(&base).into_iter().chain(strings(more)).collect() };
    for bad in [
        with(&["--dry-run", "--sweep"]),
        with(&["--sweep", "--only", "auth"]),
        with(&["--offers", "computers,gpus"]),
        with(&["--max-paid-calls", &(MAX_PAID_CALLS_MAX + 1).to_string()]),
        with(&["--max-paid-calls", "many"]),
        with(&["--rehearse"]),
        strings(&["--hosted", "--zone", "finite.place", "--branch", "p5"]),
        strings(&["--hosted", "--zone", "finite.place", "--secret-file", "/s"]),
        strings(&["--hosted", "--branch", "p5", "--secret-file", "/s"]),
        strings(&["--hosted", "--zone", "finite.place", "--branch", "P5", "--secret-file", "/s"]),
        strings(&["--hosted", "--zone", "finite.place", "--branch", "a--b", "--secret-file", "/s"]),
        strings(&["--hosted", "--zone", "localhost", "--branch", "p5", "--secret-file", "/s"]),
    ] {
        assert!(parse(&bad).is_err(), "{bad:?}");
    }
}

// ---- the plan: each section's needs against what the preview offers

fn planned(h: &Hosted) -> BTreeMap<String, Option<String>> {
    let (plan, unknown) = plan(None, vec![], h);
    assert!(unknown.is_empty());
    let mut by_section = BTreeMap::new();
    for p in plan {
        assert!(by_section.insert(p.section.clone(), p.skip).is_none(), "{} is planned once", p.section);
    }
    by_section
}

/// A preview with computers and models runs the sections whose needs it
/// meets, and skips the rest saying why; a dry run calls nothing.
#[test]
fn the_plan_follows_what_each_section_needs() {
    let full = hosted(&["--hosted", "--zone", "finite.place", "--branch", "p5", "--secret-file", "/s/test", "--offers", "computers,models", "--dry-run"]);
    let plan = planned(&full);
    let runs = |s: &str| plan.get(s).unwrap_or_else(|| panic!("{s} is planned")).is_none();
    let why = |s: &str| plan.get(s).cloned().flatten().unwrap_or_default();
    for section in ["computers", "templates", "members", "delegation", "secrets", "auth", "cli", "deploy", "keys", "watch", "routes", "public"] {
        assert!(runs(section), "{section} runs on a preview: {}", why(section));
    }
    for (section, need) in [("ops", "fakes"), ("ai", "fakes"), ("restart", "node"), ("identities", "deployment"), ("lockdown", "node"), ("frames", "two-sites"), ("chat", "fakes")] {
        assert!(!runs(section) && why(section).contains(need), "{section} is skipped for {need}: {}", why(section));
    }
    assert!(why("hermes").contains("fakes"), "the real-Hermes lane scripts its model: {}", why("hermes"));
    // a real agent runs on a preview, by name alone (it spends the run's paid calls)
    assert!(why("agent-smoke").contains("--only agent-smoke") && why("agent-smoke").contains("--branch p5"), "{}", why("agent-smoke"));
    let (named, _) = super::plan(Some(vec!["agent-smoke".into()]), vec![], &full);
    assert!(named.len() == 1 && named[0].skip.is_none(), "by name it runs: {:?}", named[0].skip);
    // every lane asked for its section: the whole list, each once
    assert!(plan.len() >= 45, "{} sections", plan.len());

    // without computers the computers section is skipped; without a secret, everything
    let bare = hosted(&["--hosted", "--zone", "finite.place", "--branch", "p5", "--secret-file", "/s/test", "--offers", "models", "--dry-run"]);
    assert!(planned(&bare)["computers"].as_deref().is_some_and(|w| w.contains("makes none")));
    let (named, _) = super::plan(Some(vec!["agent-smoke".into()]), vec![], &bare);
    assert!(named[0].skip.as_deref().is_some_and(|w| w.contains("makes none")), "no computers, no real agent: {:?}", named[0].skip);
    let unsigned = hosted(&["--hosted", "--zone", "finite.place", "--branch", "p5", "--dry-run"]);
    assert!(planned(&unsigned).values().all(|skip| skip.as_deref().is_some_and(|w| w.contains("test secret"))));
    // no paid calls to lend: no models
    let broke = hosted(&["--hosted", "--zone", "finite.place", "--branch", "p5", "--secret-file", "/s", "--offers", "computers,models", "--max-paid-calls", "0", "--dry-run"]);
    assert!(planned(&broke)["computers"].as_deref().is_some_and(|w| w.contains("models")));
}

#[test]
fn the_plan_names_the_preview_and_each_skip() {
    let args = parse(&strings(&["--hosted", "--zone", "finite.place", "--branch", "p5", "--secret-file", "/s/test", "--offers", "computers,models", "--dry-run", "--only", "computers,ops,nonesuch"])).unwrap();
    let h = args.hosted.expect("a hosted run");
    let (plan, unknown) = plan(args.only, args.except, &h);
    assert_eq!(unknown, ["nonesuch"]);
    assert_eq!(plan.iter().map(|p| p.section.as_str()).collect::<Vec<_>>(), ["ops", "computers"], "only what --only names, in the lanes' order");
    let text = render(&h, &plan, &unknown);
    for part in ["https://p5.finite.place", "<name>--p5.finite.place", "labels e2e-<run>-…", "/s/test", "would run (1)", "would skip (1)", "computers", "needs computers, models", "no section is named nonesuch"] {
        assert!(text.contains(part), "{part} in:\n{text}");
    }
    assert!(!text.contains(SECRET));
}

// ---- the client, against a server that records what it is sent

/// A request as the recorder saw it.
#[derive(Clone, Debug)]
struct Seen {
    method: String,
    path: String,
    host: String,
    headers: Vec<(String, String)>,
    body: String,
}

impl Seen {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
}

/// A server on a free local port that answers each request by `answer`,
/// one request a connection, and keeps what it saw.
fn recorder(answer: fn(&Seen) -> (u16, String)) -> (SocketAddr, Arc<Mutex<Vec<Seen>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let seen = Arc::new(Mutex::new(vec![]));
    let log = Arc::clone(&seen);
    std::thread::spawn(move || {
        // bounded by the test's requests: the thread ends with the test process
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let Some(req) = read_request(&mut stream) else { continue };
            let (status, body) = answer(&req);
            log.lock().unwrap().push(req);
            let resp = format!("HTTP/1.1 {status} Answered\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len());
            let _ = stream.write_all(resp.as_bytes());
        }
    });
    (addr, seen)
}

fn read_request(stream: &mut TcpStream) -> Option<Seen> {
    let (mut buf, mut chunk) = (Vec::new(), [0u8; 4096]);
    let end = loop {
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 || buf.len() > 1 << 20 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break end;
        }
    };
    let head = String::from_utf8_lossy(&buf[..end]).into_owned();
    let mut lines = head.split("\r\n");
    let mut first = lines.next()?.split(' ');
    let (method, path) = (first.next()?.to_string(), first.next()?.to_string());
    let headers: Vec<(String, String)> = lines.filter_map(|l| l.split_once(':')).map(|(k, v)| (k.trim().to_string(), v.trim().to_string())).collect();
    let length: usize = headers.iter().find(|(k, _)| k.eq_ignore_ascii_case("content-length")).and_then(|(_, v)| v.parse().ok()).unwrap_or(0);
    let mut body = buf[end + 4..].to_vec();
    while body.len() < length {
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..n]);
    }
    let host = headers.iter().find(|(k, _)| k.eq_ignore_ascii_case("host")).map(|(_, v)| v.clone()).unwrap_or_default();
    Some(Seen { method, path, host, headers, body: String::from_utf8_lossy(&body).into_owned() })
}

/// The levers' sign-in as a preview answers it: the session, the person,
/// and the cap it was asked for.
fn preview_answer(req: &Seen) -> (u16, String) {
    match req.path.as_str() {
        "/api/test/signin" => {
            let asked: Value = serde_json::from_str(&req.body).unwrap_or_default();
            let who = asked["email"].as_str().unwrap_or("").split('@').next().unwrap_or("").to_string();
            (200, json!({ "session": "5".repeat(64), "identity": format!("id:{who}"), "created": true, "paidCalls": asked["paidCalls"] }).to_string())
        }
        _ => (200, json!({ "ok": true }).to_string()),
    }
}

const ZONE: &str = "preview.e2e-tests.invalid";
const FRAGMENT: &str = "e2e-todo-1--p2m4";

/// A hosted API at the recorder, as the preview `p5` of `ZONE`.
fn api_at(addr: SocketAddr, run: &Arc<Run>) -> Api {
    let preview = Preview::local_http(ZONE, "p5", addr.port());
    let hosts = [format!("p5.{ZONE}"), format!("e2e-todo-1--p2m4--p5.{ZONE}")];
    Api::hosted_at(&preview, run, &hosts, addr)
}

/// The secret goes to the platform's levers alone: never to another of its
/// routes, nor to a fragment's host, where an app's code reads the headers
/// (its own route may be named `/api/test/…`).
#[test]
fn the_secret_goes_to_the_platforms_levers_alone() {
    let (addr, seen) = recorder(preview_answer);
    let run = Run::signing_in_by_levers(SECRET.into(), 0);
    let api = api_at(addr, &run);
    api.unsigned("POST", "/api/test/people", Some(&json!({}))).unwrap();
    api.unsigned("GET", "/api/fragments", None).unwrap();
    api.page(FRAGMENT, "api/test/people", None).unwrap();
    let seen = seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 3);
    assert_eq!((seen[0].method.as_str(), seen[0].path.as_str(), seen[0].host.as_str()), ("POST", "/api/test/people", format!("p5.{ZONE}:{}", addr.port()).as_str()));
    assert_eq!(seen[0].header(fragment_core::levers::SECRET_HEADER), Some(SECRET));
    assert_eq!(seen[1].header(fragment_core::levers::SECRET_HEADER), None, "another platform route");
    assert_eq!(seen[2].host, format!("e2e-todo-1--p2m4--p5.{ZONE}:{}", addr.port()), "the fragment's own host, by its name");
    assert_eq!(seen[2].header(fragment_core::levers::SECRET_HEADER), None, "a fragment's host never gets the secret");
}

/// A hosted sign-in goes through the levers, lends from the run's budget,
/// and keeps its person; one past the budget is refused before it is sent.
#[test]
fn an_e2e_sign_in_lends_from_the_run() {
    let (addr, seen) = recorder(preview_answer);
    let run = Run::signing_in_by_levers(SECRET.into(), 5);
    let api = api_at(addr, &run);
    let (session, identity) = api.e2e_sign_in("paying@e2e.test", 3).unwrap();
    assert_eq!((session.len(), identity.as_str()), (64, "id:paying"));
    assert_eq!((run.paid_calls_left(), run.paid_calls_lent(), run.people()), (2, 3, vec!["id:paying".to_string()]));
    let refused = api.e2e_sign_in("greedy@e2e.test", 3).err().map(|e| e.to_string()).unwrap_or_default();
    assert!(refused.contains("paid calls are spent"), "{refused}");
    assert_eq!(seen.lock().unwrap().len(), 1, "a sign-in past the budget is never sent");
    // a plain sign-in is the levers' too, lent nothing
    api.sign_in("plain@e2e.test").unwrap();
    let seen = seen.lock().unwrap().clone();
    let body: Value = serde_json::from_str(&seen[1].body).unwrap();
    assert_eq!((seen[1].path.as_str(), body), ("/api/test/signin", json!({ "email": "plain@e2e.test", "paidCalls": 0 })));
    assert_eq!(seen[1].header(fragment_core::levers::SECRET_HEADER), Some(SECRET));
    assert_eq!(run.people().len(), 2);
}

/// A sign-in the preview caps otherwise than asked is an error, not a
/// person spending what the run did not lend.
#[test]
fn a_sign_in_capped_otherwise_is_refused() {
    let (addr, _) = recorder(|req| match req.path.as_str() {
        "/api/test/signin" => (200, json!({ "session": "5".repeat(64), "identity": "id:x", "created": true, "paidCalls": 99 }).to_string()),
        _ => (404, "{}".into()),
    });
    let run = Run::signing_in_by_levers(SECRET.into(), 5);
    assert!(api_at(addr, &run).e2e_sign_in("x@e2e.test", 1).is_err());
}

/// Over https a browser holds the cell's host cookies under `__Host-`
/// names, and reads them back by their plain ones.
#[test]
fn https_cookies_carry_the_host_prefix() {
    assert_eq!(https_cookies("fragment_session=a; fragview=b; fragment_site=c"), "__Host-fragment_session=a; fragview=b; __Host-fragment_site=c");
    assert_eq!(https_cookies("fragment_anon=d"), "fragment_anon=d", "an anonymous visitor's cookie is a site's own");
    let mut headers = reqwest::header::HeaderMap::new();
    headers.append("set-cookie", "__Host-fragment_site=t; Path=/; Secure".parse().unwrap());
    headers.append("set-cookie", "fragment_anon=v; Path=/".parse().unwrap());
    let reply = Reply { status: 302, body: Value::Null, text: String::new(), bytes: vec![], headers };
    assert_eq!(reply.cookies(), ["fragment_site=t", "fragment_anon=v"]);
}

/// A preview's platform and fragments are its own hosts, over https.
#[test]
fn a_preview_names_its_hosts() {
    let preview = Preview::new("finite.place", "p5");
    let run = Run::signing_in_by_levers(SECRET.into(), 0);
    let api = Api::hosted(&preview, &run);
    assert_eq!(api.base, "https://p5.finite.place");
    assert_eq!(api.site_url("todo--k3x9", "x?y=1"), "https://todo--k3x9--p5.finite.place/x?y=1");
    assert_eq!(api.site_origin("todo--k3x9"), "https://todo--k3x9--p5.finite.place");
    assert!(api.signs_in_by_levers());
    let local = Api::new(8790, "fragment.localhost", &Run::new(SECRET.into(), 0));
    assert_eq!(local.site_url("todo--k3x9", ""), "http://todo--k3x9.fragment.localhost:8790/");
    assert_eq!(local.branch("rh").site_url("todo--k3x9", ""), "http://todo--k3x9--rh.fragment.localhost:8790/", "a rehearsal's node is shaped as a branch");
}

// ---- the sweep: what it chooses, and the whole of one against a fake preview

/// A run's id is six hex digits of its clock, padded and wrapped; a label
/// names its run right after `e2e-`, so a label a lane extends still names
/// it, and one of the older layout (`e2e-<base>-<run>`) names none.
#[test]
fn a_label_names_its_run() {
    assert_eq!(sweep::run_id(5), "000005");
    assert_eq!(sweep::run_id(0xc5_8b2a), "c58b2a");
    assert_eq!(sweep::run_id((1 << 24) + 5), "000005", "the clock wraps every 16^6 s");
    assert_eq!(sweep::run_id(api::now_s()).len(), sweep::RUN_HEX);
    let run = "c58b2a";
    for base in ["todo", "agent", "ledger-guest-agent", "juniper", "e2e", "000000"] {
        let label = sweep::label(run, base);
        assert!(fragment_proto::valid_label(&label), "{label}");
        assert_eq!(sweep::run_of(&label), Some(run), "{label}");
    }
    assert_eq!(sweep::run_of(&format!("{}-chat", sweep::label(run, "agent"))), Some(run), "agent-smoke's chat extends its agent's label");
    for none in ["e2e-todo-c5065a", "e2e-agent-c5065a-chat", "e2e-c58b2a", "e2e-c58b2a-", "e2e-C58B2A-todo", "e2e-c58b2-todo", "e2e-c58b2a0-todo", "c58b2a-todo", "todo", ""] {
        assert_eq!(sweep::run_of(none), None, "{none:?}");
    }
}

const NOW_MS: i64 = 1_800_000_000_000;
const MIN_MS: i64 = 60_000;

fn never_asked() -> Option<i64> {
    panic!("a run's sweep never asks a fragment's age")
}

/// A run's sweep chooses its run's fragments by their labels, whatever
/// their age (it never asks), and keeps another run's, one whose label
/// names no run, and one not the e2e's.
#[test]
fn a_runs_sweep_chooses_its_runs_fragments_alone() {
    use sweep::{choose, Choice, Kept, Scope};
    let ours = Scope::Run("c58b2a".into());
    assert_eq!(choose(&ours, "e2e-c58b2a-todo", never_asked, NOW_MS), Choice::Delete);
    assert_eq!(choose(&ours, "e2e-c58b2a-agent-chat", never_asked, NOW_MS), Choice::Delete);
    assert_eq!(choose(&ours, "e2e-c58c01-todo", never_asked, NOW_MS), Choice::Keep(Kept::OtherRun(Some("c58c01".into()))));
    assert_eq!(choose(&ours, "e2e-todo-c58b2a", never_asked, NOW_MS), Choice::Keep(Kept::OtherRun(None)), "the older layout names no run, even this one's id");
    assert_eq!(choose(&ours, "c58b2a-todo", never_asked, NOW_MS), Choice::Keep(Kept::NotE2e));
    assert_eq!(choose(&ours, "juniper", never_asked, NOW_MS), Choice::Keep(Kept::NotE2e));
}

/// The whole sweep takes every e2e fragment, any run's or none's, once it
/// is old enough; it spares a younger one (one the clocks put in the
/// future too) and one whose age does not answer, and never asks the age
/// of one not the e2e's.
#[test]
fn the_whole_sweep_takes_all_but_the_young() {
    use sweep::{choose, Choice, Kept, Scope, SPARED_FOR_MS};
    let all = Scope::All { spared_for_ms: SPARED_FOR_MS };
    let made = |ago_ms: i64| move || Some(NOW_MS - ago_ms);
    for label in ["e2e-c58b2a-todo", "e2e-c58c01-todo", "e2e-todo-c5065a", "e2e-decoy"] {
        assert_eq!(choose(&all, label, made(2 * SPARED_FOR_MS), NOW_MS), Choice::Delete, "{label}");
        assert_eq!(choose(&all, label, made(SPARED_FOR_MS), NOW_MS), Choice::Delete, "{label}, exactly as old as it spares");
        assert_eq!(choose(&all, label, made(SPARED_FOR_MS - 1), NOW_MS), Choice::Keep(Kept::Young(SPARED_FOR_MS - 1)), "{label}");
        assert_eq!(choose(&all, label, made(5 * MIN_MS), NOW_MS), Choice::Keep(Kept::Young(5 * MIN_MS)), "{label}");
        assert_eq!(choose(&all, label, made(-MIN_MS), NOW_MS), Choice::Keep(Kept::Young(-MIN_MS)), "{label}, made in the future");
        assert_eq!(choose(&all, label, || None, NOW_MS), Choice::Keep(Kept::AgeUnknown), "{label}");
    }
    assert_eq!(choose(&all, "juniper", never_asked, NOW_MS), Choice::Keep(Kept::NotE2e));
    // sparing nothing (a rehearsal's last sweep) takes even the newest
    assert_eq!(choose(&Scope::All { spared_for_ms: 0 }, "e2e-c58c01-todo", made(0), NOW_MS), Choice::Delete);
}

/// A run's sweep puts a person's computers to sleep when it deleted one of
/// their fragments and spared none of the e2e's (a person signed in by a
/// fixed email in every run may have another run's agent on it); the whole
/// sweep, unless it spared one of theirs.
#[test]
fn a_computer_sleeps_when_its_person_is_the_sweeps_alone() {
    use sweep::{computers, Computers, Scope};
    let ours = Scope::Run("c58b2a".into());
    assert_eq!(computers(&ours, 0, 0), Computers::Untouched, "no fragment of the run's: not the run's person");
    assert_eq!(computers(&ours, 0, 3), Computers::Untouched);
    assert_eq!(computers(&ours, 2, 0), Computers::Sleep);
    assert_eq!(computers(&ours, 2, 1), Computers::LeftAwake);
    let all = Scope::All { spared_for_ms: sweep::SPARED_FOR_MS };
    assert_eq!(computers(&all, 0, 0), Computers::Sleep, "an e2e person with nothing spared is the whole sweep's");
    assert_eq!(computers(&all, 2, 0), Computers::Sleep);
    assert_eq!(computers(&all, 2, 1), Computers::LeftAwake);
}

/// A fake preview for a whole sweep: two pages of e2e people, each signed
/// in to a session that names them, with these fragments (made this long
/// ago) and an awake computer each:
/// - `a`, run c58b2a's: two of its fragments and one not the e2e's;
/// - `b`, run c58c01's, still going: one made five minutes ago, and an
///   editor's seat on one of `a`'s (not theirs to delete);
/// - `c`, leftovers: one of the older layout and one of run 0a0a0a's;
/// - `d`, a fixed email signed in by both runs: one fragment of each.
fn fake_preview(req: &Seen) -> (u16, String) {
    const H: i64 = 60 * MIN_MS;
    let who = req.header("cookie").and_then(|c| c.strip_prefix("fragment_session=s-")).unwrap_or("").to_string();
    let owned = |names: &[(&str, i64)]| names.iter().map(|(n, _)| json!({ "name": n, "role": "owner" })).collect::<Vec<_>>();
    let fragments: &[(&str, i64)] = match who.as_str() {
        "a" => &[("e2e-c58b2a-todo--pa22", 2 * H), ("e2e-c58b2a-agent-chat--pa22", 2 * H), ("notes--pa22", 9 * H)],
        "b" => &[("e2e-c58c01-todo--pb22", 5 * MIN_MS)],
        "c" => &[("e2e-todo-c5065a--pc22", 48 * H), ("e2e-0a0a0a-files--pc22", 3 * H)],
        "d" => &[("e2e-c58b2a-x--pd22", 2 * H), ("e2e-c58c01-y--pd22", 10 * MIN_MS)],
        _ => &[],
    };
    let all: &[(&str, i64)] = &[
        ("e2e-c58b2a-todo--pa22", 2 * H),
        ("e2e-c58b2a-agent-chat--pa22", 2 * H),
        ("notes--pa22", 9 * H),
        ("e2e-c58c01-todo--pb22", 5 * MIN_MS),
        ("e2e-todo-c5065a--pc22", 48 * H),
        ("e2e-0a0a0a-files--pc22", 3 * H),
        ("e2e-c58b2a-x--pd22", 2 * H),
        ("e2e-c58c01-y--pd22", 10 * MIN_MS),
    ];
    let body: Value = serde_json::from_str(&req.body).unwrap_or_default();
    match (req.method.as_str(), req.path.as_str()) {
        ("POST", "/api/test/people") => {
            let person = |w: &str| json!({ "identity": format!("id:{w}"), "email": format!("{w}@e2e.test") });
            match body["after"].as_str() {
                None => (200, json!({ "people": [person("a"), person("b")], "next": "id:b" }).to_string()),
                Some("id:b") => (200, json!({ "people": [person("c"), person("d")], "next": null }).to_string()),
                Some(_) => (400, "{}".into()),
            }
        }
        ("POST", "/api/test/signin") => {
            let w = body["email"].as_str().unwrap_or("").split('@').next().unwrap_or("").to_string();
            (200, json!({ "session": format!("s-{w}"), "identity": format!("id:{w}"), "created": false, "paidCalls": body["paidCalls"] }).to_string())
        }
        ("GET", "/api/fragments") => {
            let mut listed = owned(fragments);
            if who == "b" {
                listed.push(json!({ "name": "e2e-c58b2a-todo--pa22", "role": "editor" }));
            }
            (200, json!({ "fragments": listed }).to_string())
        }
        ("GET", path) if path.starts_with("/api/f/") && path.ends_with("/members") => {
            let name = &path["/api/f/".len()..path.len() - "/members".len()];
            match all.iter().find(|(n, _)| *n == name) {
                Some((_, ago)) => (200, json!({ "members": [{ "principal": format!("id:{who}"), "role": "owner", "addedAt": NOW_MS - ago }] }).to_string()),
                None => (404, "{}".into()),
            }
        }
        ("DELETE", path) if path.starts_with("/api/f/") => (200, json!({ "ok": true, "deleted": true }).to_string()),
        ("GET", "/api/computers") => (200, json!({ "computers": [{ "computer": format!("c{who}"), "phase": "awake" }, { "computer": format!("z{who}"), "phase": "asleep" }] }).to_string()),
        ("POST", path) if path.starts_with("/api/computers/") && path.ends_with("/sleep") => (200, json!({ "phase": "asleep" }).to_string()),
        _ => (404, "{}".into()),
    }
}

/// What a sweep asked the fake preview to do: its deletes, its sleeps,
/// and whether it asked any fragment's age.
fn asked(seen: &Arc<Mutex<Vec<Seen>>>) -> (Vec<String>, Vec<String>, bool) {
    let seen = std::mem::take(&mut *seen.lock().unwrap());
    let mut deleted: Vec<String> = seen.iter().filter(|r| r.method == "DELETE").map(|r| r.path.trim_start_matches("/api/f/").to_string()).collect();
    let mut slept: Vec<String> = seen.iter().filter(|r| r.path.ends_with("/sleep")).map(|r| r.path.split('/').nth(3).unwrap_or("").to_string()).collect();
    deleted.sort();
    slept.sort();
    (deleted, slept, seen.iter().any(|r| r.path.ends_with("/members")))
}

/// Goal: on a shared preview, a run's sweep deletes that run's fragments
/// and nothing else, and puts to sleep only its people's computers, while
/// the whole sweep takes every run's but the young; each says what it kept.
/// Method: both sweeps against the fake preview above, reading what they
/// asked it to do.
#[test]
fn a_sweep_against_a_shared_preview() {
    use sweep::{sweep_on, Kept, Scope};
    let (addr, seen) = recorder(fake_preview);
    let run = Run::signing_in_by_levers(SECRET.into(), 0);
    let api = api_at(addr, &run);

    let ours = sweep_on(&api, &Scope::Run("c58b2a".into()), NOW_MS).unwrap();
    let (deleted, slept, aged) = asked(&seen);
    assert_eq!(deleted, ["e2e-c58b2a-agent-chat--pa22", "e2e-c58b2a-todo--pa22", "e2e-c58b2a-x--pd22"], "the run's, and none of another run's (b is an editor of one: not theirs to delete)");
    assert_eq!(slept, ["ca"], "a's computer alone: d also owns another run's fragment, and b and c are not the run's");
    assert!(!aged, "a run's sweep asks no fragment's age");
    assert_eq!((ours.people, ours.deleted, ours.slept, ours.not_e2e, ours.awake), (4, 3, 1, 1, 1));
    let mut spared = ours.spared.clone();
    spared.sort();
    assert_eq!(
        spared,
        [
            ("e2e-0a0a0a-files--pc22".to_string(), Kept::OtherRun(Some("0a0a0a".into()))),
            ("e2e-c58c01-todo--pb22".to_string(), Kept::OtherRun(Some("c58c01".into()))),
            ("e2e-c58c01-y--pd22".to_string(), Kept::OtherRun(Some("c58c01".into()))),
            ("e2e-todo-c5065a--pc22".to_string(), Kept::OtherRun(None)),
        ]
    );
    let report = ours.report();
    for part in ["kept 4 e2e- fragments", "4 of other runs: 0a0a0a (1), c58c01 (2), no run id (1)", "kept 1 fragment not the e2e's", "left 1 computer awake"] {
        assert!(report.contains(part), "{part} in:\n{report}");
    }

    let all = sweep_on(&api, &Scope::All { spared_for_ms: sweep::SPARED_FOR_MS }, NOW_MS).unwrap();
    let (deleted, slept, aged) = asked(&seen);
    assert_eq!(deleted, ["e2e-0a0a0a-files--pc22", "e2e-c58b2a-agent-chat--pa22", "e2e-c58b2a-todo--pa22", "e2e-c58b2a-x--pd22", "e2e-todo-c5065a--pc22"], "every run's an hour old, and none's");
    assert_eq!(slept, ["ca", "cc"], "b's and d's own a young fragment: a run may be using their computers");
    assert!(aged);
    assert_eq!((all.deleted, all.slept, all.not_e2e, all.awake), (5, 2, 1, 2));
    assert_eq!(all.spared_for(|k| matches!(k, Kept::Young(_))), ["e2e-c58c01-todo--pb22", "e2e-c58c01-y--pd22"]);
    let report = all.report();
    for part in ["kept 2 e2e- fragments", "2 younger than 60 min (a run may be using them): e2e-c58c01-todo--pb22 (5 min old), e2e-c58c01-y--pd22 (10 min old)", "left 2 computers awake"] {
        assert!(report.contains(part), "{part} in:\n{report}");
    }
}
