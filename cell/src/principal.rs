//! The `Principal` cell: one Durable Object per identity (`id:…`), holding
//! the index of the fragments it belongs to (what `GET /api/fragments`
//! lists). The fragments are the authority; each delivers its changes here
//! from an outbox, versioned so a late delivery never undoes a newer one.
//! A fragment's row in its owner's list also carries its sharing (who may
//! open it, its members and guests), sent with each change to them, so the
//! platform's page reads this one cell and wakes no fragment.
//! Which keys an identity holds is the registry's (registry.rs).

use fragment_proto::{FragmentKind, FragmentList, ListedFragment, Role, Sharing};
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
/// carries the fragment's sharing, as it was when the change was sent.
#[derive(Deserialize)]
struct IndexChange {
    fragment: String,
    role: Option<Role>,
    incarnation: i64,
    version: i64,
    #[serde(default)]
    sharing: Option<Sharing>,
    /// What it is and its title, on every row a role names.
    #[serde(default)]
    face: Option<Face>,
}

/// What a person's list shows of a fragment.
#[derive(serde::Serialize, Deserialize)]
struct Face {
    #[serde(default)]
    kind: FragmentKind,
    #[serde(default)]
    title: Option<String>,
}

/// A `memberships` row as `/list` reads it.
#[derive(Deserialize)]
struct Listed {
    name: String,
    role: Role,
    sharing: Option<String>,
    face: Option<String>,
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
        if !cols.iter().any(|c| c["name"] == "face") {
            sql.exec("ALTER TABLE memberships ADD COLUMN face TEXT", None).expect("the memberships table migrates");
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
                    let face = match &c.face {
                        Some(f) => serde_json::to_string(f).map_err(|e| CellError::host(format!("face: {e}")))?.into(),
                        None => SqlStorageValue::Null,
                    };
                    // a change that carries no face (a fragment from before faces)
                    // keeps the one the row has
                    self.rows(
                        "INSERT INTO memberships (fragment, role, incarnation, version, sharing, face) VALUES (?, ?, ?, ?, ?, ?)
                         ON CONFLICT (fragment) DO UPDATE SET role = excluded.role, incarnation = excluded.incarnation, version = excluded.version,
                           sharing = excluded.sharing, face = COALESCE(excluded.face, memberships.face)",
                        vec![c.fragment.as_str().into(), role, SqlStorageValue::Integer(c.incarnation), SqlStorageValue::Integer(c.version), sharing, face],
                    )?;
                }
                Ok(Response::from_json(&json!({ "ok": true, "applied": newer }))?)
            }
            (Method::Get, "/list") => {
                // `GET /api/fragments`'s answer, whole: the router passes it through
                let q = "SELECT fragment AS name, role, sharing, face FROM memberships WHERE role IS NOT NULL ORDER BY fragment";
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
                        let face = r.face.as_deref().and_then(|f| serde_json::from_str::<Face>(f).ok());
                        let (kind, title) = face.map_or((FragmentKind::App, None), |f| (f.kind, f.title));
                        Ok(ListedFragment { name: r.name, role: r.role, kind, title, sharing })
                    })
                    .collect::<CellResult<Vec<_>>>()?;
                Ok(Response::from_json(&FragmentList { fragments })?)
            }
            (m, p) => Err(CellError::new(fragment_proto::ErrorCode::NotFound, format!("no route {} {p}", m.as_ref()))),
        }
    }
}
