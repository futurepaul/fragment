//! Serving: each fragment on its own host, gated by visibility; the
//! machine-read plane; Open Graph pages; and the change feed.

use anyhow::Result;
use fragment_nip98::Keys;
use serde_json::json;

use crate::api::{self, Api, Call};
use crate::Suite;

pub fn site(s: &mut Suite, api: &Api) -> Result<()> {
    if !s.section("site") {
        return Ok(());
    }
    let owner = api.person()?;
    let viewer = api.person()?;
    let stranger = api.person()?;
    let name = s.named(api, &owner, "site")?;
    let c = s.create(api, &owner, &name)?;
    let view = c["viewToken"].as_str().unwrap_or("").to_string();
    let r = api.page(&name, "", None)?;
    s.ok("visibility is checked before anything about the site", r.status == 401, &r);
    let r = api.page(&name, &format!("?view={view}"), None)?;
    s.ok("a fragment with nothing deployed says so", r.status == 404 && r.message().contains("deploy first"), &r);
    s.commit(
        &c,
        &[
            ("site/index.html", Some(b"<!doctype html><html><head></head><body>front</body></html>")),
            ("site/docs/index.html", Some(b"<p>docs</p>")),
            ("site/app.3f9a1c2e.js", Some(b"console.log(1)")),
            ("notes/a.md", Some(b"alpha")),
            ("workflows/w.mjs", Some(b"code")),
            ("fragment.json", Some(br#"{"meta":{"title":"Site <Test>","description":"d"}}"#)),
        ],
    );
    s.deploy(&c);
    api.signed(&owner, "PUT", &format!("/api/f/{name}/members/{}", viewer.pubkey_hex()), Some(&json!({ "role": "viewer" })))?;

    // link (the default)
    let r = api.page(&name, "", None)?;
    s.ok("a link fragment refuses a visitor without the link", r.status == 401 && r.message().contains("share link"), &r);
    let r = api.page(&name, &format!("?view={view}"), None)?;
    let set = r.header("set-cookie");
    s.ok("the share link opens it", r.status == 200 && r.text.contains("front"), &r);
    s.ok("and sets a cookie for the fragment's origin", set.starts_with(&format!("fragview={view}; Path=/;")) && set.contains("HttpOnly"), &set);
    let cookie = format!("fragview={view}");
    let r = api.page(&name, "docs/", Some(&cookie))?;
    s.ok("the cookie opens other pages", r.status == 200 && r.text.contains("docs"), &r);
    s.ok("a link fragment's pages are private to caches", r.header("cache-control") == "private, no-cache", r.header("cache-control"));
    let r = api.page(&name, "", Some("fragview=wrong"))?;
    s.ok("a wrong link is refused", r.status == 401, &r);
    let signed = |keys: &Keys| api.call(Call { method: "GET", url: api.site_url(&name, ""), keys: Some(keys), ..Call::default() });
    let r = signed(&viewer)?;
    s.ok("a signed member needs no link", r.status == 200, &r);
    let r = signed(&stranger)?;
    s.ok("a signed stranger without the link is 403", r.status == 403, &r);

    // the page itself
    let r = api.page(&name, "", Some(&cookie))?;
    s.ok("meta becomes Open Graph tags", r.text.contains(r#"og:title" content="Site &lt;Test&gt;""#) && r.text.contains("__preview.svg"), &r);
    let r = api.page(&name, "__preview.svg", Some(&cookie))?;
    s.ok("the preview image is served", r.status == 200 && r.header("content-type") == "image/svg+xml", &r);
    let r = api.page(&name, "app.3f9a1c2e.js", Some(&cookie))?;
    s.ok("hashed assets are immutable (privately, here)", r.header("cache-control") == "private, max-age=31536000, immutable", r.header("cache-control"));
    let r = api.page(&name, "nope.html", Some(&cookie))?;
    s.ok("a missing page is 404", r.status == 404, &r);
    let r = api.call(Call { method: "HEAD", url: api.site_url(&name, ""), cookie: Some(cookie.clone()), ..Call::default() })?;
    s.ok("HEAD answers without a body", r.status == 200 && r.text.is_empty(), &r);
    let r = api.call(Call { method: "DELETE", url: api.site_url(&name, ""), cookie: Some(cookie.clone()), ..Call::default() })?;
    s.ok("other methods are for the app's routes (none here: 404)", r.status == 404 && r.message().contains("no page or app route for DELETE /"), &r);

    // the machine-read plane
    let r = api.page(&name, "__tree", Some(&cookie))?;
    let paths: Vec<&str> = r.body["files"].as_array().map(|a| a.iter().filter_map(|f| f["path"].as_str()).collect()).unwrap_or_default();
    s.ok("__tree lists the live content", r.status == 200 && paths.contains(&"notes/a.md"), &r);
    s.ok("__tree hides the fragment's machinery", !paths.iter().any(|p| p.starts_with("workflows/") || *p == "fragment.json"), &r);
    let r = api.page(&name, "__file?path=notes/a.md", Some(&cookie))?;
    s.ok("__file returns content", r.status == 200 && r.text == "alpha", &r);
    let r = api.page(&name, "__file?path=workflows/w.mjs", Some(&cookie))?;
    s.ok("__file refuses machinery", r.status == 400, &r);
    let r = api.page(&name, "__tree", None)?;
    s.ok("__tree is gated like the site", r.status == 401, &r);
    s.commit(&c, &[("data/new.json", Some(b"{}"))]);
    let r = api.page(&name, "__file?path=data/new.json", Some(&cookie))?;
    s.ok("__file reads a file only main has yet", r.status == 200 && r.text == "{}", &r);

    // rotating the link
    let r = api.signed(&owner, "POST", &format!("/api/f/{name}/rotate"), Some(&json!({ "scopes": ["view"] })))?;
    let fresh = r.body["view_token"].as_str().unwrap_or("").to_string();
    s.ok("the owner rotates the share link", r.status == 200 && fresh != view && r.body["rotated"] == json!(["view"]), &r);
    let r = api.page(&name, "", Some(&cookie))?;
    s.ok("the old link stops working", r.status == 401, &r);
    let r = api.page(&name, &format!("?view={fresh}"), None)?;
    s.ok("the new one works", r.status == 200, &r);
    let r = api.signed(&viewer, "POST", &format!("/api/f/{name}/rotate"), Some(&json!({})))?;
    s.ok("only the owner rotates", r.status == 403, &r);

    // members and public
    let r = api.signed(&viewer, "PUT", &format!("/api/f/{name}/visibility"), Some(&json!({ "visibility": "public" })))?;
    s.ok("only the owner changes visibility", r.status == 403, &r);
    api.signed(&owner, "PUT", &format!("/api/f/{name}/visibility"), Some(&json!({ "visibility": "members" })))?;
    let r = api.page(&name, &format!("?view={fresh}"), None)?;
    s.ok("a members fragment ignores the link", r.status == 401 && r.message().contains("members only"), &r);
    let r = signed(&viewer)?;
    s.ok("a members fragment serves its members", r.status == 200, &r);
    api.signed(&owner, "PUT", &format!("/api/f/{name}/visibility"), Some(&json!({ "visibility": "public" })))?;
    let r = api.page(&name, "", None)?;
    s.ok("a public fragment serves anyone", r.status == 200 && r.header("cache-control") == "public, max-age=60", &r);
    let r = api.page(&name, "__tree", None)?;
    s.ok("a public fragment's tree is public", r.status == 200, &r);

    // /f/<name>/ on a fleet with hostnames
    let r = api.call(Call { method: "GET", url: format!("{}/f/{name}/docs/?x=1", api.base), ..Call::default() })?;
    s.ok(
        "a path URL redirects to the fragment's own origin",
        r.status == 308 && r.header("location") == api.site_url(&name, "docs/?x=1"),
        r.header("location"),
    );
    let r = api.call(Call { method: "GET", url: format!("{}/f/{name}", api.base), ..Call::default() })?;
    s.ok("the bare path redirects to its root", r.status == 308 && r.header("location") == api.site_url(&name, ""), &r);
    let r = api.call(Call {
        method: "POST",
        url: format!("{}/f/{name}/__op/x", api.base),
        body: Some(b"{}".to_vec()),
        content_type: Some("application/json"),
        ..Call::default()
    })?;
    s.ok("a path URL never takes writes when fragments have hosts", r.status == 404, &r);
    Ok(())
}

pub fn watch(s: &mut Suite, api: &Api) -> Result<()> {
    if !s.section("watch") {
        return Ok(());
    }
    let owner = api.person()?;
    let name = s.named(api, &owner, "watch")?;
    let c = s.create(api, &owner, &name)?;
    let view = c["viewToken"].as_str().unwrap_or("").to_string();
    s.commit(&c, &[("a.md", Some(b"1"))]);
    let r = api.call(Call { method: "GET", url: format!("{}/f/{name}/__watch", api.base), keys: Some(&owner), ..Call::default() })?;
    s.ok("__watch without an upgrade is 400", r.status == 400, &r);
    let refused = api::watch(api, &name, "", None).is_err();
    s.ok("an anonymous feed on a link fragment is refused", refused, "");
    let mut feed = api::watch(api, &name, &format!("?view={view}"), None)?;
    let hello: serde_json::Value = serde_json::from_str(feed.read()?.to_text()?)?;
    s.ok("the link opens the feed; hello names main's pin", hello["type"] == "hello" && hello["sha"].as_str().is_some_and(|s| s.len() == 40), &hello);
    s.commit(&c, &[("b.md", Some(b"2"))]);
    let changed: serde_json::Value = serde_json::from_str(feed.read()?.to_text()?)?;
    s.ok("a push arrives as a changed frame with its paths", changed["type"] == "changed" && changed["paths"] == json!(["b.md"]), &changed);
    api.signed(&owner, "POST", &format!("/api/f/{name}/rotate"), Some(&json!({ "scopes": ["view"] })))?;
    let closed = matches!(feed.read(), Ok(tungstenite::Message::Close(Some(f))) if u16::from(f.code) == 4003);
    s.ok("rotating the link closes link holders' feeds", closed, "");
    let mut signed = api::watch(api, &name, "", Some(&owner))?;
    let hello = signed.read()?;
    s.ok("a signed member opens the feed without the link", hello.to_text().unwrap_or("").contains("hello"), &hello);
    Ok(())
}
