//! Connected clients (docs/api.md, Connected clients): the authorization
//! server's rows. A client registers (or names its metadata document,
//! which the router reads), a signed-in person says yes to it acting as
//! them on one resource (an MCP server of the platform's), and its code
//! becomes a connection: an access token and a refresh token, each kept
//! here only as its SHA-256, the refresh token replaced at every use. A
//! connection is a credential, as a session is: it names a person and
//! grants nothing; the resource it reaches decides what they may do there.
//!
//! Every table is bounded: registrations by `oauth::CLIENTS_MAX` (the
//! oldest go, as pending sign-ins do), a person's codes and connections by
//! `CODES_PER_PERSON_MAX` and `CONNECTIONS_PER_PERSON_MAX` (the oldest
//! end). Expired codes and connections go on the Registry's sweep
//! (signin.rs), never on a request.

use fragment_core::oauth::{self, Client, Error, Grant, Refused, Tokens};
use fragment_proto::{Connection, Connections};
use serde::de::IgnoredAny;

use super::calls::{ClientRegistered, Disconnect, FindClient, GrantCode, Granted, IssueTokens, Issued, ListConnections, RegisterClient, RevokeToken};
use super::signin::{fresh_token, sha};
use super::*;

pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS oauth_clients (
  id TEXT PRIMARY KEY, name TEXT NOT NULL, redirect_uris TEXT NOT NULL, created_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS oauth_codes (
  hash TEXT PRIMARY KEY, identity TEXT NOT NULL, client_id TEXT NOT NULL, client TEXT NOT NULL, redirect_uri TEXT NOT NULL,
  challenge TEXT NOT NULL, resource TEXT NOT NULL, expires_at INTEGER NOT NULL);
CREATE INDEX IF NOT EXISTS oauth_codes_identity ON oauth_codes (identity, expires_at);
CREATE INDEX IF NOT EXISTS oauth_codes_expires ON oauth_codes (expires_at);
CREATE TABLE IF NOT EXISTS connections (
  id TEXT PRIMARY KEY, identity TEXT NOT NULL, client_id TEXT NOT NULL, client TEXT NOT NULL, resource TEXT NOT NULL,
  access_hash TEXT NOT NULL UNIQUE, access_expires_at INTEGER NOT NULL, refresh_hash TEXT NOT NULL UNIQUE,
  refresh_expires_at INTEGER NOT NULL, created_at INTEGER NOT NULL);
CREATE INDEX IF NOT EXISTS connections_identity ON connections (identity, created_at);
CREATE INDEX IF NOT EXISTS connections_expires ON connections (refresh_expires_at);
";

/// `oauth_codes` (one spent).
#[derive(Deserialize)]
struct CodeRow {
    identity: String,
    client_id: String,
    client: String,
    redirect_uri: String,
    challenge: String,
    resource: String,
}

/// `connections`, as a refresh reads it.
#[derive(Deserialize)]
struct RefreshRow {
    id: String,
    client_id: String,
    resource: String,
}

#[derive(Deserialize)]
struct RowidRow {
    n: i64,
}

/// `oauth_clients`.
#[derive(Deserialize)]
struct ClientRow {
    name: String,
    redirect_uris: String,
}

fn no(error: Error, why: &str) -> Issued {
    Issued::Refused(Refused { error, why: why.to_string() })
}

impl RegistryCell {
    /// A client registered: kept among the newest `CLIENTS_MAX` (a range
    /// of the rowid, as pending sign-ins are: one row a registration).
    pub(super) fn register_client(&self, RegisterClient(client): RegisterClient) -> CellResult<ClientRegistered> {
        assert!(!client.redirect_uris.is_empty() && client.redirect_uris.len() <= oauth::REDIRECT_URIS_MAX, "the router checked the registration");
        let id = hex::encode(js::random_bytes::<16>());
        let now = js::now_ms();
        let uris = serde_json::to_string(&client.redirect_uris).map_err(|e| CellError::host(format!("redirect URIs: {e}")))?;
        let n = self
            .row::<RowidRow>(
                "INSERT INTO oauth_clients (id, name, redirect_uris, created_at) VALUES (?, ?, ?, ?) RETURNING rowid AS n",
                vec![id.as_str().into(), client.name.as_str().into(), uris.into(), SqlStorageValue::Integer(now)],
            )?
            .ok_or_else(|| CellError::host("a registration's insert answered no rowid"))?
            .n;
        let cap = i64::try_from(oauth::CLIENTS_MAX).expect("the cap fits a rowid");
        self.exec("DELETE FROM oauth_clients WHERE rowid <= ?", vec![SqlStorageValue::Integer(n.saturating_sub(cap))])?;
        Ok(ClientRegistered { client_id: id, issued_at_ms: now })
    }

    pub(super) fn find_client(&self, b: FindClient) -> CellResult<Client> {
        let row = self.row::<ClientRow>("SELECT name, redirect_uris FROM oauth_clients WHERE id = ?", vec![b.client_id.as_str().into()])?;
        let row = row.ok_or_else(|| CellError::new(ErrorCode::NotFound, format!("no client {} is registered here: it registers again", b.client_id)))?;
        let redirect_uris = serde_json::from_str(&row.redirect_uris).map_err(|e| CellError::host(format!("oauth_clients.redirect_uris of {}: {e}", b.client_id)))?;
        Ok(Client { name: row.name, redirect_uris })
    }

    /// The person a live platform session names said yes: a code for the
    /// client, kept among their newest `CODES_PER_PERSON_MAX`.
    pub(super) async fn grant_code(&self, b: GrantCode) -> CellResult<Granted> {
        let who = self.live_session(&b.token, None, false)?.session.identity;
        self.not_wiping(&who)?;
        if who.kind != IdentityKind::Person {
            return Err(CellError::new(ErrorCode::Forbidden, "only a person connects a client"));
        }
        let expires_at = js::now_ms() + oauth::CODE_TTL_MS;
        self.sweep_by(expires_at).await?;
        let code = fresh_token();
        self.exec(
            "INSERT INTO oauth_codes (hash, identity, client_id, client, redirect_uri, challenge, resource, expires_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            vec![
                sha(&code).into(),
                who.id.as_str().into(),
                b.client_id.as_str().into(),
                b.client.as_str().into(),
                b.redirect_uri.as_str().into(),
                b.challenge.as_str().into(),
                b.resource.as_str().into(),
                SqlStorageValue::Integer(expires_at),
            ],
        )?;
        self.exec(
            "DELETE FROM oauth_codes WHERE hash IN (SELECT hash FROM oauth_codes WHERE identity = ? ORDER BY expires_at DESC LIMIT -1 OFFSET ?)",
            vec![who.id.as_str().into(), SqlStorageValue::Integer(oauth::CODES_PER_PERSON_MAX as i64)],
        )?;
        Ok(Granted { code })
    }

    /// Tokens for a code (once: it is spent as it is read) or for a
    /// refresh token (replaced with the access token it renews). A refusal
    /// is OAuth's, typed (`Issued::Refused`), and changes nothing but the
    /// code it spent.
    pub(super) async fn issue_tokens(&self, IssueTokens(grant): IssueTokens) -> CellResult<Issued> {
        let now = js::now_ms();
        match grant {
            Grant::Code { code, client_id, redirect_uri, verifier, resource } => {
                let row = self.row::<CodeRow>(
                    "DELETE FROM oauth_codes WHERE hash = ? AND expires_at > ? RETURNING identity, client_id, client, redirect_uri, challenge, resource",
                    vec![sha(&code).into(), SqlStorageValue::Integer(now)],
                )?;
                let Some(row) = row else { return Ok(no(Error::InvalidGrant, "this code expired, was used, or was never issued")) };
                if row.client_id != client_id || row.redirect_uri != redirect_uri {
                    return Ok(no(Error::InvalidGrant, "this code was issued to another client, or for another redirect URI"));
                }
                if !oauth::pkce_matches(&verifier, &row.challenge) {
                    return Ok(no(Error::InvalidGrant, "code_verifier does not match the code_challenge"));
                }
                if resource.is_some_and(|r| r != row.resource) {
                    return Ok(no(Error::InvalidTarget, "this code is for another resource"));
                }
                // a person a wipe of whom runs connects nothing (their codes went as it began)
                let who = self.stored_identity(&row.identity, "a code")?;
                self.not_wiping(&who)?;
                let refresh_expires_at = now + oauth::REFRESH_TTL_MS;
                self.sweep_by(refresh_expires_at).await?;
                let (access, refresh) = (fresh_token(), fresh_token());
                self.exec(
                    "INSERT INTO connections (id, identity, client_id, client, resource, access_hash, access_expires_at, refresh_hash, refresh_expires_at, created_at)
                     VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                    vec![
                        hex::encode(js::random_bytes::<8>()).into(),
                        who.id.as_str().into(),
                        row.client_id.as_str().into(),
                        row.client.as_str().into(),
                        row.resource.as_str().into(),
                        sha(&access).into(),
                        SqlStorageValue::Integer(now + oauth::ACCESS_TTL_MS),
                        sha(&refresh).into(),
                        SqlStorageValue::Integer(refresh_expires_at),
                        SqlStorageValue::Integer(now),
                    ],
                )?;
                // bounded: this one and the person's newest others; the oldest end
                self.exec(
                    "DELETE FROM connections WHERE id IN (SELECT id FROM connections WHERE identity = ? ORDER BY created_at DESC, rowid DESC LIMIT -1 OFFSET ?)",
                    vec![who.id.as_str().into(), SqlStorageValue::Integer(oauth::CONNECTIONS_PER_PERSON_MAX as i64)],
                )?;
                Ok(Issued::Tokens(Tokens::bearer(access, refresh)))
            }
            Grant::Refresh { refresh, client_id, resource } => {
                let row = self.row::<RefreshRow>(
                    "SELECT id, client_id, resource FROM connections WHERE refresh_hash = ? AND refresh_expires_at > ?",
                    vec![sha(&refresh).into(), SqlStorageValue::Integer(now)],
                )?;
                let Some(row) = row else { return Ok(no(Error::InvalidGrant, "this refresh token expired, ended, or was replaced (each is used once)")) };
                if row.client_id != client_id {
                    return Ok(no(Error::InvalidGrant, "this refresh token was issued to another client"));
                }
                if resource.is_some_and(|r| r != row.resource) {
                    return Ok(no(Error::InvalidTarget, "this connection is for another resource"));
                }
                let refresh_expires_at = now + oauth::REFRESH_TTL_MS;
                self.sweep_by(refresh_expires_at).await?;
                let (access, next) = (fresh_token(), fresh_token());
                self.exec(
                    "UPDATE connections SET access_hash = ?, access_expires_at = ?, refresh_hash = ?, refresh_expires_at = ? WHERE id = ?",
                    vec![
                        sha(&access).into(),
                        SqlStorageValue::Integer(now + oauth::ACCESS_TTL_MS),
                        sha(&next).into(),
                        SqlStorageValue::Integer(refresh_expires_at),
                        row.id.as_str().into(),
                    ],
                )?;
                Ok(Issued::Tokens(Tokens::bearer(access, next)))
            }
        }
    }

    /// The connection either of its tokens names ends, when `client_id` is
    /// the client it was issued to; anything else changes nothing (RFC 7009 2.2).
    pub(super) fn revoke_token(&self, b: RevokeToken) -> CellResult<()> {
        let hash = sha(&b.token);
        self.exec("DELETE FROM connections WHERE (access_hash = ? OR refresh_hash = ?) AND client_id = ?", vec![hash.as_str().into(), hash.as_str().into(), b.client_id.as_str().into()])
    }

    /// The asker's connections, newest first (at most `CONNECTIONS_PER_PERSON_MAX`).
    pub(super) fn list_connections(&self, b: ListConnections) -> CellResult<Connections> {
        let who = self.by(&b.by)?;
        let connections: Vec<Connection> = self.rows(
            "SELECT id, client, client_id AS clientId, resource, created_at AS createdAt, refresh_expires_at AS expiresAt
             FROM connections WHERE identity = ? AND refresh_expires_at > ? ORDER BY created_at DESC, rowid DESC",
            vec![who.id.as_str().into(), SqlStorageValue::Integer(js::now_ms())],
        )?;
        assert!(connections.len() as u64 <= oauth::CONNECTIONS_PER_PERSON_MAX, "a person's connections are bounded");
        Ok(Connections { connections })
    }

    pub(super) fn disconnect(&self, b: Disconnect) -> CellResult<()> {
        let who = self.by(&b.by)?;
        let ended = self.rows::<IgnoredAny>("DELETE FROM connections WHERE id = ? AND identity = ? RETURNING id", vec![b.id.as_str().into(), who.id.as_str().into()])?;
        if ended.is_empty() {
            return Err(CellError::new(ErrorCode::NotFound, format!("no connection {} of yours", b.id)));
        }
        Ok(())
    }
}
