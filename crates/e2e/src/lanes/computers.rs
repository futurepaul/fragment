//! Computers (docs/computers.md): the generic Computer Durable Object
//! against the stub image (`images/stub`: the bridge with its scripted
//! runtime), under `wrangler dev` with Docker. The platform side names no
//! runtime, so these checks pass with the stub exactly as with Hermes
//! (the real-Hermes lane runs the same flows on our Hermes image).
//!
//! A person makes their computer and assigns an agent fragment to it; the
//! agent, an editor of its own fragment, joins a chat. Woken, the guest
//! subscribes to the chat as the agent (a wake subscription, which only a
//! computer's egress makes) and answers a message, a tool step on
//! `work`. A page sees a reply's draft live, then the reply; the turn's
//! asker stops a turn; only the agent's owner answers its prompt; a reply
//! carries a file. Put to sleep, a record on the chat wakes it, its
//! `/data` restored, and nothing is answered twice. A second agent on the
//! same computer answers when @mentioned, the lead otherwise. Its ports
//! answer its owner on its own origin through a one-time ticket, and no
//! one else. A routine, its agent fragment's cron, wakes it asleep.

use std::time::Duration;

use anyhow::Result;
use serde_json::{json, Value};

use sha2::{Digest, Sha256};

use super::jobs::records;
use super::ledger::{end_of, entries};
use crate::api::{Api, Call, Socket};
use crate::{Suite, SWAP_CONNECTION, SWAP_CONNECTION_HOST, SWAP_KEY, SWAP_KEY_HOST, SWAP_KEY_MICROS, SWAP_KEY_VALUE};

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

pub fn computers(s: &mut Suite, api: &Api) -> Result<()> {
    if !s.section("computers") {
        return Ok(());
    }
    let owner = api.person()?;
    let stranger = api.person()?;
    let r = api.signed(&owner, "POST", "/api/computers", Some(&json!({})))?;
    let id = r.body["computer"].as_str().unwrap_or("").to_string();
    s.ok("a person makes their computer, asleep", r.status == 200 && id.starts_with("computer:") && r.body["phase"] == "asleep", &r);
    let again = api.signed(&owner, "POST", "/api/computers", Some(&json!({})))?;
    s.ok("making it again answers the same one", again.status == 200 && again.body["computer"] == id.as_str(), &again);
    let origin = r.body["origin"].as_str().unwrap_or("").to_string();
    s.ok("it has an origin of its own, cross-site from the platform", origin.contains("--computer."), &origin);
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
    let r = api.signed(&owner, "GET", &format!("/api/f/{agent_name}/members"), None)?;
    let editor = r.body["members"].as_array().is_some_and(|m| m.iter().any(|m| m["principal"] == identity.as_str() && m["role"] == "editor"));
    s.ok("the agent is an editor of its own fragment", editor, &r);
    let r = api.signed(&stranger, "PUT", &format!("/api/computers/{id}/agents/{agent_name}"), Some(&json!({})))?;
    s.ok("no one else assigns to it", r.status == 404, &r);

    let chat_name = s.named(api, &owner, "chat")?;
    let chat = s.create(api, &owner, &chat_name)?;
    s.commit(&chat, &[("fragment.json", Some(CHAT_JSON))]);
    s.deploy(&chat);
    let r = api.signed(&owner, "PUT", &format!("/api/f/{chat_name}/members/{identity}"), Some(&json!({ "role": "editor" })))?;
    s.ok("the agent joins the chat", r.status == 200, &r);
    let joined = json!({ "id": "joined-1", "body": { "kind": "joined", "fragment": chat_name } });
    let r = api.signed(&owner, "POST", &format!("/api/f/{agent_name}/channels/tasks"), Some(&joined))?;
    s.ok("and its tasks hear so", r.status == 200, &r);

    // awake, the guest follows the chat as the agent
    let t0 = std::time::Instant::now();
    let r = api.signed(&owner, "POST", &format!("/api/computers/{id}/wake"), Some(&json!({})))?;
    s.ok("its owner wakes it", r.status == 200 && r.body["phase"] == "awake", &r);
    println!("      (awake in {:.1?})", t0.elapsed());
    let subscribed = s.eventually(WAKE, || {
        api.signed(&owner, "GET", &format!("/api/f/{chat_name}/subscriptions"), None)
            .ok()
            .is_some_and(|r| r.body["subscriptions"].as_array().is_some_and(|l| l.iter().any(|x| x["wake"] == true && x["channel"] == "chat")))
    });
    s.ok("the guest subscribes to the chat as the agent, to be woken", subscribed, "");
    let say = |n: u32, text: &str| api.signed(&owner, "POST", &format!("/api/f/{chat_name}/channels/chat"), Some(&json!({ "id": format!("m{n}"), "body": { "text": text } })));
    let r = say(1, "hello there")?;
    s.ok("a message to the chat", r.status == 200, &r);
    let answered = s.eventually(WAKE, || agent_replies(&records(api, &owner, &chat_name, "chat"), &identity).len() == 1);
    let replies = agent_replies(&records(api, &owner, &chat_name, "chat"), &identity);
    s.ok("the agent answers it, as itself, naming its turn", answered && replies[0]["body"]["text"].as_str().is_some_and(|t| t.contains("hello there")), json!(replies));
    let work = records(api, &owner, &chat_name, "work");
    s.ok("its turn starts and ends on work", ["turn.start", "turn.end"].iter().all(|k| work.iter().any(|r| r["body"]["kind"] == *k && r["principal"] == identity.as_str())), json!(work));
    say(2, "tool please")?;
    let stepped = s.eventually(WAKE, || records(api, &owner, &chat_name, "work").iter().any(|r| r["body"]["kind"] == "turn.step"));
    s.ok("a tool step is a record on work", stepped && s.eventually(WAKE, || agent_replies(&records(api, &owner, &chat_name, "chat"), &identity).len() == 2), "");
    let mut replies_so_far = 2;

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
    s.eventually(WAKE, || agent_replies(&records(api, &owner, &chat_name, "chat"), &identity).len() == replies_so_far);

    // the turn's asker stops it
    let r = say(11, "slow, then stopped")?;
    let stopped_seq = r.body["record"]["seq"].as_i64().unwrap_or(0);
    let turn = turn_of(&agent_name, &chat_name, "chat", stopped_seq);
    let stop = json!({ "id": "stop-1", "body": { "kind": "stop", "turn": turn } });
    let r = api.signed(&owner, "POST", &format!("/api/f/{chat_name}/channels/chat"), Some(&stop))?;
    s.ok("its asker posts Stop for the turn", r.status == 200, &r);
    let ended = s.eventually(WAKE, || {
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
    let asked = s.eventually(WAKE, || work_of(&records(api, &owner, &chat_name, "work"), &turn).iter().any(|r| r["body"]["kind"] == "turn.prompt"));
    let work = work_of(&records(api, &owner, &chat_name, "work"), &turn);
    let card = work.iter().find(|r| r["body"]["kind"] == "turn.prompt").cloned().unwrap_or_default();
    let prompt = card["body"]["prompt"].as_str().unwrap_or("").to_string();
    s.ok("a prompt is a card on work, asking the agent's owner", asked && card["body"]["asks"] == api.identity(&owner)?.as_str(), &card);
    let answer = json!({ "id": format!("pr:{prompt}"), "body": { "kind": "prompt_response", "prompt": prompt, "option": "once" } });
    let r = api.signed(&owner, "POST", &format!("/api/f/{chat_name}/channels/chat"), Some(&answer))?;
    s.ok("its owner answers it", r.status == 200, &r);
    let approved = s.eventually(WAKE, || agent_replies(&records(api, &owner, &chat_name, "chat"), &identity).iter().any(|r| r["body"]["turn"] == turn.as_str() && r["body"]["text"].as_str().is_some_and(|t| t.contains("(approved)"))));
    let closed = work_of(&records(api, &owner, &chat_name, "work"), &turn).iter().any(|r| r["body"]["kind"] == "turn.prompt.closed" && r["body"]["outcome"] == "answered");
    s.ok("the turn goes on approved, the card closed", approved && closed, json!(work_of(&records(api, &owner, &chat_name, "work"), &turn)));
    replies_so_far += 1;

    // a reply carries a file, one of the chat's blobs
    let r = say(13, "draw me something")?;
    let turn = turn_of(&agent_name, &chat_name, "chat", r.body["record"]["seq"].as_i64().unwrap_or(0));
    let drew = s.eventually(WAKE, || agent_replies(&records(api, &owner, &chat_name, "chat"), &identity).iter().any(|r| r["body"]["turn"] == turn.as_str() && r["body"]["attachments"][0]["sha256"].is_string()));
    let file = agent_replies(&records(api, &owner, &chat_name, "chat"), &identity).into_iter().find(|r| r["body"]["turn"] == turn.as_str()).map(|r| r["body"]["attachments"][0].clone()).unwrap_or_default();
    let sha = file["sha256"].as_str().unwrap_or("");
    let blob = api.signed(&owner, "GET", &format!("/api/f/{chat_name}/blobs/{sha}"), None)?;
    s.ok("a reply carries a file, uploaded as the chat's blob", drew && blob.status == 200 && blob.text.contains("a drawing for"), format!("{file} {}", blob.status));
    replies_so_far += 1;
    s.eventually(WAKE, || agent_replies(&records(api, &owner, &chat_name, "chat"), &identity).len() == replies_so_far);

    // the swap: the guest's request to a connection's host, with its placeholder
    let fetched = |s: &Suite, n: u32, text: &str| -> Result<String> {
        let r = say(n, text)?;
        let turn = turn_of(&agent_name, &chat_name, "chat", r.body["record"]["seq"].as_i64().unwrap_or(0));
        let reply = || agent_replies(&records(api, &owner, &chat_name, "chat"), &identity).into_iter().find(|r| r["body"]["turn"] == turn.as_str());
        s.eventually(WAKE, || reply().is_some());
        Ok(reply().and_then(|r| r["body"]["text"].as_str().map(str::to_string)).unwrap_or_default())
    };
    let gh = format!("http://{SWAP_CONNECTION_HOST}/user with fragment-connection:{SWAP_CONNECTION}");
    let seen_before = s.upstream.seen().len();
    let said = fetched(s, 14, &format!("fetch {gh}"))?;
    replies_so_far += 1;
    s.ok("an agent its owner has not allowed a connection is refused it", said.starts_with("fetched 403") && said.contains("may not use the github connection"), &said);
    s.ok("and nothing reaches the provider", s.upstream.seen().len() == seen_before, json!(s.upstream.seen()));
    let allow = |who: &crate::Keys, list: Value| api.signed(who, "PUT", &format!("/api/computers/{id}/agents/{agent_name}/connections"), Some(&json!({ "connections": list })));
    let r = allow(&stranger, json!([SWAP_CONNECTION]))?;
    s.ok("no one else allows its agents connections", r.status == 404, &r);
    let r = allow(&owner, json!(["notion"]))?;
    s.ok("a connection the deployment does not offer is refused", r.status == 400, &r);
    let r = allow(&owner, json!([SWAP_CONNECTION]))?;
    s.ok("its owner allows the agent the connection", r.status == 200 && r.body["agents"][0]["connections"] == json!([SWAP_CONNECTION]), &r);
    let said = fetched(s, 15, &format!("fetch {gh}"))?;
    replies_so_far += 1;
    s.ok("allowed, but its owner connected no account: refused, saying so", said.starts_with("fetched 403") && said.contains("not_connected") && said.contains("connect github first"), &said);
    s.workos.connect(&Api::email_of(&owner), SWAP_CONNECTION, true);
    let said = fetched(s, 16, &format!("fetch {gh}"))?;
    replies_so_far += 1;
    let tokens = s.workos.tokens(SWAP_CONNECTION);
    let seen = s.upstream.seen().last().cloned().unwrap_or_default();
    s.ok(
        "connected, the provider gets the owner's token from Pipes in the placeholder's place",
        said.starts_with("fetched 200") && tokens.len() == 1 && seen["host"] == SWAP_CONNECTION_HOST && seen["auth"]["authorization"] == format!("Bearer {}", tokens[0]),
        json!({ "said": said, "seen": seen, "tokens": tokens }),
    );
    s.ok("and never the agent's header", seen["agent"].is_null(), &seen);
    let said = fetched(s, 17, &format!("fetch http://{SWAP_CONNECTION_HOST}/redirect with fragment-connection:{SWAP_CONNECTION}"))?;
    replies_so_far += 1;
    s.ok("a token is held until shortly before it expires", s.workos.tokens(SWAP_CONNECTION).len() == 1, json!(s.workos.tokens(SWAP_CONNECTION)));
    s.ok("a provider's redirect is the guest's to follow, never followed with the token", said.starts_with("fetched 302"), &said);
    let seen_before = s.upstream.seen().len();
    let said = fetched(s, 18, &format!("fetch http://{SWAP_KEY_HOST}/q with fragment-connection:{SWAP_CONNECTION}"))?;
    replies_so_far += 1;
    s.ok(
        "a placeholder sent to a host that is not its credential's is refused",
        said.starts_with("fetched 403") && said.contains("is for api.github.test") && s.upstream.seen().len() == seen_before,
        &said,
    );
    let said = fetched(s, 19, &format!("fetch http://{SWAP_KEY_HOST}/q with fragment-key:{SWAP_KEY} in x-api-key"))?;
    replies_so_far += 1;
    let seen = s.upstream.seen().last().cloned().unwrap_or_default();
    s.ok(
        "an operator key is swapped into the header that named it",
        said.starts_with("fetched 200") && seen["host"] == SWAP_KEY_HOST && seen["auth"]["x-api-key"] == SWAP_KEY_VALUE,
        json!({ "said": said, "seen": seen }),
    );
    let owner_id = api.identity(&owner)?;
    let keyed = s.eventually(Duration::from_secs(20), || entries(api, &owner_id, &format!("key:{id}:")).len() == 1);
    let rows = entries(api, &owner_id, &format!("key:{id}:"));
    s.ok(
        "its call is metered to the agent's owner, as the agent, at the key's price and the margin",
        keyed && rows[0]["entry"]["row"]["usage"] == json!({ "kind": "key", "key": SWAP_KEY, "units": 1 }) && rows[0]["entry"]["row"]["agent"] == identity.as_str()
            && rows[0]["entry"]["charge"].as_i64().is_some_and(|c| c > SWAP_KEY_MICROS),
        json!(rows),
    );
    let said = fetched(s, 22, &format!("fetch http://{SWAP_KEY_HOST}/plain with nothing-swapped"))?;
    replies_so_far += 1;
    let seen = s.upstream.seen().last().cloned().unwrap_or_default();
    s.ok("a request with no placeholder goes on as it came", said.starts_with("fetched 200") && seen["auth"]["authorization"] == "Bearer nothing-swapped", &seen);

    // the model intercept: the agent's call is the platform's model route, its owner paying
    s.ai.clear_script();
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

    // asleep, a record wakes it, restored, and nothing is answered twice
    std::thread::sleep(QUEUE_DRAIN);
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
    let woke = s.eventually(WAKE, || agent_replies(&records(api, &owner, &chat_name, "chat"), &identity).len() == replies_so_far);
    println!("      (woken and answered in {:.1?})", t0.elapsed());
    let replies = agent_replies(&records(api, &owner, &chat_name, "chat"), &identity);
    s.ok("a record on the chat wakes it, and it answers", woke && replies.last().is_some_and(|r| r["body"]["text"].as_str().is_some_and(|t| t.contains("are you there"))), json!(replies));
    let r = api.signed(&owner, "GET", &format!("/api/computers/{id}"), None)?;
    s.ok("awake again", r.body["phase"] == "awake", &r);
    std::thread::sleep(Duration::from_secs(3));
    let replies = agent_replies(&records(api, &owner, &chat_name, "chat"), &identity);
    s.ok("its restored /data knew what it had answered: nothing twice", replies.len() == replies_so_far, json!(replies));

    // a second agent on the same computer: @mentioned it answers, else the lead
    let maple_name = s.named(api, &owner, "maple")?;
    let maple = s.create(api, &owner, &maple_name)?;
    s.commit(&maple, &[("fragment.json", Some(AGENT_JSON))]);
    s.deploy(&maple);
    let r = api.signed(&owner, "PUT", &format!("/api/computers/{id}/agents/{maple_name}"), Some(&json!({})))?;
    let maple_id = r.body["agents"].as_array().and_then(|a| a.iter().find(|x| x["fragment"] == maple_name.as_str())).and_then(|a| a["identity"].as_str()).unwrap_or("").to_string();
    s.ok("a second agent runs on the same computer", r.status == 200 && maple_id.starts_with("id:") && maple_id != identity, &r);
    api.signed(&owner, "PUT", &format!("/api/f/{chat_name}/members/{maple_id}"), Some(&json!({ "role": "editor" })))?;
    // the guest reads its agents at start: a sleep and a wake follows the new one at once
    api.signed(&owner, "POST", &format!("/api/computers/{id}/sleep"), Some(&json!({})))?;
    api.signed(&owner, "POST", &format!("/api/computers/{id}/wake"), Some(&json!({})))?;
    let maple_label = maple_name.split('.').next().unwrap_or("").to_string();
    say(20, &format!("@{maple_label} what do you think"))?;
    let heard = s.eventually(WAKE, || agent_replies(&records(api, &owner, &chat_name, "chat"), &maple_id).len() == 1);
    s.ok("@mentioned, the second agent answers", heard, json!(agent_replies(&records(api, &owner, &chat_name, "chat"), &maple_id)));
    std::thread::sleep(Duration::from_secs(2));
    let lead_quiet = agent_replies(&records(api, &owner, &chat_name, "chat"), &identity).len() == replies_so_far;
    s.ok("and the lead does not", lead_quiet, json!(agent_replies(&records(api, &owner, &chat_name, "chat"), &identity).len()));
    say(21, "anyone home")?;
    replies_so_far += 1;
    let led = s.eventually(WAKE, || agent_replies(&records(api, &owner, &chat_name, "chat"), &identity).len() == replies_so_far);
    s.ok("unmentioned, the lead answers", led && agent_replies(&records(api, &owner, &chat_name, "chat"), &maple_id).len() == 1, "");

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

    // its image pin: an upgrade at the next wake, then a rollback, its data kept
    let version = || api.call(Call { method: "GET", url: format!("{origin}/p/6080/version.txt"), keys: Some(&owner), ..Call::default() }).map(|r| r.text.trim().to_string()).unwrap_or_default();
    s.ok("it runs its first build", version() == "1", version());
    let r = api.signed(&owner, "PUT", &format!("/api/computers/{id}/image"), Some(&json!({ "image": "no-such-image" })))?;
    s.ok("an image the deployment does not have is refused", r.status == 400, &r);
    let r = api.signed(&owner, "PUT", &format!("/api/computers/{id}/image"), Some(&json!({ "image": "stub-next" })))?;
    s.ok("the next build is pinned", r.status == 200 && r.body["image"] == "stub-next", &r);
    s.ok("it keeps running the build it started with until it sleeps", version() == "1", version());
    std::thread::sleep(QUEUE_DRAIN);
    api.signed(&owner, "POST", &format!("/api/computers/{id}/sleep"), Some(&json!({})))?;
    s.ok("woken, it runs the next build (an upgrade)", version() == "2", version());
    say(30, "after the upgrade")?;
    replies_so_far += 1;
    let kept = s.eventually(WAKE, || agent_replies(&records(api, &owner, &chat_name, "chat"), &identity).len() == replies_so_far);
    s.ok("with its /data restored: it answers anew, and nothing twice", kept, json!(agent_replies(&records(api, &owner, &chat_name, "chat"), &identity).len()));
    let r = api.signed(&owner, "PUT", &format!("/api/computers/{id}/image"), Some(&json!({ "image": "stub" })))?;
    s.ok("the first build is pinned again", r.status == 200 && r.body["image"] == "stub", &r);
    std::thread::sleep(QUEUE_DRAIN);
    api.signed(&owner, "POST", &format!("/api/computers/{id}/sleep"), Some(&json!({})))?;
    s.ok("woken, it runs the first build again (a rollback)", version() == "1", version());

    // a computer's egress alone asks for a wake subscription
    let r = api.signed(&owner, "POST", &format!("/api/f/{chat_name}/subscriptions"), Some(&json!({ "channel": "chat", "wake": true })))?;
    s.ok("a wake subscription from outside a computer is no wake (it names no URL)", r.status == 400, &r);

    // a routine: its agent fragment's cron, on time, wakes it asleep
    s.commit(&agent, &[("app.mjs", Some(routine_app(&chat_name).as_bytes())), ("fragment.json", Some(ROUTINE_JSON))]);
    s.deploy(&agent);
    std::thread::sleep(QUEUE_DRAIN);
    let r = api.signed(&owner, "POST", &format!("/api/computers/{id}/sleep"), Some(&json!({})))?;
    s.ok("asleep, it waits for its routine", r.body["phase"] == "asleep", &r);
    let routine = |recs: &[Value]| agent_replies(recs, &identity).iter().any(|r| r["body"]["text"].as_str().is_some_and(|t| t.contains("water the plants")));
    let ran = s.eventually(Duration::from_secs(150), || routine(&records(api, &owner, &chat_name, "chat")));
    s.ok("its cron's routine wakes it, and the agent does it in the chat", ran, "");
    api.signed(&owner, "POST", &format!("/api/f/{agent_name}/pause"), Some(&json!({ "op": "routine", "paused": true })))?;
    let routines = agent_replies(&records(api, &owner, &chat_name, "chat"), &identity).len();

    // the platform crashes while it is awake: a new isolate takes the
    // computer over (lesson 6), and it answers what comes next, once
    api.signed(&owner, "POST", &format!("/api/computers/{id}/wake"), Some(&json!({})))?;
    s.crash()?;
    let api = s.start(false, true)?;
    let r = api.signed(&owner, "POST", &format!("/api/f/{chat_name}/channels/chat"), Some(&json!({ "id": "m40", "body": { "text": "after the crash" } })))?;
    s.ok("after a crash of the platform, a message to the chat", r.status == 200, &r);
    let after = s.eventually(WAKE, || agent_replies(&records(&api, &owner, &chat_name, "chat"), &identity).len() == routines + 1);
    std::thread::sleep(Duration::from_secs(2));
    let replies = agent_replies(&records(&api, &owner, &chat_name, "chat"), &identity);
    s.ok(
        "the computer answers it, once",
        after && replies.len() == routines + 1 && replies.last().is_some_and(|r| r["body"]["text"].as_str().is_some_and(|t| t.contains("after the crash"))),
        json!(replies.len()),
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
