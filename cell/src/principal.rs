//! The `Principal` cell: one Durable Object per identity (`id:…`), holding
//! the index of the fragments it belongs to (what `GET /api/fragments`
//! lists). The fragments are the authority; each delivers its changes here
//! from an outbox, versioned so a late delivery never undoes a newer one.
//! A fragment's row in its owner's list also carries its sharing (who may
//! open it, its members and guests), sent with each change to them, and
//! every row says whether it is a chat, so the desktop's badges and its
//! chats read this one cell and wake no fragment.
//! Which keys an identity holds is the registry's (registry.rs).

use fragment_proto::{FragmentList, ListedFragment, Role, Sharing};
use serde::Deserialize;
use serde_json::{json, Value};
use worker::*;

use crate::error::{CellError, CellResult};

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS memberships (
  fragment TEXT PRIMARY KEY, role TEXT, incarnation INTEGER NOT NULL, version INTEGER NOT NULL);
";

#[durable_object]
pub struct PrincipalCell {
    state: State,
}

/// A fragment's change to this key's membership. `incarnation` is the
/// fragment's creation time (a deleted and re-created fragment is a new
/// incarnation); `version` orders changes within one. The owner's row
/// carries the fragment's sharing, as it was when the change was sent;
/// each row, whether it was a chat then (none from a fragment from before
/// rows said: the list keeps that unknown, `NULL`).
#[derive(Deserialize)]
struct IndexChange {
    fragment: String,
    role: Option<Role>,
    incarnation: i64,
    version: i64,
    #[serde(default)]
    sharing: Option<Sharing>,
    #[serde(default)]
    chat: Option<bool>,
}

/// A `memberships` row as `/list` reads it.
#[derive(Deserialize)]
struct Listed {
    name: String,
    role: Role,
    sharing: Option<String>,
    chat: Option<i64>,
}

impl DurableObject for PrincipalCell {
    fn new(state: State, _env: Env) -> Self {
        let sql = state.storage().sql();
        sql.exec(SCHEMA, None).expect("the Principal schema applies");
        // a list from before rows carried a fragment's sharing
        let cols: Vec<Value> = sql.exec("PRAGMA table_info(memberships)", None).and_then(|c| c.to_array()).unwrap_or_default();
        if !cols.iter().any(|c| c["name"] == "sharing") {
            sql.exec("ALTER TABLE memberships ADD COLUMN sharing TEXT", None).expect("the memberships table migrates");
        }
        // and from before rows said whether a fragment is a chat
        if !cols.iter().any(|c| c["name"] == "chat") {
            sql.exec("ALTER TABLE memberships ADD COLUMN chat INTEGER", None).expect("the memberships table migrates");
        }
        PrincipalCell { state }
    }

    async fn fetch(&self, req: Request) -> Result<Response> {
        match self.route(req).await {
            Ok(r) => Ok(r),
            Err(e) => e.response(),
        }
    }
}

impl PrincipalCell {
    fn rows(&self, q: &str, binds: Vec<SqlStorageValue>) -> CellResult<Vec<Value>> {
        Ok(self.state.storage().sql().exec(q, binds)?.to_array::<Value>()?)
    }

    async fn route(&self, mut req: Request) -> CellResult<Response> {
        match (req.method(), req.path().as_str()) {
            (Method::Post, "/index") => {
                let c: IndexChange = serde_json::from_slice(&req.bytes().await?).map_err(|e| CellError::invalid(format!("body: {e}")))?;
                let stored = self.rows("SELECT incarnation, version FROM memberships WHERE fragment = ?", vec![c.fragment.as_str().into()])?;
                let newer = match stored.first() {
                    None => true,
                    Some(r) => {
                        let (inc, ver) = (r["incarnation"].as_i64().unwrap_or(0), r["version"].as_i64().unwrap_or(0));
                        c.incarnation > inc || (c.incarnation == inc && c.version > ver)
                    }
                };
                if newer {
                    let role = c.role.map_or(SqlStorageValue::Null, |r| r.as_str().into());
                    let sharing = match &c.sharing {
                        Some(s) => serde_json::to_string(s).map_err(|e| CellError::host(format!("sharing: {e}")))?.into(),
                        None => SqlStorageValue::Null,
                    };
                    self.rows(
                        "INSERT INTO memberships (fragment, role, incarnation, version, sharing, chat) VALUES (?, ?, ?, ?, ?, ?)
                         ON CONFLICT (fragment) DO UPDATE SET role = excluded.role, incarnation = excluded.incarnation, version = excluded.version, sharing = excluded.sharing, chat = excluded.chat",
                        vec![
                            c.fragment.as_str().into(),
                            role,
                            SqlStorageValue::Integer(c.incarnation),
                            SqlStorageValue::Integer(c.version),
                            sharing,
                            c.chat.map_or(SqlStorageValue::Null, |chat| SqlStorageValue::Integer(i64::from(chat))),
                        ],
                    )?;
                }
                Ok(Response::from_json(&json!({ "ok": true, "applied": newer }))?)
            }
            (Method::Get, "/list") => {
                // `GET /api/fragments`'s answer, whole: the router passes it through
                let q = "SELECT fragment AS name, role, sharing, chat FROM memberships WHERE role IS NOT NULL ORDER BY fragment";
                let rows: Vec<Listed> = self.state.storage().sql().exec(q, None)?.to_array()?;
                let fragments = rows
                    .into_iter()
                    // fragments from before usernames (decision 16's hard cut)
                    // are served nowhere: they are not listed
                    .filter(|r| fragment_proto::valid_fragment_name(&r.name))
                    .map(|r| {
                        let sharing = match r.sharing.as_deref().map(serde_json::from_str::<Sharing>) {
                            Some(Ok(s)) => Some(s),
                            Some(Err(e)) => return Err(CellError::host(format!("{}'s stored sharing: {e}", r.name))),
                            None => None,
                        };
                        Ok(ListedFragment { name: r.name, role: r.role, sharing, chat: r.chat.map(|c| c == 1) })
                    })
                    .collect::<CellResult<Vec<_>>>()?;
                Ok(Response::from_json(&FragmentList { fragments })?)
            }
            (m, p) => Err(CellError::new(fragment_proto::ErrorCode::NotFound, format!("no route {} {p}", m.as_ref()))),
        }
    }
}
