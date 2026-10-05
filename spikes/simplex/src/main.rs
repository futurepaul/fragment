//! The SimpleX connector spike's tool (docs/explorations/simplex-connector.md).
//!
//!   simplex-spike api <port> <command>...   one bot API command, its answer, then 2 s of events
//!   simplex-spike pair <dir>                a lab: the SMP server, the person and the connector,
//!                                           connected; a message each way, timed
//!   simplex-spike down                      removes the lab's containers and network

mod sx;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};

use sx::{type_of, Api, ClientSpec, Lab};

pub const PREFIX: &str = "sxs";

fn docker_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("docker")
}

pub fn person() -> ClientSpec {
    ClientSpec { name: "person".into(), port: 15226, bot: false }
}

pub fn connector() -> ClientSpec {
    ClientSpec { name: "connector".into(), port: 15225, bot: true }
}

/// A contact's id in an event that carries one contact.
fn contact_id(e: &Value) -> Option<i64> {
    e["contact"]["contactId"].as_i64()
}

/// The texts of the messages a `newChatItems` event carries that were
/// received (not sent), with their contact.
pub fn received_texts(e: &Value) -> Vec<(i64, String)> {
    if type_of(e) != "newChatItems" {
        return vec![];
    }
    e["chatItems"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|ci| ci["chatItem"]["content"]["type"] == "rcvMsgContent")
        .filter_map(|ci| {
            let contact = ci["chatInfo"]["contact"]["contactId"].as_i64()?;
            let text = ci["chatItem"]["content"]["msgContent"]["text"].as_str()?;
            Some((contact, text.to_string()))
        })
        .collect()
}

/// Sends `text` to contact `id` as a bot does (`/_send @<id> json`).
pub fn send(api: &mut Api, id: i64, text: &str) -> Result<Value> {
    let body = json!([{ "msgContent": { "type": "text", "text": text } }]);
    let r = api.cmd(&format!("/_send @{id} json {body}"))?;
    if type_of(&r) != "newChatItems" {
        bail!("sending to @{id}: {}", r);
    }
    Ok(r)
}

/// Waits for a received message from anyone whose text is `text`.
pub fn wait_text(api: &mut Api, text: &str, within: Duration) -> Result<(Instant, i64)> {
    let (at, e) = api.wait(within, |e| received_texts(e).iter().any(|(_, t)| t == text))?;
    let id = received_texts(&e).into_iter().find(|(_, t)| t == text).map(|(c, _)| c).expect("the predicate found it");
    Ok((at, id))
}

/// The pair: both clients up and connected (the person used the
/// connector's address, which auto-accepts). Returns both APIs and each
/// side's contact id for the other.
pub fn pair(lab: &Lab) -> Result<(Api, Api, i64, i64)> {
    let mut conn = lab.start(&connector())?;
    let mut me = lab.start(&person())?;
    let r = conn.cmd("/ad")?;
    let link = r["connLinkContact"]["connFullLink"].as_str().ok_or_else(|| anyhow!("no address: {r}"))?.to_string();
    let r = conn.cmd("/auto_accept on")?;
    println!("connector: address made, auto-accept ({})", type_of(&r));
    let t0 = Instant::now();
    let r = me.cmd(&format!("/c {link}"))?;
    println!("person: /c → {}", type_of(&r));
    let (_, e) = conn.wait(Duration::from_secs(90), |e| type_of(e) == "contactConnected")?;
    let person_at_connector = contact_id(&e).ok_or_else(|| anyhow!("no contact: {e}"))?;
    let (_, e) = me.wait(Duration::from_secs(90), |e| type_of(e) == "contactConnected")?;
    let connector_at_person = contact_id(&e).ok_or_else(|| anyhow!("no contact: {e}"))?;
    println!(
        "connected in {:.2?} (person is @{person_at_connector} at the connector, connector is @{connector_at_person} at the person)",
        t0.elapsed()
    );
    Ok((me, conn, connector_at_person, person_at_connector))
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("api") => {
            let port: u16 = args.get(1).ok_or_else(|| anyhow!("a port"))?.parse()?;
            let mut api = Api::connect(&format!("ws://127.0.0.1:{port}"), Duration::from_secs(10))?;
            for cmd in &args[2..] {
                let r = api.cmd(cmd)?;
                println!("{}", serde_json::to_string_pretty(&r)?);
            }
            api.drain(Duration::from_secs(2))?;
            for (_, e) in &api.events {
                println!("event: {}", serde_json::to_string(e)?);
            }
            Ok(())
        }
        Some("pair") => {
            let dir = PathBuf::from(args.get(1).ok_or_else(|| anyhow!("a directory for the lab's state"))?);
            if dir.exists() {
                std::fs::remove_dir_all(&dir).context("a fresh lab")?;
            }
            let lab = Lab::up(PREFIX, &dir, &docker_dir())?;
            println!("SMP server: {}", lab.server);
            let (mut me, mut conn, conn_id, me_id) = pair(&lab)?;
            for i in 0..5 {
                let text = format!("hello {i}");
                let t0 = Instant::now();
                send(&mut me, conn_id, &text)?;
                let (at, _) = wait_text(&mut conn, &text, Duration::from_secs(30))?;
                let there = at - t0;
                let back = format!("echo {i}");
                let t1 = Instant::now();
                send(&mut conn, me_id, &back)?;
                let (at, _) = wait_text(&mut me, &back, Duration::from_secs(30))?;
                println!("person → connector {there:.1?}, connector → person {:.1?}", at - t1);
            }
            println!("leftover events: person {:?}, connector {:?}", me.event_types(), conn.event_types());
            Ok(())
        }
        Some("rollback") => {
            let dir = PathBuf::from(args.get(1).ok_or_else(|| anyhow!("a directory for the lab's state"))?);
            let mode = args.get(2).map(String::as_str).unwrap_or("both");
            let fixer = args.get(3).map(String::as_str).unwrap_or("connector");
            rollback(&dir, mode, fixer)
        }
        Some("autosync") => {
            let dir = PathBuf::from(args.get(1).ok_or_else(|| anyhow!("a directory for the lab's state"))?);
            autosync(&dir)
        }
        Some("crash") => {
            let dir = PathBuf::from(args.get(1).ok_or_else(|| anyhow!("a directory for the lab's state"))?);
            crash(&dir)
        }
        Some("coldstart") => {
            let dir = PathBuf::from(args.get(1).ok_or_else(|| anyhow!("a directory for the lab's state"))?);
            coldstart(&dir)
        }
        Some("down") => {
            let lab = Lab { prefix: PREFIX.into(), dir: PathBuf::new(), server: String::new() };
            lab.down(&["person", "connector"]);
            Ok(())
        }
        _ => bail!("usage: simplex-spike api <port> <command>... | pair <dir> | rollback <dir> [both|inbound|none] | coldstart <dir> | down"),
    }
}

/// One line for an event: its type, and for chat items what each one is.
pub fn summary(e: &Value) -> String {
    let t = type_of(e);
    match t {
        "newChatItems" | "chatItemUpdated" => {
            let items: Vec<String> = e["chatItems"]
                .as_array()
                .cloned()
                .unwrap_or_else(|| vec![e["chatItem"].clone()])
                .iter()
                .map(|ci| {
                    let c = &ci["chatItem"]["content"];
                    let detail = [&c["msgDecryptError"], &c["msgError"], &c["rcvConnEvent"], &c["sndConnEvent"], &c["rcvDirectEvent"]]
                        .iter()
                        .find(|d| !d.is_null())
                        .map(|d| format!(" {d}"))
                        .unwrap_or_default();
                    let text = ci["chatItem"]["meta"]["itemText"].as_str().unwrap_or("");
                    format!("{}{} \"{}\"", c["type"].as_str().unwrap_or("?"), detail, text)
                })
                .collect();
            format!("{t}: {}", items.join(" | "))
        }
        "contactRatchetSync" | "ratchetSync" => format!("{t}: {}", e["ratchetSyncProgress"]["ratchetSyncStatus"]),
        "chatItemsStatusesUpdated" => {
            let s: Vec<String> = e["chatItems"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|ci| ci["chatItem"]["meta"]["itemStatus"]["type"].as_str().unwrap_or("?").to_string())
                .collect();
            format!("{t}: {}", s.join(","))
        }
        "chatError" | "chatCmdError" => format!("{t}: {}", e["chatError"]),
        "contactInfo" => format!("{t}: ratchetSyncState {}", e["connectionStats_"]["ratchetSyncState"]),
        "contactRatchetSyncStarted" => format!("{t}: {}", e["connectionStats"]["ratchetSyncState"]),
        _ => t.to_string(),
    }
}

/// Every event the API has queued, as summary lines (delivery statuses
/// left out), and empties the queue.
fn take_events(api: &mut Api) -> Vec<String> {
    api.events.drain(..).map(|(_, e)| summary(&e)).filter(|s| !s.starts_with("chatItemsStatusesUpdated")).collect()
}

/// A connection's ratchet state, as `/_info @<id>` says.
fn ratchet_state(api: &mut Api, id: i64) -> Result<String> {
    let r = api.cmd(&format!("/_info @{id}"))?;
    let st = if r["connectionStats_"].is_null() { &r["connectionStats"] } else { &r["connectionStats_"] };
    Ok(format!("ratchetSyncState={} ratchetSyncSupported={} ({})", st["ratchetSyncState"], st["ratchetSyncSupported"], type_of(&r)))
}

/// The client's database files on the host (`db_chat.db`, `db_agent.db`,
/// and their `-wal`/`-shm` when present).
fn db_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut v = vec![];
    for e in std::fs::read_dir(dir)? {
        let p = e?.path();
        if p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("db_")) {
            v.push(p);
        }
    }
    v.sort();
    Ok(v)
}

fn copy_db(from: &Path, to: &Path) -> Result<u64> {
    std::fs::create_dir_all(to)?;
    for old in db_files(to)? {
        std::fs::remove_file(old)?;
    }
    let mut bytes = 0;
    for f in db_files(from)? {
        bytes += std::fs::copy(&f, to.join(f.file_name().expect("a file")))?;
    }
    Ok(bytes)
}

fn names(files: &[PathBuf]) -> Vec<String> {
    files.iter().map(|p| p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()).collect()
}

/// `n` messages each way, each received before the next is sent.
fn exchange(me: &mut Api, conn: &mut Api, conn_id: i64, me_id: i64, tag: &str, n: usize) -> Result<()> {
    for i in 0..n {
        let a = format!("{tag} person {i}");
        send(me, conn_id, &a)?;
        wait_text(conn, &a, Duration::from_secs(30))?;
        let b = format!("{tag} connector {i}");
        send(conn, me_id, &b)?;
        wait_text(me, &b, Duration::from_secs(30))?;
    }
    Ok(())
}

fn show(label: &str, lines: Vec<String>) {
    if lines.is_empty() {
        println!("   {label}: nothing");
    }
    for l in lines {
        println!("   {label}: {l}");
    }
}

/// Sends, saying so when the core refuses (a connection that needs
/// re-synchronization refuses to send).
fn try_send(api: &mut Api, id: i64, text: &str, who: &str) -> bool {
    match send(api, id, text) {
        Ok(_) => true,
        Err(e) => {
            println!("   the {who} cannot send \"{text}\": {e}");
            false
        }
    }
}

/// A new message each way: whether each arrives within 10 s.
fn both_ways(me: &mut Api, conn: &mut Api, conn_id: i64, me_id: i64, tag: &str) -> Result<(bool, bool)> {
    let a = format!("{tag} person");
    let there = try_send(me, conn_id, &a, "person") && wait_text(conn, &a, Duration::from_secs(10)).is_ok();
    let b = format!("{tag} connector");
    let back = try_send(conn, me_id, &b, "connector") && wait_text(me, &b, Duration::from_secs(10)).is_ok();
    Ok((there, back))
}

/// What a rolled-back connector does to its connection (question 2).
/// `mode`: `both` (both sides spoke after the snapshot), `inbound` (only the
/// person spoke after it: the connector received, acked and lost them), or
/// `none` (nothing was said after it: the control). `fixer`: who asks for
/// the re-synchronization when it does not heal by itself.
fn rollback(dir: &Path, mode: &str, fixer: &str) -> Result<()> {
    if dir.exists() {
        std::fs::remove_dir_all(dir)?;
    }
    let lab = Lab::up(PREFIX, dir, &docker_dir())?;
    let (mut me, mut conn, conn_id, me_id) = pair(&lab)?;
    exchange(&mut me, &mut conn, conn_id, me_id, "A", 3)?;
    conn.drain(Duration::from_secs(1))?;
    take_events(&mut me);
    take_events(&mut conn);
    println!("A: 3 messages each way; the connector stops and its databases are copied (the snapshot)");
    drop(conn);
    lab.stop("connector")?;
    let snap = dir.join("snapshot");
    let bytes = copy_db(&lab.db_dir("connector"), &snap)?;
    println!("   snapshot: {bytes} bytes, {:?}", names(&db_files(&snap)?));
    let mut conn = lab.start(&connector())?;
    match mode {
        "both" => {
            exchange(&mut me, &mut conn, conn_id, me_id, "B", 3)?;
            println!("B: 3 more each way after the snapshot");
        }
        "inbound" => {
            for i in 0..3 {
                let a = format!("B person {i}");
                send(&mut me, conn_id, &a)?;
                wait_text(&mut conn, &a, Duration::from_secs(30))?;
            }
            println!("B: the person sent 3 more after the snapshot, which the connector received");
        }
        "none" => println!("B: nothing said after the snapshot"),
        other => bail!("mode: both | inbound | none, not {other}"),
    }
    conn.drain(Duration::from_secs(2))?;
    take_events(&mut me);
    take_events(&mut conn);
    drop(conn);
    lab.stop("connector")?;
    copy_db(&snap, &lab.db_dir("connector"))?;
    println!("rollback: the connector's databases are the snapshot again, and it starts");
    let mut conn = lab.start(&connector())?;
    conn.drain(Duration::from_secs(2))?;
    show("connector at start", take_events(&mut conn));
    println!("   person sees: {}", ratchet_state(&mut me, conn_id)?);
    println!("   connector sees: {}", ratchet_state(&mut conn, me_id)?);

    try_send(&mut me, conn_id, "C person 0", "person");
    conn.drain(Duration::from_secs(5))?;
    me.drain(Duration::from_secs(1))?;
    println!("C: the person sends \"C person 0\"");
    show("connector got", take_events(&mut conn));
    show("person got", take_events(&mut me));
    try_send(&mut conn, me_id, "C connector 0", "connector");
    me.drain(Duration::from_secs(5))?;
    conn.drain(Duration::from_secs(1))?;
    println!("C: the connector sends \"C connector 0\"");
    show("person got", take_events(&mut me));
    show("connector got", take_events(&mut conn));
    println!("   person sees: {}", ratchet_state(&mut me, conn_id)?);
    println!("   connector sees: {}", ratchet_state(&mut conn, me_id)?);

    me.drain(Duration::from_secs(10))?;
    conn.drain(Duration::from_secs(1))?;
    println!("D: 10 s later, nothing asked of either");
    show("person got", take_events(&mut me));
    show("connector got", take_events(&mut conn));
    println!("   person sees: {}", ratchet_state(&mut me, conn_id)?);
    println!("   connector sees: {}", ratchet_state(&mut conn, me_id)?);
    let (there, back) = both_ways(&mut me, &mut conn, conn_id, me_id, "D")?;
    println!("   a new message each way arrives: person→connector {there}, connector→person {back}");
    show("person got", take_events(&mut me));
    show("connector got", take_events(&mut conn));
    if there && back {
        println!("it healed by itself");
        return Ok(());
    }

    let (api, id) = if fixer == "person" { (&mut me, conn_id) } else { (&mut conn, me_id) };
    let t0 = Instant::now();
    let r = api.cmd(&format!("/_sync @{id}"))?;
    println!("E: the {fixer} asks to re-synchronize (/_sync @{id}): {}", summary(&r));
    if type_of(&r) == "chatCmdError" {
        let r = api.cmd(&format!("/_sync @{id} force=on"))?;
        println!("   forced: {}", summary(&r));
    }
    let ok = |e: &Value| type_of(e) == "contactRatchetSync" && e["ratchetSyncProgress"]["ratchetSyncStatus"] == "ok";
    let person_ok = me.wait(Duration::from_secs(30), ok).map(|(at, _)| at - t0);
    let conn_ok = conn.wait(Duration::from_secs(30), ok).map(|(at, _)| at - t0);
    println!(
        "   synchronized: the person {:?}, the connector {:?} after the ask",
        person_ok.map_err(|e| e.to_string()),
        conn_ok.map_err(|e| e.to_string())
    );
    me.drain(Duration::from_secs(1))?;
    show("person got", take_events(&mut me));
    show("connector got", take_events(&mut conn));
    println!("   person sees: {}", ratchet_state(&mut me, conn_id)?);
    println!("   connector sees: {}", ratchet_state(&mut conn, me_id)?);
    let (there, back) = both_ways(&mut me, &mut conn, conn_id, me_id, "E")?;
    println!("   after the fix, a new message each way arrives: person→connector {there}, connector→person {back}");
    show("person got", take_events(&mut me));
    show("connector got", take_events(&mut conn));
    last_items(&mut me, conn_id, "the person's chat")?;
    last_items(&mut conn, me_id, "the connector's chat")?;
    Ok(())
}

/// The chat's last items as its client shows them: who said it, what, and
/// its delivery status.
fn last_items(api: &mut Api, id: i64, label: &str) -> Result<()> {
    let r = api.cmd(&format!("/_get chat @{id} count=24"))?;
    println!("{label}, its last items:");
    for ci in r["chat"]["chatItems"].as_array().into_iter().flatten() {
        let meta = &ci["meta"];
        println!(
            "   {:<14} {:<22} {}",
            meta["itemStatus"]["type"].as_str().unwrap_or("?"),
            ci["content"]["type"].as_str().unwrap_or("?"),
            meta["itemText"].as_str().unwrap_or("")
        );
    }
    Ok(())
}

/// The recovery a connector can run on its own: a rolled-back connector
/// that sees `contactRatchetSync: required` asks for the re-synchronization
/// at once. What it costs: the messages sent while out of sync, and how long
/// until both sides speak again.
fn autosync(dir: &Path) -> Result<()> {
    if dir.exists() {
        std::fs::remove_dir_all(dir)?;
    }
    let lab = Lab::up(PREFIX, dir, &docker_dir())?;
    let (mut me, mut conn, conn_id, me_id) = pair(&lab)?;
    exchange(&mut me, &mut conn, conn_id, me_id, "A", 3)?;
    drop(conn);
    lab.stop("connector")?;
    let snap = dir.join("snapshot");
    copy_db(&lab.db_dir("connector"), &snap)?;
    let mut conn = lab.start(&connector())?;
    exchange(&mut me, &mut conn, conn_id, me_id, "B", 3)?;
    drop(conn);
    lab.stop("connector")?;
    copy_db(&snap, &lab.db_dir("connector"))?;
    let mut conn = lab.start(&connector())?;
    println!("the connector rolled back past 3 messages each way, and started");
    let t0 = Instant::now();
    send(&mut me, conn_id, "C person 0")?;
    let required = |e: &Value| type_of(e) == "contactRatchetSync" && e["ratchetSyncProgress"]["ratchetSyncStatus"] == "required";
    let (at, _) = conn.wait(Duration::from_secs(30), required)?;
    println!("   {:>5} ms  the person's message fails to decrypt: the connector's connection needs re-synchronization", (at - t0).as_millis());
    let r = conn.cmd(&format!("/_sync @{me_id}"))?;
    println!("   {:>5} ms  the connector asks for it (/_sync): {}", t0.elapsed().as_millis(), summary(&r));
    let ok = |e: &Value| type_of(e) == "contactRatchetSync" && e["ratchetSyncProgress"]["ratchetSyncStatus"] == "ok";
    let (at, _) = conn.wait(Duration::from_secs(30), ok)?;
    println!("   {:>5} ms  the connector's connection is synchronized", (at - t0).as_millis());
    let (at, _) = me.wait(Duration::from_secs(30), ok)?;
    println!("   {:>5} ms  the person's is", (at - t0).as_millis());
    send(&mut me, conn_id, "D person 0")?;
    let (at, _) = wait_text(&mut conn, "D person 0", Duration::from_secs(30))?;
    println!("   {:>5} ms  the person's next message reaches the connector", (at - t0).as_millis());
    send(&mut conn, me_id, "D connector 0")?;
    let (at, _) = wait_text(&mut me, "D connector 0", Duration::from_secs(30))?;
    println!("   {:>5} ms  and the connector's reply reaches the person", (at - t0).as_millis());
    conn.drain(Duration::from_secs(1))?;
    let lost = !conn.events.iter().any(|(_, e)| received_texts(e).iter().any(|(_, t)| t == "C person 0"));
    println!("   \"C person 0\", sent while out of sync, is lost: {lost}");
    last_items(&mut me, conn_id, "the person's chat")?;
    Ok(())
}

/// A crash with no rollback: the connector is SIGKILLed mid-burst, each way,
/// and starts again on the database the kill left. If its core commits
/// before it acts on the network, nothing breaks and nothing is lost: what
/// a durable home for that database is worth.
fn crash(dir: &Path) -> Result<()> {
    if dir.exists() {
        std::fs::remove_dir_all(dir)?;
    }
    let lab = Lab::up(PREFIX, dir, &docker_dir())?;
    let (mut me, mut conn, conn_id, me_id) = pair(&lab)?;
    exchange(&mut me, &mut conn, conn_id, me_id, "A", 2)?;
    for round in 0..3 {
        // inbound: the person sends 5; the connector is killed once the first arrives
        let texts: Vec<String> = (0..5).map(|i| format!("in {round}.{i}")).collect();
        for t in &texts {
            send(&mut me, conn_id, t)?;
        }
        wait_text(&mut conn, &texts[0], Duration::from_secs(30))?;
        let seen_before = conn.events.iter().flat_map(|(_, e)| received_texts(e)).count() + 1;
        drop(conn);
        lab.kill("connector")?;
        conn = lab.start(&connector())?;
        conn.drain(Duration::from_secs(3))?;
        let after: Vec<String> = conn.events.iter().flat_map(|(_, e)| received_texts(e)).map(|(_, t)| t).collect();
        let errors: Vec<String> = take_events(&mut conn)
            .into_iter()
            .filter(|l| l.contains("Error") || l.contains("rcvIntegrityError") || l.contains("RatchetSync"))
            .map(|l| l.chars().take(160).collect())
            .collect();
        // what its database holds: each of the 5, once?
        let r = conn.cmd(&format!("/_get chat @{me_id} count=40"))?;
        let mut held: Vec<String> = r["chat"]["chatItems"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|ci| ci["meta"]["itemText"].as_str().filter(|t| t.starts_with(&format!("in {round}."))).map(str::to_string))
            .collect();
        held.sort();
        println!(
            "round {round}, inbound: killed once the first of 5 arrived ({seen_before} seen before the kill, {} after the restart); its chat holds {held:?}; errors {errors:?}",
            after.len()
        );
        // outbound: the connector sends 5 and is killed at once
        let texts: Vec<String> = (0..5).map(|i| format!("out {round}.{i}")).collect();
        for t in &texts {
            send(&mut conn, me_id, t)?;
        }
        drop(conn);
        lab.kill("connector")?;
        me.drain(Duration::from_secs(1))?;
        let before = me.events.iter().flat_map(|(_, e)| received_texts(e)).count();
        conn = lab.start(&connector())?;
        me.drain(Duration::from_secs(4))?;
        let mut got: Vec<String> = me.events.iter().flat_map(|(_, e)| received_texts(e)).map(|(_, t)| t).collect();
        got.sort();
        let dupes = got.windows(2).filter(|w| w[0] == w[1]).count();
        let errors: Vec<String> = take_events(&mut me).into_iter().filter(|l| l.contains("Error") || l.contains("rcvIntegrityError") || l.contains("RatchetSync")).collect();
        println!("round {round}, outbound: killed right after sending 5 ({before} had arrived); the person got {got:?}, {dupes} twice; errors {errors:?}");
        take_events(&mut conn);
    }
    println!("person sees: {}", ratchet_state(&mut me, conn_id)?);
    println!("connector sees: {}", ratchet_state(&mut conn, me_id)?);
    let (there, back) = both_ways(&mut me, &mut conn, conn_id, me_id, "after")?;
    println!("a new message each way arrives: person→connector {there}, connector→person {back}");
    Ok(())
}

/// How long a stopped connector takes to start and receive a message sent
/// while it was down (a stateless worker's cost per wake).
fn coldstart(dir: &Path) -> Result<()> {
    if dir.exists() {
        std::fs::remove_dir_all(dir)?;
    }
    let lab = Lab::up(PREFIX, dir, &docker_dir())?;
    let (mut me, conn, conn_id, me_id) = pair(&lab)?;
    drop(conn);
    for i in 0..5 {
        lab.stop("connector")?;
        let text = format!("while down {i}");
        send(&mut me, conn_id, &text)?;
        let t0 = Instant::now();
        let mut conn = lab.start(&connector())?;
        let api_up = t0.elapsed();
        let (at, _) = wait_text(&mut conn, &text, Duration::from_secs(60))?;
        let got = at - t0;
        let back = format!("reply {i}");
        let t1 = Instant::now();
        send(&mut conn, me_id, &back)?;
        let (at2, _) = wait_text(&mut me, &back, Duration::from_secs(30))?;
        println!("start {i}: bot API up {api_up:.2?}, the queued message received {got:.2?}, its reply delivered {:.0?} later", at2 - t1);
        if i == 4 {
            println!("connector container: {}", sx::docker_stats(&lab.container("connector")));
        }
    }
    let size: u64 = db_files(&lab.db_dir("connector"))?.iter().map(|p| std::fs::metadata(p).map(|m| m.len()).unwrap_or(0)).sum();
    println!("connector databases: {size} bytes, {:?}", names(&db_files(&lab.db_dir("connector"))?));
    Ok(())
}
