//! One-click fragments (docs/phase-6.md, step 2): a create from one of the
//! platform's templates, the server-side commit and deploy routes (an
//! agent's tools use them too), the platform's "new" page, and the
//! `fragments` capability, which only the fragment's owner is granted.

use std::time::Duration;

use anyhow::{Context, Result};
use fragment_core::blob::sha256_hex;
use fragment_core::npub;
use fragment_fakes::openrouter::Reply as Say;
use fragment_nip98::Keys;
use fragment_proto::ErrorCode;
use serde_json::{json, Value};

use super::builder::{stand_in, CUA_TOOLS, CUA_VERSION, PNG};
use super::jobs::{settle, started};
use super::signin::{site_cookie, with_session};
use crate::api::{url_enc, Api, Call, Reply, Socket};
use crate::Suite;

/// A person with a CLI key and a platform session.
pub(super) fn person(api: &Api) -> Result<(Keys, String)> {
    let keys = Keys::generate();
    let session = api.sign_in(&format!("t-{}@e2e.test", &keys.pubkey_hex()[..12]))?;
    api.approve(&session, &keys)?;
    Ok((keys, session))
}

pub(super) fn post_form(api: &Api, path: &str, form: &str, session: &str, origin: &str) -> Result<Reply> {
    api.call(Call {
        method: "POST",
        url: format!("{}{path}", api.base),
        body: Some(form.as_bytes().to_vec()),
        content_type: Some("application/x-www-form-urlencoded"),
        cookie: Some(format!("fragment_session={session}")),
        extra: vec![("origin", origin.to_string())],
        ..Call::default()
    })
}

pub fn templates(s: &mut Suite, api: &Api) -> Result<()> {
    if !s.section("templates") {
        return Ok(());
    }
    let (owner, owner_session) = person(api)?;
    let (editor, editor_session) = person(api)?;
    let viewer = api.person()?;
    let npub_of = |k: &Keys| npub::encode(k.pubkey_hex());

    // a create from a template is a working fragment at once
    let chat = s.named(api, &owner, "tchat")?;
    let r = api.create_with(&owner, json!({ "name": chat, "template": "chat" }))?;
    s.ok(
        "a create from a template answers the fragment, open to whoever holds its link (the template declares no computer)",
        r.status == 200 && r.body["name"] == chat.as_str() && r.body["visibility"] == "link",
        &r,
    );
    s.hook(api, &r.body);
    let chat_cookie = format!("fragview={}", r.body["viewToken"].as_str().unwrap_or(""));
    let st = api.status(&owner, &chat)?;
    s.ok("its template is main's first commit, and live", st.body["pins"]["live"].is_string() && st.body["pins"]["live"] == st.body["pins"]["main"], &st);
    let channels = api.signed(&owner, "GET", &format!("/api/f/{chat}/channels"), None)?;
    let declared = |n: &str| channels.body["channels"].as_array().into_iter().flatten().find(|c| c["name"] == n).map(|c| (c["read"].clone(), c["post"].clone()));
    s.ok(
        "a chat is its channels: viewers post to chat, and read the agent's work, which editors post",
        declared("chat") == Some((json!("public"), json!("viewer"))) && declared("work") == Some((json!("viewer"), json!("editor"))),
        &channels,
    );
    let m = api.signed(&owner, "GET", &format!("/api/f/{chat}/manifest"), None)?;
    s.ok("its fragment.json carries the fragment's own name", m.body["name"] == chat.as_str(), &m);
    let page = api.page(&chat, "", Some(&chat_cookie))?;
    s.ok("its site serves the template's page", page.status == 200 && page.text.contains("<title>Chat"), &page);
    // a chat made from the template has its owner's own agent in it
    let members = api.signed(&owner, "GET", &format!("/api/f/{chat}/members"), None)?;
    let agent = members.body["members"].as_array().into_iter().flatten().find(|m| m["kind"] == "agent").cloned().unwrap_or_default();
    let owner_id = api.identity(&owner)?;
    s.ok("a chat from the template has its owner's agent in it, as an editor", agent["role"] == "editor" && agent["owner"] == owner_id.as_str(), &members);
    let listening = |subs: &Reply| subs.body["subscriptions"].as_array().map(|a| a.iter().filter(|x| x["principal"] == agent["principal"] && x["channel"] == "chat").count());
    let subs = api.signed(&owner, "GET", &format!("/api/f/{chat}/subscriptions"), None)?;
    s.ok("listening to the chat, once", listening(&subs) == Some(1), &subs);
    let agent_name = api.qualified(&owner, "agent")?;
    let mine = api.signed(&owner, "GET", "/api/a/agent", None)?;
    s.ok("it is agent.<username>, made on first need", mine.status == 200 && mine.body["name"] == agent_name.as_str(), &mine);
    // a join that did not finish is retried by the chat's alarm: the same
    // listen, sent again as its owner, leaves the one subscription
    let again = api.signed(&owner, "POST", &format!("/api/a/{agent_name}/listen"), Some(&json!({ "fragment": chat })))?;
    let subs = api.signed(&owner, "GET", &format!("/api/f/{chat}/subscriptions"), None)?;
    s.ok("and a join sent again leaves the one subscription", again.status == 200 && listening(&subs) == Some(1), json!({ "listen": again.body, "subscriptions": subs.body }));
    // a message is a post; the model calls a `say` that is not there (as a
    // chat made before this had), then answers: one answer lands
    s.openrouter.clear_script();
    let say = fragment_core::tools::tool_name(&chat, "say").expect("a tool name");
    s.openrouter.script(&[Say::Tools(vec![(say, json!({ "text": "Hello from the tool." }))]), Say::Text("Hello! I'm here.".into())]);
    let said = api.signed(&owner, "POST", &format!("/api/f/{chat}/channels/chat"), Some(&json!({ "id": "t1", "body": { "text": "hello from a template" } })))?;
    s.ok("a message is posted to its chat channel", said.status == 200 && said.body["record"]["principal"] == owner_id.as_str(), &said);
    let agents_records = || -> Vec<Value> {
        let records = api.signed(&owner, "GET", &format!("/api/f/{chat}/channels/chat"), None).ok().and_then(|r| r.body["records"].as_array().cloned()).unwrap_or_default();
        records.into_iter().filter(|x| x["principal"] == agent["principal"]).collect()
    };
    let answered = s.eventually(Duration::from_secs(30), || agents_records().iter().any(|x| x["body"]["text"] == "Hello! I'm here."));
    // the answer comes after anything the turn did, so it is all there now
    let records = agents_records();
    s.ok(
        "and the agent answers in the chat, once, naming its turn",
        answered && records.len() == 1 && records[0]["body"]["turn"].as_str().is_some_and(|t| t.len() == 24),
        json!(records),
    );
    // a chat has no app code: no worker, however it is used
    let st = api.status(&owner, &chat)?;
    let builds = api.unsigned("POST", "/api/test/fragment", Some(&json!({ "fragment": chat, "op": "code-builds" })))?;
    s.ok(
        "a new chat has no app code, and after a message and an answer it has loaded no worker",
        st.body["code"]["sha"].is_null() && builds.status == 200 && builds.body["builds"] == 0,
        format!("{} {}", st.body["code"], builds.body),
    );
    let people = api.page(&chat, &format!("__people?id={}&id={}&id=anon:00", agent["principal"].as_str().unwrap_or(""), owner_id), Some(&chat_cookie))?;
    let username = api.username(&owner)?;
    let profiles = &people.body["profiles"];
    s.ok(
        "its page can name who is in it: a person by username, an agent as its owner's",
        profiles[owner_id.as_str()]["username"] == username.as_str()
            && profiles[agent["principal"].as_str().unwrap_or("")]["kind"] == "agent"
            && profiles[agent["principal"].as_str().unwrap_or("")]["username"] == username.as_str()
            && profiles.get("anon:00").is_none(),
        &people,
    );
    let r = api.create_with(&owner, json!({ "name": s.name("tchat2"), "template": "chat" }))?;
    let second = r.body["name"].as_str().unwrap_or("").to_string();
    let members = api.signed(&owner, "GET", &format!("/api/f/{second}/members"), None)?;
    s.ok(
        "a second chat has the same agent",
        members.body["members"].as_array().is_some_and(|a| a.iter().any(|m| m["principal"] == agent["principal"])),
        &members,
    );

    let none = s.name("tnone");
    let r = api.create_with(&owner, json!({ "name": none, "template": "nope" }))?;
    s.ok("an unknown template is refused, naming the templates", r.status == 400 && r.message().contains("blank, todo, inbox, calories, pet, builder, chat, desktop"), &r);
    let r = api.status(&owner, &api.qualified(&owner, &none)?)?;
    s.ok("and nothing is made", r.status == 404, &r);

    // the files and deploy routes
    let blank = s.named(api, &owner, "tblank")?;
    let r = api.create_with(&owner, json!({ "name": blank, "template": "blank" }))?;
    s.hook(api, &r.body);
    let blank_cookie = format!("fragview={}", r.body["viewToken"].as_str().unwrap_or(""));
    for (k, role) in [(&editor, "editor"), (&viewer, "viewer")] {
        let r = api.signed(&owner, "PUT", &format!("/api/f/{blank}/members/{}", npub_of(k)), Some(&json!({ "role": role })))?;
        anyhow::ensure!(r.status == 200, "adding a {role}: {r}");
    }
    let page = |api: &Api| api.page(&blank, "", Some(&blank_cookie)).map(|r| r.text).unwrap_or_default();
    let files = |k: &Keys, body: Value| api.signed(k, "POST", &format!("/api/f/{blank}/files"), Some(&body));
    let write = json!({ "files": [
        { "path": "site/index.html", "text": "<h1>made through the api</h1>" },
        { "path": "site/extra.txt", "text": "extra" },
    ], "message": "from the files route", "key": "w1" });
    let r = files(&viewer, write.clone())?;
    s.ok("a viewer cannot write files", r.status == 403, &r);
    let wrote = files(&editor, write.clone())?;
    s.ok("an editor commits files to main", wrote.status == 200 && wrote.body["commit"].is_string(), &wrote);
    let again = files(&editor, write)?;
    s.ok("the same key commits nothing twice", again.status == 200 && again.body["commit"] == wrote.body["commit"], &again);
    let st = api.status(&owner, &blank)?;
    s.ok("main moves at once, live stays", st.body["pins"]["main"] == wrote.body["commit"] && st.body["pins"]["live"] != st.body["pins"]["main"], &st);
    s.ok("the site still serves live", !page(api).contains("made through the api"), page(api));
    let r = files(&editor, json!({ "files": [{ "path": "../escape", "text": "x" }] }))?;
    s.ok("a path outside the repo is refused", r.status == 400, &r);
    let many: Vec<Value> = (0..17).map(|i| json!({ "path": format!("f/{i}.txt"), "text": "x" })).collect();
    let r = files(&editor, json!({ "files": many }))?;
    s.ok("more files than one write takes are refused", r.status == 400, &r);

    let deploy = |k: &Keys| api.signed(k, "POST", &format!("/api/f/{blank}/deploy"), Some(&json!({ "note": "from the e2e" })));
    let r = deploy(&viewer)?;
    s.ok("a viewer cannot deploy", r.status == 403, &r);
    let r = deploy(&editor)?;
    s.ok("an editor deploys: live is main's tip", r.status == 200 && r.body["live"] == st.body["pins"]["main"], &r);
    s.ok("the site serves the deploy", page(api).contains("made through the api"), page(api));
    let r = files(&editor, json!({ "files": [{ "path": "site/extra.txt", "delete": true }] }))?;
    let gone = api.signed(&owner, "GET", &format!("/api/f/{blank}/file?path=site/extra.txt"), None)?;
    s.ok("a write can remove a file", r.status == 200 && gone.status == 404, &gone);

    // the fragments capability: the owner's own fragments, to the owner
    let dash = s.named(api, &owner, "tdash")?;
    let r = api.create_with(&owner, json!({ "name": dash, "template": "blank" }))?;
    s.hook(api, &r.body);
    let manifest = json!({ "name": dash, "capabilities": ["fragments"] }).to_string();
    let r = api.signed(&owner, "POST", &format!("/api/f/{dash}/files"), Some(&json!({ "files": [{ "path": "fragment.json", "text": manifest }] })))?;
    anyhow::ensure!(r.status == 200, "asking for the capability: {r}");
    let r = api.signed(&owner, "POST", &format!("/api/f/{dash}/deploy"), None)?;
    anyhow::ensure!(r.status == 200, "deploying it: {r}");
    let r = api.signed(&owner, "PUT", &format!("/api/f/{dash}/members/{}", npub_of(&editor)), Some(&json!({ "role": "editor" })))?;
    anyhow::ensure!(r.status == 200, "adding an editor: {r}");
    let listed = |cookie: Option<String>, name: &str| {
        api.call(Call { method: "GET", url: api.site_url(name, "__fragments"), cookie: cookie.map(|c| format!("fragment_site={c}")), ..Call::default() })
    };
    let owner_site = site_cookie(api, &owner_session, &dash)?;
    let r = listed(Some(owner_site), &dash)?;
    let names: Vec<&str> = r.body["fragments"].as_array().into_iter().flatten().filter_map(|f| f["name"].as_str()).collect();
    s.ok("the owner, on a page that asks, lists their fragments", r.status == 200 && [&chat, &blank, &dash].iter().all(|n| names.contains(&n.as_str())), &r);
    let url = r.body["fragments"].as_array().into_iter().flatten().find(|f| f["name"] == chat.as_str()).map(|f| f["url"].clone()).unwrap_or_default();
    s.ok("each with its URL", url == api.site_url(&chat, "").as_str(), &url);
    let r = listed(Some(site_cookie(api, &editor_session, &dash)?), &dash)?;
    s.ok("an editor of that page is refused", r.status == 403, &r);
    let r = listed(None, &dash)?;
    s.ok("so is someone signed out", r.status == 403 || r.status == 401, &r);
    let owner_on_blank = site_cookie(api, &owner_session, &blank)?;
    let r = listed(Some(owner_on_blank), &blank)?;
    s.ok("a page that does not ask is refused, even to its owner", r.status == 403, &r);
    let r = listed(Some(site_cookie(api, &owner_session, &dash)?), &dash)?;
    s.ok("and the page whether it may frame them (null: it does not ask)", r.status == 200 && r.body["frame"].is_null(), &r);
    // a desktop made through a page's __fragments: its owner's alone, and no
    // grant (a page's code never gives one)
    let desk_label = s.name("tdesk");
    let made = api.call(Call {
        method: "POST",
        url: api.site_url(&dash, "__fragments"),
        body: Some(json!({ "label": desk_label, "template": "desktop" }).to_string().into_bytes()),
        content_type: Some("application/json"),
        cookie: Some(format!("fragment_site={}", site_cookie(api, &owner_session, &dash)?)),
        ..Call::default()
    })?;
    let desk = api.qualified(&owner, &desk_label)?;
    let st = api.status(&owner, &desk)?;
    s.ok(
        "a desktop made through __fragments is its owner's alone (members only), and may not frame their fragments until they allow it",
        made.status == 200 && made.body["name"] == desk.as_str() && st.body["visibility"] == "members" && st.body["frame"] == json!(false),
        format!("{made} / {st}"),
    );

    // the platform's home: the person's fragments, and making one
    let home = with_session(api, "GET", "/", &owner_session)?;
    let row = |page: &Reply, name: &str| page.text.split("<li>").find(|li| li.contains(&format!("/share/{name}\""))).unwrap_or_default().to_string();
    s.ok(
        "the platform's home lists the person's fragments: each one's link, who may open it, and its share sheet",
        home.text.contains("Your fragments")
            && row(&home, &chat).contains(&format!("href=\"{}\"", api.site_url(&chat, "")))
            && row(&home, &chat).contains("yours · anyone with the link")
            && row(&home, &desk).contains("yours · only the people in it"),
        &home,
    );
    let theirs = with_session(api, "GET", "/", &editor_session)?;
    s.ok("and says which are shared with them, and as what", row(&theirs, &blank).contains("shared with you · editor"), &theirs);
    let offered: Vec<usize> = ["blank", "todo", "inbox", "calories", "pet", "builder", "chat", "desktop"].iter().filter_map(|t| home.text.find(&format!("value=\"{t}\""))).collect();
    s.ok(
        "and offers the templates, the simplest first and the desktop last, as the demo it is, saying it will show their fragments inside it",
        home.text.contains("New fragment")
            && offered.len() == 8
            && offered.is_sorted()
            && home.text.contains("A demo of what fragments can do")
            && home.text.contains("It will show your fragments inside it, signed in as you"),
        &home,
    );
    let label = s.name("tnew");
    let r = post_form(api, "/auth/new", &format!("label={}&template=desktop", url_enc(&label)), &owner_session, &api.site_origin(&dash))?;
    let st = api.status(&owner, &api.qualified(&owner, &label)?)?;
    s.ok("a form from another origin (a fragment's page) is refused, and makes nothing", r.status == 403 && st.status == 404, format!("{r} / {st}"));
    let r = post_form(api, "/auth/new", &format!("label={}&template=todo", url_enc(&label)), &owner_session, &api.base)?;
    let name = api.qualified(&owner, &label)?;
    s.ok("the form makes it and walks to its sign-in", r.status == 302 && r.header("location") == format!("/auth/fragment?name={}&return=/", url_enc(&name)), &r);
    let r = api.page(&name, "", Some(&format!("fragment_site={}", site_cookie(api, &owner_session, &name)?)))?;
    s.ok("the new fragment serves its template to its owner", r.status == 200 && r.text.contains("<title>Todo"), &r);
    let r = post_form(api, "/auth/new", &format!("label={}&template=todo", url_enc(&label)), &owner_session, &api.base)?;
    s.ok("a label already taken says so", r.status == 400 && r.text.contains("already exists"), &r);
    let st = api.status(&owner, &name)?;
    s.ok("(a todo made there opens to anyone with its link, as before)", st.body["visibility"] == "link", &st);
    let desk_label = s.name("tnewdesk");
    let r = post_form(api, "/auth/new", &format!("label={}&template=desktop", url_enc(&desk_label)), &owner_session, &api.base)?;
    let st = api.status(&owner, &api.qualified(&owner, &desk_label)?)?;
    s.ok(
        "a desktop made there is its owner's alone (members only), and may frame their fragments: the form's submit is their grant",
        r.status == 302 && st.body["visibility"] == "members" && st.body["frame"] == json!(true),
        format!("{r} / {st}"),
    );
    pet(s, api, &owner, &chat)
}

/// A stand-in for Stagehand 4.1.0, where the pet's `do` installs the real
/// one: no browser, and act, observe, and extract each ask the model once
/// through the `generate` callback, as Stagehand's runtime does, with the
/// page as its accessibility tree.
const STAGEHAND: &str = r#"const TREE = "[0-2] RootWebArea: Milk\n  [0-9] StaticText: Milk costs $3.49\n  [0-16] textbox: New todo\n  [0-21] button: Add";
const ID = { type: "string", pattern: "^\\d+-\\d+$" };
let [at, cdp] = ["about:blank", ""];
const page = { goto: async (url) => void (at = url), url: async () => at, title: async () => `Milk, on the Chrome at ${cdp}`, screenshot: async () => new Uint8Array([255, 216, 255]) };
export const localBrowser = { connect: async ({ cdpUrl }) => ((cdp = cdpUrl), {}) };
export class Stagehand {
  static async create({ model }) {
    return Object.assign(new Stagehand(), { model, browser: { context: { activePage: async () => page } } });
  }
  async ask(name, instruction, properties) {
    const schema = { type: "object", properties, required: Object.keys(properties), additionalProperties: false };
    const messages = [{ role: "user", content: { type: "text", text: `Instruction: ${instruction}\nDOM: ${TREE}` } }];
    return (await this.model.generate({ systemPrompt: `the stand-in's ${name}`, messages, responseFormat: { type: "json_schema", name, schema } })).structuredContent;
  }
  async act(action) {
    const element = { type: "object", properties: { elementId: ID, method: { type: "string" } }, required: ["elementId", "method"] };
    const a = (await this.ask("Act", action, { action: { anyOf: [element, { type: "null" }] }, twoStep: { type: "boolean" } })).action;
    return { data: { success: !!a, message: a ? `${a.method} on ${a.elementId}` : "no such element", actionDescription: action, actions: [] } };
  }
  async observe(about) {
    return { data: (await this.ask("Observation", about ?? "anything", { elements: { type: "array", items: { type: "object", properties: { elementId: ID, description: { type: "string" } } } } })).elements };
  }
  async extract(what) {
    return { data: await this.ask("Extraction", what, { extraction: { type: "string" } }) };
  }
}
"#;

/// The stand-in Stagehand where the pet's `do` installs Stagehand, with the
/// pet's own lockfile beside it, so that nothing is installed.
fn stand_in_stagehand(home: &std::path::Path) -> Result<()> {
    let dir = home.join(".local/share/pet-browser");
    let module = dir.join("node_modules/@browserbasehq/stagehand");
    std::fs::create_dir_all(&module)?;
    std::fs::copy(home.join("fragment/computer/browser/package-lock.json"), dir.join("package-lock.json")).context("the pet's lockfile, synced")?;
    std::fs::write(module.join("package.json"), r#"{"name": "@browserbasehq/stagehand", "type": "module", "exports": "./index.mjs"}"#)?;
    std::fs::write(module.join("index.mjs"), STAGEHAND)?;
    Ok(())
}

/// An 8×5 JPEG, as the pet's computer sends its screen.
const JPEG: &str = "/9j/4AAQSkZJRgABAQAAAQABAAD/2wBDAAYEBQYFBAYGBQYHBwYIChAKCgkJChQODwwQFxQYGBcUFhYaHSUfGhsjHBYWICwgIyYnKSopGR8tMC0oMCUoKSj/2wBDAQcHBwoIChMKChMoGhYaKCgoKCgoKCgoKCgoKCgoKCgoKCgoKCgoKCgoKCgoKCgoKCgoKCgoKCgoKCgoKCgoKCj/wAARCAAFAAgDASIAAhEBAxEB/8QAFQABAQAAAAAAAAAAAAAAAAAAAAb/xAAUEAEAAAAAAAAAAAAAAAAAAAAA/8QAFAEBAAAAAAAAAAAAAAAAAAAABf/EABQRAQAAAAAAAAAAAAAAAAAAAAD/2gAMAwEAAhEDEQA/AIcA2Kf/2Q==";

/// The bytes of a base64 fixture.
fn decoded(b64: &str) -> Vec<u8> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.decode(b64).expect("a fixture decodes")
}

/// The pet (templates/pet): a computer, a `control` channel people drive it
/// through, and its screen, one row its computer stores through `frame` and
/// `screen` answers (the JPEG a blob its page shows, never in the app's
/// database); and its agent, `do`, goose with the pet's browser
/// (its Stagehand MCP server, computer/browser) and Cua Driver as its hands
/// (goose, Stagehand, and Cua Driver are stand-ins here, as in the builder
/// section: this proves the plumbing). Its display loop, Chrome, and the
/// real agent need a real Sprite (docs/computers.md).
fn pet(s: &mut Suite, api: &Api, owner: &Keys, chat: &str) -> Result<()> {
    let viewer = api.person()?;
    let viewer_id = api.identity(&viewer)?;
    let name = s.named(api, owner, "tpet")?;
    let made = api.create_with(owner, json!({ "name": name, "template": "pet" }))?;
    s.hook(api, &made.body);
    let st = api.status(owner, &name)?;
    let m = api.signed(owner, "GET", &format!("/api/f/{name}/manifest"), None)?;
    let ops = &st.body["code"]["operations"];
    s.ok(
        "a pet from its template declares its computer, so it starts its owner's alone (members), and its code installs: frame (ephemeral) for editors, screen for viewers, the run job for editors",
        made.status == 200
            && made.body["visibility"] == "members"
            && m.body["computer"]["start"] == "node computer/pet.mjs"
            && st.body["code"]["error"].is_null()
            && ops["frame"]["role"] == "editor"
            && ops["frame"]["ephemeral"] == true
            && ops["screen"]["kind"] == "query"
            && ops["run"]["kind"] == "job"
            && ops["run"]["role"] == "editor"
            && ops["do"]["kind"] == "job"
            && ops["do"]["role"] == "editor",
        format!("{made} / {st}"),
    );
    let channels = api.signed(owner, "GET", &format!("/api/f/{name}/channels"), None)?;
    let channel = |n: &str| channels.body["channels"].as_array().into_iter().flatten().find(|c| c["name"] == n).cloned().unwrap_or_default();
    let (control, work) = (channel("control"), channel("work"));
    s.ok(
        "and a control channel viewers signed in post to, and a work channel its editors post to",
        control["read"] == "viewer" && control["post"] == "viewer" && control["signedIn"] == true && work["read"] == "viewer" && work["post"] == "editor",
        &channels,
    );
    // shared by its link: who holds it is a viewer, signed in or not
    let r = api.signed(owner, "PUT", &format!("/api/f/{name}/visibility"), Some(&json!({ "visibility": "link" })))?;
    anyhow::ensure!(r.status == 200, "sharing the pet by its link: {r}");
    let link = format!("fragview={}", made.body["viewToken"].as_str().unwrap_or(""));
    let page = api.page(&name, "", Some(&link))?;
    s.ok("its page is the template's", page.status == 200 && page.text.contains("<title>Pet"), &page);

    // driving: someone signed in posts; someone who is not, does not
    let r = api.signed(owner, "PUT", &format!("/api/f/{name}/members/{}", npub::encode(viewer.pubkey_hex())), Some(&json!({ "role": "viewer" })))?;
    anyhow::ensure!(r.status == 200, "adding a viewer: {r}");
    let click = json!({ "kind": "click", "x": 100, "y": 60 });
    let r = api.signed(&viewer, "POST", &format!("/api/f/{name}/channels/control"), Some(&json!({ "id": "p1", "body": click })))?;
    s.ok("a signed-in viewer posts a control record", r.status == 200 && r.body["record"]["principal"] == viewer_id.as_str(), &r);
    let anonymous = |cookie: Option<&str>| {
        let body = json!({ "id": "p2", "input": click }).to_string().into_bytes();
        let url = api.site_url(&name, "__op/channels/control");
        api.call(Call { method: "POST", url, body: Some(body), content_type: Some("application/json"), cookie: cookie.map(str::to_string), ..Call::default() })
    };
    let r = anonymous(None)?;
    s.ok("an anonymous visitor cannot", r.status == 401 || r.status == 403, &r);
    let r = anonymous(Some(&link))?;
    let held = api.signed(owner, "GET", &format!("/api/f/{name}/channels/control"), None)?;
    s.ok(
        "nor can one holding the link, a viewer but anonymous: control is for people signed in (401), and nothing is appended",
        r.code() == Some(ErrorCode::Unauthenticated) && held.body["records"].as_array().map(Vec::len) == Some(1),
        format!("{r} / {held}"),
    );

    // frames: its computer's (an editor here), one row that screen answers
    let mut sprite = String::new();
    s.eventually(Duration::from_secs(30), || {
        let events = api.signed(owner, "GET", &format!("/api/f/{name}/events?tail=50"), None).map(|r| r.body).unwrap_or_default();
        let ready = events["events"].as_array().into_iter().flatten().find(|e| e["kind"] == "computer.ready");
        sprite = ready.and_then(|e| e["data"]["sprite"].as_str()).unwrap_or_default().to_string();
        !sprite.is_empty()
    });
    let computer = s.cli_keys(&s.scratch.join("sprites/sprites").join(&sprite)).context("the pet's computer, paired on its Sprite")?;
    let (jpeg, png) = (decoded(JPEG), decoded(PNG));
    let shot = sha256_hex(&jpeg);
    let frame = |keys: &Keys, id: &str, input: Value| api.signed(keys, "POST", &format!("/api/f/{name}/ops/frame"), Some(&json!({ "id": id, "input": input })));
    let named = |title: &str, driver: Option<&str>| {
        let mut input = json!({ "shot": shot, "size": jpeg.len(), "width": 8, "height": 5, "title": title });
        if let Some(driver) = driver {
            input["driver"] = json!(driver);
        }
        input
    };
    let r = frame(&viewer, "f1", named("a viewer's", None))?;
    s.ok("a viewer cannot store a frame", r.status == 403, &r);
    let r = frame(&computer, "f2", json!({ "jpeg": JPEG, "width": 8, "height": 5 }))?;
    s.ok(
        "nor can anyone send the JPEG itself, as frames were once sent (400: a frame names its blob): the app's database holds no image",
        r.status == 400 && r.message().contains("/shot"),
        &r,
    );
    let stored = [frame(&computer, "f3", named("first", Some(&viewer_id)))?, frame(&computer, "f4", named("Hello from your pet", None))?];
    let screen = api.signed(&viewer, "POST", &format!("/api/f/{name}/ops/screen"), Some(&json!({ "id": "s1", "input": {} })))?;
    let got = &screen.body["result"];
    let mut fields: Vec<&str> = got.as_object().into_iter().flatten().map(|(k, _)| k.as_str()).collect();
    fields.sort();
    s.ok(
        "its computer names frames by their blob, and the live query answers the latest: its hash and size, what is on screen, and who drove it (kept by a frame that names no one), and nothing more",
        stored.iter().all(|r| r.status == 200)
            && got["shot"] == shot.as_str()
            && got["size"] == jpeg.len()
            && got["title"] == "Hello from your pet"
            && got["driver"] == viewer_id.as_str()
            && got["width"] == 8
            && fields == ["at", "driver", "height", "shot", "size", "title", "width"],
        format!("{} / {screen}", stored[1]),
    );

    // its computer runs start from the live files: shown a fixed screen (as
    // off a Sprite), it stores that frame, and follows control
    let mut manifest = m.body.clone();
    manifest["computer"]["start"] = json!("PET_FAKE_SCREEN=computer/screen.jpg node computer/pet.mjs");
    let files = json!({ "files": [{ "path": "fragment.json", "text": manifest.to_string() }, { "path": "computer/screen.jpg", "base64": JPEG }] });
    let page = Socket::open(api, &name, "__live", Some(owner), None)?;
    let wrote = api.signed(owner, "POST", &format!("/api/f/{name}/files"), Some(&files))?;
    let deployed = api.signed(owner, "POST", &format!("/api/f/{name}/deploy"), None)?;
    let screen = || api.op(&viewer, &name, "screen", "s2", json!({})).map(|r| r.body["result"].clone()).unwrap_or_default();
    let shown = s.eventually(Duration::from_secs(60), || screen()["width"] == 1024 && screen()["shot"] == shot.as_str());
    s.ok("its computer runs start from the live files: shown a fixed screen, it stores that frame", wrote.status == 200 && shown, format!("{deployed} / {}", screen()));
    let blob = |fragment: &str, sha: &str, cookie: &str| api.call(Call { method: "GET", url: api.site_url(fragment, &format!("__blob/{sha}")), cookie: Some(cookie.to_string()), ..Call::default() });
    let r = blob(&name, &shot, &link)?;
    s.ok(
        "the frame is a blob of the pet's (the computer uploaded it, a frame), which whoever holds its link reads on its origin as a JPEG",
        r.status == 200 && r.bytes == jpeg && r.header("content-type") == "image/jpeg",
        &r,
    );
    if let Some(mut chrome) = s.browser()? {
        let tab = chrome.open(&api.site_url(&name, &format!("?view={}", made.body["viewToken"].as_str().unwrap_or(""))))?;
        let expr = format!("(() => {{ const i = document.getElementById('frame'); return !i.hidden && i.complete && i.naturalWidth === 8 && i.getAttribute('src') === './__blob/{shot}'; }})()");
        let drawn = chrome.until(&tab, &expr, Duration::from_secs(30));
        chrome.screenshot(&tab, &s.scratch.join("pet-page.png"))?;
        chrome.close(tab)?;
        s.ok("and its page shows it, from `__blob`", drawn, "");
    } else {
        s.ok("Chrome is installed for the pet's page (set CHROME_BIN)", false, "no Chrome found");
    }
    let owner_id = api.identity(owner)?;
    let r = api.signed(owner, "POST", &format!("/api/f/{name}/channels/control"), Some(&json!({ "id": "p3", "body": { "kind": "key", "key": "Enter" } })))?;
    let drove = s.eventually(Duration::from_secs(20), || screen()["driver"] == owner_id.as_str());
    let log = std::fs::read_to_string(s.scratch.join("sprites/sprites").join(&sprite).join("fragment.log")).unwrap_or_default();
    let seq = r.body["record"]["seq"].clone();
    s.ok(
        "and follows control: a signed-in poster's record, taken once, makes them its driver",
        drove && log.matches(&format!("#{seq} {{")).count() == 1,
        &log,
    );

    // run: a command on its computer, answered with what it printed; editors only
    let r = api.op(&viewer, &name, "run", "r1", json!({ "command": "echo nope" }))?;
    s.ok("a viewer cannot run a command on it", r.status == 403, &r);
    let r = api.op(owner, &name, "run", "r2", json!({ "command": "echo ran $((6 * 7)); echo oops >&2; exit 3" }))?;
    let ran = settle(api, owner, &name, started(&r), &["succeeded", "held"], Duration::from_secs(60));
    let out = &ran["output"];
    s.ok(
        "its owner runs a command on it: the run answers the command's code and output",
        ran["status"] == "succeeded" && out["code"] == 3 && out["stdout"] == "ran 42\n" && out["stderr"] == "oops\n" && out["truncated"] == false,
        &ran,
    );

    // its owner's agent runs a command there, asked in their chat: the
    // platform's verbs reach it (the agent is no member of the pet, so no
    // tool of its own names it), as an editor, and a job's call answers
    // what the job answered
    let r = api.signed(owner, "GET", "/api/a/agent/tools", None)?;
    let tools: Vec<&str> = r.body["tools"].as_array().into_iter().flatten().filter_map(|t| t.as_str()).collect();
    s.ok("its owner's agent has the platform's verbs that reach it", tools.contains(&"platform__operations") && tools.contains(&"platform__call"), &r);
    s.openrouter.clear_script();
    s.openrouter.script(&[
        Say::Tools(vec![("platform__operations".into(), json!({ "fragment": name }))]),
        Say::Tools(vec![("platform__call".into(), json!({ "fragment": name, "operation": "run", "input": { "command": "echo ran $((6 * 7))" } }))]),
        Say::Text("Your computer says 42.".into()),
    ]);
    let asked = api.signed(owner, "POST", &format!("/api/f/{chat}/channels/chat"), Some(&json!({ "id": "t-run", "body": { "text": "what is 6 times 7 on my computer?" } })))?;
    let answer = || {
        let records = api.signed(owner, "GET", &format!("/api/f/{chat}/channels/chat"), None).ok().and_then(|r| r.body["records"].as_array().cloned()).unwrap_or_default();
        records.iter().any(|x| x["body"]["text"] == "Your computer says 42.")
    };
    let answered = asked.status == 200 && s.eventually(Duration::from_secs(60), answer);
    let read = s.openrouter.chats().last().map(|c| c["messages"].to_string()).unwrap_or_default();
    s.ok(
        "asked in its owner's chat, their agent finds run on the pet, as an editor, runs a command there, and reads what it printed",
        answered && read.contains(r#"\"role\":\"editor\""#) && read.contains(r#"\"run\":{"#) && read.contains(r#"\"stdout\":\"ran 42\\n\""#),
        &read,
    );

    // do: its agent drives its screen, for editors only: the web through
    // the browser's MCP server (Stagehand's model calls through the hands'
    // endpoint, Jev first), a desktop app through Cua Driver
    let home = s.scratch.join("sprites/sprites").join(&sprite);
    let computer_id = api.identity(&computer)?;
    stand_in(&home, &format!(".local/share/cua-driver-{CUA_VERSION}/cua-driver"), "cua-driver")?;
    stand_in_stagehand(&home)?;
    let r = api.op(&viewer, &name, "do", "d1", json!({ "task": "click it" }))?;
    s.ok("a viewer cannot ask its agent", r.status == 403, &r);
    // each of Jev's calls holds $0.50 until it settles: more than this month has
    let op_session = api.sign_in("operator@e2e.test")?;
    api.approve(&op_session, &s.operator)?;
    let top = api.signed(&s.operator, "POST", &format!("/api/budget/{owner_id}/top-up"), Some(&json!({ "usd": 1.0 })))?;
    anyhow::ensure!(top.status == 200, "topping up the owner's month: {top}");

    // a session made before the hands' tools changed: the pet's own, when
    // its goose config named Cua Driver alone, as it does now but for its
    // tools (get_desktop_state, not launch_app: before the browser), and no
    // browser; the task client run by hand, as `do` runs it
    let cua = home.join(format!(".local/share/cua-driver-{CUA_VERSION}/cua-driver"));
    let bus = format!("DBUS_SESSION_BUS_ADDRESS=unix:path={}", home.join(".pet/bus").display());
    let args = json!(["DISPLAY=:99", bus, "CUA_DRIVER_RS_TELEMETRY_ENABLED=false", "CUA_DRIVER_RS_UPDATE_CHECK=false", cua, "mcp"]);
    let then_tools = ["get_desktop_state", "get_window_state", "list_windows", "click", "type_text", "press_key", "hotkey", "scroll"];
    let old = json!({ "enabled": true, "type": "stdio", "name": "cua", "description": "desktop apps on the pet's screen", "cmd": "/usr/bin/env", "args": args, "timeout": 120, "available_tools": then_tools });
    std::fs::create_dir_all(home.join(".config/goose"))?;
    std::fs::write(home.join(".config/goose/config.yaml"), json!({ "extensions": { "cua": old } }).to_string())?;
    // what the task client said of a session's tools, in hands.log
    let refreshed = || -> Vec<String> {
        let log = std::fs::read_to_string(home.join(".fragment/agent/hands.log")).unwrap_or_default();
        log.lines().filter_map(|l| l.strip_prefix("[task] ")?.split_once(' ').map(|(_, said)| said.to_string())).collect()
    };
    let named = |prefix: &str, tools: &[&str]| tools.iter().map(|t| format!("{prefix}__{t}")).collect::<Vec<_>>();
    let hello = |openrouter: &fragment_fakes::openrouter::OpenRouter, id: &str| -> Result<(Value, Value)> {
        openrouter.clear_script();
        openrouter.script(&[Say::Text("Hello.".into())]);
        let before = openrouter.chats().len();
        let r = api.op(owner, &name, "run", id, json!({ "command": "PROMPT='say hello' node \"$HOME/.fragment/agent/task.mjs\"" }))?;
        let ran = settle(api, owner, &name, started(&r), &["succeeded", "held"], Duration::from_secs(60));
        Ok((ran, openrouter.chats().into_iter().nth(before).unwrap_or_default()))
    };
    let tools_of = |c: &Value| -> Vec<String> {
        let mut tools: Vec<String> = c["tools"].as_array().into_iter().flatten().filter_map(|t| t["function"]["name"].as_str()).map(str::to_string).collect();
        tools.sort();
        tools
    };
    let (ran, then) = hello(&s.openrouter, "r3")?;
    let session = then["session_id"].as_str().unwrap_or_default().to_string();
    let changes = || {
        let store = std::fs::read_to_string(home.join(".local/share/goose/sessions/stand-in.json")).unwrap_or_default();
        serde_json::from_str::<Value>(&store).unwrap_or_default()["sessions"][&session]["changes"].clone()
    };
    let mut had = named("cua", &then_tools);
    had.push("shell".into());
    had.sort();
    s.ok(
        "a session made before a tool change has the tools of then: its shell and those Cua Driver's config named",
        ran["output"]["code"] == 0 && tools_of(&then) == had && !session.is_empty() && changes() == 0 && refreshed().is_empty(),
        json!({ "run": ran, "tools": tools_of(&then), "hands.log": refreshed() }),
    );

    let said = "Milk is $3.49; I put it in the todo box and clicked the app.";
    s.openrouter.clear_script();
    s.openrouter.script(&[
        Say::Tools(vec![("browser__open".into(), json!({ "url": "https://shop.test/milk" }))]),
        Say::Tools(vec![("browser__extract".into(), json!({ "what": "the price of milk" }))]),
        // Stagehand's calls, on flashx: a markdown list, as flashx answered on
        // Paul's pet, read as the extraction
        Say::Text("1. Milk: $3.49".into()),
        Say::Tools(vec![("browser__observe".into(), json!({ "about": "the todo box" }))]),
        // its elements as a bare array, as there, read as the observation
        Say::Text(json!([{ "elementId": "0-16", "description": "the New todo box" }]).to_string()),
        Say::Tools(vec![("browser__act".into(), json!({ "action": "type milk into the todo box" }))]),
        Say::Text(json!({ "action": { "elementId": "0-16", "method": "fill" }, "twoStep": false }).to_string()),
        Say::Tools(vec![("cua__get_window_state".into(), json!({ "pid": 1 }))]),
        Say::Tools(vec![("cua__click".into(), json!({ "x": 10, "y": 20 }))]),
        Say::Text(said.into()),
    ]);
    let before = s.openrouter.chats().len();
    let r = api.op(owner, &name, "do", "d2", json!({ "task": "find the price of milk, add it to my todo, then click the app" }))?;
    let run = started(&r);
    let done = settle(api, owner, &name, run, &["succeeded", "held"], Duration::from_secs(90));
    s.ok(
        "its owner asks its agent: Cua Driver and the browser's server are installed, the hands do the task, and the run answers goose's last words",
        done["status"] == "succeeded" && done["output"]["code"] == 0 && done["output"]["message"] == said,
        &done,
    );
    let chats: Vec<Value> = s.openrouter.chats().into_iter().skip(before).collect();
    let (goose, stagehand): (Vec<&Value>, Vec<&Value>) = chats.iter().partition(|c| c["stream"] == true);
    let offered = goose.first().map(|c| tools_of(c)).unwrap_or_default();
    let mut tools = [named("browser", &["act", "extract", "observe", "open", "screenshot"]), named("cua", &CUA_TOOLS), vec!["shell".into()]].concat();
    tools.sort();
    s.ok(
        "goose's model calls went through the platform, on the hands' model (flashx), offered its shell, the browser's five tools, and the eight Cua Driver tools for desktop apps its config names",
        goose.len() == 7 && goose.iter().all(|c| c["model"] == "z-ai/glm-5.3-flashx") && offered == tools,
        json!(offered),
    );
    let config: Value = serde_json::from_str(&std::fs::read_to_string(home.join(".config/goose/config.yaml")).unwrap_or_default()).unwrap_or_default();
    let but_tools = |x: &Value| -> Value { x.as_object().into_iter().flatten().filter(|(k, _)| *k != "available_tools").map(|(k, v)| (k.clone(), v.clone())).collect() };
    let said_so = format!("{name}/work session {session}: browser added, cua replaced; goose offers the configured tools");
    s.ok(
        "in the session made before the tool change, now with the config's tools: Cua Driver's replaced (the same definition but for its tools), the browser's added, the shell kept, and hands.log names what changed",
        goose.iter().all(|c| c["session_id"] == session.as_str())
            && changes() == 2
            && but_tools(&config["extensions"]["cua"]) == but_tools(&old)
            && config["extensions"]["cua"]["available_tools"] == json!(CUA_TOOLS)
            && refreshed() == [said_so.clone()],
        json!({ "session": session, "changes": changes(), "config": config, "hands.log": refreshed() }),
    );
    let (ran, again) = hello(&s.openrouter, "r4")?;
    s.ok(
        "and its next task changes nothing: the same tools in the same order, so the prefix holds, and hands.log says nothing new",
        ran["output"]["code"] == 0 && again["session_id"] == session.as_str() && goose.first().is_some_and(|c| c["tools"] == again["tools"]) && changes() == 2 && refreshed() == [said_so],
        json!({ "run": ran, "changes": changes(), "hands.log": refreshed() }),
    );
    // what goose was asked: its session's last user message
    let prompt_of = |c: &Value| c["messages"].as_array().into_iter().flatten().rev().find(|m| m["role"] == "user").and_then(|m| m["content"].as_str()).unwrap_or_default().to_string();
    let told = goose.first().map(|c| prompt_of(c)).unwrap_or_default();
    let mut cua_now = CUA_TOOLS.to_vec();
    cua_now.sort();
    let note = format!(
        "Your tools changed since your earlier work here. New browser tools: act, extract, observe, open, screenshot. They drive Chrome through the page itself: use them for anything on a web page. The cua tools are now: {}. They are the desktop tools (Cua): use them only for native apps.\n\n",
        cua_now.join(", ")
    );
    s.ok(
        "the task after the change starts with a note of what changed, from what goose now offers, before the pet's own prompt; a new session's task and the task after are told nothing",
        told.strip_prefix(&note).is_some_and(|t| t.starts_with("You are on a Linux computer") && t.ends_with("then click the app")) && prompt_of(&then) == "say hello" && prompt_of(&again) == "say hello",
        json!({ "told": told, "new": prompt_of(&then), "after": prompt_of(&again) }),
    );
    let text = |c: &Value| c.to_string();
    let (jev, flashx) = (fragment_proto::ROUTER_MODEL, "z-ai/glm-5.3-flashx");
    let models = |calls: &[&Value]| calls.iter().filter_map(|c| c["model"].as_str().map(str::to_string)).collect::<Vec<_>>();
    let schemas = |calls: &[&Value]| {
        let named = |p: &str| ["Extraction", "Observation", "Act"].into_iter().find(|n| p.starts_with(&format!("the stand-in's {n}")));
        calls.iter().filter_map(|c| c["messages"][0]["content"].as_str().and_then(named)).collect::<Vec<_>>()
    };
    // Jev picks its own reasoning effort (and provider: stealth ones too);
    // flashx is asked for JSON mode, from a provider that takes it
    let asked = |calls: &[&Value]| {
        calls.iter().all(|c| {
            let format = match c["model"].as_str() {
                Some(fragment_proto::ROUTER_MODEL) => c["response_format"]["type"] == "json_schema" && c.get("reasoning").is_none() && c["provider"] == json!({ "sort": "latency" }),
                _ => c["response_format"] == json!({ "type": "json_object" }) && c["reasoning"] == json!({ "effort": "low" }) && c["provider"] == json!({ "sort": "latency", "require_parameters": true }),
            };
            format && text(c).contains("in this JSON schema") && text(c).contains("[0-16] textbox: New todo")
        })
    };
    let log = || std::fs::read_to_string(home.join(".fragment/agent/browser.log")).unwrap_or_default();
    let logged = |from: usize| -> Vec<(String, bool)> {
        log().lines().skip(from).filter_map(|l| serde_json::from_str::<Value>(l).ok()).map(|l| (l["model"].as_str().unwrap_or("").to_string(), l["ok"] == true)).collect()
    };
    s.ok(
        "a browser step goes through Stagehand's MCP server to the model, through the hands' endpoint: flashx by default, in JSON mode, told the schema, its markdown and bare array read as Stagehand's shapes, each call in browser.log",
        models(&stagehand) == [flashx; 3] && schemas(&stagehand) == ["Extraction", "Observation", "Act"] && asked(&stagehand) && logged(0) == vec![(flashx.to_string(), true); 3],
        json!({ "asked": stagehand, "log": log() }),
    );
    let images = |c: &Value| c["messages"].as_array().into_iter().flatten().filter_map(|m| m["content"].as_array()).flatten().filter(|p| p["type"] == "image_url").count();
    let seen: Vec<usize> = goose.iter().map(|c| images(c)).collect();
    let answered = |i: usize, what: &str| goose.get(i).is_some_and(|c| text(c).contains(what));
    s.ok(
        "no browser step sends the model a screenshot (goose's calls, and Stagehand's, whose messages are text); the answers came back as text, a bare array of elements as the observation",
        seen.get(..5) == Some(&[0, 0, 0, 0, 0][..])
            && stagehand.iter().all(|c| images(c) == 0 && c["messages"].as_array().into_iter().flatten().all(|m| m["content"].is_string()))
            && answered(1, "on the Chrome at http://127.0.0.1:9222")
            && answered(2, "1. Milk: $3.49")
            && answered(3, "- the New todo box")
            && answered(4, "Done: fill on 0-16"),
        json!(seen),
    );
    let capped = std::fs::read_to_string(home.join(".cua-driver/config.json")).unwrap_or_default();
    s.ok(
        "a desktop app's look (Cua Driver's) reaches the model and stays, its screenshots capped, and Cua Driver drives the pet's display",
        seen.get(5..) == Some(&[1, 1][..])
            && answered(5, &format!("data:image/png;base64,{PNG}"))
            && answered(6, "clicked at 10, 20 on :99")
            && capped.contains("\"max_image_dimension\": 768"),
        json!({ "seen": seen, "cua config": capped }),
    );
    let work = api.signed(owner, "GET", &format!("/api/f/{name}/channels/work"), None)?;
    let records: Vec<Value> = work.body["records"].as_array().into_iter().flatten().filter(|x| x["body"]["run"] == run).cloned().collect();
    let steps: Vec<String> = records.iter().map(|x| format!("{} {}", x["body"]["kind"].as_str().unwrap_or(""), x["body"]["tool"].as_str().unwrap_or(""))).collect();
    let by_computer = records.iter().filter(|x| x["body"]["kind"] == "turn.step").all(|x| x["principal"] == computer_id.as_str());
    s.ok(
        "asked on its page, work has the run's start, each step as its computer posted it, and its end",
        steps == ["start ", "turn.step open", "turn.step extract", "turn.step observe", "turn.step act", "turn.step get_window_state", "turn.step click", "end "]
            && by_computer
            && records.first().is_some_and(|x| x["body"]["asker"] == owner_id.as_str())
            && records.last().is_some_and(|x| x["body"]["message"] == said),
        &work,
    );
    // a step that showed a screenshot (Cua Driver's look) names it as a blob
    // of the fragment its steps go to; no record holds the image
    let shots: Vec<(String, String)> =
        records.iter().filter_map(|x| x["body"]["shot"].as_str().map(|sha| (x["body"]["tool"].as_str().unwrap_or("").to_string(), sha.to_string()))).collect();
    let r = blob(&name, &sha256_hex(&png), &link)?;
    s.ok(
        "a step that showed a screenshot names it (`shot`), a blob of the pet's that its page reads as the PNG, and no record holds the image",
        shots == [("get_window_state".to_string(), sha256_hex(&png))] && r.status == 200 && r.bytes == png && r.header("content-type") == "image/png" && !work.text.contains(PNG),
        json!({ "shots": shots, "blob": r.to_string() }),
    );
    let agent = format!("agent:{owner_id}");
    let drove = s.eventually(Duration::from_secs(20), || screen()["driver"] == agent.as_str());
    let r = api.signed(owner, "POST", &format!("/api/f/{name}/channels/control"), Some(&json!({ "id": "p4", "body": { "kind": "key", "key": "Escape" } })))?;
    let back = r.status == 200 && s.eventually(Duration::from_secs(20), || screen()["driver"] == owner_id.as_str());
    s.ok("its agent is the driver the screen names, for who asked, until a person drives it again", drove && back, screen());

    // Jev first, its one setting (JEV_FIRST) turned on in the pet's own
    // files, as its owner would: Jev asked for the schema, naming no
    // reasoning effort (the fake refuses one, as Jev did) and no provider
    // (its stealth picks too), and its JSON cut short falls back to flashx
    let file = home.join("fragment/computer/browser/browser-mcp.mjs");
    let server = std::fs::read_to_string(&file).context("the pet's browser server, synced")?;
    anyhow::ensure!(server.contains("const JEV_FIRST = false;"), "the browser server's one setting");
    let on = json!({ "files": [{ "path": "computer/browser/browser-mcp.mjs", "text": server.replace("const JEV_FIRST = false;", "const JEV_FIRST = true;") }] });
    let wrote = api.signed(owner, "POST", &format!("/api/f/{name}/files"), Some(&on))?;
    let deployed = api.signed(owner, "POST", &format!("/api/f/{name}/deploy"), None)?;
    let synced = s.eventually(Duration::from_secs(60), || std::fs::read_to_string(&file).is_ok_and(|t| t.contains("const JEV_FIRST = true;")));
    anyhow::ensure!(wrote.status == 200 && synced, "turning Jev on in the pet's files: {wrote} / {deployed}");
    let from = log().lines().count();
    s.openrouter.clear_script();
    s.openrouter.script(&[
        Say::Tools(vec![("browser__extract".into(), json!({ "what": "the price of milk" }))]),
        Say::Text(r#"{"extraction": "$3.4"#.into()),
        Say::Text(json!({ "extraction": "$3.49" }).to_string()),
        Say::Tools(vec![("browser__act".into(), json!({ "action": "click Add" }))]),
        Say::Text(json!({ "action": { "elementId": "0-21", "method": "click" }, "twoStep": false }).to_string()),
        Say::Text("Milk is still $3.49; I clicked Add.".into()),
    ]);
    let before = s.openrouter.chats().len();
    let r = api.op(owner, &name, "do", "d3", json!({ "task": "the price of milk again, then click Add" }))?;
    let done = settle(api, owner, &name, started(&r), &["succeeded", "held"], Duration::from_secs(90));
    let chats: Vec<Value> = s.openrouter.chats().into_iter().skip(before).collect();
    let (turns, stagehand): (Vec<&Value>, Vec<&Value>) = chats.iter().partition(|c| c["stream"] == true);
    s.ok(
        "with Jev first: Jev asked for Stagehand's schema, naming no reasoning effort and no provider, its answer cut short falling back to flashx, the next on Jev; the session's tools unchanged",
        done["output"]["code"] == 0
            && models(&stagehand) == [jev, flashx, jev]
            && schemas(&stagehand) == ["Extraction", "Extraction", "Act"]
            && asked(&stagehand)
            && logged(from) == [(jev.to_string(), false), (flashx.to_string(), true), (jev.to_string(), true)]
            && turns.get(1).is_some_and(|c| text(c).contains("$3.49"))
            && changes() == 2,
        json!({ "run": done, "asked": stagehand, "log": log() }),
    );
    let usage = api.signed(owner, "GET", "/api/budget/usage", None)?;
    let routed = usage.body["usage"].as_array().into_iter().flatten().filter(|u| u["kind"] == "computer.text" && u["fragment"] == computer_id.as_str() && u["model"] == jev).count();
    s.ok("billed to its owner, naming the computer and the router", routed == 2, &usage);

    // its owner's agent hands work to the pet by name: its `do` runs there,
    // and the pet's computer answers in the chat that asked (the agent's
    // turn ends on the hand-off; goose looks at the pet, then answers)
    let dos = || api.signed(owner, "GET", &format!("/api/f/{name}/runs?op=do"), None).map_or(0, |r| r.body["runs"].as_array().map_or(0, Vec::len));
    let (before, sprites) = (dos(), s.sprites.sprites().len());
    s.openrouter.clear_script();
    s.openrouter.script(&[
        Say::Tools(vec![("platform__hand_off".into(), json!({ "task": "click the pet's button", "computer": name }))]),
        Say::Tools(vec![("cua__get_window_state".into(), json!({ "pid": 1 }))]),
        Say::Text("I looked, then I'd click the pet's button.".into()),
    ]);
    api.signed(owner, "POST", &format!("/api/f/{chat}/channels/chat"), Some(&json!({ "id": "t-hand-off", "body": { "text": "have my pet click its button" } })))?;
    let result = || {
        let records = api.signed(owner, "GET", &format!("/api/f/{chat}/channels/chat"), None).ok().and_then(|r| r.body["records"].as_array().cloned()).unwrap_or_default();
        let turn = format!("hand-off:{name}:");
        records.into_iter().find(|x| x["principal"] == computer_id.as_str() && x["body"]["turn"].as_str().is_some_and(|t| t.starts_with(&turn)))
    };
    let landed = s.eventually(Duration::from_secs(90), || result().is_some());
    let said = result().map(|x| x["body"]["text"].to_string()).unwrap_or_default();
    s.ok(
        "handed work by name, the owner's agent picks the pet's do: it runs there, the pet stays, and its computer answers in the chat",
        landed && dos() == before + 1 && said.contains("click the pet") && s.sprites.sprites().len() == sprites,
        json!({ "said": said, "do runs": dos() }),
    );
    // its look is a step in the chat, its screenshot a blob of the chat's
    // (the computer is an editor there), which the chat's page shows small
    let work = api.signed(owner, "GET", &format!("/api/f/{chat}/channels/work"), None)?;
    let step = work.body["records"].as_array().into_iter().flatten().find(|x| x["principal"] == computer_id.as_str() && x["body"]["tool"] == "get_window_state").cloned().unwrap_or_default();
    let token = api.status(owner, chat)?.body["viewToken"].as_str().unwrap_or("").to_string();
    let r = blob(chat, &sha256_hex(&png), &format!("fragview={token}"))?;
    s.ok(
        "a step with a screenshot in the chat names its blob, which whoever reads the chat reads on its origin, and no record holds the image",
        step["body"]["shot"] == sha256_hex(&png).as_str() && r.status == 200 && r.bytes == png && !work.text.contains(PNG),
        json!({ "step": step, "blob": r.to_string() }),
    );
    if let Some(mut chrome) = s.browser()? {
        let tab = chrome.open(&api.site_url(chat, &format!("?view={token}")))?;
        // the turn is over, so its steps are folded: open them
        let expr = format!(
            "(() => {{ const i = document.querySelector('details.tools .step img.shot'); if (!i) return false; i.closest('details').open = true; \
             return i.complete && i.naturalWidth > 0 && i.getAttribute('src') === '__blob/{}' && i.closest('a').target === '_blank'; }})()",
            sha256_hex(&png)
        );
        let drawn = chrome.until(&tab, &expr, Duration::from_secs(30));
        chrome.screenshot(&tab, &s.scratch.join("chat-step-shot.png"))?;
        chrome.close(tab)?;
        s.ok("and the chat's page shows it small in the step, a link to it whole", drawn, "");
    }
    page.close();
    Ok(())
}
