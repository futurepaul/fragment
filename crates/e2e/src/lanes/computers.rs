//! Computers (docs/computers.md): the generic Computer Durable Object
//! against the stub image (`images/stub`: the bridge with its scripted
//! runtime), under `wrangler dev` with Docker. The platform side names no
//! runtime, so these checks pass with the stub exactly as with Hermes
//! (the real-Hermes lane runs the same flows on our Hermes image).
//!
//! A person makes their computer and assigns an agent fragment to it; the
//! agent, an editor of its own fragment, joins a chat, and the platform
//! posts `joined` on the agent's `tasks` and wakes the computer (no one
//! posts it by hand). Woken, the guest subscribes to the chat as the agent
//! (a wake subscription, which only a computer's egress makes) and
//! answers a message, a tool step on `work`. A page sees a reply's draft
//! live, then the reply; the turn's asker stops a turn; only the agent's
//! owner answers its prompt; a reply carries a file. The swap lends the
//! agent every connection its owner has unless its owner narrows them
//! (decision 44). Put to sleep, a record on the chat wakes it, its `/data`
//! restored, and nothing is answered twice. A second agent on the same
//! computer answers when @mentioned, the lead otherwise. Its ports answer
//! its owner on its own origin through a one-time ticket, in a tab or in a
//! frame of the platform's page (frames.rs, `computer_ports`), and no one
//! else.
//! A routine, its agent fragment's cron, wakes it asleep, and so does its
//! agent being added to a new chat, which it then follows at once.

use std::time::Duration;

use anyhow::Result;
use serde_json::{json, Value};

use sha2::{Digest, Sha256};

use super::jobs::records;
use super::ledger::{end_of, entries};
use crate::api::{Api, Call, Socket};
use super::credentials::{placeholder, swap_checks, Swapping, SWAP_CHECKS};
use crate::{Suite, SWAP_CONNECTION, SWAP_CONNECTION_HOST};

pub(super) const CHAT_JSON: &[u8] = br#"{ "channels": { "chat": { "read": "public", "post": "viewer" }, "work": { "read": "viewer", "post": "editor" } } }"#;
pub(super) const AGENT_JSON: &[u8] = br#"{ "channels": { "tasks": { "read": "editor", "post": "editor" } } }"#;
/// A start of the stub, its restore, and its bridge's first follow: well
/// under this on any machine that built the image.
const WAKE: Duration = Duration::from_secs(90);
/// A record's wake reaches its computer through the delivery queue, which
/// batches for up to a second: a sleep asked for sooner is followed by
/// that wake, and the computer starts again (the newest push wins, so no
/// wake is lost to a sleep). The lane lets the queue drain before its
/// owner's sleeps.
pub(super) const QUEUE_DRAIN: Duration = Duration::from_secs(3);

pub(super) fn agent_replies(recs: &[Value], agent: &str) -> Vec<Value> {
    recs.iter().filter(|r| r["principal"] == agent && r["body"]["turn"].is_string() && r["body"]["text"].is_string()).cloned().collect()
}

/// A turn's id, as docs/chat-records.md defines it: what one agent does
/// about one record.
pub(super) fn turn_of(agent: &str, fragment: &str, channel: &str, seq: i64) -> String {
    hex::encode(&Sha256::digest(format!("{agent}|{fragment}/{channel}/{seq}").as_bytes())[..12])
}

/// The work records of one turn.
pub(super) fn work_of(recs: &[Value], turn: &str) -> Vec<Value> {
    recs.iter().filter(|r| r["body"]["turn"] == turn).cloned().collect()
}

/// The agent fragment's app: its routine, which its cron runs.
pub(super) fn routine_app(chat: &str) -> String {
    format!(
        "import {{ DurableObject }} from \"cloudflare:workers\";\nexport class App extends DurableObject {{\n  routine(input, call) {{\n    call.publish(\"tasks\", {{ kind: \"routine\", text: \"water the plants\", chat: {chat:?} }});\n    return {{ ok: true }};\n  }}\n}}\n"
    )
}

pub(super) const ROUTINE_JSON: &[u8] = br#"{ "operations": { "routine": { "kind": "mutation", "role": "editor" } }, "channels": { "tasks": { "read": "editor", "post": "editor" } }, "triggers": [{ "cron": "* * * * *", "run": "routine" }] }"#;

/// The `joined` records on an agent fragment's `tasks` for `fragment`
/// (docs/chat-records.md): the platform posts one when the agent is added.
pub(super) fn told(api: &Api, owner: &crate::Keys, agent: &str, fragment: &str) -> Vec<Value> {
    records(api, owner, agent, "tasks").into_iter().filter(|r| r["body"]["kind"] == "joined" && r["body"]["fragment"] == fragment).collect()
}

/// A computer's phase, as its owner reads it.
pub(super) fn phase(api: &Api, owner: &crate::Keys, id: &str) -> String {
    api.signed(owner, "GET", &format!("/api/computers/{id}"), None).ok().and_then(|r| r.body["phase"].as_str().map(str::to_string)).unwrap_or_default()
}

/// The paid calls the computers section lends its owner on the hosted lane:
/// the turns a real model answers there (a reply, a wake's, two agents'
/// three, a routine's, a new chat's), each a few model calls at most.
const HOSTED_PAID_CALLS: u64 = 40;
/// A first start on a preview pulls the deployment's image (3.8 GB for our
/// Hermes', about 29 s) and boots its runtime: far longer than the stub's.
const HOSTED_WAKE: Duration = Duration::from_secs(300);

/// A reply a real model could have made: some text, as the agent, naming
/// its turn. Never its words.
fn said_something(reply: &Value) -> bool {
    reply["body"]["text"].as_str().is_some_and(|t| !t.trim().is_empty())
}

/// What a second run of a turn cannot hide, where the fakes count it
/// (docs/explorations/pi-durable.md, "Count runs, not records"): the model
/// fake's calls, its owner's ledger's model and key rows, and the
/// computer's uses. A run again posts records that replay the first's (the
/// same ids and bodies) or are refused, so a count of records sees nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Runs {
    model_calls: usize,
    model_rows: usize,
    key_rows: usize,
    uses: u64,
}

fn runs(s: &Suite, api: &Api, owner: &crate::Keys, owner_id: &str, id: &str) -> Runs {
    let uses = api.signed(owner, "GET", &format!("/api/computers/{id}/uses"), None).ok().and_then(|r| r.body["uses"].as_array().map(|l| l.iter().filter_map(|u| u["calls"].as_u64()).sum())).unwrap_or(0);
    Runs { model_calls: s.ai.chats().len(), model_rows: entries(api, owner_id, "aig:").len(), key_rows: entries(api, owner_id, &format!("key:{id}:")).len(), uses }
}

/// A lever on computer `id` (`POST /api/test/computer`, docs/api.md).
fn lever(api: &Api, id: &str, op: &str) -> Result<crate::api::Reply> {
    api.unsigned("POST", "/api/test/computer", Some(&json!({ "computer": id, "op": op })))
}

/// The crash checks, each skipped off the stub and the fakes. The ones
/// marked P1 pass once the bridge claims each turn on `work` before it runs
/// it (docs/explorations/pi-durable.md, P1): until then a crash's restore
/// runs every turn since the save again.
const CRASH_CHECKS: [&str; 9] = [
    "asleep, it is no computer to kill, and the lever refuses what it does not have",
    "after its owner's sleep and wake, its wake says what it restored: that sleep's save, no rollback",
    "the lever answers its saves: the current save's record, the one its last start restored",
    "a think turn and a key's fetch turn run once each, before the crash",
    "the lever's kill (SIGKILL to the guest's PID 1) is a crash: it comes back at once, from its last save",
    "a crash wakes it from its last save, and nothing that started runs again (P1)",
    "its wake says what it restored: after the kill, a rollback is counted",
    "a turn cut by a crash ends once, as an error, and its model was called once (P1)",
    "and the next message is answered",
];

/// Who the crash checks act as, and on what.
struct Crashing<'a> {
    owner: &'a crate::Keys,
    owner_id: &'a str,
    id: &'a str,
    chat: &'a str,
    agent: &'a str,
    identity: &'a str,
    wake: Duration,
}

/// A crash, by the lever's kill, of a computer the stub runs, with the
/// fakes counting runs (rung 4 of docs/explorations/pi-durable.md): the
/// owner's sleep (the save), a wake, a model turn and a key turn, the kill,
/// one more message; then a turn cut by a second kill. Starts and ends
/// awake. Answers how many replies of its agent it added.
fn crash_checks(s: &mut Suite, api: &Api, c: &Crashing, say: &dyn Fn(u32, &str) -> Result<crate::api::Reply>) -> Result<usize> {
    let view = || api.signed(c.owner, "GET", &format!("/api/computers/{}", c.id), None).map(|r| r.body).unwrap_or(Value::Null);
    let replied = |seq: i64| {
        let turn = turn_of(c.agent, c.chat, "chat", seq);
        agent_replies(&records(api, c.owner, c.chat, "chat"), c.identity).into_iter().find(|r| r["body"]["turn"] == turn.as_str())
    };
    let seq_of = |r: &crate::api::Reply| r.body["record"]["seq"].as_i64().unwrap_or(0);
    // the save: its owner's sleep
    std::thread::sleep(QUEUE_DRAIN);
    let r = api.signed(c.owner, "POST", &format!("/api/computers/{}/sleep", c.id), Some(&json!({})))?;
    let (asleep, unknown, nobody) = (lever(api, c.id, "kill")?, lever(api, c.id, "explode")?, lever(api, "computer:000000000000000000000000", "saves")?);
    s.ok(
        CRASH_CHECKS[0],
        r.body["phase"] == "asleep" && asleep.status == 400 && unknown.status == 400 && nobody.status == 404,
        format!("{asleep} / {unknown} / {nobody}"),
    );
    let r = api.signed(c.owner, "POST", &format!("/api/computers/{}/wake", c.id), Some(&json!({})))?;
    let restored = r.body["restored"].clone();
    let rollbacks = r.body["rollbacks"].as_u64().unwrap_or(u64::MAX);
    s.ok(
        CRASH_CHECKS[1],
        r.body["phase"] == "awake" && restored["from"] == "backup" && restored["after"] == "sleep" && restored["rollback"] == false && restored["ageMs"].as_i64().is_some_and(|a| a >= 0),
        &r,
    );
    let saves = lever(api, c.id, "saves")?;
    let save = saves.body["record"]["id"].as_str().unwrap_or("").to_string();
    s.ok(
        CRASH_CHECKS[2],
        saves.status == 200 && !save.is_empty() && restored["save"] == save.as_str() && saves.body["restored"] == restored && saves.body["generation"] == restored["generation"],
        &saves,
    );
    // a model turn and a key turn, each run once
    let before = runs(s, api, c.owner, c.owner_id, c.id);
    let think = say(50, "think before the crash")?;
    let fetch = say(51, "fetch http://api.perplexity.ai/search with $PERPLEXITY_API_KEY")?;
    let answered = s.eventually(c.wake, || replied(seq_of(&think)).is_some() && replied(seq_of(&fetch)).is_some());
    let once = Runs { model_calls: before.model_calls + 1, model_rows: before.model_rows + 1, key_rows: before.key_rows + 1, uses: before.uses + 1 };
    // the key's meter and use land just after the provider answered
    let settled = s.eventually(Duration::from_secs(20), || runs(s, api, c.owner, c.owner_id, c.id) == once);
    let ran = runs(s, api, c.owner, c.owner_id, c.id);
    s.ok(CRASH_CHECKS[3], answered && settled, format!("{before:?} -> {ran:?}; {:?}", replied(seq_of(&fetch)).map(|r| r["body"]["text"].clone())));
    // the crash
    let generation = restored["generation"].as_u64().unwrap_or(0);
    let killed = lever(api, c.id, "kill")?;
    let back = |after: u64| {
        let v = view();
        v["phase"] == "awake" && v["restored"]["generation"].as_u64().is_some_and(|g| g > after)
    };
    let came_back = killed.status == 200 && s.eventually(c.wake, || back(generation));
    let restored = view()["restored"].clone();
    s.ok(
        CRASH_CHECKS[4],
        came_back && killed.body["killed"] == generation && restored["from"] == "backup" && restored["save"] == save.as_str() && restored["after"] == "exit",
        format!("{killed} / {restored}"),
    );
    // one more message: turns of a chat run in order, so by its end every
    // turn the restore made it read again was run again or fenced
    let after = say(52, "after the crash")?;
    let answered = s.eventually(c.wake, || replied(seq_of(&after)).is_some());
    let again = runs(s, api, c.owner, c.owner_id, c.id);
    s.ok(CRASH_CHECKS[5], answered && again == ran, format!("before the crash {ran:?}, after it {again:?}"));
    let v = view();
    s.ok(CRASH_CHECKS[6], v["restored"]["rollback"] == true && v["rollbacks"].as_u64() == rollbacks.checked_add(1), &v);
    // a turn cut by a crash: its model call is held, and the guest killed under it
    let generation = v["restored"]["generation"].as_u64().unwrap_or(0);
    let called = s.ai.chats().len();
    s.ai.delay_next(&[12_000]);
    let cut = say(53, "think slowly, cut by a crash")?;
    let cut_turn = turn_of(c.agent, c.chat, "chat", seq_of(&cut));
    let in_flight = s.eventually(c.wake, || s.ai.chats().len() > called);
    let killed = lever(api, c.id, "kill")?;
    let came_back = in_flight && killed.status == 200 && s.eventually(c.wake, || back(generation));
    let ends = || work_of(&records(api, c.owner, c.chat, "work"), &cut_turn).into_iter().filter(|r| r["body"]["kind"] == "turn.end").collect::<Vec<_>>();
    let ended = came_back && s.eventually(c.wake, || !ends().is_empty());
    let next = say(54, "and after the cut")?;
    let answered = s.eventually(c.wake, || replied(seq_of(&next)).is_some());
    let ends = ends();
    s.ok(
        CRASH_CHECKS[7],
        ended && ends.len() == 1 && ends[0]["body"]["outcome"] == "error" && replied(seq_of(&cut)).is_none() && s.ai.chats().len() == called + 1,
        format!("{} model calls since; {}", s.ai.chats().len() - called, json!(work_of(&records(api, c.owner, c.chat, "work"), &cut_turn))),
    );
    s.ok(CRASH_CHECKS[8], answered, json!(replied(seq_of(&next))));
    // its replies: the think, the fetch, after the crash, after the cut (and
    // the cut turn's, where a crash's restore ran it again)
    let added = [&think, &fetch, &after, &cut, &next].iter().filter(|r| replied(seq_of(r)).is_some()).count();
    Ok(added)
}

pub fn computers(s: &mut Suite, api: &Api) -> Result<()> {
    if !s.section("computers", &[crate::Need::Computers, crate::Need::Models]) {
        return Ok(());
    }
    // a hosted run's owner pays a real model, from the run's budget
    let owner = api.person_paying(HOSTED_PAID_CALLS)?;
    let stranger = api.person()?;
    let r = api.signed(&owner, "POST", "/api/computers", Some(&json!({})))?;
    let id = r.body["computer"].as_str().unwrap_or("").to_string();
    s.ok("a person makes their computer, asleep", r.status == 200 && id.starts_with("computer:") && r.body["phase"] == "asleep", &r);
    // The stub's scripted runtime (images/stub) answers its commands ("tool
    // please", "slow…", "fetch …", "think …") and echoes the rest; the
    // deployment's own image (our Hermes) answers as its real model does.
    // Checks that need the script run on the stub alone; the rest assert
    // what any real model's answer satisfies. The fakes (the swap's
    // upstream and accounts), the node, and the operator are a local run's.
    let scripted = r.body["image"] == "stub";
    let fakes = !s.hosted();
    let wake = if scripted { WAKE } else { HOSTED_WAKE };
    let unscripted = |s: &mut Suite, label: &str| s.skip(label, "it needs the stub image's scripted runtime, and this computer runs the deployment's own image");
    let again = api.signed(&owner, "POST", "/api/computers", Some(&json!({})))?;
    s.ok("making it again answers the same one", again.status == 200 && again.body["computer"] == id.as_str(), &again);
    let origin = r.body["origin"].as_str().unwrap_or("").to_string();
    s.ok("it has an origin of its own, cross-site from the platform", origin.contains("--computer.") || (s.hosted() && origin.contains("--computer--")), &origin);
    let r = api.signed(&stranger, "GET", &format!("/api/computers/{id}"), None)?;
    s.ok("no one else sees it", r.status == 404, &r);

    // an agent fragment, and a chat it is in
    let agent_name = s.named(api, &owner, "juniper")?;
    let agent = s.create(api, &owner, &agent_name)?;
    s.commit(&agent, &[("fragment.json", Some(AGENT_JSON))]);
    s.deploy(&agent);
    let r = api.signed(&owner, "PUT", &format!("/api/computers/{id}/agents/{agent_name}"), Some(&json!({})))?;
    let identity = r.body["agents"][0]["identity"].as_str().unwrap_or("").to_string();
    s.ok("its owner assigns the agent fragment to it", r.status == 200 && identity.starts_with("id:") && r.body["agents"][0]["fragment"] == agent_name.as_str(), &r);
    s.ok(
        "by default the agent may use every connection its owner has (decision 44)",
        r.body["agents"][0].get("connections").is_some_and(Value::is_null),
        &r,
    );
    s.ok("assigning it wakes nothing", r.body["phase"] == "asleep", &r);
    let r = api.signed(&owner, "GET", &format!("/api/f/{agent_name}/members"), None)?;
    let editor = r.body["members"].as_array().is_some_and(|m| m.iter().any(|m| m["principal"] == identity.as_str() && m["role"] == "editor"));
    s.ok("the agent is an editor of its own fragment", editor, &r);
    let r = api.signed(&stranger, "PUT", &format!("/api/computers/{id}/agents/{agent_name}"), Some(&json!({})))?;
    s.ok("no one else assigns to it", r.status == 404, &r);

    let chat_name = s.named(api, &owner, "chat")?;
    let chat = s.create(api, &owner, &chat_name)?;
    s.commit(&chat, &[("fragment.json", Some(CHAT_JSON))]);
    s.deploy(&chat);
    // the platform tells the agent's computer it joined: no one posts `joined` by hand
    let t0 = std::time::Instant::now();
    let r = api.signed(&owner, "PUT", &format!("/api/f/{chat_name}/members/{identity}"), Some(&json!({ "role": "editor" })))?;
    s.ok("the agent joins the chat", r.status == 200, &r);
    let agent_npub = agent["npub"].as_str().unwrap_or("").to_string();
    let heard = s.eventually(Duration::from_secs(10), || told(api, &owner, &agent_name, &chat_name).len() == 1);
    let notices = told(api, &owner, &agent_name, &chat_name);
    s.ok(
        "the platform posts joined on its agent's tasks, as the agent fragment itself",
        heard && notices[0]["principal"] == agent_npub.as_str() && notices[0]["body"] == json!({ "kind": "joined", "fragment": chat_name }),
        json!(notices),
    );
    // awake, the guest follows the chat as the agent
    let woke = s.eventually(wake, || phase(api, &owner, &id) == "awake");
    s.ok("and wakes its computer, unasked", woke, phase(api, &owner, &id));
    println!("      (awake in {:.1?})", t0.elapsed());
    let r = api.signed(&owner, "POST", &format!("/api/computers/{id}/wake"), Some(&json!({})))?;
    s.ok("its owner's wake of it awake answers awake", r.status == 200 && r.body["phase"] == "awake", &r);
    let subscribed = s.eventually(wake, || {
        api.signed(&owner, "GET", &format!("/api/f/{chat_name}/subscriptions"), None)
            .ok()
            .is_some_and(|r| r.body["subscriptions"].as_array().is_some_and(|l| l.iter().any(|x| x["wake"] == true && x["channel"] == "chat")))
    });
    s.ok("the guest subscribes to the chat as the agent, to be woken", subscribed, "");
    let say = |n: u32, text: &str| api.signed(&owner, "POST", &format!("/api/f/{chat_name}/channels/chat"), Some(&json!({ "id": format!("m{n}"), "body": { "text": text } })));
    let r = say(1, "hello there")?;
    s.ok("a message to the chat", r.status == 200, &r);
    let answered = s.eventually(wake, || agent_replies(&records(api, &owner, &chat_name, "chat"), &identity).len() == 1);
    let replies = agent_replies(&records(api, &owner, &chat_name, "chat"), &identity);
    let echoed = |r: &Value, text: &str| r["body"]["text"].as_str().is_some_and(|t| t.contains(text));
    s.ok(
        "the agent answers it, as itself, naming its turn",
        answered && replies.first().is_some_and(|r| if scripted { echoed(r, "hello there") } else { said_something(r) }),
        json!(replies),
    );
    // its end is posted just after its reply
    let started_and_ended = |work: &[Value]| ["turn.start", "turn.end"].iter().all(|k| work.iter().any(|r| r["body"]["kind"] == *k && r["principal"] == identity.as_str()));
    s.eventually(Duration::from_secs(10), || started_and_ended(&records(api, &owner, &chat_name, "work")));
    let work = records(api, &owner, &chat_name, "work");
    s.ok("its turn starts and ends on work", started_and_ended(&work), json!(work));
    let mut replies_so_far = 1;
    if !scripted {
        for label in [
            "a tool step is a record on work",
            "a page sees the reply's draft live, as the agent",
            "then the reply, naming the draft's turn",
            "its asker posts Stop for the turn",
            "the turn stops, and never gives its whole answer",
            "a prompt is a card on work, asking the agent's owner",
            "its owner answers it",
            "the turn goes on approved, the card closed",
            "a reply carries a file, uploaded as the chat's blob",
        ] {
            unscripted(s, label);
        }
    } else {
        say(2, "tool please")?;
        let stepped = s.eventually(wake, || records(api, &owner, &chat_name, "work").iter().any(|r| r["body"]["kind"] == "turn.step"));
        s.ok("a tool step is a record on work", stepped && s.eventually(wake, || agent_replies(&records(api, &owner, &chat_name, "chat"), &identity).len() == 2), "");
        replies_so_far = 2;

        // a page sees the reply's draft live, then the reply that replaces it
        let mut page = Socket::open(api, &chat_name, "__live", Some(&owner), None)?;
        page.until("hello", 5)?;
        let seq = records(api, &owner, &chat_name, "chat").last().and_then(|r| r["seq"].as_i64()).unwrap_or(0);
        page.send(&json!({ "type": "subscribe", "channel": "chat", "after": seq }))?;
        page.until("subscribed", 20)?;
        say(10, "slow, for the page")?;
        let draft = page.until("draft", 200);
        let reply = page.until("record", 200);
        let drafted = draft.as_ref().is_ok_and(|d| d["principal"] == identity.as_str() && d["text"].as_str().is_some_and(|t| !t.is_empty()));
        let replaced = matches!((&draft, &reply), (Ok(d), Ok(r)) if r["body"]["turn"] == d["turn"] && r["principal"] == identity.as_str());
        s.ok("a page sees the reply's draft live, as the agent", drafted, format!("{draft:?}"));
        s.ok("then the reply, naming the draft's turn", replaced, format!("{reply:?}"));
        page.close();
        replies_so_far += 1;
        s.eventually(wake, || agent_replies(&records(api, &owner, &chat_name, "chat"), &identity).len() == replies_so_far);

        // the turn's asker stops it
        let r = say(11, "slow, then stopped")?;
        let stopped_seq = r.body["record"]["seq"].as_i64().unwrap_or(0);
        let turn = turn_of(&agent_name, &chat_name, "chat", stopped_seq);
        let stop = json!({ "id": "stop-1", "body": { "kind": "stop", "turn": turn } });
        let r = api.signed(&owner, "POST", &format!("/api/f/{chat_name}/channels/chat"), Some(&stop))?;
        s.ok("its asker posts Stop for the turn", r.status == 200, &r);
        let ended = s.eventually(wake, || {
            let w = work_of(&records(api, &owner, &chat_name, "work"), &turn);
            // stopped as it ran, or never run at all
            w.iter().any(|r| r["body"]["kind"] == "turn.end" && r["body"]["outcome"] == "stopped") || (w.is_empty() && records(api, &owner, &chat_name, "chat").iter().any(|r| r["body"]["kind"] == "stop"))
        });
        std::thread::sleep(Duration::from_secs(2));
        let full = agent_replies(&records(api, &owner, &chat_name, "chat"), &identity).iter().any(|r| r["body"]["turn"] == turn.as_str());
        s.ok("the turn stops, and never gives its whole answer", ended && !full, json!(work_of(&records(api, &owner, &chat_name, "work"), &turn)));

        // only the agent's owner answers its prompt
        let r = say(12, "approve this, please")?;
        let turn = turn_of(&agent_name, &chat_name, "chat", r.body["record"]["seq"].as_i64().unwrap_or(0));
        let asked = s.eventually(wake, || work_of(&records(api, &owner, &chat_name, "work"), &turn).iter().any(|r| r["body"]["kind"] == "turn.prompt"));
        let work = work_of(&records(api, &owner, &chat_name, "work"), &turn);
        let card = work.iter().find(|r| r["body"]["kind"] == "turn.prompt").cloned().unwrap_or_default();
        let prompt = card["body"]["prompt"].as_str().unwrap_or("").to_string();
        s.ok("a prompt is a card on work, asking the agent's owner", asked && card["body"]["asks"] == api.identity(&owner)?.as_str(), &card);
        let answer = json!({ "id": format!("pr:{prompt}"), "body": { "kind": "prompt_response", "prompt": prompt, "option": "once" } });
        let r = api.signed(&owner, "POST", &format!("/api/f/{chat_name}/channels/chat"), Some(&answer))?;
        s.ok("its owner answers it", r.status == 200, &r);
        let approved = s.eventually(wake, || agent_replies(&records(api, &owner, &chat_name, "chat"), &identity).iter().any(|r| r["body"]["turn"] == turn.as_str() && r["body"]["text"].as_str().is_some_and(|t| t.contains("(approved)"))));
        let closed = work_of(&records(api, &owner, &chat_name, "work"), &turn).iter().any(|r| r["body"]["kind"] == "turn.prompt.closed" && r["body"]["outcome"] == "answered");
        s.ok("the turn goes on approved, the card closed", approved && closed, json!(work_of(&records(api, &owner, &chat_name, "work"), &turn)));
        replies_so_far += 1;

        // a reply carries a file, one of the chat's blobs
        let r = say(13, "draw me something")?;
        let turn = turn_of(&agent_name, &chat_name, "chat", r.body["record"]["seq"].as_i64().unwrap_or(0));
        let drew = s.eventually(wake, || agent_replies(&records(api, &owner, &chat_name, "chat"), &identity).iter().any(|r| r["body"]["turn"] == turn.as_str() && r["body"]["attachments"][0]["sha256"].is_string()));
        let file = agent_replies(&records(api, &owner, &chat_name, "chat"), &identity).into_iter().find(|r| r["body"]["turn"] == turn.as_str()).map(|r| r["body"]["attachments"][0].clone()).unwrap_or_default();
        let sha = file["sha256"].as_str().unwrap_or("");
        let blob = api.signed(&owner, "GET", &format!("/api/f/{chat_name}/blobs/{sha}"), None)?;
        s.ok("a reply carries a file, uploaded as the chat's blob", drew && blob.status == 200 && blob.text.contains("a drawing for"), format!("{file} {}", blob.status));
        replies_so_far += 1;
        s.eventually(wake, || agent_replies(&records(api, &owner, &chat_name, "chat"), &identity).len() == replies_so_far);
    }
    let owner_id = api.identity(&owner)?;

    // the swap (Paul, 2026-10-04): the guest finds each credential its agent
    // may use as a placeholder in its environment variable, and sends it as
    // any SDK would, with no header of ours
    let fetched = |s: &Suite, n: u32, text: &str| -> Result<String> {
        let r = say(n, text)?;
        let turn = turn_of(&agent_name, &chat_name, "chat", r.body["record"]["seq"].as_i64().unwrap_or(0));
        let reply = || agent_replies(&records(api, &owner, &chat_name, "chat"), &identity).into_iter().find(|r| r["body"]["turn"] == turn.as_str());
        s.eventually(wake, || reply().is_some());
        Ok(reply().and_then(|r| r["body"]["text"].as_str().map(str::to_string)).unwrap_or_default())
    };
    let connections = format!("/api/computers/{id}/agents/{agent_name}/connections");
    let allow = |who: &crate::Keys, list: Value| api.signed(who, "PUT", &connections, Some(&json!({ "connections": list })));
    if !(fakes && scripted) {
        // the agent's narrowing as its owner sets it; what the swap does with
        // it needs the stub's fetch and the fakes' accounts and providers
        let r = allow(&stranger, json!([]))?;
        s.ok("no one else narrows its agents' providers", r.status == 404, &r);
        let r = allow(&owner, json!(["nonesuch"]))?;
        s.ok("a provider the deployment does not offer is refused", r.status == 400, &r);
        let r = api.signed(&owner, "PUT", &connections, Some(&json!({})))?;
        s.ok("a body that names no providers is refused, never read as every one", r.status == 400, &r);
        let (r, again) = (allow(&owner, json!([]))?, allow(&owner, json!([]))?);
        s.ok(
            "its owner narrows the agent to none (a role's specialization, decision 44); again is the same",
            r.status == 200 && r.body["agents"][0]["connections"] == json!([]) && again.status == 200 && again.body["agents"] == r.body["agents"],
            &r,
        );
        let r = allow(&owner, Value::Null)?;
        s.ok("null gives it every provider its owner has again", r.status == 200 && r.body["agents"][0]["connections"].is_null(), &r);
        for label in SWAP_CHECKS.iter().chain(&["narrowed, it is refused a provider its owner has"]) {
            s.skip(label, "it needs the stub's fetch, and the fakes' accounts and providers behind the swap");
        }
    } else {
        replies_so_far += swap_checks(s, api, &Swapping { owner: &owner, stranger: &stranger, id: &id, agent: &agent_name, identity: &identity }, &fetched)?;
        let r = allow(&stranger, json!([]))?;
        s.ok("no one else narrows its agents' providers", r.status == 404, &r);
        let r = allow(&owner, json!(["notion"]))?;
        s.ok("a provider the deployment does not offer is refused", r.status == 400, &r);
        let r = api.signed(&owner, "PUT", &connections, Some(&json!({})))?;
        s.ok("a body that names no providers is refused, never read as every one", r.status == 400, &r);
        let r = allow(&owner, json!([]))?;
        let again = allow(&owner, json!([]))?;
        s.ok(
            "its owner narrows the agent to none (a role's specialization, decision 44); again is the same",
            r.status == 200 && r.body["agents"][0]["connections"] == json!([]) && again.status == 200 && again.body["agents"] == r.body["agents"],
            &r,
        );
        let seen_before = s.upstream.seen().len();
        let said = fetched(s, 29, &format!("fetch http://{SWAP_CONNECTION_HOST}/drive/v3/files with {}", placeholder(s, &id, &agent_name, SWAP_CONNECTION)))?;
        replies_so_far += 1;
        s.ok("narrowed, it is refused a provider its owner has", said.starts_with("fetched 403") && said.contains("may not use google") && s.upstream.seen().len() == seen_before, &said);
        let r = allow(&owner, Value::Null)?;
        s.ok("null gives it every provider its owner has again", r.status == 200 && r.body["agents"][0]["connections"].is_null(), &r);
    }

    // the model intercept: the agent's call is the platform's model route, its owner paying
    if scripted {
        if fakes {
            s.ai.clear_script();
        }
        let aig_before = entries(api, &owner_id, "aig:").len();
        let said = fetched(s, 23, "think hello model")?;
        replies_so_far += 1;
        s.ok("a model call through the computer's intercept answers as the model did", said == "thought: echo: hello model", &said);
        let aig = entries(api, &owner_id, "aig:");
        s.ok(
            "and is metered to the agent's owner, settled from its usage",
            aig.len() == aig_before + 1 && aig.iter().filter(|e| end_of(e) == "settled").count() == aig_before + 1,
            json!(aig),
        );
    } else {
        // a real runtime called its model to answer at all: each call went
        // through the intercept, reserved and settled on its owner's ledger
        let settled = |aig: &[Value]| !aig.is_empty() && aig.iter().all(|e| end_of(e) == "settled");
        s.eventually(Duration::from_secs(20), || settled(&entries(api, &owner_id, "aig:")));
        let aig = entries(api, &owner_id, "aig:");
        s.ok(
            "its answer's model calls went through the computer's intercept, each metered to the agent's owner and settled from its usage",
            settled(&aig),
            json!(aig),
        );
    }

    // asleep, a record wakes it, restored, and nothing runs twice
    std::thread::sleep(QUEUE_DRAIN);
    let ran = fakes.then(|| runs(s, api, &owner, &owner_id, &id));
    let r = api.signed(&owner, "POST", &format!("/api/computers/{id}/sleep"), Some(&json!({})))?;
    s.ok("its owner puts it to sleep", r.status == 200 && r.body["phase"] == "asleep", &r);
    let awake = entries(api, &owner_id, &format!("awake:{id}:"));
    s.ok(
        "its awake time reaches its owner's ledger at the sleep, priced",
        !awake.is_empty() && awake.iter().all(|e| e["entry"]["row"]["usage"]["ms"].as_u64().is_some_and(|ms| ms > 0) && e["entry"]["charge"].as_i64().is_some_and(|c| c > 0)),
        json!(awake),
    );
    let t0 = std::time::Instant::now();
    say(3, "are you there")?;
    replies_so_far += 1;
    let woke = s.eventually(wake, || agent_replies(&records(api, &owner, &chat_name, "chat"), &identity).len() == replies_so_far);
    println!("      (woken and answered in {:.1?})", t0.elapsed());
    let replies = agent_replies(&records(api, &owner, &chat_name, "chat"), &identity);
    s.ok(
        "a record on the chat wakes it, and it answers",
        woke && replies.last().is_some_and(|r| if scripted { echoed(r, "are you there") } else { said_something(r) }),
        json!(replies),
    );
    let r = api.signed(&owner, "GET", &format!("/api/computers/{id}"), None)?;
    s.ok("awake again", r.body["phase"] == "awake", &r);
    // its answer came after every turn its restore read again: the runs
    // (where the fakes count them) and the replies are as they were
    let replies = agent_replies(&records(api, &owner, &chat_name, "chat"), &identity);
    let again = fakes.then(|| runs(s, api, &owner, &owner_id, &id));
    s.ok(
        "its restored /data knew what it had run: nothing runs twice",
        replies.len() == replies_so_far && again == ran,
        format!("runs {ran:?} -> {again:?}; {}", json!(replies)),
    );

    // a crash wakes it from its last save, and nothing that started runs again
    if fakes && scripted {
        let crashing = Crashing { owner: &owner, owner_id: &owner_id, id: &id, chat: &chat_name, agent: &agent_name, identity: &identity, wake };
        replies_so_far += crash_checks(s, api, &crashing, &|n, text| say(n, text))?;
    } else {
        for label in CRASH_CHECKS {
            s.skip(label, "it needs the stub's scripted runtime, and the fakes to hold a model call and count runs");
        }
    }

    // a second agent on the same computer: @mentioned it answers, else the lead
    let maple_name = s.named(api, &owner, "maple")?;
    let maple = s.create(api, &owner, &maple_name)?;
    s.commit(&maple, &[("fragment.json", Some(AGENT_JSON))]);
    s.deploy(&maple);
    let r = api.signed(&owner, "PUT", &format!("/api/computers/{id}/agents/{maple_name}"), Some(&json!({})))?;
    let maple_id = r.body["agents"].as_array().and_then(|a| a.iter().find(|x| x["fragment"] == maple_name.as_str())).and_then(|a| a["identity"].as_str()).unwrap_or("").to_string();
    s.ok("a second agent runs on the same computer", r.status == 200 && maple_id.starts_with("id:") && maple_id != identity, &r);
    api.signed(&owner, "PUT", &format!("/api/f/{chat_name}/members/{maple_id}"), Some(&json!({ "role": "editor" })))?;
    // the stub's bridge reads its agents again every minute (the Hermes lane
    // proves the seconds of our Hermes image): a sleep and a wake follows the
    // new one at once
    api.signed(&owner, "POST", &format!("/api/computers/{id}/sleep"), Some(&json!({})))?;
    api.signed(&owner, "POST", &format!("/api/computers/{id}/wake"), Some(&json!({})))?;
    let maple_label = maple_name.split('.').next().unwrap_or("").to_string();
    say(20, &format!("@{maple_label} what do you think"))?;
    let heard = s.eventually(wake, || agent_replies(&records(api, &owner, &chat_name, "chat"), &maple_id).len() == 1);
    s.ok("@mentioned, the second agent answers", heard, json!(agent_replies(&records(api, &owner, &chat_name, "chat"), &maple_id)));
    std::thread::sleep(Duration::from_secs(2));
    let lead_quiet = agent_replies(&records(api, &owner, &chat_name, "chat"), &identity).len() == replies_so_far;
    s.ok("and the lead does not", lead_quiet, json!(agent_replies(&records(api, &owner, &chat_name, "chat"), &identity).len()));
    say(21, "anyone home")?;
    replies_so_far += 1;
    let led = s.eventually(wake, || agent_replies(&records(api, &owner, &chat_name, "chat"), &identity).len() == replies_so_far);
    s.ok("unmentioned, the lead answers", led && agent_replies(&records(api, &owner, &chat_name, "chat"), &maple_id).len() == 1, "");

    // a placeholder names its agent, not whoever sends it: the second
    // agent's, sent by the lead, is the second's (a person's agents are not
    // fenced: decision 44); removed from the computer, its tags are refused
    if fakes && scripted {
        let theirs = placeholder(s, &id, &maple_name, "perplexity");
        let said = fetched(s, 24, &format!("fetch http://api.perplexity.ai/search with {theirs}"))?;
        replies_so_far += 1;
        let uses = || api.signed(&owner, "GET", &format!("/api/computers/{id}/uses"), None).map(|r| r.body).unwrap_or(Value::Null);
        let maples = |u: &Value| u["uses"].as_array().is_some_and(|l| l.iter().any(|x| x["provider"] == "perplexity" && x["agent"] == maple_name.as_str() && x["calls"] == 1));
        let counted = s.eventually(Duration::from_secs(20), || maples(&uses()));
        s.ok("another agent's placeholder on the same computer is that agent's: its call is counted as the second agent's", said.starts_with("fetched 200") && counted, json!({ "said": said, "uses": uses() }));
        let r = api.signed(&owner, "DELETE", &format!("/api/computers/{id}/agents/{maple_name}"), None)?;
        let seen_before = s.upstream.seen().len();
        let said = fetched(s, 25, &format!("fetch http://api.perplexity.ai/search with {theirs}"))?;
        replies_so_far += 1;
        s.ok(
            "an agent removed from the computer: its tags are refused, reaching no provider",
            r.status == 200 && said.starts_with("fetched 403") && said.contains("names no agent on this computer") && s.upstream.seen().len() == seen_before,
            json!({ "removed": r.status, "said": said }),
        );
    } else {
        for label in ["another agent's placeholder on the same computer is that agent's: its call is counted as the second agent's", "an agent removed from the computer: its tags are refused, reaching no provider"] {
            s.skip(label, "it needs the stub's fetch, and the fakes' providers behind the swap");
        }
    }

    // its ports, on its own origin, for its owner
    let r = api.signed(&stranger, "POST", &format!("/api/computers/{id}/ports/6080/ticket"), Some(&json!({})))?;
    s.ok("no one else opens its ports", r.status == 404, &r);
    let r = api.signed(&owner, "POST", &format!("/api/computers/{id}/ports/6080/ticket"), Some(&json!({})))?;
    let ticket = r.body["url"].as_str().unwrap_or("").to_string();
    s.ok("its owner mints a one-time ticket to its screen", r.status == 200 && ticket.starts_with(&origin) && ticket.contains("/__ticket?t="), &r);
    let r = api.call(Call { method: "GET", url: ticket.clone(), ..Call::default() })?;
    let cookie = r.cookies().into_iter().find(|c| c.starts_with("fragment_computer=")).map(|c| c.split(';').next().unwrap_or("").to_string());
    s.ok("the ticket signs the browser in to the computer's origin", r.status == 303 && r.header("location") == "/p/6080/" && cookie.is_some(), &r);
    let r = api.call(Call { method: "GET", url: ticket, ..Call::default() })?;
    s.ok("a ticket works once", r.status == 401, &r);
    let screen = api.call(Call { method: "GET", url: format!("{origin}/p/6080/"), cookie: cookie.clone(), ..Call::default() })?;
    s.ok("the port answers its owner's browser", screen.status == 200 && screen.text.to_ascii_lowercase().contains("<html"), format!("{} {}", screen.status, &screen.text[..screen.text.len().min(200)]));
    let r = api.call(Call { method: "GET", url: format!("{origin}/p/6080/"), ..Call::default() })?;
    s.ok("and no one without a session", r.status == 401, &r);
    let r = api.call(Call { method: "GET", url: format!("{origin}/p/6080/"), keys: Some(&stranger), ..Call::default() })?;
    s.ok("nor anyone else who signs", r.status == 401, &r);
    let r = api.call(Call { method: "GET", url: format!("{origin}/p/6080/"), keys: Some(&owner), ..Call::default() })?;
    s.ok("its owner's signed request needs no session", r.status == 200, &r);
    // a socket on its port, bridged through the Computer DO both ways: the
    // screen's control socket speaks first (who holds control), as an RFB
    // server does, and that first word reaches the page
    let control = || Socket::connect(api, &format!("{origin}/p/6080/control?viewer=e2e"), None, cookie.as_deref(), Some(&origin)).map(|(socket, _)| socket);
    let heard = control().and_then(|mut c| {
        let first = c.next()?;
        c.send(&json!({ "type": "take" }))?;
        let taken = c.next()?;
        c.close();
        Ok((first, taken))
    });
    s.ok(
        "a socket on its port opens from its own page, and carries the container's first word and the page's answer",
        heard.as_ref().is_ok_and(|(first, taken)| *first == json!({ "type": "control", "holder": null }) && taken["holder"] == "e2e"),
        format!("{heard:?}"),
    );
    // and in a frame of the platform's page (the shell's tab onto its screen),
    // where the platform is cross-site from the computer's origin
    match s.hosted() {
        true => s.skip("its ports open in a frame of the platform's page", "a branch preview puts the platform and the computer's origin in one zone: one site (frames.rs needs two)"),
        false => super::frames::computer_ports(s, api, &owner, &id, &origin, cookie.as_deref().unwrap_or(""))?,
    }

    // its image pin: an upgrade at the next wake, then a rollback, its data kept
    let version = || api.call(Call { method: "GET", url: format!("{origin}/p/6080/version.txt"), keys: Some(&owner), ..Call::default() }).map(|r| r.text.trim().to_string()).unwrap_or_default();
    let r = api.signed(&owner, "PUT", &format!("/api/computers/{id}/image"), Some(&json!({ "image": "no-such-image" })))?;
    s.ok("an image the deployment does not have is refused", r.status == 400, &r);
    if !scripted {
        for label in ["it runs its first build", "the next build is pinned", "woken, it runs the next build (an upgrade)", "with its /data restored: it answers anew, and nothing twice", "woken, it runs the first build again (a rollback)"] {
            s.skip(label, "it needs the stub's two builds (stub, stub-next) and the version its screen serves");
        }
    } else {
        s.ok("it runs its first build", version() == "1", version());
        let r = api.signed(&owner, "PUT", &format!("/api/computers/{id}/image"), Some(&json!({ "image": "stub-next" })))?;
        s.ok("the next build is pinned", r.status == 200 && r.body["image"] == "stub-next", &r);
        s.ok("it keeps running the build it started with until it sleeps", version() == "1", version());
        std::thread::sleep(QUEUE_DRAIN);
        let ran = fakes.then(|| runs(s, api, &owner, &owner_id, &id));
        api.signed(&owner, "POST", &format!("/api/computers/{id}/sleep"), Some(&json!({})))?;
        s.ok("woken, it runs the next build (an upgrade)", version() == "2", version());
        say(30, "after the upgrade")?;
        replies_so_far += 1;
        let kept = s.eventually(wake, || agent_replies(&records(api, &owner, &chat_name, "chat"), &identity).len() == replies_so_far);
        let again = fakes.then(|| runs(s, api, &owner, &owner_id, &id));
        s.ok(
            "with its /data restored: it answers anew, and nothing runs twice",
            kept && again == ran,
            format!("runs {ran:?} -> {again:?}; {} replies", agent_replies(&records(api, &owner, &chat_name, "chat"), &identity).len()),
        );
        let r = api.signed(&owner, "PUT", &format!("/api/computers/{id}/image"), Some(&json!({ "image": "stub" })))?;
        s.ok("the first build is pinned again", r.status == 200 && r.body["image"] == "stub", &r);
        std::thread::sleep(QUEUE_DRAIN);
        api.signed(&owner, "POST", &format!("/api/computers/{id}/sleep"), Some(&json!({})))?;
        s.ok("woken, it runs the first build again (a rollback)", version() == "1", version());
    }

    // a computer's egress alone asks for a wake subscription
    let r = api.signed(&owner, "POST", &format!("/api/f/{chat_name}/subscriptions"), Some(&json!({ "channel": "chat", "wake": true })))?;
    s.ok("a wake subscription from outside a computer is no wake (it names no URL)", r.status == 400, &r);

    // a routine: its agent fragment's cron, on time, wakes it asleep
    s.commit(&agent, &[("app.mjs", Some(routine_app(&chat_name).as_bytes())), ("fragment.json", Some(ROUTINE_JSON))]);
    s.deploy(&agent);
    std::thread::sleep(QUEUE_DRAIN);
    // its cron runs every minute, so a routine may wake it before it sleeps:
    // asked until asleep, and only a routine after that counts
    let mut last = Value::Null;
    let slept = s.eventually(Duration::from_secs(90), || {
        let r = api.signed(&owner, "POST", &format!("/api/computers/{id}/sleep"), Some(&json!({})));
        last = r.as_ref().map(|r| r.body.clone()).unwrap_or(Value::Null);
        last["phase"] == "asleep"
    });
    s.ok("asleep, it waits for its routine", slept, &last);
    // the stub says the routine's text; a real model says what it likes, in a reply of its own
    let replied_before = agent_replies(&records(api, &owner, &chat_name, "chat"), &identity).len();
    let routine = |recs: &[Value]| match scripted {
        true => agent_replies(recs, &identity).iter().skip(replied_before).any(|r| echoed(r, "water the plants")),
        false => agent_replies(recs, &identity).iter().skip(replied_before).any(said_something),
    };
    let ran = s.eventually(Duration::from_secs(150) + if scripted { Duration::ZERO } else { wake }, || routine(&records(api, &owner, &chat_name, "chat")));
    s.ok("its cron's routine wakes it, and the agent does it in the chat", ran, "");
    api.signed(&owner, "POST", &format!("/api/f/{agent_name}/pause"), Some(&json!({ "op": "routine", "paused": true })))?;

    // asleep, its agent added to a new chat wakes it, and it follows that
    // chat before anyone speaks there (Paul, 2026-10-03)
    let next_name = s.named(api, &owner, "chat-next")?;
    let next = s.create(api, &owner, &next_name)?;
    s.commit(&next, &[("fragment.json", Some(CHAT_JSON))]);
    s.deploy(&next);
    let deployed = s.eventually(Duration::from_secs(30), || {
        api.signed(&owner, "GET", &format!("/api/f/{next_name}/channels"), None).is_ok_and(|r| r.body["channels"].as_array().is_some_and(|c| c.iter().any(|x| x["name"] == "chat")))
    });
    std::thread::sleep(QUEUE_DRAIN);
    // a real runtime's last turn may still post a record that wakes it
    // again (a preview's model is slower than the stub): asked until asleep
    let mut last = Value::Null;
    let slept = s.eventually(Duration::from_secs(90), || {
        let r = api.signed(&owner, "POST", &format!("/api/computers/{id}/sleep"), Some(&json!({})));
        last = r.as_ref().map(|r| r.body.clone()).unwrap_or(Value::Null);
        last["phase"] == "asleep"
    });
    s.ok("asleep, with a new chat deployed", deployed && slept, &last);
    let t0 = std::time::Instant::now();
    let r = api.signed(&owner, "PUT", &format!("/api/f/{next_name}/members/{identity}"), Some(&json!({ "role": "editor" })))?;
    s.ok("its agent is added to the new chat", r.status == 200, &r);
    let woke = s.eventually(wake, || phase(api, &owner, &id) == "awake");
    s.ok("which wakes the sleeping computer", woke && told(api, &owner, &agent_name, &next_name).len() == 1, phase(api, &owner, &id));
    let followed = s.eventually(wake, || {
        api.signed(&owner, "GET", &format!("/api/f/{next_name}/subscriptions"), None)
            .ok()
            .is_some_and(|r| r.body["subscriptions"].as_array().is_some_and(|l| l.iter().any(|x| x["wake"] == true && x["channel"] == "chat")))
    });
    println!("      (following the new chat {:.1?} after the agent was added)", t0.elapsed());
    s.ok("and the guest follows the new chat as the agent, unasked", followed, "");
    let r = api.signed(&owner, "PUT", &format!("/api/f/{next_name}/members/{identity}"), Some(&json!({ "role": "editor" })))?;
    let all_joined = |api: &Api| records(api, &owner, &agent_name, "tasks").into_iter().filter(|r| r["body"]["kind"] == "joined").count();
    let before = all_joined(api);
    s.ok("adding it again is no new join: nothing is posted twice", r.status == 200 && told(api, &owner, &agent_name, &next_name).len() == 1, json!(told(api, &owner, &agent_name, &next_name)));
    let stranger_id = api.identity(&stranger)?;
    let r = api.signed(&owner, "PUT", &format!("/api/f/{next_name}/members/{stranger_id}"), Some(&json!({ "role": "viewer" })))?;
    s.ok("a person added is no agent's join: nothing is posted", r.status == 200 && all_joined(api) == before, json!(all_joined(api)));
    let r = api.signed(&owner, "POST", &format!("/api/f/{next_name}/channels/chat"), Some(&json!({ "id": "n1", "body": { "text": "hello in the new chat" } })))?;
    let answered = s.eventually(wake, || agent_replies(&records(api, &owner, &next_name, "chat"), &identity).len() == 1);
    s.ok("it answers there", r.status == 200 && answered, json!(agent_replies(&records(api, &owner, &next_name, "chat"), &identity)));

    if !fakes {
        // the node and the deployment's operator are a local run's; a
        // preview's computer is put to sleep, so it bills no awake time after
        s.skip("after a crash of the platform, the computer answers what comes next, once", "it crashes the node");
        s.skip("at zero credit no wake starts, nor does a record wake it", "it needs the deployment's operator (to make its owner a guest)");
        std::thread::sleep(QUEUE_DRAIN);
        let r = api.signed(&owner, "POST", &format!("/api/computers/{id}/sleep"), Some(&json!({})))?;
        s.ok("it sleeps at the end", r.body["phase"] == "asleep", &r);
        return Ok(());
    }
    // the platform crashes while it is awake: a new isolate takes the
    // computer over (lesson 6), and it answers what comes next, once
    api.signed(&owner, "POST", &format!("/api/computers/{id}/wake"), Some(&json!({})))?;
    s.crash()?;
    let api = s.start(false, true)?;
    let r = api.signed(&owner, "POST", &format!("/api/f/{chat_name}/channels/chat"), Some(&json!({ "id": "m40", "body": { "text": "after the crash" } })))?;
    s.ok("after a crash of the platform, a message to the chat", r.status == 200, &r);
    // its own turn's replies: a routine's, on its cron minute, may land in
    // the same window and is no second answer
    let turn = turn_of(&agent_name, &chat_name, "chat", r.body["record"]["seq"].as_i64().unwrap_or(0));
    let its = |api: &Api| agent_replies(&records(api, &owner, &chat_name, "chat"), &identity).into_iter().filter(|r| r["body"]["turn"] == turn.as_str()).collect::<Vec<_>>();
    let after = s.eventually(wake, || its(&api).len() == 1);
    std::thread::sleep(Duration::from_secs(2));
    let replies = its(&api);
    s.ok(
        "the computer answers it, once",
        after && replies.len() == 1 && replies[0]["body"]["text"].as_str().is_some_and(|t| t.contains("after the crash")),
        json!(replies),
    );
    std::thread::sleep(QUEUE_DRAIN);
    let r = api.signed(&owner, "POST", &format!("/api/computers/{id}/sleep"), Some(&json!({})))?;
    s.ok("it sleeps at the end", r.body["phase"] == "asleep", &r);

    // at zero credit agents stop, and no wake starts (decision 27): an
    // operator makes its owner a guest
    let op_session = api.sign_in("operator@e2e.test")?;
    // the ledger section approves the operator's key when it runs first
    let _ = api.approve(&op_session, &s.operator);
    let r = api.signed(&s.operator, "POST", &format!("/api/ledger/{owner_id}/plan"), Some(&json!({ "id": "computers-guest", "plan": "guest" })))?;
    s.ok("an operator makes its owner a guest", r.status == 200, &r);
    let r = api.signed(&owner, "POST", &format!("/api/computers/{id}/wake"), Some(&json!({})))?;
    // a guest's ledger answers 403 (a guest pays for nothing); one at zero, 402
    s.ok("its owner's ledger refuses the wake, saying why", r.status == 403 && r.text.contains("a guest pays for nothing"), &r);
    let r = api.signed(&owner, "GET", &format!("/api/computers/{id}"), None)?;
    s.ok("it stays asleep, the refusal its why", r.body["phase"] == "asleep" && r.body["why"].as_str().is_some_and(|w| w.contains("a guest pays for nothing")), &r);
    let replies_now = agent_replies(&records(&api, &owner, &chat_name, "chat"), &identity).len();
    say(31, "anyone there?")?;
    std::thread::sleep(Duration::from_secs(5));
    let r = api.signed(&owner, "GET", &format!("/api/computers/{id}"), None)?;
    s.ok(
        "nor does a record on its chat wake it",
        r.body["phase"] == "asleep" && agent_replies(&records(&api, &owner, &chat_name, "chat"), &identity).len() == replies_now,
        &r,
    );
    Ok(())
}
