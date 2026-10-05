//! A person's nodes (bring your own computer, experimental:
//! docs/self-host.md, seam 2). The router's half: the node's two unsigned
//! calls, the page a person approves a node at, and the settings' API.
//!
//!   POST /api/nodes/pair             a node asks to be paired (unsigned):
//!                                    its codes, and the link to approve it at
//!   POST /api/nodes/pair/poll        the node's poll: pending, slow_down,
//!                                    expired, or (once) its id and secret
//!   GET  /nodes/pair?code=           the signed-in person's page for that
//!                                    code: which node, and a button
//!   POST /nodes/pair                 (the form, from the platform's own page)
//!   GET  /api/nodes                  every node the person may use, theirs
//!                                    and the deployment's, each up or down
//!                                    with their computers on it, and their choice
//!   PUT  /api/nodes/prefer           {node}: where new computers run (null:
//!                                    the deployment's rule)
//!   DELETE /api/nodes/{id}           revokes a node of theirs: its secret
//!                                    goes, and its uplink is cut
//!
//! The decisions are `fragment_core::pairing`'s and the rows the
//! registry's (`registry/nodes.rs`); a node's uplink is its `Node`
//! object's (entry.mjs, uplink.mjs). Approval happens only on the
//! platform's page, in a session, from its own origin, as a CLI key's does.

use fragment_core::placement::Byoc;
use fragment_proto::computer::ComputerView;
use fragment_proto::nodes::{NodeKind, NodeState, NodeView, NodesView, PairPoll, PairStart, PairStarted, PreferNode};
use fragment_proto::{ErrorCode, IdentityKind};
use serde_json::{json, Value};
use worker::*;

use crate::auth::{self, esc};
use crate::config::Config;
use crate::error::{CellError, CellResult};
use crate::fragment::json_response;
use crate::registry::calls::{self, OwnNodesAnswer};
use crate::{ask_registry, read_body};

/// A node's start or poll is a few short fields.
const NODE_BODY_MAX_BYTES: usize = 1024;
/// The approval form: one code.
const FORM_MAX_BYTES: usize = 1024;

/// `/api/nodes…` on the platform's host.
pub(crate) async fn api(mut req: Request, env: &Env, cfg: &Config, url: &Url, rest: &[&str]) -> CellResult<Response> {
    match (req.method(), rest) {
        (Method::Post, ["pair"]) => {
            let body = read_body(&mut req, NODE_BODY_MAX_BYTES).await?;
            let start: PairStart = serde_json::from_slice(&body).map_err(|e| CellError::invalid(format!("body: {e}")))?;
            let begun = ask_registry(env, &calls::PairBegin { name: start.name, arch: start.arch }).await?;
            let platform = cfg.platform(url);
            let expires_in_s = u32::try_from((begun.expires_at - crate::js::now_ms()).max(0) / 1000).unwrap_or(0);
            json_response(&PairStarted {
                verify_url: format!("{platform}/nodes/pair?code={}", begun.user_code),
                user_code: begun.user_code,
                device_code: begun.device_code,
                expires_in_s,
                interval_s: begun.interval_s,
            })
        }
        (Method::Post, ["pair", "poll"]) => {
            let body = read_body(&mut req, NODE_BODY_MAX_BYTES).await?;
            let poll: PairPoll = serde_json::from_slice(&body).map_err(|e| CellError::invalid(format!("body: {e}")))?;
            json_response(&ask_registry(env, &calls::PairPoll { device_code: poll.device_code }).await?)
        }
        (method, rest) => {
            let body = read_body(&mut req, NODE_BODY_MAX_BYTES).await?;
            let who = crate::signer(env, &req, url, &body).await?;
            if who.identity.kind != IdentityKind::Person {
                return Err(CellError::new(ErrorCode::Forbidden, "nodes are people's: an agent runs on its owner's computer"));
            }
            let owner = who.identity.id.clone();
            match (method.clone(), rest) {
                (Method::Get, []) => json_response(&view(env, cfg, &owner, ask_registry(env, &calls::OwnNodes { owner: owner.clone() }).await?).await?),
                (Method::Put, ["prefer"]) => {
                    let v: Value = serde_json::from_slice(&body).map_err(|e| CellError::invalid(format!("body: {e}")))?;
                    // a body that forgot the field is not a reset to the deployment's rule
                    if v.get("node").is_none() {
                        return Err(CellError::invalid("name a node, or null for the deployment's rule"));
                    }
                    let b: PreferNode = serde_json::from_value(v).map_err(|e| CellError::invalid(format!("body: {e}")))?;
                    let own = ask_registry(env, &calls::PreferNode { owner: owner.clone(), node: b.node }).await?;
                    json_response(&view(env, cfg, &owner, own).await?)
                }
                (Method::Delete, [id]) => {
                    let own = ask_registry(env, &calls::RevokeNode { owner: owner.clone(), id: id.to_string() }).await?;
                    // its uplink is cut, and its object answers every call
                    // `node_revoked` from now on: the registry has dropped its
                    // secret, so it cannot dial again either way
                    crate::routed::ask_object(env, "NODE", id, "__node/revoke", &json!({ "id": id })).await?;
                    json_response(&view(env, cfg, &owner, own).await?)
                }
                _ => Err(CellError::new(ErrorCode::NotFound, format!("no route {} {}", method.as_ref(), url.path()))),
            }
        }
    }
}

/// What a node's object says of it now: up or down, and why.
async fn status(env: &Env, id: &str) -> (NodeState, Option<String>) {
    match crate::routed::ask_object(env, "NODE", id, "__node/status", &json!({ "id": id })).await {
        Ok(v) if v["revoked"] == true => (NodeState::Revoked, None),
        Ok(v) if v["up"] == true => (NodeState::Up, None),
        Ok(v) => (NodeState::Down, v["why"].as_str().map(str::to_string)),
        Err(e) => (NodeState::Down, Some(e.message)),
    }
}

/// Every node the person may use (the deployment's, then theirs, revoked
/// ones too while they hold the row), each as its object finds it now,
/// with the person's computers on it.
async fn view(env: &Env, cfg: &Config, owner: &str, own: OwnNodesAnswer) -> CellResult<NodesView> {
    // a person has one computer today (docs/self-host.md: several is the
    // experimental part): where it is placed, if anywhere
    let id = fragment_core::computer::default_computer_of(owner);
    let mine = match crate::computer::ask(env, &id, "computer/view", &json!({})).await {
        Ok(v) => serde_json::from_value::<ComputerView>(v).ok(),
        Err(e) if e.code == ErrorCode::NotFound => None,
        Err(e) => return Err(e),
    };
    let on = |node: &str| mine.iter().filter(|c| c.node.as_deref() == Some(node)).map(|c| c.computer.clone()).collect::<Vec<_>>();
    let mut rows: Vec<(String, String, NodeKind, String, Option<i64>, bool)> = vec![];
    for n in cfg.nodes.as_ref().map(|n| n.nodes()).unwrap_or_default() {
        rows.push((n.id.clone(), n.id.clone(), NodeKind::Deployment, n.arch.name().to_string(), None, false));
    }
    for n in &own.nodes {
        rows.push((n.id.clone(), n.name.clone(), NodeKind::Own, n.arch.clone(), Some(n.paired_at), n.revoked_at.is_some()));
    }
    // each object asked at once: a node that is down answers within its health's bound
    let states = futures_util::future::join_all(rows.iter().map(|(id, _, _, _, _, revoked)| async move {
        if *revoked {
            (NodeState::Revoked, None)
        } else {
            status(env, id).await
        }
    }))
    .await;
    let nodes = rows
        .into_iter()
        .zip(states)
        .map(|((id, name, kind, arch, paired_at, _), (state, why))| NodeView { computers: on(&id), id, name, kind, arch, state, why, paired_at })
        .collect();
    Ok(NodesView { byoc: cfg.nodes.as_ref().is_some_and(|n| n.byoc() == Byoc::On), prefer: own.prefer, nodes })
}

/// `/nodes/pair` on the platform's host: the person's page for a node's
/// code, and its form.
pub(crate) async fn page(mut req: Request, env: &Env, cfg: &Config, url: &Url) -> CellResult<Response> {
    let platform = cfg.platform(url);
    match req.method() {
        Method::Get => {
            let code = auth::query(url, "code").unwrap_or_default();
            let Some((token, _)) = auth::platform_session(&req, env, url).await? else {
                return auth::to_login(&platform, &format!("/nodes/pair?code={}", auth::enc(&code)));
            };
            let shown = match ask_registry(env, &calls::PairShow { token, code: code.clone() }).await {
                Ok(shown) => shown,
                Err(e) if matches!(e.code, ErrorCode::NotFound | ErrorCode::InvalidRequest | ErrorCode::RateLimited | ErrorCode::Forbidden) => {
                    return auth::page(e.code.status(), "This node can't be added", &format!("<p>{}</p>", esc(&e.message)));
                }
                Err(e) => return Err(e),
            };
            auth::page(
                200,
                "Add a computer of yours (experimental)",
                &format!(
                    "<p>A sandcastle node named <b>{name}</b> ({arch}) wants to run your computers.</p>
<p>Its code is <code>{code}</code>. Check that the terminal where you ran <code>sandcastle-node pair</code> shows the same code.</p>
<p>It runs only computers you choose for it, in settings. Approve only a machine you started yourself: whoever runs it sees what your computers on it do.</p>
<form method=\"post\" action=\"/nodes/pair\"><input type=\"hidden\" name=\"code\" value=\"{code}\"><button>Add this node</button></form>
<p>Didn't run <code>sandcastle-node pair</code>? Close this page.</p>",
                    name = esc(&shown.name),
                    arch = esc(&shown.arch),
                    code = esc(&shown.user_code),
                ),
            )
        }
        Method::Post => {
            auth::same_origin(&req, &platform)?;
            // read before anyone is known to be signed in: bounded as it arrives
            let bytes = read_body(&mut req, FORM_MAX_BYTES).await?;
            let code = url::form_urlencoded::parse(&bytes).find(|(k, _)| k == "code").map(|(_, v)| v.into_owned()).unwrap_or_default();
            let Some(token) = auth::platform_session_token(&req, url)? else {
                return Err(CellError::new(ErrorCode::Unauthenticated, "sign in first"));
            };
            let approved = ask_registry(env, &calls::PairApprove { token, code }).await?;
            auth::page(
                200,
                "Node added",
                &format!(
                    "<p><b>{}</b> is yours now, as <code>{}</code>. The <code>sandcastle-node pair</code> waiting in its terminal finishes on its own.</p><p>To run your next computer there, choose it in <a href=\"/settings\">settings</a>, under Computers.</p>",
                    esc(&approved.name),
                    esc(&approved.node)
                ),
            )
        }
        m => Err(CellError::new(ErrorCode::NotFound, format!("no route {} {}", m.as_ref(), url.path()))),
    }
}
