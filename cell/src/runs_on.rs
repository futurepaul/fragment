//! A fragment's side of computers (docs/computers.md): an agent fragment
//! running on one, and a fragment whose channel wakes one.
//!
//! - **An agent fragment** is assigned to its owner's computer. Its own
//!   key becomes the agent's identity (registered to the fragment's owner),
//!   and it signs the guest's requests (NIP-98) only for the computer it is
//!   assigned to: the computer's egress asks, naming itself.
//! - **A wake subscription** (`{channel, wake: true}` from a computer's
//!   guest, which only its egress can ask for) is a subscription whose URL
//!   is `computer:<id>`: each new record on the channel wakes it, and the
//!   guest reads the channel itself. A page opening the fragment, which a
//!   computer may be about to hear from, wakes it early (a pre-wake).

use fragment_core::npub;
use fragment_proto::{split_fragment_name, valid_channel_name, ErrorCode, IdentityKind, Role};
use serde::Deserialize;
use serde_json::{json, Value};
use worker::*;

use crate::error::{CellError, CellResult};
use crate::fragment::{FragmentCell, MetaKey};
use crate::registry::calls::{self, By};

/// At most one pre-wake a computer from one fragment this often.
const PREWAKE_EVERY_MS: i64 = 30_000;

#[derive(Deserialize)]
struct Assign {
    computer: String,
    owner: String,
}

#[derive(Deserialize)]
struct Sign {
    computer: String,
    method: String,
    url: String,
    payload: Option<String>,
}

#[derive(Deserialize)]
struct Subscribe {
    computer: String,
    identity: String,
    channel: String,
}

/// A wake subscription's URL: no one fetches it.
pub(crate) fn wake_url(computer: &str) -> String {
    format!("computer:{}", computer.trim_start_matches("computer:"))
}

impl FragmentCell {
    /// `computer/…`: the internal calls a computer's routes and egress make.
    pub(crate) async fn runs_on(&self, route: &str, body: &Value) -> CellResult<Value> {
        self.must(MetaKey::CreatedAt).map_err(|_| CellError::new(ErrorCode::NotFound, "no such fragment"))?;
        match route {
            "assign" => {
                let b: Assign = serde_json::from_value(body.clone()).map_err(|e| CellError::invalid(format!("body: {e}")))?;
                if self.must(MetaKey::Owner)? != b.owner {
                    return Err(CellError::new(ErrorCode::Forbidden, "only the agent fragment's owner runs it on their computer"));
                }
                let identity = match self.meta(MetaKey::AgentIdentity)? {
                    Some(id) => id,
                    None => {
                        let npub = self.must(MetaKey::Npub)?;
                        let key = npub::parse(&npub).ok_or_else(|| CellError::host("the fragment's npub does not decode"))?;
                        let agent = crate::ask_registry(&self.env, &calls::RegisterAgent { owner: By::Identity(b.owner.clone()), key }).await?;
                        self.set_meta(MetaKey::AgentIdentity, &agent.id)?;
                        agent.id
                    }
                };
                self.set_meta(MetaKey::Computer, &b.computer)?;
                // the agent keeps its own state here (its SOUL.md, memories,
                // skills, routines): it is an editor of its own fragment
                if self.member_role(&identity)?.is_none() {
                    let as_owner = self.as_owner()?;
                    self.set_member(&as_owner, &identity, fragment_proto::SetRole { role: Role::Editor, people_only: false }).await?;
                }
                self.event("computer.assigned", &format!("runs on {}", b.computer), json!({ "computer": b.computer, "identity": identity }));
                Ok(json!({ "identity": identity }))
            }
            "unassign" => {
                let b: Assign = serde_json::from_value(body.clone()).map_err(|e| CellError::invalid(format!("body: {e}")))?;
                if self.must(MetaKey::Owner)? != b.owner {
                    return Err(CellError::new(ErrorCode::Forbidden, "only the agent fragment's owner moves it"));
                }
                if self.meta(MetaKey::Computer)?.as_deref() == Some(b.computer.as_str()) {
                    self.del_meta(MetaKey::Computer)?;
                    self.event("computer.unassigned", &format!("no longer runs on {}", b.computer), json!({ "computer": b.computer }));
                }
                Ok(json!({ "ok": true }))
            }
            "identity" => {
                let computer = body["computer"].as_str().unwrap_or("");
                self.assigned_to(computer)?;
                Ok(json!({ "identity": self.must(MetaKey::AgentIdentity)? }))
            }
            "sign" => {
                let b: Sign = serde_json::from_value(body.clone()).map_err(|e| CellError::invalid(format!("body: {e}")))?;
                self.assigned_to(&b.computer)?;
                let sealed = self.must(MetaKey::FragmentSecret)?;
                let opened = crate::keys::open(&self.env, &self.scope(), &sealed)?;
                if let Some(fresh) = opened.resealed {
                    self.set_meta(MetaKey::FragmentSecret, &fresh)?;
                }
                let secret = std::str::from_utf8(&opened.plaintext).map_err(|_| CellError::host("the fragment's key is not text"))?;
                let keys = fragment_nip98::Keys::from_secret_hex(secret).ok_or_else(|| CellError::host("the fragment's sealed key is not a nostr key"))?;
                let header = keys.header_for_payload(&b.method, &b.url, b.payload.as_deref(), crate::js::now_ms() / 1000);
                Ok(json!({ "header": header }))
            }
            "subscribe" => {
                let b: Subscribe = serde_json::from_value(body.clone()).map_err(|e| CellError::invalid(format!("body: {e}")))?;
                if !valid_channel_name(&b.channel) {
                    return Err(CellError::invalid("a channel name must match ^[a-z][a-z0-9_-]{0,63}$"));
                }
                let Some(role) = self.member_role(&b.identity)? else {
                    return Err(CellError::new(ErrorCode::Forbidden, "only a member subscribes to a channel"));
                };
                if role < self.channel_read_role(&b.channel)? {
                    return Err(CellError::new(ErrorCode::Forbidden, format!("that agent's role may not read {}", b.channel)));
                }
                let id = self.add_subscription(&b.identity, &b.channel, &wake_url(&b.computer))?;
                Ok(json!({ "id": id, "channel": b.channel, "wake": true }))
            }
            r => Err(CellError::new(ErrorCode::NotFound, format!("no route computer/{r}"))),
        }
    }

    /// The fragment's owner as a caller, for what the platform does on the
    /// owner's behalf here (adding the agent as a member).
    fn as_owner(&self) -> CellResult<crate::fragment::Caller> {
        let (name, owner) = (self.name()?, self.must(MetaKey::Owner)?);
        let (_, username) = split_fragment_name(&name).ok_or_else(|| CellError::host(format!("{name} is not <label>.<username>")))?;
        let identity = fragment_proto::Identity { id: owner, kind: IdentityKind::Person, owner: None, username: Some(username.to_string()), held: None };
        let signed = crate::routed::Signed::new(identity, None);
        Ok(crate::fragment::Caller { signed: Some(signed), unresolved: None, url: url::Url::parse("https://fragment.internal/").expect("a URL"), mode: None })
    }

    /// This agent fragment runs on `computer`, or refuses.
    fn assigned_to(&self, computer: &str) -> CellResult<()> {
        if self.meta(MetaKey::Computer)?.as_deref() != Some(computer) {
            return Err(CellError::new(ErrorCode::Forbidden, "this agent does not run on that computer"));
        }
        Ok(())
    }

    /// The computers this fragment's channels wake.
    pub(crate) fn woken_computers(&self) -> CellResult<Vec<String>> {
        let rows = self.rows("SELECT DISTINCT url FROM subs WHERE url LIKE 'computer:%'", vec![])?;
        Ok(rows.iter().filter_map(|r| r["url"].as_str()).map(|u| format!("computer:{}", u.trim_start_matches("computer:"))).collect())
    }

    /// A page opened this fragment: the computers its channels wake may
    /// hear from it soon, so each starts now (decision 39), at most every
    /// `PREWAKE_EVERY_MS`. Each wake runs on its own, so the page's socket
    /// never waits for a start; a failed pre-wake costs only the wait.
    pub(crate) fn prewake(&self) {
        let now = crate::js::now_ms();
        let due = self.meta(MetaKey::PrewakeAt).ok().flatten().and_then(|v| v.parse::<i64>().ok()).is_none_or(|at| now >= at);
        if !due {
            return;
        }
        let Ok(computers) = self.woken_computers() else { return };
        if computers.is_empty() {
            return;
        }
        let _ = self.set_meta(MetaKey::PrewakeAt, &(now + PREWAKE_EVERY_MS).to_string());
        for c in computers {
            let env = self.env.clone();
            worker::wasm_bindgen_futures::spawn_local(async move {
                if let Err(e) = crate::computer::ask(&env, &c, "computer/wake", &json!({ "why": "presence" })).await {
                    console_log!("{{\"fragment\":\"prewake\",\"computer\":{},\"error\":{}}}", json!(c), json!(e.message));
                }
            });
        }
    }
}
