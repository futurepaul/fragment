//! The `Principal` cell: one Durable Object per identity (`id:…`), holding
//! the index of the fragments it belongs to (what `GET /api/fragments`
//! lists). The fragments are the authority; each delivers its changes here
//! from an outbox, versioned so a late delivery never undoes a newer one.
//! Which keys an identity holds is the registry's (registry.rs).

use fragment_proto::{FragmentList, ListedFragment, Role};
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
/// incarnation); `version` orders changes within one.
#[derive(Deserialize)]
struct IndexChange {
    fragment: String,
    role: Option<Role>,
    incarnation: i64,
    version: i64,
}

impl DurableObject for PrincipalCell {
    fn new(state: State, _env: Env) -> Self {
        state.storage().sql().exec(SCHEMA, None).expect("the Principal schema applies");
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
                    self.rows(
                        "INSERT INTO memberships (fragment, role, incarnation, version) VALUES (?, ?, ?, ?)
                         ON CONFLICT (fragment) DO UPDATE SET role = excluded.role, incarnation = excluded.incarnation, version = excluded.version",
                        vec![c.fragment.as_str().into(), role, SqlStorageValue::Integer(c.incarnation), SqlStorageValue::Integer(c.version)],
                    )?;
                }
                Ok(Response::from_json(&json!({ "ok": true, "applied": newer }))?)
            }
            (Method::Get, "/list") => {
                // `GET /api/fragments`'s answer, whole: the router passes it through
                let q = "SELECT fragment AS name, role FROM memberships WHERE role IS NOT NULL ORDER BY fragment";
                let mut fragments: Vec<ListedFragment> = self.state.storage().sql().exec(q, None)?.to_array()?;
                // fragments from before usernames (decision 16's hard cut)
                // are served nowhere: they are not listed
                fragments.retain(|f| fragment_proto::valid_fragment_name(&f.name));
                Ok(Response::from_json(&FragmentList { fragments })?)
            }
            (m, p) => Err(CellError::new(fragment_proto::ErrorCode::NotFound, format!("no route {} {p}", m.as_ref()))),
        }
    }
}
