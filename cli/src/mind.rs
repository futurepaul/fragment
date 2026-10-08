//! `fragment mind import`: the conversations `import.rs` reads, played into
//! a mind (docs/optchat.md, "Importing chats"): each in parts of its
//! `import` mutation, from where the mind says it stopped (`imported`), so
//! a rerun resumes and a conversation that grew sends what is new. Then it
//! follows the mind's compactor until every message is summarized, starting
//! a `pump` again whenever one stopped with work left (its chain of runs
//! ends at the platform's 16 hops).

use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use fragment_proto::{OpCall, OpResult};
use serde_json::{json, Value};

use crate::api::Client;
use crate::import::{self, Conversation, Estimate, Found, Source};

/// Conversations one `imported` call asks about, at most.
const ASK_MAX: usize = 200;
/// How often the wait looks at the compactor.
const POLL: Duration = Duration::from_secs(15);
/// A pump that has planned nothing this long has stopped (the template's
/// PUMP_KICK_MS); one this started is given this long to plan.
const PUMP_QUIET_MS: i64 = 3 * 60_000;
const KICK_AGAIN: Duration = Duration::from_secs(60);
/// Pumps started again in a row with nothing summarized between: past
/// this, the wait stops (a node that keeps failing is the mind's `status`'s
/// `failing`).
const RESTARTS_MAX: u32 = 5;

pub struct Options {
    pub paths: Vec<std::path::PathBuf>,
    pub from: Option<Source>,
    pub since: Option<i64>,
    pub limit: Option<usize>,
    pub mind: String,
    pub wait: bool,
}

/// The conversations to import, read and prepared.
pub fn read(o: &Options) -> Result<(Vec<Conversation>, Found)> {
    let (convs, found) = import::read(&o.paths, o.from)?;
    Ok((import::prepare(convs, o.since, o.limit), found))
}

fn mb(bytes: usize) -> String {
    format!("{:.1} MB", bytes as f64 / 1e6)
}

fn usd(micros: i64) -> String {
    format!("${:.2}", micros as f64 / 1e6)
}

/// What was read, and what compacting it costs: `fragment mind import --dry-run`.
pub fn report(convs: &[Conversation], found: &Found) -> Value {
    let e = import::estimate(convs);
    let by: Vec<Value> = import::counts(convs)
        .into_iter()
        .map(|(s, (c, u, a, b))| json!({ "source": s.as_str(), "conversations": c, "messages": u + a, "user": u, "assistant": a, "bytes": b }))
        .collect();
    json!({
        "files": found.files,
        "skippedFiles": found.skipped_files,
        "emptyFiles": found.empty,
        "sqlite": found.sqlite,
        "sources": by,
        "conversations": convs.len(),
        "estimate": e,
    })
}

pub fn print_report(convs: &[Conversation], found: &Found) {
    println!("read {} files ({} of no format it reads, {} with no words of yours: subagents' runs, metadata)", found.files, found.skipped_files, found.empty.len());
    for path in &found.sqlite {
        println!("  {path}: SQLite (a Hermes state.db?): run `hermes sessions export sessions.jsonl` and import that file");
    }
    for (s, (c, u, a, b)) in import::counts(convs) {
        println!("  {:<14} {c:>6} conversations, {:>7} messages ({u} yours, {a} replies), {}", s.as_str(), u + a, mb(b));
    }
    let e: Estimate = import::estimate(convs);
    println!("in all: {} conversations, {} messages, {}", convs.len(), e.messages, mb(e.bytes));
    if let (Some(first), Some(last)) = (convs.first(), convs.iter().map(|c| c.started).max()) {
        println!("  from {} to {}", day(first.started), day(last));
    }
    println!("the compactor's work (estimated, into an empty mind):");
    println!(
        "  {} messages over {} bytes need a model call: {} calls batched 8 a call (one a call: {})",
        e.level0_calls,
        import::NODE,
        e.level0_batches,
        e.level0_calls
    );
    println!("  {} merges need a model call ({} free): {} calls batched", e.merge_calls, e.merges_free, e.merge_batches);
    println!(
        "  about {}M tokens in (of them {}M the view as context), {}M out, on the cheap tier ({})",
        e.input_tokens.saturating_add(e.context_tokens) / 1_000_000,
        e.context_tokens / 1_000_000,
        e.output_tokens / 1_000_000,
        fragment_core::models::CHEAP_MODEL
    );
    println!(
        "  cost: {} to {} at list price (the view's context cached, or not); charged {} to {}",
        usd(e.list_cached),
        usd(e.list_uncached),
        usd(e.charge_cached),
        usd(e.charge_uncached)
    );
    println!("  time: about {:.1} hours, a call at a time (~8 s each)", e.hours);
}

/// The first `n` chats as they would go: a line a message, its first line
/// cut to 100 characters.
pub fn show(convs: &[Conversation], n: usize) {
    for c in convs.iter().take(n) {
        println!("\n{} {} {} {:?} ({} messages)", day(c.started), c.source.as_str(), c.id, c.title.as_deref().unwrap_or(""), c.messages.len());
        for m in &c.messages {
            let first: String = m.text.lines().find(|l| !l.trim().is_empty()).unwrap_or("").chars().take(100).collect();
            let who = if m.role == import::Role::User { "you" } else { "agent" };
            println!("  {who:>5} {:>6}B  {first}", m.text.len());
        }
    }
}

/// A day, `YYYY-MM-DD`, of ms since the epoch.
fn day(ms: i64) -> String {
    let (y, m, d) = fragment_core::cron::civil(ms.div_euclid(86_400_000));
    format!("{y:04}-{m:02}-{d:02}")
}

/// An operation of the mind, with a fresh id (the client retries it with
/// the same one, so it runs once).
fn op(c: &Client, mind: &str, name: &str, input: Value) -> Result<Value> {
    let id = format!("cli-{:016x}", rand::random::<u64>());
    let call = OpCall { id: id.clone(), input };
    let done: OpResult = c.post_json_by_id(&format!("/api/f/{mind}/ops/{name}"), &call).and_then(|r| c.call_as(r)).with_context(|| format!("{mind}'s {name} (operation id {id})"))?;
    Ok(done.result)
}

/// How many messages of each conversation the mind has already.
fn landed(c: &Client, mind: &str, convs: &[Conversation]) -> Result<Vec<usize>> {
    let mut out = Vec::with_capacity(convs.len());
    for chunk in convs.chunks(ASK_MAX) {
        let ask: Vec<Value> = chunk.iter().map(|c| json!({ "source": c.source.as_str(), "id": c.id })).collect();
        let r = op(c, mind, "imported", json!({ "conversations": ask }))?;
        let n = r["landed"].as_array().filter(|l| l.len() == chunk.len()).with_context(|| format!("{mind}'s imported answered no count per conversation: {r}"))?;
        out.extend(n.iter().map(|v| v.as_u64().unwrap_or(0) as usize));
    }
    Ok(out)
}

/// Sends what the mind lacks of each conversation; answers the messages sent.
pub fn send(c: &Client, o: &Options, convs: &[Conversation], quiet: bool) -> Result<(usize, usize)> {
    let have = landed(c, &o.mind, convs)?;
    let todo: Vec<(&Conversation, usize)> = convs.iter().zip(have).filter(|(c, n)| *n < c.messages.len()).collect();
    if !quiet {
        println!("{} of {} conversations are in {} already; sending {}", convs.len() - todo.len(), convs.len(), o.mind, todo.len());
    }
    let (mut sent, mut convs_sent) = (0, 0);
    for (k, (conv, from)) in todo.iter().enumerate() {
        let mut thread = String::new();
        for (start, end) in import::parts(conv, *from) {
            let r = op(c, &o.mind, "import", import::part_input(conv, start, &conv.messages[start..end]))
                .with_context(|| format!("importing {} {} (a rerun resumes where it stopped)", conv.source.as_str(), conv.id))?;
            thread = r["thread"].as_str().unwrap_or("").to_string();
            sent += end - start;
        }
        convs_sent += 1;
        if !quiet {
            let title = conv.title.clone().unwrap_or_else(|| conv.messages[0].text.lines().next().unwrap_or("").chars().take(60).collect());
            println!("[{}/{}] {} {} {:?}: {} messages → {thread}", k + 1, todo.len(), day(conv.started), conv.source.as_str(), title, conv.messages.len() - from);
        }
    }
    Ok((convs_sent, sent))
}

/// Follows the compactor until every message is summarized and nothing is
/// left to merge, starting a pump when none ran lately.
pub fn wait(c: &Client, mind: &str, quiet: bool) -> Result<Value> {
    let began = Instant::now();
    let mut restarts = 0;
    let mut kicked: Option<Instant> = None;
    let mut last_progress: Option<(i64, i64)> = None;
    let mut last_line = String::new();
    loop {
        let s = op(c, mind, "status", json!({}))?;
        let t = s["T"].as_i64().unwrap_or(0);
        let unbuilt = s["unbuilt"].as_i64().unwrap_or(0);
        let ready = s["ready"].as_bool().unwrap_or(false);
        let failing = s["failing"].as_array().map_or(0, Vec::len);
        let line = format!(
            "compacting: {} of {t} messages summarized; the view {} KB; {failing} failing ({}s)",
            t - unbuilt,
            s["view"].as_i64().unwrap_or(0) / 1000,
            began.elapsed().as_secs()
        );
        if line != last_line && !quiet {
            println!("{line}");
            last_line = line;
        }
        if unbuilt == 0 && !ready {
            return Ok(s);
        }
        // no pump at work (none planned work lately), and none this started
        // in the last minute (a run takes a moment to plan)
        let idle = s["pump"]["at"].as_i64().is_none_or(|at| s["now"].as_i64().unwrap_or(at) - at > PUMP_QUIET_MS);
        if idle && kicked.is_none_or(|k| k.elapsed() > KICK_AGAIN) {
            kicked = Some(Instant::now());
            let progress = (t - unbuilt, s["nodes"].as_i64().unwrap_or(0));
            if last_progress == Some(progress) {
                restarts += 1;
            } else {
                restarts = 0;
            }
            last_progress = Some(progress);
            if restarts >= RESTARTS_MAX {
                anyhow::bail!("{mind}'s compactor made no progress in {RESTARTS_MAX} runs: see `fragment call {mind} status` (failing) and `fragment runs {mind}`");
            }
            let r = op(c, mind, "pump", json!({}))?;
            if !quiet {
                println!("started the compactor (run {})", r["run"]);
            }
        }
        std::thread::sleep(POLL);
    }
}
