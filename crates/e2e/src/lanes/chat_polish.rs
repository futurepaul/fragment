//! Rendering and history on a quiet chat, so pagination fixtures start no
//! agent turns. Uses real channel writes, blobs, and the released page.
use anyhow::Result;
use serde_json::json;
use crate::api::{Api, Call};
use crate::browser::Browser;
use crate::Suite;
use fragment_nip98::Keys;

pub(super) fn check(s: &mut Suite, api: &Api, owner: &Keys, session: &str, chrome: &mut Browser, agent: &str, label: &str) -> Result<()> {
    let name = s.named(api, owner, "chat-polish")?;
    let made = api.create_with(owner, json!({ "name": name, "template": "chat" }))?;
    anyhow::ensure!(made.status == 200, "making the rendering chat: {made}");
    s.owned(&made.body, owner);
    let mut attachments = vec![];
    for (filename, mime, bytes) in [
        ("clip.mp4", "video/mp4", include_bytes!("../../fixtures/chat-video.mp4").as_slice()),
        ("preview.pdf", "application/pdf", include_bytes!("../../fixtures/chat-preview.pdf").as_slice()),
    ] {
        use sha2::{Digest, Sha256};
        let sha = hex::encode(Sha256::digest(bytes));
        let uploaded = api.call(Call {
            method: "PUT", url: format!("{}/api/f/{name}/blobs/{sha}", api.base),
            body: Some(bytes.to_vec()), content_type: Some(mime), keys: Some(owner), ..Call::default()
        })?;
        anyhow::ensure!(uploaded.status == 200, "uploading {filename}: {uploaded}");
        attachments.push(json!({ "sha256": sha, "type": mime, "size": bytes.len(), "name": filename }));
    }
    let posted = api.signed(owner, "POST", &format!("/api/f/{name}/channels/chat"), Some(&json!({ "id": "media", "body": { "text": "Video and PDF", "attachments": attachments } })))?;
    anyhow::ensure!(posted.status == 200, "posting media: {posted}");
    chrome.set_cookie(&format!("{}/", api.base), "fragment_session", session)?;
    let page = chrome.open(&api.site_url(&name, "__signin?return=/"))?;
    chrome.viewport(&page, 1280, 860, false)?;
    s.ok("a video attachment is a labelled inline player with loaded metadata", super::shows(chrome, &page, "!!document.querySelector('.attachment-video video[controls][aria-label=\"clip.mp4\"]') && document.querySelector('video').readyState >= 1"), "");
    s.ok("a PDF attachment has an inline view and open/download/share actions", super::shows(chrome, &page, "!!document.querySelector('.attachment-pdf[title=\"preview.pdf\"]') && document.querySelectorAll('.attachment-download[download]').length === 2 && document.querySelectorAll('.attachment-share').length === 2"), "");
    let shots = s.dir("chat-polish");
    chrome.screenshot(&page, &shots.join("media-desktop.png"))?;
    chrome.viewport(&page, 375, 812, true)?;
    s.ok("video and PDF attachments fit a phone without sideways page scrolling", chrome.eval(&page, "document.documentElement.scrollWidth <= innerWidth && document.getElementById('scroll').scrollWidth === document.getElementById('scroll').clientWidth")? == true, "");
    chrome.screenshot(&page, &shots.join("media-phone.png"))?;
    // More than the initial 400 records, then a fresh page: the browser's
    // live cursor keeps its end while a separate cursor pages backwards.
    chrome.close(page)?;
    for i in 0..1005 {
        let body = match i {
            0 => json!({ "kind": "turn.start", "turn": "old-progress", "agent": agent }),
            1 => json!({ "kind": "turn.end", "turn": "old-progress", "agent": agent, "outcome": "stopped" }),
            _ => json!({ "kind": "history.marker", "turn": "work-marker", "agent": agent }),
        };
        let r = api.signed(owner, "POST", &format!("/api/f/{name}/channels/work"), Some(&json!({ "id": format!("work-history-{i}"), "body": body })))?;
        anyhow::ensure!(r.status == 200, "posting work history {i}: {r}");
    }
    for i in 0..415 {
        let mut body = json!({ "text": format!("History {i:03}") });
        if i == 414 { body["reply_to"] = json!(2); }
        let r = api.signed(owner, "POST", &format!("/api/f/{name}/channels/chat"), Some(&json!({ "id": format!("history-{i}"), "body": body })))?;
        anyhow::ensure!(r.status == 200, "posting history {i}: {r}");
    }
    let backwards = api.signed(owner, "GET", &format!("/api/f/{name}/channels/chat?before=20&limit=3"), None)?;
    let seqs: Vec<_> = backwards.body["records"].as_array().into_iter().flatten().filter_map(|r| r["seq"].as_i64()).collect();
    s.ok("backwards channel reads are exclusive, nearest first, returned in ascending display order", backwards.status == 200 && seqs == [17, 18, 19] && backwards.body["next"] == 17, &backwards);
    let empty = api.signed(owner, "GET", &format!("/api/f/{name}/channels/chat?before=1"), None)?;
    s.ok("an empty backwards page keeps its cursor", empty.body["records"] == json!([]) && empty.body["next"] == 1, &empty);
    let bad = api.signed(owner, "GET", &format!("/api/f/{name}/channels/chat?after=0&before=20"), None)?;
    let malformed = api.signed(owner, "GET", &format!("/api/f/{name}/channels/chat?before=no"), None)?;
    s.ok("ambiguous and malformed history cursors are refused", bad.status == 400 && malformed.status == 400, format!("{bad} | {malformed}"));
    let page = chrome.open(&api.site_url(&name, ""))?;
    chrome.viewport(&page, 1280, 860, false)?;
    s.ok("the initial backlog is bounded to 400 and offers earlier messages", super::shows(chrome, &page, "document.querySelectorAll('.msg.user').length === 400 && !document.getElementById('earlier').hidden"), "");
    s.ok("a quote outside the initial history has an earlier-message placeholder", chrome.eval(&page, "document.querySelector('.msg[data-seq=\"416\"] .quote')?.disabled === true")? == true, "");
    chrome.eval(&page, "(() => { const s = document.getElementById('scroll'); s.scrollTop = 100; return true; })()")?;
    super::shows(chrome, &page, "document.getElementById('scroll').scrollTop === 100");
    let anchor = chrome.eval(&page, "(() => { const s = document.getElementById('scroll'); const m = [...document.querySelectorAll('.msg')].find(m => m.getBoundingClientRect().bottom > s.getBoundingClientRect().top); window.__historyAnchor = m; window.__historyTop = m.getBoundingClientRect().top; document.getElementById('earlier').click(); return m.dataset.seq; })()")?;
    let loaded = super::shows(chrome, &page, "document.querySelectorAll('.msg.user').length === 416 && !document.getElementById('earlier').disabled");
    let position = chrome.eval(&page, "Math.abs(window.__historyAnchor.getBoundingClientRect().top - window.__historyTop)")?;
    let ordered = chrome.eval(&page, "(() => { const seqs = [...document.querySelectorAll('.msg[data-seq]')].map(m => Number(m.dataset.seq)); return seqs.every((seq, i) => !i || seq > seqs[i-1]); })()")?;
    s.ok("loading earlier messages preserves the visible message, ascending order, and no duplicates", loaded && position.as_f64().is_some_and(|n| n < 2.0) && ordered == true, json!({ "anchor": anchor, "moved": position, "ordered": ordered }));
    s.ok("loading earlier history fills the quote above its message", super::shows(chrome, &page, "document.querySelector('.msg[data-seq=\"416\"] .quote .quote-text')?.textContent === 'History 000' && !document.querySelector('.msg[data-seq=\"416\"] .quote').disabled"), "");
    s.ok("the earlier control disappears at the beginning of retained history", chrome.eval(&page, "document.getElementById('earlier').hidden")? == true, "");
    s.ok("earlier messages also bring their older work records beyond the initial 1000", super::shows(chrome, &page, "!!document.querySelector('.notice.stopped[data-turn=\"old-progress\"]')"), "");
    chrome.screenshot(&page, &shots.join("history-desktop.png"))?;
    s.ok("jump to latest appears while reading earlier messages", super::shows(chrome, &page, "!document.getElementById('latest').hidden"), "");
    for i in 0..2 {
        let r = api.signed(owner, "POST", &format!("/api/f/{name}/channels/chat"), Some(&json!({ "id": format!("new-{i}"), "body": { "text": format!("New message {i}") } })))?;
        anyhow::ensure!(r.status == 200, "posting a new message: {r}");
    }
    s.ok("new live messages are counted without moving the reader", super::shows(chrome, &page, "document.getElementById('latest').textContent.includes('2 new messages')") && chrome.eval(&page, "Math.abs(window.__historyAnchor.getBoundingClientRect().top - window.__historyTop) < 2")? == true, "");
    let command = api.signed(owner, "POST", &format!("/api/f/{name}/channels/chat"), Some(&json!({ "id": "new-command", "body": { "kind": "command", "command": "usage", "args": "", "to": [agent] } })))?;
    s.ok("live command messages also count toward jump to latest", command.status == 200 && super::shows(chrome, &page, "document.getElementById('latest').textContent.includes('3 new messages') && !!document.querySelector('.bubble.command .command-name')"), &command);
    chrome.click(&page, "#latest")?;
    s.ok("jump to latest reaches the end and clears the count", super::shows(chrome, &page, "document.getElementById('latest').hidden && (() => { const s = document.getElementById('scroll'); return s.scrollHeight - s.scrollTop - s.clientHeight < 2; })()"), "");
    // Render the fixture through the released markdown module. This also
    // checks incomplete streamed fences and quotes without HTML parsing.
    let fixture = super::js(include_str!("../../fixtures/chat-markdown.md"));
    chrome.eval(&page, &format!("import('./markdown.js').then(m => {{ const node = document.createElement('div'); node.id = 'markdown-fixture'; node.className = 'msg agent md'; node.append(m.renderMarkdown({fixture})); document.getElementById('messages').replaceChildren(node); return true; }})"))?;
    s.ok("markdown renders headings, strikethrough, tasks, nested lists, blockquotes and rules", chrome.eval(&page, "(() => { const m = document.getElementById('markdown-fixture'); return !!m.querySelector('h1') && !!m.querySelector('del') && m.querySelectorAll('input[type=checkbox][disabled]').length === 2 && !!m.querySelector('input:checked') && !!m.querySelector('ul ul ol') && !!m.querySelector('blockquote h2') && !!m.querySelector('blockquote ul') && !!m.querySelector('hr'); })()")? == true, "");
    s.ok("tables preserve escaped/code pipes and autolinks use safe URLs", chrome.eval(&page, "(() => { const m = document.getElementById('markdown-fixture'); return m.querySelectorAll('tbody tr').length === 2 && m.querySelector('tbody td code').textContent === 'a|b' && m.querySelectorAll('tbody td')[1].textContent === 'a|b' && !!m.querySelector('a[href=\"mailto:paul@example.com\"]') && !!m.querySelector('a[href=\"https://example.com/docs\"]') && m.querySelector('table th:last-child').style.textAlign === 'right'; })()")? == true, "");
    s.ok("raw HTML, script URLs and remote images remain inert text", chrome.eval(&page, "(() => { const m = document.getElementById('markdown-fixture'); return !m.querySelector('img,script,a[href^=\"javascript:\"]') && !window.__injected && m.textContent.includes('<img src=x') && m.textContent.includes('<script>window.__injected'); })()")? == true, "");
    s.ok("fenced code is literal and carries its language label", chrome.eval(&page, "document.querySelector('.md-language').textContent === 'rust' && document.querySelector('pre code').dataset.language === 'rust' && document.querySelector('pre code').textContent.includes('<script>') && document.querySelectorAll('.md-code-copy').length === 2")? == true, "");
    chrome.eval(&page, "(() => { navigator.clipboard.writeText = async text => { window.__codeCopied = text; }; document.querySelector('.md-code-copy').click(); return true; })()")?;
    s.ok("a code block's copy button copies only its literal source", super::shows(chrome, &page, "window.__codeCopied === document.querySelector('pre code').textContent && document.querySelector('.md-code-copy').textContent === 'Copied'"), "");
    let streamed = chrome.eval(&page, "import('./markdown.js').then(m => { const n = document.createElement('div'); n.append(m.renderMarkdown('```js\\n<script>')); const deep = document.createElement('div'); deep.append(m.renderMarkdown('> '.repeat(100) + 'safe')); return n.querySelector('pre code').textContent === '<script>' && !n.querySelector('script') && deep.textContent === '> '.repeat(68) + 'safe'; })")?;
    s.ok("incomplete streamed fences and deeply nested quotes remain safe", streamed == true, "");
    chrome.viewport(&page, 375, 812, true)?;
    chrome.eval(&page, "(() => { const t = document.querySelector('.md-table'); t.querySelector('th').textContent = 'wide '.repeat(80); t.querySelector('table').style.width = '900px'; return true; })()")?;
    s.ok("wide tables scroll within the reply on a phone", chrome.eval(&page, "(() => { const t = document.querySelector('.md-table'); t.scrollLeft = 100; return t.scrollLeft > 0 && t.scrollWidth > t.clientWidth && document.documentElement.scrollWidth <= innerWidth; })()")? == true, "");
    chrome.screenshot(&page, &shots.join("markdown-wide-table-phone.png"))?;
    chrome.eval(&page, "(() => { const t = document.querySelector('.md-table'); t.querySelector('th').textContent = 'Language'; t.querySelector('table').style.removeProperty('width'); t.scrollLeft = 0; document.getElementById('scroll').scrollTop = 0; return true; })()")?;
    chrome.screenshot(&page, &shots.join("markdown-phone.png"))?;
    chrome.viewport(&page, 1280, 860, false)?;
    chrome.eval(&page, "(() => { document.getElementById('scroll').scrollTop = 0; return true; })()")?;
    chrome.screenshot(&page, &shots.join("markdown-desktop.png"))?;
    // Reopen to discard only the markdown fixture. A real network outage
    // exercises the library's retries, then the message's own Retry.
    chrome.close(page)?;
    let page = chrome.open(&api.site_url(&name, ""))?;
    anyhow::ensure!(super::shows(chrome, &page, "document.getElementById('say')?.dataset.ready === '1'"), "the failure page is ready");
    anyhow::ensure!(super::shows(chrome, &page, "[...document.querySelectorAll('.msg.user')].some(m => m.querySelector('.bubble')?.textContent === 'History 414' && !!m.querySelector('.reply'))"), "the quoted message has loaded");
    let quoted_seq = chrome.eval(&page, "(() => { const m = [...document.querySelectorAll('.msg.user')].find(m => m.querySelector('.bubble')?.textContent === 'History 414'); m.querySelector('.reply').click(); return Number(m.dataset.seq); })()")?;
    chrome.offline(&page, true)?;
    chrome.eval(&page, "(() => { document.getElementById('text').value = 'Retry my message'; document.getElementById('say').requestSubmit(); return true; })()")?;
    s.ok("a failed channel write shows its error and Retry on that message", super::shows(chrome, &page, "!![...document.querySelectorAll('.msg.user.mine')].find(m => m.textContent.includes('Retry my message') && m.querySelector('.message-error .message-retry'))"), "");
    s.ok("a failed quoted message keeps its quote and is not marked queued or offered Reply", chrome.eval(&page, "(() => { const m = document.querySelector('.message-error').closest('.msg'); return m.querySelector('.quote .quote-text')?.textContent === 'History 414' && !m.querySelector('.queued, .reply'); })()")? == true, "");
    chrome.screenshot(&page, &shots.join("post-error-desktop.png"))?;
    chrome.offline(&page, false)?;
    chrome.eval(&page, "(() => { document.querySelector('.message-retry').click(); return true; })()")?;
    s.ok("retry sends the same message successfully and clears its local error", super::shows(chrome, &page, "!document.querySelector('.message-error') && [...document.querySelectorAll('.msg.user.mine .bubble')].filter(m => m.textContent === 'Retry my message').length === 1"), "");
    let retried = super::super::jobs::records(api, owner, &name, "chat").iter().filter(|r| r["body"]["text"] == "Retry my message").count();
    s.ok("the retried message is stored once", retried == 1, retried);
    let quote_kept = super::super::jobs::records(api, owner, &name, "chat").iter().any(|r| r["body"]["text"] == "Retry my message" && r["body"]["reply_to"] == quoted_seq);
    s.ok("retry preserves the quoted message's reply_to", quote_kept, &quoted_seq);

    // A completed record fixture names an actual agent as the lead, but
    // does not join its computer to this quiet chat. The frame receives
    // the existing shell roster seam, and actual channel work records end
    // its waiting status; no new platform test levers are needed.
    for body in [json!({ "kind": "turn.start", "turn": "fixture-old", "agent": agent }), json!({ "kind": "turn.end", "turn": "fixture-old", "outcome": "idle" })] {
        let r = api.signed(owner, "POST", &format!("/api/f/{name}/channels/work"), Some(&json!({ "id": body["kind"], "body": body })))?;
        anyhow::ensure!(r.status == 200, "posting a completed work fixture: {r}");
    }
    chrome.eval(&page, "(() => { const frame = document.createElement('iframe'); frame.id = 'polish-frame'; frame.src = './'; frame.style = 'width:100%;height:100%;border:0'; document.body.replaceChildren(frame); window.__frame = frame; return true; })()")?;
    anyhow::ensure!(super::shows(chrome, &page, "window.__frame.contentDocument?.getElementById('say')?.dataset.ready === '1'"), "the status frame is ready");
    let roster = json!({ "fragment": "agents", "agents": [{ "identity": agent, "name": label, "title": "Bob", "phase": "starting" }] });
    chrome.eval(&page, &format!("(() => {{ window.__frame.contentWindow.postMessage({roster}, location.origin); return true; }})()"))?;
    chrome.eval(&page, "(() => { const d = window.__frame.contentDocument; d.getElementById('text').value = 'A slow first reply'; d.getElementById('say').requestSubmit(); return true; })()")?;
    s.ok("the shell's optional starting phase quietly explains a slow first reply", super::shows(chrome, &page, "window.__frame.contentDocument.querySelector('.working')?.textContent.includes('Bob is starting up…')"), "");
    chrome.screenshot(&page, &shots.join("starting-desktop.png"))?;
    chrome.eval(&page, &format!("new Promise(resolve => {{ const w = window.__frame.contentWindow; window.__bootNow = w.Date.now; w.Date.now = () => window.__bootNow() + 91000; w.postMessage({roster}, location.origin); w.requestAnimationFrame(() => w.requestAnimationFrame(() => resolve(true))); }})"))?;
    s.ok("a known computer startup remains explained after ninety seconds", super::shows(chrome, &page, "window.__frame.contentDocument.querySelector('.working')?.textContent.includes('Bob is starting up…')"), "");
    chrome.eval(&page, "(() => { window.__frame.contentWindow.Date.now = window.__bootNow; return true; })()")?;
    for (phase, expected) in [
        ("asleep", "Bob is starting up…"),
        ("awake", "Waiting for Bob…"),
        ("sleeping", "Bob's computer is going to sleep…"),
        ("wont_wake", "Bob's computer could not start. Open the computer settings to restart it."),
    ] {
        let roster = json!({ "fragment": "agents", "agents": [{ "identity": agent, "name": label, "title": "Bob", "phase": phase }] });
        chrome.eval(&page, &format!("(() => {{ window.__frame.contentWindow.postMessage({roster}, location.origin); return true; }})()"))?;
        s.ok(&format!("the shell's {phase} roster phase has the matching quiet status"), super::shows(chrome, &page, &format!("window.__frame.contentDocument.querySelector('.working')?.textContent === {}", super::js(expected))), "");
    }
    let absent = json!({ "fragment": "agents", "agents": [{ "identity": agent, "name": label, "title": "Bob" }] });
    chrome.eval(&page, &format!("(() => {{ window.__frame.contentWindow.postMessage({absent}, location.origin); return true; }})()"))?;
    s.ok("without a phase the page waits plainly, without claiming the agent is working", super::shows(chrome, &page, "window.__frame.contentDocument.querySelector('.working')?.textContent === 'Waiting for Bob…'"), "");
    chrome.eval(&page, "(() => { const w = window.__frame.contentWindow; window.__now = w.Date.now; w.Date.now = () => window.__now() + 91000; w.postMessage({fragment:'agents',agents:[]}, location.origin); return true; })()")?;
    s.ok("after ninety seconds without a turn the page says a reply has not started", super::shows(chrome, &page, "window.__frame.contentDocument.querySelector('.working')?.textContent.includes('has not started a reply yet')"), "");
    chrome.eval(&page, "(() => { window.__frame.contentWindow.Date.now = window.__now; return true; })()")?;
    let cause = super::super::jobs::records(api, owner, &name, "chat").into_iter().find(|r| r["body"]["text"] == "A slow first reply").unwrap_or_default();
    let started = api.signed(owner, "POST", &format!("/api/f/{name}/channels/work"), Some(&json!({ "id": "fixture-new", "body": { "kind": "turn.start", "turn": "fixture-new", "agent": agent, "asker": api.identity(owner)?, "cause": { "channel": "chat", "seq": cause["seq"] } } })))?;
    anyhow::ensure!(started.status == 200, "starting the fixture turn: {started}");
    s.ok("a turn.start replaces startup or delay status with the agent's working status", super::shows(chrome, &page, "window.__frame.contentDocument.querySelectorAll('.working').length === 1 && window.__frame.contentDocument.querySelector('.working').textContent.includes('is working')"), "");
    println!("      (polish screenshots in {})", shots.display());
    chrome.close(page)?;
    Ok(())
}

// Real work records, including an older bridge's step, on a quiet chat:
// no computer joins it, so the eight-step turn stays open for inspection.
pub(super) fn working_card(s: &mut Suite, api: &Api, owner: &Keys, chrome: &mut Browser, agent: &str, agent_name: &str) -> Result<()> {
    let name = s.named(api, owner, "working-card")?;
    let made = api.create_with(owner, json!({ "name": name, "template": "chat" }))?;
    anyhow::ensure!(made.status == 200, "making the working-card chat: {made}");
    s.owned(&made.body, owner);
    let home = format!("/data/work/{agent_name}/home/");
    let bodies = [
        json!({ "kind": "turn.start", "turn": "working-card", "agent": agent }),
        json!({ "kind": "turn.step", "turn": "working-card", "step": 1, "category": "read", "tool": "read_file", "args": format!("{home}apps-finite/README.md"), "ok": true, "text": "It works **partially.** I’ll check `apps-finite`.\n\n> **Design standards** — keep the card readable.\n\n- Read the files\n- [Check the docs](https://example.com/docs)\n\n<img src=x onerror=\"window.__noteInjected=true\">" }),
        json!({ "kind": "turn.step", "turn": "working-card", "step": 2, "category": "write", "tool": "patch", "args": format!("{home}apps-finite/src/components/chat/working-card/variants/desktop/theme/layout/WorkingCard.tsx"), "ok": true }),
        json!({ "kind": "turn.step", "turn": "working-card", "step": 3, "tool": "terminal", "args": "ls -R /data/hermes/managed-skills/software-development/references/engineering-style.md", "ok": true, "text": "I’ll keep the filename visible and shorten the directories. This sentence is deliberately long enough to make the note fold at a sentence boundary while retaining its markdown and the full explanation when expanded. **The final sentence stays available.**" }),
        json!({ "kind": "turn.step", "turn": "working-card", "step": 4, "category": "shell", "tool": "terminal", "args": format!("ls /data/work/{agent_name}/apps-finite"), "ok": true, "excerpt": "README.md\nsrc" }),
        json!({ "kind": "turn.step", "turn": "working-card", "step": 5, "category": "shell", "tool": "terminal", "args": "cargo test", "ok": false, "excerpt": "error: expected a closing delimiter" }),
        json!({ "kind": "turn.step", "turn": "working-card", "step": 6, "category": "read", "tool": "read_file", "args": "/data/work/another-agent/home/SOUL.md", "ok": true }),
        json!({ "kind": "turn.step", "turn": "working-card", "step": 7, "category": "web", "tool": "web_search", "args": "working card design", "ok": true }),
        json!({ "kind": "turn.step", "turn": "working-card", "step": 8, "category": "shell", "tool": "terminal", "args": "cargo check" }),
    ];
    for (i, body) in bodies.iter().enumerate() {
        let r = api.signed(owner, "POST", &format!("/api/f/{name}/channels/work"), Some(&json!({ "id": format!("fixture-{i}"), "body": body })))?;
        anyhow::ensure!(r.status == 200, "posting working-card fixture {i}: {r}");
    }
    let page = chrome.open(&api.site_url(&name, "__signin?return=/"))?;
    chrome.viewport(&page, 1000, 1100, false)?;
    anyhow::ensure!(super::shows(chrome, &page, "document.querySelectorAll('details.tools[open] .step').length === 8"), "eight-step working card rendered");
    s.ok("commentary renders bold, code, quotes, lists and safe links with the reply renderer", chrome.eval(&page, "(() => { const n = document.querySelector('.step-text'); return n.querySelector('strong').textContent === 'partially.' && n.querySelector('code').textContent === 'apps-finite' && !!n.querySelector('blockquote strong') && n.querySelectorAll('li').length === 2 && n.querySelector('a').getAttribute('href') === 'https://example.com/docs' && n.querySelector('a').rel === 'noopener noreferrer'; })()")? == true, "");
    s.ok("HTML in commentary is escaped, including after expanding it", chrome.eval(&page, "(() => { const n = document.querySelector('.step-text'); n.querySelector('.note-expand').click(); return !n.querySelector('img,script') && !window.__noteInjected && n.innerText.includes('<img src=x onerror=') && n.querySelector('.note-expand').getAttribute('aria-expanded') === 'true'; })()")? == true, "");
    s.ok("done steps have a check on the tool line and no separate done result", chrome.eval(&page, "(() => { const s = document.querySelector('.step'); const m = s.querySelector('.step-name .step-status.done'); return !!m?.querySelector('svg') && m.getAttribute('aria-label') === 'done' && !s.querySelector('pre') && !s.innerText.includes('done'); })()")? == true, "");
    let full = format!("{home}apps-finite/README.md");
    s.ok("the agent's own home becomes ~/ with the full original path in its title", super::shows(chrome, &page, &format!("document.querySelector('.step-args')?.textContent === '~/apps-finite/README.md' && document.querySelector('.step-args').title === {}", super::js(&full))), &full);
    s.ok("workspace and managed-skill prefixes shorten, including an older bridge's tool fallback", chrome.eval(&page, "(() => { const steps = document.querySelectorAll('.step'); return steps[2].dataset.category === 'other' && steps[2].querySelector('.step-verb').textContent === 'terminal:' && steps[2].querySelector('.step-args').textContent === 'ls -R skills/software-development/references/engineering-style.md' && !!steps[2].querySelector('.step-status.done') && steps[3].querySelector('.step-args').textContent === 'ls work/apps-finite' && steps[5].querySelector('.step-args').textContent.startsWith('/data/work/another-agent/home/'); })()")? == true, "");
    s.ok("a failed step has a red error mark and its result excerpt", chrome.eval(&page, "(() => { const s = document.querySelector('.step.error'); const m = s.querySelector('.step-status.failed'); return m?.getAttribute('aria-label') === 'failed' && !!m.querySelector('svg') && s.querySelector('.step-result').textContent === 'error: expected a closing delimiter' && getComputedStyle(m.querySelector('svg')).color === getComputedStyle(s.querySelector('.step-result')).color; })()")? == true, "");
    s.ok("a step without its result has a running spinner", chrome.eval(&page, "!!document.querySelector('.step:last-child .step-status.running .spin')")? == true, "");
    s.ok("long commentary folds at a complete sentence and expands its full markdown", chrome.eval(&page, "(() => { const n = document.querySelectorAll('.step-text')[1]; const p = n.querySelector('.note-preview'); const b = n.querySelector('.note-expand'); const folded = p.innerText === 'I’ll keep the filename visible and shorten the directories.' && b.textContent === '…' && b.getAttribute('aria-expanded') === 'false'; b.click(); return folded && p.hidden && n.innerText.includes('The final sentence stays available.') && n.querySelector('.md:not([hidden]) strong')?.textContent === 'The final sentence stays available.'; })()")? == true, "");
    chrome.eval(&page, "(() => { document.querySelectorAll('.step-text')[1].querySelector('.note-expand').click(); return true; })()")?;
    chrome.eval(&page, "(() => { document.querySelector('details.tools').scrollIntoView({block:'start'}); return true; })()")?;
    let shots = s.dir("working-card");
    chrome.screenshot(&page, &shots.join("desktop.png"))?;
    chrome.viewport(&page, 375, 1000, true)?;
    s.ok("a narrow step wraps to two lines, shortening directories in the middle while keeping the filename", super::shows(chrome, &page, "(() => { const a = document.querySelectorAll('.step-args')[1]; const h = Number.parseFloat(getComputedStyle(a).lineHeight); return a.textContent.startsWith('~/') && a.textContent.includes('…/') && a.textContent.endsWith('/WorkingCard.tsx') && a.clientHeight > h && a.scrollHeight <= 2*h + 1 && a.title.endsWith('/layout/WorkingCard.tsx'); })()"), chrome.eval(&page, "document.querySelectorAll('.step-args')[1].textContent")?);
    s.ok("the older bridge's path also keeps its filename visible when shortened on a phone", super::shows(chrome, &page, "(() => { const a = document.querySelectorAll('.step-args')[2]; const r = document.createRange(); r.setStart(a.firstChild, a.textContent.length - 20); r.setEnd(a.firstChild, a.textContent.length); return a.textContent.startsWith('ls -R skills/') && a.textContent.includes('…/') && a.textContent.endsWith('/engineering-style.md') && a.title.startsWith('ls -R /data/hermes/managed-skills/') && a.scrollHeight <= 2*Number.parseFloat(getComputedStyle(a).lineHeight)+1 && r.getBoundingClientRect().bottom <= a.getBoundingClientRect().bottom + 1; })()"), "");
    s.ok("the working card fits the phone without horizontal page scrolling", chrome.eval(&page, "document.documentElement.scrollWidth <= innerWidth && document.getElementById('scroll').scrollWidth === document.getElementById('scroll').clientWidth")? == true, "");
    chrome.eval(&page, "(() => { document.querySelector('details.tools').scrollIntoView({block:'start'}); return true; })()")?;
    chrome.screenshot(&page, &shots.join("phone.png"))?;
    chrome.viewport(&page, 1000, 1100, false)?;
    s.ok("resizing back restores the full shortened path when two lines fit", super::shows(chrome, &page, "document.querySelectorAll('.step-args')[1].textContent === '~/apps-finite/src/components/chat/working-card/variants/desktop/theme/layout/WorkingCard.tsx'"), "");
    let ended = api.signed(owner, "POST", &format!("/api/f/{name}/channels/work"), Some(&json!({ "id": "fixture-end", "body": { "kind": "turn.end", "turn": "working-card", "outcome": "idle" } })))?;
    anyhow::ensure!(ended.status == 200, "ending working-card fixture: {ended}");
    s.ok("a completed card folds and remembers expanded commentary when opened again", super::shows(chrome, &page, "(() => { const d = document.querySelector('details.tools'); if (d.open || !d.innerText.includes('Worked through 8 steps')) return false; d.open = true; return d.querySelector('.note-expand').getAttribute('aria-expanded') === 'true'; })()"), "");
    println!("      (working-card screenshots in {})", shots.display());
    chrome.close(page)?;
    Ok(())
}
