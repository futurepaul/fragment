//! Rendering and history on a quiet chat, so pagination fixtures start no
//! agent turns. Uses real channel writes, blobs, and the released page.
use anyhow::Result;
use serde_json::json;
use crate::api::{Api, Call};
use crate::browser::Browser;
use crate::Suite;
use fragment_nip98::Keys;

pub(super) fn check(s: &mut Suite, api: &Api, owner: &Keys, session: &str, chrome: &mut Browser) -> Result<()> {
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
            body: Some(bytes.to_vec()), content_type: Some(mime.into()), signer: Some(owner.clone()), ..Call::default()
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
    for i in 0..415 {
        let r = api.signed(owner, "POST", &format!("/api/f/{name}/channels/chat"), Some(&json!({ "id": format!("history-{i}"), "body": { "text": format!("History {i:03}") } })))?;
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
    chrome.eval(&page, "(() => { const s = document.getElementById('scroll'); s.scrollTop = 100; return true; })()")?;
    super::shows(chrome, &page, "document.getElementById('scroll').scrollTop === 100");
    let anchor = chrome.eval(&page, "(() => { const s = document.getElementById('scroll'); const m = [...document.querySelectorAll('.msg')].find(m => m.getBoundingClientRect().bottom > s.getBoundingClientRect().top); window.__historyAnchor = m; window.__historyTop = m.getBoundingClientRect().top; document.getElementById('earlier').click(); return m.dataset.seq; })()")?;
    let loaded = super::shows(chrome, &page, "document.querySelectorAll('.msg.user').length === 416 && !document.getElementById('earlier').disabled");
    let position = chrome.eval(&page, "Math.abs(window.__historyAnchor.getBoundingClientRect().top - window.__historyTop)")?;
    let ordered = chrome.eval(&page, "(() => { const seqs = [...document.querySelectorAll('.msg[data-seq]')].map(m => Number(m.dataset.seq)); return seqs.every((seq, i) => !i || seq > seqs[i-1]); })()")?;
    s.ok("loading earlier messages preserves the visible message, ascending order, and no duplicates", loaded && position.as_f64().is_some_and(|n| n < 2.0) && ordered == true, json!({ "anchor": anchor, "moved": position, "ordered": ordered }));
    s.ok("the earlier control disappears at the beginning of retained history", chrome.eval(&page, "document.getElementById('earlier').hidden")? == true, "");
    chrome.screenshot(&page, &shots.join("history-desktop.png"))?;
    s.ok("jump to latest appears while reading earlier messages", super::shows(chrome, &page, "!document.getElementById('latest').hidden"), "");
    for i in 0..2 {
        let r = api.signed(owner, "POST", &format!("/api/f/{name}/channels/chat"), Some(&json!({ "id": format!("new-{i}"), "body": { "text": format!("New message {i}") } })))?;
        anyhow::ensure!(r.status == 200, "posting a new message: {r}");
    }
    s.ok("new live messages are counted without moving the reader", super::shows(chrome, &page, "document.getElementById('latest').textContent.includes('2 new messages')") && chrome.eval(&page, "Math.abs(window.__historyAnchor.getBoundingClientRect().top - window.__historyTop) < 2")? == true, "");
    chrome.click(&page, "#latest")?;
    s.ok("jump to latest reaches the end and clears the count", super::shows(chrome, &page, "document.getElementById('latest').hidden && (() => { const s = document.getElementById('scroll'); return s.scrollHeight - s.scrollTop - s.clientHeight < 2; })()"), "");
    println!("      (polish screenshots in {})", shots.display());
    chrome.close(page)?;
    Ok(())
}
