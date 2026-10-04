//! The code store itself, probed through the contract's routes
//! (macrofiche's docs/contract.md, section 5) where fragment relies on an
//! answer no lane asserts: refused commit packs, the compare-and-swap, a
//! file's identity headers, a listing's pages and last commits, branch and
//! merge conflicts, restore's 412, history's order, and the auth floor.
//! Each failure names the request, what the contract expects, and what
//! came back, as docs/macrofiche-conformance.md records them.
//!
//! It runs only on a store outside the run (`Suite::store_section`): the
//! fake in the process is the contract's reference. Where the fake and the
//! service differ by design (the contract's section 10: the url form,
//! problem bodies, ephemeral refs, restore to the tip, a token for another
//! repo), the service's answer is probed only on a store that follows it
//! (`External::service`: macrofiche), and is a skip elsewhere.

use anyhow::{Context, Result};
use base64::Engine;
use fragment_core::codestorage as cs;
use serde_json::{json, Value};
use sha1::Digest;

use crate::api::Api;
use crate::store::{Answer, External};
use crate::Suite;

/// A sha no repo holds.
const UNKNOWN_SHA: &str = "0123456789abcdef0123456789abcdef01234567";
/// The page size a listing is walked with, so a three-file tree is three pages.
const LISTING_PAGE: &str = "1";
/// Pages a listing walk reads at most: the probe's tree is three files.
const LISTING_PAGES_MAX: usize = 10;

/// One probe's outcome: a check, or why it is not made on this store.
enum Probe {
    Check { label: String, ok: bool, detail: String },
    Skip { label: String, why: String },
}

/// The probes, made before any is reported (they borrow the suite's store).
struct Probes {
    out: Vec<Probe>,
    service: bool,
}

impl Probes {
    /// A check: `request` sent, `expected` by the contract, `actual` back.
    fn check(&mut self, label: &str, ok: bool, request: &str, expected: &str, actual: impl std::fmt::Display) {
        let detail = format!("{request}: expected {expected}; got {actual}");
        self.out.push(Probe::Check { label: label.into(), ok, detail });
    }

    /// A check of the service's answer where the fake's differs by design.
    fn service(&mut self, label: &str, ok: impl FnOnce() -> Result<(bool, String, String, String)>) -> Result<()> {
        if !self.service {
            let why = "the fake differs here by design (macrofiche's contract, section 10), and this store may be the fake: probed on macrofiche, which follows the service";
            self.out.push(Probe::Skip { label: label.into(), why: why.into() });
            return Ok(());
        }
        let (ok, request, expected, actual) = ok()?;
        self.check(label, ok, &request, &expected, actual);
        Ok(())
    }
}

/// git's blob id of `bytes`: what `x-blob-sha` names.
fn blob_sha(bytes: &[u8]) -> String {
    let mut h = sha1::Sha1::new();
    h.update(format!("blob {}\0", bytes.len()).as_bytes());
    h.update(bytes);
    hex::encode(h.finalize())
}

/// An NDJSON pack from its lines, as a client streams one.
fn pack(lines: &[Value]) -> Vec<u8> {
    lines.iter().map(|l| format!("{l}\n")).collect::<String>().into_bytes()
}

fn chunk(id: &str, data: &[u8], eof: bool) -> Value {
    json!({ "blob_chunk": { "content_id": id, "data": base64::engine::general_purpose::STANDARD.encode(data), "eof": eof } })
}

fn meta(expected: Option<&str>, files: Value) -> Value {
    let mut m = json!({ "target_branch": "main", "commit_message": "probe", "author": { "name": "e2e", "email": "e2e@users.fragment" }, "files": files });
    if let Some(sha) = expected {
        m["expected_target_sha"] = json!(sha);
    }
    json!({ "metadata": m })
}

fn upsert(path: &str, id: &str) -> Value {
    json!({ "path": path, "operation": "upsert", "content_id": id, "mode": "100644" })
}

fn post_pack(x: &External, repo: &str, lines: &[Value]) -> Result<Answer> {
    x.repo_call("POST", repo, "commit-pack", &[], "git:write", Some(("application/x-ndjson", pack(lines))))
}

fn post_json(x: &External, repo: &str, op: &str, body: &Value) -> Result<Answer> {
    x.repo_call("POST", repo, op, &[], "git:write", Some(("application/json", body.to_string().into_bytes())))
}

fn restore(x: &External, repo: &str, base: &str, expected: &str) -> Result<Answer> {
    let line = json!({ "metadata": { "target_branch": "live", "base_ref": base, "expected_target_sha": expected, "commit_message": "probe restore", "author": { "name": "e2e", "email": "e2e@users.fragment" } } });
    x.repo_call("POST", repo, "restore-commit", &[], "git:write", Some(("application/x-ndjson", pack(&[line]))))
}

/// A listing at `at`, walked a file a page: (path, size, last commit) each.
fn listing(x: &External, repo: &str, at: &str) -> Result<Vec<(String, u64, String)>> {
    let mut all = vec![];
    let mut cursor: Option<String> = None;
    for _ in 0..LISTING_PAGES_MAX {
        let mut query = vec![("ref", at), ("limit", LISTING_PAGE)];
        if let Some(c) = &cursor {
            query.push(("cursor", c.as_str()));
        }
        let a = x.repo_call("GET", repo, "files/metadata", &query, "git:read", None)?;
        anyhow::ensure!(a.status == 200, "GET files/metadata: {a}");
        let (page, next) = cs::tree_page(&a.json()).map_err(|e| anyhow::anyhow!("{e}"))?;
        all.extend(page.into_iter().map(|e| (e.path, e.size, e.last_commit_sha)));
        cursor = next;
        if cursor.is_none() {
            return Ok(all);
        }
    }
    anyhow::bail!("a three-file listing ran past {LISTING_PAGES_MAX} pages")
}

pub fn codestore(s: &mut Suite, _api: &Api) -> Result<()> {
    if !s.store_section("codestore") {
        return Ok(());
    }
    let name = s.name("probe");
    let probes = {
        let x = s.external_store().context("the codestore section runs on an external store")?;
        let mut p = Probes { out: vec![], service: x.service };
        probe(&mut p, x, &name)?;
        p.out
    };
    for p in probes {
        match p {
            Probe::Check { label, ok, detail } => s.ok(&label, ok, detail),
            Probe::Skip { label, why } => s.skip(&label, &why),
        }
    }
    Ok(())
}

fn probe(p: &mut Probes, x: &External, name: &str) -> Result<()> {
    let repo = x.create_repo(name)?;
    let body = json!({ "repo_name": name, "default_branch": "main" });
    let again = x.call("POST", "/api/repos", &[], name, "repo:write", Some(("application/json", body.to_string().into_bytes())))?;
    p.check("a repo's name made again is 409", again.status == 409, &format!("POST /api/repos {body}, the name taken"), "409", &again);
    let bare = x.unsigned("GET", &format!("/api/repos/{repo}/branch?name=main"))?;
    p.check("a call without a bearer is 401", bare.status == 401, "GET branch with no authorization", "401", &bare);

    // a pack whose chunks interleave, with an empty file: one commit
    let lines = [
        meta(None, json!([upsert("a.txt", "c0"), upsert("dir/b.txt", "c1"), upsert("dir/sub/c.txt", "c2")])),
        chunk("c0", b"al", false),
        chunk("c1", b"beta", true),
        chunk("c0", b"pha", true),
        chunk("c2", b"", true),
    ];
    let a = post_pack(x, &repo, &lines)?;
    let c1 = cs::committed(&a.json()).unwrap_or_default();
    p.check(
        "a commit pack whose chunks interleave lands as one commit",
        matches!(a.status, 200 | 201) && cs::is_sha(&c1),
        "POST commit-pack: a.txt's chunks around dir/b.txt's, and an empty dir/sub/c.txt",
        "201 with result.success and a 40-hex new_sha",
        &a,
    );
    let read = x.file(&repo, &c1, "a.txt")?;
    p.check("its chunks join in order", read.as_deref() == Some(&b"alpha"[..]), "GET file a.txt at the commit", "alpha", format!("{:?}", read.map(|b| String::from_utf8_lossy(&b).into_owned())));
    let head = x.repo_call("HEAD", &repo, "file", &[("path", "dir/b.txt"), ("ref", &c1)], "git:read", None)?;
    let beta = blob_sha(b"beta");
    p.check(
        "HEAD file names the blob, its size and its last commit, with no body",
        head.status == 200 && head.header("content-length") == "4" && head.header("x-blob-sha") == beta && head.header("x-last-commit-sha") == c1 && head.body.is_empty(),
        "HEAD file dir/b.txt at the commit",
        &format!("200, content-length 4, x-blob-sha {beta}, x-last-commit-sha {c1}"),
        format!("{} content-length {:?} x-blob-sha {:?} x-last-commit-sha {:?}", head.status, head.header("content-length"), head.header("x-blob-sha"), head.header("x-last-commit-sha")),
    );
    let get = x.repo_call("GET", &repo, "file", &[("path", "dir/b.txt"), ("ref", &c1)], "git:read", None)?;
    p.check(
        "GET file carries the same identity, and an etag of the blob",
        get.status == 200 && get.body == b"beta" && get.header("etag") == format!("\"{beta}\"") && get.header("x-blob-sha") == beta && get.header("content-length") == "4",
        "GET file dir/b.txt at the commit",
        &format!("200 beta, etag \"{beta}\""),
        format!("{} etag {:?} x-blob-sha {:?}", get.status, get.header("etag"), get.header("x-blob-sha")),
    );

    // a second commit: last commits move only for what it changed
    let a = post_pack(x, &repo, &[meta(Some(&c1), json!([upsert("a.txt", "c0")])), chunk("c0", b"alpha 2", true)])?;
    let c2 = cs::committed(&a.json()).unwrap_or_default();
    let files = listing(x, &repo, &c2)?;
    let want = vec![("a.txt".to_string(), 7, c2.clone()), ("dir/b.txt".to_string(), 4, c1.clone()), ("dir/sub/c.txt".to_string(), 0, c1.clone())];
    let mut got = files.clone();
    got.sort();
    p.check(
        "a listing walked a file a page names each file once, recursively, with git's last commits",
        cs::is_sha(&c2) && got == want,
        "GET files/metadata at the second commit, limit 1, following next_cursor",
        &format!("{want:?}"),
        format!("{files:?}"),
    );

    // refusals: nothing lands
    let a = post_pack(x, &repo, &[meta(Some(&c1), json!([upsert("stale.txt", "c0")])), chunk("c0", b"x", true)])?;
    let main = x.head(&repo, "main")?;
    p.check(
        "a pack on a stale head is 409 precondition_failed, and nothing lands",
        a.status == 409 && a.json()["result"]["status"] == "precondition_failed" && main.as_deref() == Some(c2.as_str()),
        "POST commit-pack expecting the first commit while main is at the second",
        "409, result.status precondition_failed, main unmoved",
        format!("{a}; main {main:?}"),
    );
    let delete = json!({ "path": "a.txt", "operation": "delete", "content_id": "c0" });
    let a = post_pack(x, &repo, &[meta(Some(&c2), json!([delete])), chunk("c0", b"", true)])?;
    p.check(
        "a chunk for a delete is refused (400 invalid), and nothing lands",
        a.status == 400 && a.json()["result"]["status"] == "invalid" && x.head(&repo, "main")?.as_deref() == Some(c2.as_str()),
        "POST commit-pack deleting a.txt with a blob_chunk for its content_id",
        "400, result.status invalid",
        &a,
    );
    let a = post_pack(x, &repo, &[meta(Some(&c2), json!([upsert("z.txt", "c0")])), chunk("c0", b"z", false)])?;
    p.check(
        "an upsert whose stream has no eof is 400, and nothing lands",
        a.status == 400 && x.head(&repo, "main")?.as_deref() == Some(c2.as_str()),
        "POST commit-pack of z.txt, its only chunk without eof",
        "400",
        &a,
    );

    // branches, merges, restores
    let create = |target: &str, base: &str, ephemeral: bool| post_json(x, &repo, "branches/create", &json!({ "base_ref": base, "target_branch": target, "target_is_ephemeral": ephemeral }));
    let a = create("live", &c1, false)?;
    let again = create("live", &c1, false)?;
    p.check(
        "a branch made twice is 409 the second time",
        matches!(a.status, 200 | 201) && a.json()["commit_sha"] == c1.as_str() && again.status == 409,
        "POST branches/create live at the first commit, twice",
        "200 or 201 naming the base, then 409",
        format!("{a} | {again}"),
    );
    let a = create("other", UNKNOWN_SHA, false)?;
    p.check("a branch on an unknown base is 404", a.status == 404, &format!("POST branches/create other at {UNKNOWN_SHA}"), "404", &a);
    let merge = |expected: &str| {
        post_json(x, &repo, "merge", &json!({ "target_branch": "live", "source_ref": "main", "strategy": "ff_prefer", "expected_target_sha": expected, "commit_message": "probe", "author": { "name": "e2e", "email": "e2e@users.fragment" } }))
    };
    let a = merge(&c2)?;
    p.check("a merge on a stale live is 409", a.status == 409, "POST merge main into live expecting the second commit while live is at the first", "409", &a);
    let a = merge(&c1)?;
    p.check(
        "a merge that can fast-forward does: live at main's tip",
        a.status == 200 && a.json()["target"]["new_sha"] == c2.as_str() && x.head(&repo, "live")?.as_deref() == Some(c2.as_str()),
        "POST merge main into live (ff_prefer), live behind main",
        &format!("200, target.new_sha {c2}"),
        &a,
    );
    let a = post_pack(x, &repo, &[meta(Some(&c2), json!([upsert("d.txt", "c0")])), chunk("c0", b"d", true)])?;
    let c3 = cs::committed(&a.json()).unwrap_or_default();
    let a = restore(x, &repo, &c3, &c2)?;
    p.check("a restore to a commit live never held is 412", a.status == 412, "POST restore-commit of live to main's newer tip", "412", &a);
    let a = restore(x, &repo, &c1, &c2)?;
    let restored = cs::committed(&a.json()).unwrap_or_default();
    let tree = x.file(&repo, "live", "a.txt")?;
    let history = x.history(&repo, "live")?;
    p.check(
        "a restore is a new commit on live with the old tree, its first parent live's tip",
        matches!(a.status, 200 | 201) && cs::is_sha(&restored) && tree.as_deref() == Some(&b"alpha"[..]) && history.get(..2) == Some(&[restored.clone(), c2.clone()][..]),
        "POST restore-commit of live to the first commit",
        "201, live's a.txt back to alpha, history [restore, second]",
        format!("{a}; a.txt {:?}; history {history:?}", tree.map(|b| String::from_utf8_lossy(&b).into_owned())),
    );
    let main_history = x.history(&repo, "main")?;
    p.check(
        "history is newest first, the ref's tip first",
        main_history == [c3.clone(), c2.clone(), c1.clone()],
        "GET commits?ref=main",
        &format!("[{c3}, {c2}, {c1}]"),
        format!("{main_history:?}"),
    );

    // where the service's answer is not the fake's
    p.service("the url form is the repo's name", || Ok((repo == name, "POST /api/repos, then GET /api/repo-urls/{id}".into(), format!("url {name}"), format!("url {repo}"))))?;
    p.service("a refusal is a problem body with a code and a title", || {
        let ok = again_problem(&again);
        Ok((ok, "the 409 of a branch made twice".into(), "content-type application/problem+json, with code and title".into(), format!("{} {}", again.header("content-type"), again.text())))
    })?;
    p.service("a preview ref lives apart: a plain read misses it, an ephemeral one finds it", || {
        let a = create("preview/probe", &c3, true)?;
        let plain = x.head(&repo, "preview/probe")?;
        let ephemeral = x.head_ephemeral(&repo, "preview/probe")?;
        let ok = matches!(a.status, 200 | 201) && plain.is_none() && ephemeral.as_deref() == Some(c3.as_str());
        Ok((ok, "POST branches/create preview/probe ephemeral, then GET branch with and without ephemeral=true".into(), format!("404, then {c3}"), format!("{a}; plain {plain:?}; ephemeral {ephemeral:?}")))
    })?;
    p.service("a restore to live's own tip is 412", || {
        let a = restore(x, &repo, &restored, &restored)?;
        Ok((a.status == 412, "POST restore-commit of live to its tip".into(), "412".into(), a.to_string()))
    })?;
    p.service("a token for another repo on this repo's path is 404", || {
        let other = x.create_repo(&format!("{name}-other"))?;
        let token_for_other = x.call("GET", &format!("/api/repos/{repo}/branch"), &[("name", "main")], &other, "git:read", None)?;
        Ok((token_for_other.status == 404, "GET branch of this repo, the token's repo claim the other's".into(), "404".into(), token_for_other.to_string()))
    })?;
    Ok(())
}

/// A problem body as the service sends one (contract, section 4).
fn again_problem(a: &Answer) -> bool {
    let v = a.json();
    a.header("content-type").starts_with("application/problem+json") && v["code"].is_string() && v["title"].is_string()
}

#[cfg(test)]
mod tests {
    /// Goal: the probe's blob ids are git's. Method: ids git itself
    /// prints (`printf beta | git hash-object --stdin`, and the empty blob).
    #[test]
    fn blob_ids_are_gits() {
        assert_eq!(super::blob_sha(b""), "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391");
        assert_eq!(super::blob_sha(b"beta"), "e1d65540f4fdf72431ec47e001282cc7e8ed7c0c");
    }
}
