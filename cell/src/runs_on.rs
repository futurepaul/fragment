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
//! - **An agent added as a member** (Paul, 2026-10-03: agents are woken
//!   eagerly, and no template must remember a convention): the platform
//!   tells its computer. The membership's write keeps a row in this
//!   fragment's `joined_outbox` in the same turn; delivering it asks the
//!   agent's owner's computer (one per person: decision 13), which has
//!   the agent's own fragment post `{kind: "joined", fragment}` on its
//!   `tasks` (keyed by this membership, so a retry posts nothing twice),
//!   and then this fragment wakes that computer (`Wake::Joined`) on its
//!   own. A row that fails for a passing reason is tried again from the
//!   alarm; an agent that runs on no computer is told nothing.

use fragment_core::npub;
use fragment_proto::{split_fragment_name, valid_channel_name, valid_fragment_name, ErrorCode, IdentityKind, Role};
use serde::Deserialize;
use serde_json::{json, Value};
use worker::*;

use crate::error::{CellError, CellResult};
use crate::fragment::{Caller, FragmentCell, MetaKey};
use crate::js;
use crate::registry::calls::{self, By};

/// At most one pre-wake a computer from one fragment this often.
const PREWAKE_EVERY_MS: i64 = 30_000;
/// The agent fragment's channel its computer follows for its routines and
/// its new fragments (docs/chat-records.md, `tasks`).
pub(crate) const TASKS: &str = "tasks";
/// Notices one flush sends: a member's change makes one, so a flush has
/// few; the rest wait for the next.
const JOINED_FLUSH_MAX: i64 = 16;
/// Tries at one notice before it is dropped, with an event: with the wait
/// doubling to ten minutes, about two and a half hours of a computer that
/// does not answer.
const JOINED_ATTEMPTS_MAX: i64 = 20;

/// A `joined_outbox` row, as `agent_added` wrote it.
#[derive(Deserialize)]
struct Notice {
    principal: String,
    owner: String,
    added_at: i64,
    attempts: i64,
}

#[derive(Deserialize)]
struct Joined {
    computer: String,
    fragment: String,
    at: i64,
}

/// A failure that trying again may change: the computer, or the agent's
/// fragment, did not answer.
fn passing(code: ErrorCode) -> bool {
    matches!(code, ErrorCode::HostFailed | ErrorCode::UpstreamFailed | ErrorCode::NodeFull | ErrorCode::RateLimited | ErrorCode::RegistryUnavailable)
}

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
                let identity = self.agent_identity(&b.owner).await?;
                self.set_meta(MetaKey::Computer, &b.computer)?;
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
            // its face (kind, title), for a page that names it: `__people`
            // shows an agent by its fragment's title
            "face" => Ok(self.meta(MetaKey::Face)?.and_then(|f| serde_json::from_str::<Value>(&f).ok()).unwrap_or(Value::Null)),
            "identity" => {
                let computer = body["computer"].as_str().unwrap_or("");
                self.assigned_to(computer)?;
                Ok(json!({ "identity": self.must(MetaKey::AgentIdentity)? }))
            }
            "sign" => {
                let b: Sign = serde_json::from_value(body.clone()).map_err(|e| CellError::invalid(format!("body: {e}")))?;
                self.assigned_to(&b.computer)?;
                let sealed = self.must(MetaKey::FragmentSecret)?;
                let opened = crate::keys::open(&self.env, &self.scope(), &sealed).await?;
                if let Some(fresh) = opened.resealed {
                    // only over the value opened (the store's read may yield)
                    self.exec(
                        "UPDATE meta SET value = ? WHERE key = ? AND value = ?",
                        vec![fresh.into(), MetaKey::FragmentSecret.key().into(), sealed.as_str().into()],
                    )?;
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
            "joined" => {
                // this agent was added to `fragment`: its guest hears so on
                // `tasks`, when it declares one it can post to
                let b: Joined = serde_json::from_value(body.clone()).map_err(|e| CellError::invalid(format!("body: {e}")))?;
                self.assigned_to(&b.computer)?;
                if !valid_fragment_name(&b.fragment) || b.at <= 0 {
                    return Err(CellError::invalid("a join names a fragment and when it joined"));
                }
                if self.declared_channel(TASKS)?.is_none_or(|d| d.post.is_none()) {
                    return Ok(json!({ "posted": false, "why": "its fragment declares no postable tasks channel" }));
                }
                let notice = json!({ "kind": "joined", "fragment": b.fragment });
                let (record, appended) = self.append_own(TASKS, &notice, &format!("joined:{}:{}", b.fragment, b.at)).await?;
                assert_eq!(record.channel, TASKS, "the notice is on tasks");
                Ok(json!({ "posted": true, "seq": record.seq, "replayed": !appended }))
            }
            r => Err(CellError::new(ErrorCode::NotFound, format!("no route computer/{r}"))),
        }
    }

    /// The agent this agent fragment is: its own key registered as the
    /// agent's identity, its owner's (once: a computer's assignment or a
    /// machine's pairing makes it), an editor of its own fragment, where
    /// it keeps its own state (its SOUL.md, memories, skills, routines).
    async fn agent_identity(&self, owner: &str) -> CellResult<String> {
        let identity = match self.meta(MetaKey::AgentIdentity)? {
            Some(id) => id,
            None => {
                let npub = self.must(MetaKey::Npub)?;
                let key = npub::parse(&npub).ok_or_else(|| CellError::host("the fragment's npub does not decode"))?;
                // the agent is named for its fragment (a page shows it so: `__people`)
                let register = calls::RegisterAgent { owner: By::Identity(owner.to_string()), key, fragment: Some(self.name()?) };
                let agent = crate::ask_registry(&self.env, &register).await?;
                self.set_meta(MetaKey::AgentIdentity, &agent.id)?;
                agent.id
            }
        };
        if self.member_role(&identity)?.is_none() {
            let as_owner = self.as_owner()?;
            self.set_member(&as_owner, &identity, fragment_proto::SetRole { role: Role::Editor, people_only: false }).await?;
        }
        Ok(identity)
    }

    /// The owner of this agent fragment asking about its machines' keys
    /// (docs/api.md, "A machine's keys"): a person, signed by a key of
    /// theirs, on a fragment of kind `agent`. No agent asks, not even one
    /// acting for them (`registry::may_pair` says so again).
    fn machine_owner<'a>(&self, caller: &'a Caller) -> CellResult<&'a crate::routed::Signed> {
        let signed = caller.signed.as_ref().ok_or_else(|| CellError::new(ErrorCode::Unauthenticated, "sign the request"))?;
        if signed.identity.kind != IdentityKind::Person || self.must(MetaKey::Owner)? != signed.identity.id {
            return Err(CellError::new(ErrorCode::Forbidden, "only the agent fragment's owner pairs their machines to it"));
        }
        let face: Value = self.meta(MetaKey::Face)?.and_then(|f| serde_json::from_str(&f).ok()).unwrap_or(Value::Null);
        if face["kind"] != "agent" {
            return Err(CellError::invalid("a machine is paired to an agent fragment (template agent)"));
        }
        Ok(signed)
    }

    /// `GET /api/f/{agent}/keys`: its machines' keys (none until its first
    /// pairing).
    pub(crate) async fn machine_keys(&self, caller: &Caller) -> CellResult<fragment_proto::PairedKeys> {
        let signed = self.machine_owner(caller)?;
        let Some(agent) = self.meta(MetaKey::AgentIdentity)? else {
            return Ok(fragment_proto::PairedKeys { agent: None, keys: vec![], changed: None });
        };
        crate::ask_registry(&self.env, &calls::Paired { agent, by: By::Identity(signed.identity.id.clone()) }).await
    }

    /// `POST /api/f/{agent}/keys {proof, name}`: a machine of its owner's
    /// paired as this agent's hands. The proof is a NIP-98 event by the
    /// machine's new key for this same request, naming the owner's signing
    /// key (`p`), so the machine holds it and meant it for them. The agent
    /// gets its identity first when it has none (no computer runs it).
    pub(crate) async fn pair_machine(&self, caller: &Caller, body: &[u8]) -> CellResult<fragment_proto::PairedKeys> {
        let signed = self.machine_owner(caller)?;
        let b: fragment_proto::PairKey = serde_json::from_slice(body).map_err(|e| CellError::invalid(format!("body: {e}")))?;
        let signer = signed.key.as_deref().ok_or_else(|| CellError::new(ErrorCode::Unauthenticated, "pairing is signed by a key you hold (`fragment login`)"))?;
        let now_s = js::now_ms() / 1000;
        let key = fragment_nip98::verify_proof(&b.proof, "POST", caller.url.as_str(), signer, now_s, fragment_proto::limits::AUTH_WINDOW_S)
            .map_err(|e| CellError::invalid(format!("proof: {e}")))?;
        if key == signer {
            return Err(CellError::invalid("proof: the machine's key must not be the key that signs the request"));
        }
        let owner = signed.identity.id.clone();
        let agent = self.agent_identity(&owner).await?;
        let paired = crate::ask_registry(&self.env, &calls::Pair { agent, key: key.clone(), name: b.name.clone(), by: By::Identity(owner) }).await?;
        if paired.changed == Some(true) {
            self.event("machine.paired", &format!("{} paired as its hands", b.name), json!({ "name": b.name, "npub": npub::encode(&key) }));
        }
        Ok(paired)
    }

    /// `DELETE /api/f/{agent}/keys/{npub}`: a machine unpaired; its key is
    /// 401 from the next request on.
    pub(crate) async fn unpair_machine(&self, caller: &Caller, named: &str) -> CellResult<fragment_proto::PairedKeys> {
        let signed = self.machine_owner(caller)?;
        let key = npub::parse(named).ok_or_else(|| CellError::invalid(format!("{named:?} is not an npub or a 64-hex key")))?;
        let agent = self.meta(MetaKey::AgentIdentity)?.ok_or_else(|| CellError::new(ErrorCode::NotFound, "no machine is paired to this agent"))?;
        let unpaired = crate::ask_registry(&self.env, &calls::Unpair { agent, key: key.clone(), by: By::Identity(signed.identity.id.clone()) }).await?;
        if unpaired.changed == Some(true) {
            let name = unpaired.keys.iter().find(|k| npub::parse(&k.npub).as_deref() == Some(key.as_str())).map_or("a machine", |k| k.name.as_str());
            self.event("machine.unpaired", &format!("{name} unpaired: its key signs nothing from now on"), json!({ "name": name, "npub": npub::encode(&key) }));
        }
        Ok(unpaired)
    }

    /// An agent became a member here (`set_member`, `join`): its computer
    /// is to hear so, in the turn of the membership's write (the row is
    /// sent by `flush_joined`). A deployment without computers tells no one.
    pub(crate) fn agent_added(&self, identity: &str, owner: Option<&str>, added_at: i64) -> CellResult<()> {
        if self.cfg.computer_image.is_none() {
            return Ok(());
        }
        let owner = owner.ok_or_else(|| CellError::host(format!("agent {identity} has no owner")))?;
        assert!(npub::is_identity(identity), "a notice names an agent's identity, not {identity:?}");
        assert!(added_at > 0, "a notice names when its agent joined");
        self.exec(
            "INSERT INTO joined_outbox (principal, owner, added_at, attempts, next_at) VALUES (?, ?, ?, 0, ?)
             ON CONFLICT (principal) DO UPDATE SET owner = excluded.owner, added_at = excluded.added_at, attempts = 0, next_at = excluded.next_at",
            vec![identity.into(), owner.into(), SqlStorageValue::Integer(added_at), SqlStorageValue::Integer(js::now_ms())],
        )
    }

    /// A member that left takes its notice with it.
    pub(crate) fn agent_removed(&self, identity: &str) -> CellResult<()> {
        self.exec("DELETE FROM joined_outbox WHERE principal = ?", vec![identity.into()])
    }

    /// When the joined outbox next has a row due (for the alarm).
    pub(crate) fn joined_due_at(&self) -> CellResult<Option<i64>> {
        Ok(self.rows("SELECT MIN(next_at) AS at FROM joined_outbox", vec![])?.first().and_then(|r| r["at"].as_i64()))
    }

    /// Sends the due notices, oldest first: each agent's owner's computer
    /// has the agent's fragment post `joined`, then is woken on its own,
    /// so the member's change never waits for a start (as a pre-wake does
    /// not). A row goes once its computer answered, or for good when
    /// trying again cannot help (the agent runs nowhere, or moved); a
    /// passing failure waits, doubling, and after its last try goes with
    /// an event (never silently).
    pub(crate) async fn flush_joined(&self) {
        let Ok(name) = self.name() else { return };
        let Ok(due) = self.typed::<Notice>(
            "SELECT principal, owner, added_at, attempts FROM joined_outbox WHERE next_at <= ? ORDER BY next_at LIMIT ?",
            vec![SqlStorageValue::Integer(js::now_ms()), SqlStorageValue::Integer(JOINED_FLUSH_MAX)],
        ) else {
            return;
        };
        let mut waiting = false;
        for n in due {
            // the agent's owner's one computer (decision 13): an agent runs only on its owner's
            let computer = fragment_core::computer::default_computer_of(&n.owner);
            let asked = json!({ "identity": n.principal, "fragment": name, "at": n.added_at });
            let told = crate::computer::ask(&self.env, &computer, "computer/joined", &asked).await;
            // only this membership's row: a newer one (left and added again) stays
            let done = |cell: &FragmentCell| {
                let _ = cell.exec("DELETE FROM joined_outbox WHERE principal = ? AND added_at = ?", vec![n.principal.as_str().into(), SqlStorageValue::Integer(n.added_at)]);
            };
            match told {
                Ok(v) if v["runs"] == true => {
                    done(self);
                    let heard = if v["posted"] == true { "its tasks say so" } else { "its fragment has no tasks to say so on" };
                    self.event(
                        "agent.told",
                        &format!("{}'s computer is told it joined, and woken ({heard})", npub::display(&n.principal)),
                        json!({ "principal": npub::display(&n.principal), "computer": computer, "agent": v["agent"], "posted": v["posted"] }),
                    );
                    let env = self.env.clone();
                    worker::wasm_bindgen_futures::spawn_local(async move {
                        if let Err(e) = crate::computer::ask(&env, &computer, "computer/wake", &json!({ "why": "joined" })).await {
                            console_log!("{}", json!({ "fragment": "joined-wake", "computer": computer, "error": e.message }));
                        }
                    });
                }
                Ok(_) => done(self),
                Err(e) if !passing(e.code) => {
                    done(self);
                    if e.code != ErrorCode::NotFound {
                        self.event("agent.untold", &format!("{}'s computer was not told it joined: {}", npub::display(&n.principal), e.message), json!({ "code": e.code }));
                    }
                }
                Err(e) => {
                    let attempts = n.attempts + 1;
                    if attempts >= JOINED_ATTEMPTS_MAX {
                        done(self);
                        self.event("agent.untold", &format!("{}'s computer did not answer in {attempts} tries: {}", npub::display(&n.principal), e.message), json!({ "code": e.code }));
                        continue;
                    }
                    waiting = true;
                    let next_at = js::now_ms() + fragment_core::backoff::outbox_retry_ms(attempts);
                    let _ = self.exec(
                        "UPDATE joined_outbox SET attempts = ?, next_at = ? WHERE principal = ? AND added_at = ?",
                        vec![SqlStorageValue::Integer(attempts), SqlStorageValue::Integer(next_at), n.principal.as_str().into(), SqlStorageValue::Integer(n.added_at)],
                    );
                }
            }
        }
        if waiting {
            let _ = self.schedule().await;
        }
    }

    /// The fragment's owner as a caller, for what the platform does on the
    /// owner's behalf here (adding the agent as a member).
    fn as_owner(&self) -> CellResult<crate::fragment::Caller> {
        let (name, owner) = (self.name()?, self.must(MetaKey::Owner)?);
        let (_, username) = split_fragment_name(&name).ok_or_else(|| CellError::host(format!("{name} is not <label>.<username>")))?;
        let identity = fragment_proto::Identity { id: owner, kind: IdentityKind::Person, owner: None, username: Some(username.to_string()), held: None };
        let signed = crate::routed::Signed::new(identity, None);
        Ok(crate::fragment::Caller { signed: Some(signed), unresolved: None, url: url::Url::parse("https://fragment.internal/").expect("a URL"), site: false })
    }

    /// This agent fragment runs on `computer`, or refuses.
    fn assigned_to(&self, computer: &str) -> CellResult<()> {
        if self.meta(MetaKey::Computer)?.as_deref() != Some(computer) {
            return Err(CellError::new(ErrorCode::Forbidden, "this agent does not run on that computer"));
        }
        Ok(())
    }

    /// The computers `principal`'s socket on this fragment pre-wakes: those
    /// its channels wake, but none that `principal`'s own wake subscriptions
    /// name (`fragment_core::computer::prewoken`; at most `SUBS_MAX` rows).
    pub(crate) fn woken_computers(&self, principal: &str) -> CellResult<Vec<String>> {
        let rows = self.rows("SELECT principal, url FROM subs WHERE url LIKE 'computer:%' ORDER BY id", vec![])?;
        let subs: Vec<(&str, &str)> = rows.iter().filter_map(|r| Some((r["principal"].as_str()?, r["url"].as_str()?))).collect();
        Ok(fragment_core::computer::prewoken(&subs, principal).into_iter().map(|u| format!("computer:{}", u.trim_start_matches("computer:"))).collect())
    }

    /// A page opened this fragment (`principal`'s socket): the computers its
    /// channels wake may hear from it soon, so each starts now (decision 39),
    /// at most every `PREWAKE_EVERY_MS`. Each wake runs on its own, so the
    /// page's socket never waits for a start; a failed pre-wake costs only
    /// the wait. An agent's own socket (its guest following this fragment,
    /// opened as it boots and again after one drops) pre-wakes no computer
    /// it runs on: one opened as that computer goes to sleep would start it
    /// again at once.
    pub(crate) fn prewake(&self, principal: &str) {
        let now = crate::js::now_ms();
        let due = self.meta(MetaKey::PrewakeAt).ok().flatten().and_then(|v| v.parse::<i64>().ok()).is_none_or(|at| now >= at);
        if !due {
            return;
        }
        let Ok(computers) = self.woken_computers(principal) else { return };
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
