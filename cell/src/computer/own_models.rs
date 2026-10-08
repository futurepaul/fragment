//! A person's own models, kept by their computer beside their own keys
//! (docs/optchat.md, "Your own models"; providers/mod.rs calls these): their
//! choice for each role, their Sign in with ChatGPT tokens (sealed for this
//! object, as an own key is, and refreshed here: one object, so a rotating
//! refresh token is never spent twice), and what their own models' calls
//! used, counted by month, never charged.
//!
//! - `computer/models` → `{choices, own, chatgpt, uses}`: the choices by
//!   role, the own keys' providers, the sign-in's state (`connected`,
//!   `needs_reauthorization`, `not_connected`; its account, client id, this
//!   host's id and the access token's expiry), and this month's counts.
//! - `computer/model-choice {role, choice}`: a role's choice (`null`: the
//!   default, kept as none).
//! - `computer/model-call {role}` → `{choice, credential?}`: the choice,
//!   with an own provider's credential for the call (a ChatGPT token
//!   refreshed when it has less than five minutes left).
//! - `computer/model-credential {provider}` → `{credential}`: for listing a
//!   provider's models.
//! - `computer/model-used {provider, model, role, counted}`.
//! - `computer/chatgpt {tokens}`: the sign-in's tokens kept, or (`null`)
//!   revoked at OpenAI and forgotten; the client id and the host's id stay
//!   for the next sign-in.

use fragment_core::providers::{self as own, ChatgptTokens, Choice, Counted, Role, Vendor};
use fragment_proto::computer::ModelUse;
use serde::Serialize;

use super::*;

/// A ChatGPT access token with less than this left is refreshed first.
const REFRESH_BEFORE_MS: i64 = 5 * 60_000;
/// A refresh holds the others this long at most, while it runs.
const REFRESH_LEASE_MS: i64 = 30_000;
/// How often a call waiting on another's refresh looks again.
const REFRESH_WAIT_MS: u64 = 250;
/// A token endpoint's answer, read whole: at most this.
const TOKEN_ANSWER_MAX_BYTES: usize = 64 * 1024;
/// Model-use rows a month keeps at most (providers × models × roles).
const MODEL_USES_ROWS_MAX: i64 = 256;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ChoiceAsk {
    role: Role,
    choice: Option<Choice>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CallAsk {
    role: Role,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CredentialAsk {
    provider: Vendor,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelUsed {
    provider: Vendor,
    model: String,
    role: Role,
    counted: Counted,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ChatgptAsk {
    tokens: Option<ChatgptTokens>,
}

/// The sign-in's row.
#[derive(Deserialize)]
struct Signin {
    client_id: String,
    sealed: Option<String>,
    expires_at: i64,
    scopes: String,
    email: Option<String>,
    state: String,
    refreshing_until: i64,
}

/// The sealed part of a sign-in: its two tokens.
#[derive(Serialize, Deserialize)]
struct Sealed {
    access_token: String,
    refresh_token: String,
}

impl ComputerCell {
    pub(super) async fn own_models(&self, path: &str, req: &mut Request) -> CellResult<Response> {
        self.must(MetaKey::Id)?;
        match path {
            "computer/models" => json_response(&self.models_view()?),
            "computer/model-choice" => {
                let b: ChoiceAsk = body_json(req).await?;
                match b.choice.filter(|c| !c.is_default()) {
                    Some(c) => {
                        c.check().map_err(CellError::invalid)?;
                        self.exec(
                            "INSERT INTO model_choices (role, provider, model, set_at) VALUES (?, ?, ?, ?) ON CONFLICT (role) DO UPDATE SET provider = excluded.provider, model = excluded.model, set_at = excluded.set_at",
                            vec![b.role.as_str().into(), c.provider.as_str().into(), c.model.as_str().into(), js::now_ms().into()],
                        )?;
                    }
                    None => self.exec("DELETE FROM model_choices WHERE role = ?", vec![b.role.as_str().into()])?,
                }
                json_response(&self.models_view()?)
            }
            "computer/model-call" => {
                let b: CallAsk = body_json(req).await?;
                let choice = self.choice_of(b.role)?;
                let credential = match &choice {
                    Some(c) if c.provider != Vendor::Fragment => Some(self.credential(c.provider).await?),
                    _ => None,
                };
                json_response(&json!({ "choice": choice, "credential": credential }))
            }
            "computer/model-credential" => {
                let b: CredentialAsk = body_json(req).await?;
                json_response(&json!({ "credential": self.credential(b.provider).await? }))
            }
            "computer/model-used" => {
                let b: ModelUsed = body_json(req).await?;
                self.count_model(&b)?;
                json_response(&json!({ "ok": true }))
            }
            "computer/chatgpt" => {
                let b: ChatgptAsk = body_json(req).await?;
                let revoked = match b.tokens {
                    Some(t) => {
                        own::tokens_check(&t).map_err(CellError::invalid)?;
                        self.keep_signin(&t).await?;
                        None
                    }
                    None => Some(self.forget_signin().await?),
                };
                let mut v = self.models_view()?;
                v["revoked"] = json!(revoked);
                json_response(&v)
            }
            p => Err(CellError::new(ErrorCode::NotFound, format!("no route {p}"))),
        }
    }

    fn choice_of(&self, role: Role) -> CellResult<Option<Choice>> {
        let rows = self.rows("SELECT provider, model FROM model_choices WHERE role = ?", vec![role.as_str().into()])?;
        Ok(rows.first().and_then(|r| Some(Choice { provider: Vendor::parse(r["provider"].as_str()?)?, model: r["model"].as_str()?.to_string() })))
    }

    /// This host's id for Sign in with ChatGPT, made once and kept through
    /// sign-outs (its `ext_agent_host_id`).
    fn host_id(&self) -> CellResult<String> {
        if let Some(id) = self.meta(MetaKey::ChatgptHost)? {
            return Ok(id);
        }
        let id = own::host_id(js::random_bytes());
        self.set_meta(MetaKey::ChatgptHost, &id)?;
        Ok(id)
    }

    fn signin(&self) -> CellResult<Option<Signin>> {
        let rows: Vec<Signin> = self
            .sql()
            .exec("SELECT client_id, sealed, expires_at, scopes, email, state, refreshing_until FROM chatgpt WHERE one = 1", None)?
            .to_array()?;
        Ok(rows.into_iter().next())
    }

    fn models_view(&self) -> CellResult<Value> {
        let mut choices = serde_json::Map::new();
        for role in Role::ALL {
            if let Some(c) = self.choice_of(role)? {
                choices.insert(role.as_str().into(), json!(c));
            }
        }
        let own_keys: Vec<String> = self.own_key_names()?.into_iter().filter(|p| p == "anthropic" || p == "openai").collect();
        let s = self.signin()?;
        let chatgpt = json!({
            "state": s.as_ref().map_or("not_connected", |s| s.state.as_str()),
            "account": s.as_ref().and_then(|s| s.email.clone()),
            "clientId": s.as_ref().map(|s| s.client_id.clone()),
            "expiresAt": s.as_ref().filter(|s| s.sealed.is_some()).map(|s| s.expires_at),
            "scopes": s.as_ref().map(|s| s.scopes.clone()),
            "hostId": self.host_id()?,
        });
        Ok(json!({ "choices": choices, "own": own_keys, "chatgpt": chatgpt, "uses": self.model_uses(Month::of(js::now_ms()))? }))
    }

    /// A month's counts of the owner's own models' calls.
    pub(super) fn model_uses(&self, month: Month) -> CellResult<Vec<ModelUse>> {
        let rows = self.rows(
            "SELECT provider, model, role, calls, input, cached, cache_write, output FROM model_uses WHERE month = ? ORDER BY provider, model, role",
            vec![i64::from(month.0).into()],
        )?;
        let n = |r: &Value, k: &str| r[k].as_u64().unwrap_or(0);
        Ok(rows
            .iter()
            .map(|r| ModelUse {
                provider: r["provider"].as_str().unwrap_or_default().into(),
                model: r["model"].as_str().unwrap_or_default().into(),
                role: r["role"].as_str().unwrap_or_default().into(),
                calls: n(r, "calls"),
                input: n(r, "input"),
                cached: n(r, "cached"),
                cache_write: n(r, "cache_write"),
                output: n(r, "output"),
            })
            .collect())
    }

    /// Counts one call's use, in this month, and forgets months past
    /// `USES_MONTHS_KEPT`. A month already holding its most rows takes no
    /// new one (the call is logged).
    fn count_model(&self, b: &ModelUsed) -> CellResult<()> {
        if b.provider == Vendor::Fragment || !own::valid_model_id(&b.model) {
            return Err(CellError::invalid("a use names an own provider and its model"));
        }
        let c = &b.counted;
        let largest = c.input.max(c.cached).max(c.cache_write).max(c.output);
        if largest > fragment_core::price::QUANTITY_MAX {
            return Err(CellError::invalid("a use's tokens are past any call's"));
        }
        let month = Month::of(js::now_ms());
        let m = i64::from(month.0);
        let rows = self.rows("SELECT COUNT(*) AS n FROM model_uses WHERE month = ?", vec![m.into()])?;
        let held = rows.first().and_then(|r| r["n"].as_i64()).unwrap_or(0);
        let there = !self.rows("SELECT 1 FROM model_uses WHERE month = ? AND provider = ? AND model = ? AND role = ?", vec![m.into(), b.provider.as_str().into(), b.model.as_str().into(), b.role.as_str().into()])?.is_empty();
        if !there && held >= MODEL_USES_ROWS_MAX {
            console_error!("{}", json!({ "computer": "model-uses-full", "month": month.label(), "provider": b.provider, "model": b.model }));
            return Ok(());
        }
        let i = |n: u64| SqlStorageValue::Integer(i64::try_from(n).expect("bounded by QUANTITY_MAX"));
        self.exec(
            "INSERT INTO model_uses (month, provider, model, role, calls, input, cached, cache_write, output) VALUES (?, ?, ?, ?, 1, ?, ?, ?, ?)
             ON CONFLICT (month, provider, model, role) DO UPDATE SET calls = calls + 1, input = input + excluded.input, cached = cached + excluded.cached, cache_write = cache_write + excluded.cache_write, output = output + excluded.output",
            vec![m.into(), b.provider.as_str().into(), b.model.as_str().into(), b.role.as_str().into(), i(c.input), i(c.cached), i(c.cache_write), i(c.output)],
        )?;
        self.exec("DELETE FROM model_uses WHERE month < ?", vec![(m - i64::from(USES_MONTHS_KEPT) + 1).into()])
    }

    /// `provider`'s credential for a call: an own key, or the sign-in's
    /// access token (refreshed first when it is nearly spent).
    async fn credential(&self, provider: Vendor) -> CellResult<String> {
        match provider {
            Vendor::Fragment => Err(CellError::invalid("Fragment's models take no credential")),
            Vendor::Chatgpt => self.chatgpt_token().await,
            v => {
                let row = v.catalog_row().expect("an own key's provider has its catalog row");
                self.own_key(row).await?.ok_or_else(|| CellError::new(ErrorCode::NotConnected, format!("give your {row} key first (Settings, Models; PUT /api/connections/{row}/key)")))
            }
        }
    }

    /// The sign-in's tokens, opened (resealed when the host secret rotated).
    async fn opened(&self, sealed: &str) -> CellResult<Sealed> {
        let scope = crate::keys::scope(SEAL_CLASS, &self.state);
        let opened = crate::keys::open(&self.env, &scope, sealed).await?;
        if let Some(resealed) = opened.resealed {
            self.exec("UPDATE chatgpt SET sealed = ? WHERE one = 1 AND sealed = ?", vec![resealed.into(), sealed.into()])?;
        }
        serde_json::from_slice(&opened.plaintext).map_err(|e| CellError::host(format!("the sign-in's sealed tokens: {e}")))
    }

    /// Keeps a sign-in's tokens, sealed, connected.
    async fn keep_signin(&self, t: &ChatgptTokens) -> CellResult<()> {
        let sealed = serde_json::to_vec(&Sealed { access_token: t.access_token.clone(), refresh_token: t.refresh_token.clone() }).expect("tokens serialize");
        let sealed = crate::keys::seal(&self.env, &crate::keys::scope(SEAL_CLASS, &self.state), &sealed).await?;
        let now = js::now_ms();
        self.exec(
            "INSERT INTO chatgpt (one, client_id, sealed, expires_at, scopes, email, state, refreshing_until, set_at) VALUES (1, ?, ?, ?, ?, ?, 'connected', 0, ?)
             ON CONFLICT (one) DO UPDATE SET client_id = excluded.client_id, sealed = excluded.sealed, expires_at = excluded.expires_at, scopes = excluded.scopes, email = COALESCE(excluded.email, email), state = 'connected', refreshing_until = 0, set_at = excluded.set_at",
            vec![
                t.client_id.as_str().into(),
                sealed.into(),
                (now + t.expires_in * 1000).into(),
                t.scope.as_str().into(),
                t.email.clone().map_or(SqlStorageValue::Null, SqlStorageValue::from),
                now.into(),
            ],
        )
    }

    /// The sign-in's access token for a call: as kept while it has more
    /// than `REFRESH_BEFORE_MS` left, else refreshed, by one call at a time
    /// (a lease in its row: another call waits, or uses the old token while
    /// it is still good).
    async fn chatgpt_token(&self) -> CellResult<String> {
        let gone = || CellError::new(ErrorCode::NotConnected, "sign in with ChatGPT again: run `fragment connect chatgpt`");
        // bounded: a lease is at most REFRESH_LEASE_MS, looked at every REFRESH_WAIT_MS
        for _ in 0..(REFRESH_LEASE_MS as u64 / REFRESH_WAIT_MS + 2) {
            let s = self.signin()?.ok_or_else(gone)?;
            let sealed = s.sealed.clone().ok_or_else(gone)?;
            let now = js::now_ms();
            if s.expires_at - now > REFRESH_BEFORE_MS {
                return Ok(self.opened(&sealed).await?.access_token);
            }
            if s.refreshing_until > now {
                if s.expires_at - now > 10_000 {
                    return Ok(self.opened(&sealed).await?.access_token);
                }
                Delay::from(std::time::Duration::from_millis(REFRESH_WAIT_MS)).await;
                continue;
            }
            // the lease, taken with no await since the read: no other call holds it
            self.exec("UPDATE chatgpt SET refreshing_until = ? WHERE one = 1", vec![(now + REFRESH_LEASE_MS).into()])?;
            let refreshed = self.refresh(&s, &sealed).await;
            self.exec("UPDATE chatgpt SET refreshing_until = 0 WHERE one = 1", vec![])?;
            return refreshed;
        }
        Err(CellError::new(ErrorCode::UpstreamFailed, "ChatGPT's sign-in is being refreshed: try again"))
    }

    /// Refreshes the sign-in at OpenAI's token endpoint and keeps what it
    /// answers. Tokens it refuses for good are dropped (`needs_reauthorization`:
    /// sign in again); a failure that may pass keeps them.
    async fn refresh(&self, s: &Signin, sealed: &str) -> CellResult<String> {
        let tokens = self.opened(sealed).await?;
        let form = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("grant_type", "refresh_token")
            .append_pair("client_id", &s.client_id)
            .append_pair("refresh_token", &tokens.refresh_token)
            .append_pair("resource", own::CHATGPT_RESOURCE)
            .finish();
        let headers = [("content-type", "application/x-www-form-urlencoded"), ("accept", "application/json")];
        let mut resp = crate::providers::vendor_fetch(self.cfg, Method::Post, own::CHATGPT_TOKEN_URL, &headers, Some(form.into_bytes())).await?;
        let status = resp.status_code();
        let (body, _) = crate::cs::read_answer(&mut resp, TOKEN_ANSWER_MAX_BYTES).await?;
        let v: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
        if status != 200 {
            if own::refresh_spent(status, &v) {
                self.exec("UPDATE chatgpt SET sealed = NULL, state = 'needs_reauthorization' WHERE one = 1", vec![])?;
                console_log!("{}", json!({ "computer": "chatgpt-refresh-spent", "status": status }));
                return Err(CellError::new(ErrorCode::NotConnected, "ChatGPT's sign-in ended: run `fragment connect chatgpt` again"));
            }
            return Err(CellError::new(ErrorCode::UpstreamFailed, format!("ChatGPT's sign-in did not refresh ({status}); it is tried again at the next call")));
        }
        let next = own::refreshed(&v, &s.client_id, &s.scopes, s.email.clone()).map_err(|why| CellError::new(ErrorCode::UpstreamFailed, format!("ChatGPT's refresh: {why}")))?;
        self.keep_signin(&next).await?;
        console_log!("{}", json!({ "computer": "chatgpt-refreshed" }));
        Ok(next.access_token)
    }

    /// Signs out: the refresh token revoked at OpenAI (its discovery's
    /// `revocation_endpoint`), then the tokens forgotten whatever OpenAI
    /// said. Answers whether OpenAI confirmed it.
    async fn forget_signin(&self) -> CellResult<bool> {
        let Some(s) = self.signin()? else { return Ok(false) };
        let mut confirmed = false;
        if let Some(sealed) = &s.sealed {
            let tokens = self.opened(sealed).await?;
            confirmed = self.revoke(&s.client_id, &tokens.refresh_token).await.unwrap_or_else(|e| {
                console_error!("{}", json!({ "computer": "chatgpt-revoke", "error": e.message }));
                false
            });
        }
        self.exec("UPDATE chatgpt SET sealed = NULL, state = 'not_connected', refreshing_until = 0 WHERE one = 1", vec![])?;
        Ok(confirmed)
    }

    async fn revoke(&self, client_id: &str, refresh_token: &str) -> CellResult<bool> {
        let discovery = format!("{}/.well-known/openid-configuration", own::CHATGPT_ISSUER);
        let mut resp = crate::providers::vendor_fetch(self.cfg, Method::Get, &discovery, &[("accept", "application/json")], None).await?;
        let (body, _) = crate::cs::read_answer(&mut resp, TOKEN_ANSWER_MAX_BYTES).await?;
        let v: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
        let Some(endpoint) = v["revocation_endpoint"].as_str().filter(|e| e.starts_with(own::CHATGPT_ISSUER)) else { return Ok(false) };
        let form = url::form_urlencoded::Serializer::new(String::new()).append_pair("token", refresh_token).append_pair("token_type_hint", "refresh_token").append_pair("client_id", client_id).finish();
        let resp = crate::providers::vendor_fetch(self.cfg, Method::Post, endpoint, &[("content-type", "application/x-www-form-urlencoded")], Some(form.into_bytes())).await?;
        Ok(resp.status_code() == 200)
    }
}
