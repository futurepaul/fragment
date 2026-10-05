//! The SimpleX connector spike, end to end (docs/explorations/simplex-connector.md,
//! question 4), as an e2e section that runs only by name:
//!
//!   cargo xtask e2e --only simplex
//!
//! crates/e2e/src/lanes/mod.rs includes this file (`#[path]`); it is spike
//! code, on the spike's branch only.
//!
//! The platform half is the computers section's: a person's computer runs
//! an agent fragment (the stub image's scripted runtime, which echoes) that
//! is in a chat, and the computer is put to sleep once its guest has
//! subscribed to the chat to be woken. The SimpleX half is the spike's lab
//! (`src/sx.rs`): a local SMP server and two simplex-chat CLIs in Docker,
//! "the person" and "the connector", connected through the connector's
//! address. The connector here is a prototype of the connector Durable
//! Object's job, run from this process: it hears a message on SimpleX,
//! posts it on the chat as the person (the record wakes the computer), waits
//! for the agent's reply record, and sends it back on SimpleX. Each hop is
//! timed, asleep (the wake included) and awake.

#[path = "src/sx.rs"]
mod sx;

use std::net::TcpListener;
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use serde_json::{json, Value};

use super::computers::{agent_replies, phase, AGENT_JSON, CHAT_JSON, QUEUE_DRAIN};
use super::jobs::records;
use crate::api::Api;
use crate::{Keys, Need, Suite};

pub const SECTION: &str = "simplex";

/// A start of the stub and its bridge's first follow (the computers
/// section's bound).
const WAKE: Duration = Duration::from_secs(90);

fn free_port() -> Result<u16> {
    Ok(TcpListener::bind("127.0.0.1:0")?.local_addr()?.port())
}

/// The lab's containers go when the section ends, however it ends.
struct Down<'a>(&'a sx::Lab);

impl Drop for Down<'_> {
    fn drop(&mut self) {
        self.0.down(&["person", "connector"]);
    }
}

/// Sends `text` to contact `id` (`/_send @<id> json`), as a bot does.
fn send(api: &mut sx::Api, id: i64, text: &str) -> Result<()> {
    let body = json!([{ "msgContent": { "type": "text", "text": text } }]);
    let r = api.cmd(&format!("/_send @{id} json {body}"))?;
    anyhow::ensure!(sx::type_of(&r) == "newChatItems", "sending to @{id}: {r}");
    Ok(())
}

/// A received message in a `newChatItems` event: (contact, item id, text).
fn received(e: &Value) -> Vec<(i64, i64, String)> {
    if sx::type_of(e) != "newChatItems" {
        return vec![];
    }
    e["chatItems"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|ci| ci["chatItem"]["content"]["type"] == "rcvMsgContent")
        .filter_map(|ci| {
            Some((
                ci["chatInfo"]["contact"]["contactId"].as_i64()?,
                ci["chatItem"]["meta"]["itemId"].as_i64()?,
                ci["chatItem"]["content"]["msgContent"]["text"].as_str()?.to_string(),
            ))
        })
        .collect()
}

/// One leg's timings.
#[derive(Debug, Default, Clone, Copy)]
struct Hops {
    /// The person's client sent it → the connector's client had it.
    simplex_in: Duration,
    /// The connector posted the record (its answer back).
    post: Duration,
    /// The record → the agent's reply record seen by the connector (a wake,
    /// when asleep, and the turn).
    agent: Duration,
    /// The connector sent the reply → the person's client had it.
    simplex_out: Duration,
    total: Duration,
}

/// The connector prototype: one linked SimpleX account (its client's bot
/// API), one person, one chat. What a connector Durable Object would hold
/// in its own storage is here in fields: the contact, and how far it has
/// read the chat.
struct Connector<'a> {
    api: &'a Api,
    /// Whose account it is: it posts their messages as them.
    person: &'a Keys,
    chat: String,
    agent: String,
    client: sx::Api,
    /// The chat's last record this connector has read.
    after: i64,
}

impl Connector<'_> {
    fn new_records(&mut self) -> Vec<Value> {
        let recs = self
            .api
            .signed(self.person, "GET", &format!("/api/f/{}/channels/chat?after={}", self.chat, self.after), None)
            .ok()
            .and_then(|r| r.body["records"].as_array().cloned())
            .unwrap_or_default();
        if let Some(seq) = recs.iter().filter_map(|r| r["seq"].as_i64()).max() {
            self.after = self.after.max(seq);
        }
        recs
    }

    /// The person says `text` on SimpleX; the connector hears it, posts it on
    /// the chat, waits for the agent's reply, and sends it back on SimpleX,
    /// where the person hears it. Returns the reply's text and the hops.
    fn round(&mut self, person: &mut sx::Api, to_connector: i64, text: &str) -> Result<(String, Hops)> {
        self.new_records();
        let t0 = Instant::now();
        send(person, to_connector, text)?;
        // the connector hears it
        let (t1, e) = self.client.wait(Duration::from_secs(30), |e| received(e).iter().any(|(_, _, t)| t == text))?;
        let (contact, item, said) = received(&e).into_iter().find(|(_, _, t)| t == text).ok_or_else(|| anyhow!("heard"))?;
        // and posts it as the person: its id is the SimpleX item's, so a
        // connector that posts it again appends nothing
        let r = self.api.signed(
            self.person,
            "POST",
            &format!("/api/f/{}/channels/chat", self.chat),
            Some(&json!({ "id": format!("sx:{contact}:{item}"), "body": { "text": said } })),
        )?;
        anyhow::ensure!(r.status == 200, "posting the message: {r}");
        let t2 = Instant::now();
        let seq = r.body["seq"].as_i64().or_else(|| r.body["record"]["seq"].as_i64()).unwrap_or(self.after);
        // the agent's reply: a record by the agent naming its turn
        let reply = loop {
            if let Some(reply) = agent_replies(&self.new_records(), &self.agent).into_iter().find(|r| r["seq"].as_i64() > Some(seq)) {
                break reply;
            }
            anyhow::ensure!(t2.elapsed() < WAKE, "no reply from the agent in {WAKE:?}");
            std::thread::sleep(Duration::from_millis(50));
        };
        let t3 = Instant::now();
        let back = reply["body"]["text"].as_str().unwrap_or("").to_string();
        send(&mut self.client, contact, &back)?;
        let (t4, _) = person.wait(Duration::from_secs(30), |e| received(e).iter().any(|(_, _, t)| *t == back))?;
        Ok((
            back,
            Hops { simplex_in: t1 - t0, post: t2 - t1, agent: t3 - t2, simplex_out: t4 - t3, total: t4 - t0 },
        ))
    }
}

fn ms(d: Duration) -> u128 {
    d.as_millis()
}

fn print_hops(label: &str, h: &Hops) {
    println!(
        "      ({label}: SimpleX in {} ms, post {} ms, agent {} ms, SimpleX out {} ms; total {} ms)",
        ms(h.simplex_in),
        ms(h.post),
        ms(h.agent),
        ms(h.simplex_out),
        ms(h.total)
    );
}

/// Puts the computer to sleep, as its owner, until it is.
fn sleep_it(s: &Suite, api: &Api, owner: &Keys, id: &str) -> bool {
    std::thread::sleep(QUEUE_DRAIN);
    s.eventually(Duration::from_secs(60), || {
        api.signed(owner, "POST", &format!("/api/computers/{id}/sleep"), Some(&json!({}))).is_ok_and(|r| r.body["phase"] == "asleep")
    })
}

pub fn simplex(s: &mut Suite, api: &Api) -> Result<()> {
    if !s.section_by_name(
        SECTION,
        &[Need::Fakes, Need::LocalDocker, Need::Computers, Need::Levers],
        "the SimpleX connector spike runs a local SMP server and two simplex-chat clients in Docker",
    ) {
        return Ok(());
    }
    // the platform: a person, their computer, an agent on it, and a chat
    let owner = api.person()?;
    let r = api.signed(&owner, "POST", "/api/computers", Some(&json!({})))?;
    let id = r.body["computer"].as_str().unwrap_or("").to_string();
    s.ok("a person makes their computer", r.status == 200 && id.starts_with("computer:"), &r);
    let agent_name = s.named(api, &owner, "juniper")?;
    let agent = s.create(api, &owner, &agent_name)?;
    s.commit(&agent, &[("fragment.json", Some(AGENT_JSON))]);
    s.deploy(&agent);
    let r = api.signed(&owner, "PUT", &format!("/api/computers/{id}/agents/{agent_name}"), Some(&json!({})))?;
    let identity = r.body["agents"][0]["identity"].as_str().unwrap_or("").to_string();
    s.ok("the agent runs on it", r.status == 200 && identity.starts_with("id:"), &r);
    let chat_name = s.named(api, &owner, "chat")?;
    let chat = s.create(api, &owner, &chat_name)?;
    s.commit(&chat, &[("fragment.json", Some(CHAT_JSON))]);
    s.deploy(&chat);
    let r = api.signed(&owner, "PUT", &format!("/api/f/{chat_name}/members/{identity}"), Some(&json!({ "role": "editor" })))?;
    s.ok("the agent joins the chat", r.status == 200, &r);
    let subscribed = s.eventually(WAKE, || {
        api.signed(&owner, "GET", &format!("/api/f/{chat_name}/subscriptions"), None)
            .ok()
            .is_some_and(|r| r.body["subscriptions"].as_array().is_some_and(|l| l.iter().any(|x| x["wake"] == true && x["channel"] == "chat")))
    });
    s.ok("its computer woke, and its guest subscribed to the chat to be woken", subscribed, phase(api, &owner, &id));
    let slept = sleep_it(s, api, &owner, &id);
    s.ok("then it sleeps", slept, phase(api, &owner, &id));

    // SimpleX: the lab, and the person connected to the connector's address
    let docker_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../spikes/simplex/docker");
    let lab = sx::Lab::up(&format!("sxe{}", std::process::id()), &s.scratch.join("simplex"), &docker_dir)?;
    let _down = Down(&lab);
    let connector = sx::ClientSpec { name: "connector".into(), port: free_port()?, bot: true };
    let person = sx::ClientSpec { name: "person".into(), port: free_port()?, bot: false };
    let mut conn = lab.start(&connector)?;
    let mut me = lab.start(&person)?;
    let r = conn.cmd("/ad")?;
    let link = r["connLinkContact"]["connFullLink"].as_str().unwrap_or("").to_string();
    conn.cmd("/auto_accept on")?;
    me.cmd(&format!("/c {link}"))?;
    let connected = |e: &Value| sx::type_of(e) == "contactConnected";
    let (_, e) = me.wait(Duration::from_secs(60), connected)?;
    let to_connector = e["contact"]["contactId"].as_i64().unwrap_or(0);
    let linked = conn.wait(Duration::from_secs(60), connected).is_ok();
    s.ok("the person's SimpleX client connects to the connector's address (a local SMP server)", linked && to_connector > 0, &lab.server);
    let mut c = Connector { api, person: &owner, chat: chat_name.clone(), agent: identity.clone(), client: conn, after: 0 };

    // asleep: the message's record wakes the computer
    s.ok("the computer is asleep before the person speaks", phase(api, &owner, &id) == "asleep", phase(api, &owner, &id));
    let mut cold = vec![];
    let mut warm = vec![];
    let (back, h) = c.round(&mut me, to_connector, "hello from simplex")?;
    print_hops("asleep", &h);
    cold.push(h);
    s.ok(
        "a SimpleX message wakes the sleeping computer through its chat, and the agent's reply reaches the person on SimpleX",
        back.contains("hello from simplex"),
        &back,
    );
    let r = records(api, &owner, &chat_name, "chat");
    let posted = r.iter().find(|x| x["body"]["text"] == "hello from simplex");
    s.ok(
        "the message is a record on the chat, posted as the person",
        posted.is_some_and(|x| x["principal"].as_str().is_some_and(|p| !p.is_empty() && p != identity)),
        json!(posted),
    );
    for i in 0..5 {
        let (back, h) = c.round(&mut me, to_connector, &format!("awake {i}"))?;
        print_hops(&format!("awake {i}"), &h);
        s.ok(&format!("awake, round {i} comes back"), back.contains(&format!("awake {i}")), &back);
        warm.push(h);
    }
    for i in 0..4 {
        let slept = sleep_it(s, api, &owner, &id);
        s.ok(&format!("asleep again ({i})"), slept, phase(api, &owner, &id));
        let (back, h) = c.round(&mut me, to_connector, &format!("asleep {i}"))?;
        print_hops(&format!("asleep {i}"), &h);
        s.ok(&format!("asleep, round {i} wakes it and comes back"), back.contains(&format!("asleep {i}")), &back);
        cold.push(h);
    }
    let median = |v: &mut Vec<Duration>| {
        v.sort();
        v[v.len() / 2]
    };
    for (label, set) in [("asleep", &cold), ("awake", &warm)] {
        let pick = |f: fn(&Hops) -> Duration| median(&mut set.iter().map(f).collect());
        println!(
            "      (median {label}, {} rounds: SimpleX in {} ms, post {} ms, agent {} ms, SimpleX out {} ms; total {} ms)",
            set.len(),
            ms(pick(|h| h.simplex_in)),
            ms(pick(|h| h.post)),
            ms(pick(|h| h.agent)),
            ms(pick(|h| h.simplex_out)),
            ms(pick(|h| h.total))
        );
    }
    let slept = sleep_it(s, api, &owner, &id);
    s.ok("it sleeps at the end", slept, phase(api, &owner, &id));
    Ok(())
}
