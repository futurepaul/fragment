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
//! `work`. Put to sleep, a record on the chat wakes it, its `/data`
//! restored, and nothing is answered twice. Its ports answer its owner on
//! its own origin through a one-time ticket, and no one else.

use std::time::Duration;

use anyhow::Result;
use serde_json::{json, Value};

use super::jobs::records;
use crate::api::{Api, Call};
use crate::Suite;

const CHAT_JSON: &[u8] = br#"{ "channels": { "chat": { "read": "public", "post": "viewer" }, "work": { "read": "viewer", "post": "editor" } } }"#;
const AGENT_JSON: &[u8] = br#"{ "channels": { "tasks": { "read": "editor", "post": "editor" } } }"#;
/// A start of the stub, its restore, and its bridge's first follow: well
/// under this on any machine that built the image.
const WAKE: Duration = Duration::from_secs(90);

fn agent_replies(recs: &[Value], agent: &str) -> Vec<Value> {
    recs.iter().filter(|r| r["principal"] == agent && r["body"]["turn"].is_string() && r["body"]["text"].is_string()).cloned().collect()
}

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

    // asleep, a record wakes it, restored, and nothing is answered twice
    let r = api.signed(&owner, "POST", &format!("/api/computers/{id}/sleep"), Some(&json!({})))?;
    s.ok("its owner puts it to sleep", r.status == 200 && r.body["phase"] == "asleep", &r);
    let t0 = std::time::Instant::now();
    say(3, "are you there")?;
    let woke = s.eventually(WAKE, || agent_replies(&records(api, &owner, &chat_name, "chat"), &identity).len() == 3);
    println!("      (woken and answered in {:.1?})", t0.elapsed());
    let replies = agent_replies(&records(api, &owner, &chat_name, "chat"), &identity);
    s.ok("a record on the chat wakes it, and it answers", woke && replies.last().is_some_and(|r| r["body"]["text"].as_str().is_some_and(|t| t.contains("are you there"))), json!(replies));
    let r = api.signed(&owner, "GET", &format!("/api/computers/{id}"), None)?;
    s.ok("awake again", r.body["phase"] == "awake", &r);
    std::thread::sleep(Duration::from_secs(3));
    let replies = agent_replies(&records(api, &owner, &chat_name, "chat"), &identity);
    s.ok("its restored /data knew what it had answered: nothing twice", replies.len() == 3, json!(replies));

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

    // its image pin
    let r = api.signed(&owner, "PUT", &format!("/api/computers/{id}/image"), Some(&json!({ "image": "no-such-image" })))?;
    s.ok("an image the deployment does not have is refused", r.status == 400, &r);
    let r = api.signed(&owner, "PUT", &format!("/api/computers/{id}/image"), Some(&json!({ "image": "stub" })))?;
    s.ok("one it has is pinned", r.status == 200 && r.body["image"] == "stub", &r);

    // a computer's egress alone asks for a wake subscription
    let r = api.signed(&owner, "POST", &format!("/api/f/{chat_name}/subscriptions"), Some(&json!({ "channel": "chat", "wake": true })))?;
    s.ok("a wake subscription from outside a computer is no wake (it names no URL)", r.status == 400, &r);
    let r = api.signed(&owner, "POST", &format!("/api/computers/{id}/sleep"), Some(&json!({})))?;
    s.ok("it sleeps at the end", r.body["phase"] == "asleep", &r);
    Ok(())
}
