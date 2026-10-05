//! People's own nodes (bring your own computer, experimental:
//! docs/self-host.md, seam 2): their pairings, the nodes paired, and where
//! each person's new computers run. The decisions are
//! `fragment_core::pairing`'s; this keeps the rows.
//!
//! - `pairings`: a node waiting for its person, by its user code; the
//!   device code only as its SHA-256. Approved, it names the node; the
//!   node's next poll takes the node's secret, and the row goes with it.
//!   Expired rows go as the next pairing starts.
//! - `paired_nodes`: each person's nodes, their secrets sealed for the
//!   node's id (`keys::seal`, scope `PairedNode:<id>`). A revoked node keeps
//!   its row (so its computers can say what became of it) and loses its
//!   secret.
//! - `pair_misses`: each wrong code a person tried, for `MISSES_MAX`.
//! - `node_prefs`: the node a person chose for their new computers.

use fragment_core::pairing::{self, Holder, Load, PairError, Paired, Pending, Polled};
use fragment_core::placement::{Arch, Byoc};
use fragment_proto::nodes::PairPolled;

use super::calls::{ChoiceAnswer, ChoiceOf, OwnNode, OwnNodes, OwnNodesAnswer, PairApprove, PairApproved, PairBegin, PairBegun, PairPoll, PairShow, PairShown, PairedNode, PairedRecord, PreferNode, RevokeNode};
use super::*;

pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS pairings (
  user_code TEXT PRIMARY KEY, device_hash TEXT NOT NULL UNIQUE, name TEXT NOT NULL, arch TEXT NOT NULL,
  created_at INTEGER NOT NULL, expires_at INTEGER NOT NULL, interval_s INTEGER NOT NULL, polled_at INTEGER, node TEXT);
CREATE INDEX IF NOT EXISTS pairings_created ON pairings (created_at);
CREATE TABLE IF NOT EXISTS paired_nodes (
  id TEXT PRIMARY KEY, owner TEXT NOT NULL, name TEXT NOT NULL, arch TEXT NOT NULL, paired_at INTEGER NOT NULL,
  sealed TEXT, revoked_at INTEGER);
CREATE INDEX IF NOT EXISTS paired_nodes_owner ON paired_nodes (owner);
CREATE TABLE IF NOT EXISTS pair_misses (identity TEXT NOT NULL, at INTEGER NOT NULL);
CREATE INDEX IF NOT EXISTS pair_misses_identity ON pair_misses (identity, at);
CREATE TABLE IF NOT EXISTS node_prefs (owner TEXT PRIMARY KEY, node TEXT NOT NULL);
";

/// A user code made again when it is taken, at most this often.
const CODE_TRIES: usize = 4;

#[derive(Deserialize)]
struct PendingRow {
    user_code: String,
    name: String,
    arch: String,
    created_at: i64,
    expires_at: i64,
    interval_s: i64,
    polled_at: Option<i64>,
    node: Option<String>,
}

impl PendingRow {
    fn pending(self) -> CellResult<Pending> {
        let arch = Arch::parse(&self.arch).ok_or_else(|| CellError::host(format!("pairings.arch {:?} is no architecture", self.arch)))?;
        let interval_s = u32::try_from(self.interval_s).map_err(|_| CellError::host("pairings.interval_s is out of range"))?;
        Ok(Pending { user_code: self.user_code, name: self.name, arch, created_at: self.created_at, expires_at: self.expires_at, interval_s, polled_at: self.polled_at, node: self.node })
    }
}

#[derive(Deserialize)]
struct NodeRow {
    id: String,
    owner: String,
    name: String,
    arch: String,
    paired_at: i64,
    sealed: Option<String>,
    revoked_at: Option<i64>,
}

impl NodeRow {
    fn paired(&self) -> Paired {
        Paired { id: self.id.clone(), owner: self.owner.clone(), revoked_at: self.revoked_at }
    }
}

#[derive(Deserialize)]
struct PrefRow {
    node: String,
}

/// A refusal of pairing, as the platform answers it.
fn refused(e: PairError) -> CellError {
    let code = match e {
        PairError::ByocOff => ErrorCode::Forbidden,
        PairError::Busy(_) | PairError::Misses => ErrorCode::RateLimited,
        PairError::Invalid(_) | PairError::Expired | PairError::Used | PairError::Full(_) => ErrorCode::InvalidRequest,
        PairError::NoSuchCode | PairError::NotYours(_) => ErrorCode::NotFound,
        PairError::Revoked(_) => ErrorCode::NodeRevoked,
    };
    CellError::new(code, e.to_string())
}

/// The scope a node's secret is sealed for: its id, so a sealed value
/// opens for that node alone.
fn scope(id: &str) -> String {
    format!("PairedNode:{id}")
}

impl RegistryCell {
    fn byoc(&self) -> Byoc {
        self.cfg.nodes.as_ref().map_or(Byoc::Off, |n| n.byoc())
    }

    fn node_row(&self, id: &str) -> CellResult<Option<NodeRow>> {
        self.row::<NodeRow>("SELECT id, owner, name, arch, paired_at, sealed, revoked_at FROM paired_nodes WHERE id = ?", vec![id.into()])
    }

    fn pending_by_code(&self, code: &str) -> CellResult<Option<Pending>> {
        self.row::<PendingRow>("SELECT user_code, name, arch, created_at, expires_at, interval_s, polled_at, node FROM pairings WHERE user_code = ?", vec![code.into()])?
            .map(PendingRow::pending)
            .transpose()
    }

    /// `by`'s count of nodes and wrong codes, as approving needs it.
    fn holder(&self, by: &str, now: i64) -> CellResult<Holder> {
        let live = self.count("SELECT COUNT(*) AS n FROM paired_nodes WHERE owner = ? AND revoked_at IS NULL", vec![by.into()])?;
        let rows = self.count("SELECT COUNT(*) AS n FROM paired_nodes WHERE owner = ?", vec![by.into()])?;
        let misses = self.count("SELECT COUNT(*) AS n FROM pair_misses WHERE identity = ? AND at > ?", vec![by.into(), SqlStorageValue::Integer(now - pairing::MISS_WINDOW_MS)])?;
        Ok(Holder { live, rows, misses })
    }

    /// A wrong code `by` tried: counted, and the window's older ones dropped.
    fn miss(&self, by: &str, now: i64) -> CellResult<()> {
        self.exec("DELETE FROM pair_misses WHERE identity = ? AND at <= ?", vec![by.into(), SqlStorageValue::Integer(now - pairing::MISS_WINDOW_MS)])?;
        self.exec("INSERT INTO pair_misses (identity, at) VALUES (?, ?)", vec![by.into(), SqlStorageValue::Integer(now)])
    }

    /// The pairing a person's code names, if they may see it: their
    /// session live, the code theirs to approve. A wrong code is counted.
    fn approvable(&self, token: &str, code: &str) -> CellResult<(Identity, Pending)> {
        let who = self.live_session(token, None, false)?.session.identity;
        if who.kind != IdentityKind::Person {
            return Err(CellError::new(ErrorCode::Forbidden, "a person pairs their own nodes"));
        }
        let now = js::now_ms();
        let holder = self.holder(&who.id, now)?;
        // past the bound, nothing is looked up: a guess costs the same
        let found = if holder.misses >= pairing::MISSES_MAX { None } else { pairing::parse_user_code(code).map(|c| self.pending_by_code(&c)).transpose()?.flatten() };
        match pairing::approvable(self.byoc(), found.as_ref(), holder, now) {
            Ok(()) => Ok((who, found.expect("approvable found a pairing"))),
            Err(e) => {
                if e == PairError::NoSuchCode {
                    self.miss(&who.id, now)?;
                }
                Err(refused(e))
            }
        }
    }

    pub(super) fn pair_begin(&self, b: PairBegin) -> CellResult<PairBegun> {
        let now = js::now_ms();
        if self.byoc() == Byoc::Off {
            return Err(refused(PairError::ByocOff));
        }
        // a pairing past its time is no one's: gone before it is counted
        self.exec("DELETE FROM pairings WHERE expires_at <= ?", vec![SqlStorageValue::Integer(now)])?;
        let load = Load {
            pending: self.count("SELECT COUNT(*) AS n FROM pairings", vec![])?,
            last_minute: self.count("SELECT COUNT(*) AS n FROM pairings WHERE created_at > ?", vec![SqlStorageValue::Integer(now - 60_000)])?,
        };
        let device = pairing::device_code(js::random_bytes());
        let hash = pairing::device_hash(&device).expect("a device code is its own form");
        // bounded: CODE_TRIES codes, each 20^8 to one against the few waiting
        for _ in 0..CODE_TRIES {
            let p = pairing::start(self.byoc(), load, &b.name, &b.arch, js::random_bytes(), now).map_err(refused)?;
            if self.pending_by_code(&p.user_code)?.is_some() {
                continue;
            }
            self.exec(
                "INSERT INTO pairings (user_code, device_hash, name, arch, created_at, expires_at, interval_s) VALUES (?, ?, ?, ?, ?, ?, ?)",
                vec![
                    p.user_code.as_str().into(),
                    hash.as_str().into(),
                    p.name.as_str().into(),
                    p.arch.name().into(),
                    SqlStorageValue::Integer(p.created_at),
                    SqlStorageValue::Integer(p.expires_at),
                    SqlStorageValue::Integer(i64::from(p.interval_s)),
                ],
            )?;
            return Ok(PairBegun { user_code: p.user_code, device_code: device, expires_at: p.expires_at, interval_s: p.interval_s });
        }
        Err(CellError::new(ErrorCode::RateLimited, "no free pairing code: try again in a minute"))
    }

    pub(super) fn pair_show(&self, b: PairShow) -> CellResult<PairShown> {
        let (_, p) = self.approvable(&b.token, &b.code)?;
        Ok(PairShown { user_code: p.user_code, name: p.name, arch: p.arch.name().to_string(), expires_at: p.expires_at })
    }

    /// Approves a pairing: the platform names the node and mints its secret
    /// (sealed for it), all in one turn; the node's next poll takes them.
    pub(super) fn pair_approve(&self, b: PairApprove) -> CellResult<PairApproved> {
        let (who, p) = self.approvable(&b.token, &b.code)?;
        let now = js::now_ms();
        // a random id, never the node's choice: one that is taken is made again
        let id = (0..CODE_TRIES)
            .map(|_| pairing::paired_id(js::random_bytes()))
            .find(|id| self.node_row(id).ok().flatten().is_none())
            .ok_or_else(|| CellError::host("no free node id in four tries"))?;
        let secret = pairing::secret(js::random_bytes());
        let sealed = crate::keys::seal(&self.env, &scope(&id), secret.as_bytes())?;
        self.exec(
            "INSERT INTO paired_nodes (id, owner, name, arch, paired_at, sealed) VALUES (?, ?, ?, ?, ?, ?)",
            vec![id.as_str().into(), who.id.as_str().into(), p.name.as_str().into(), p.arch.name().into(), SqlStorageValue::Integer(now), sealed.into()],
        )?;
        self.exec("UPDATE pairings SET node = ? WHERE user_code = ?", vec![id.as_str().into(), p.user_code.as_str().into()])?;
        if b.prefer {
            // their own node, just approved: choosable by them alone (the
            // same row `prefer_node` writes after its checks)
            self.exec(
                "INSERT INTO node_prefs (owner, node) VALUES (?, ?) ON CONFLICT (owner) DO UPDATE SET node = excluded.node",
                vec![who.id.as_str().into(), id.as_str().into()],
            )?;
        }
        Ok(PairApproved { node: id, name: p.name })
    }

    /// A node's poll: its pairing's state; approved, its id and secret,
    /// once (the pairing goes with the answer).
    pub(super) fn pair_poll(&self, b: PairPoll) -> CellResult<PairPolled> {
        let gone = || CellError::new(ErrorCode::NotFound, "no pairing has this code: it was used, or expired, or never made (run `sandcastle-node pair` again)");
        let hash = pairing::device_hash(&b.device_code).ok_or_else(gone)?;
        let row = self
            .row::<PendingRow>("SELECT user_code, name, arch, created_at, expires_at, interval_s, polled_at, node FROM pairings WHERE device_hash = ?", vec![hash.as_str().into()])?
            .ok_or_else(gone)?
            .pending()?;
        if self.byoc() == Byoc::Off {
            return Err(refused(PairError::ByocOff));
        }
        let (polled, next) = pairing::poll(&row, js::now_ms());
        match polled {
            Polled::Approved { node } => {
                let n = self.node_row(&node)?.ok_or_else(|| CellError::host(format!("a pairing names a node {node} that is not there")))?;
                self.exec("DELETE FROM pairings WHERE user_code = ?", vec![row.user_code.as_str().into()])?;
                let Some(sealed) = n.sealed.filter(|_| n.revoked_at.is_none()) else {
                    return Err(refused(PairError::Revoked(node)));
                };
                let secret = String::from_utf8(crate::keys::open(&self.env, &scope(&node), &sealed)?.plaintext).map_err(|_| CellError::host("a node's secret is not text"))?;
                Ok(PairPolled::Approved { node, secret })
            }
            Polled::Expired => {
                self.exec("DELETE FROM pairings WHERE user_code = ?", vec![row.user_code.as_str().into()])?;
                Ok(PairPolled::Expired)
            }
            Polled::Pending { interval_s } | Polled::SlowDown { interval_s } => {
                self.exec(
                    "UPDATE pairings SET polled_at = ?, interval_s = ? WHERE user_code = ?",
                    vec![next.polled_at.map_or(SqlStorageValue::Null, SqlStorageValue::Integer), SqlStorageValue::Integer(i64::from(next.interval_s)), row.user_code.as_str().into()],
                )?;
                Ok(match polled {
                    Polled::SlowDown { .. } => PairPolled::SlowDown { interval_s },
                    _ => PairPolled::Pending { interval_s },
                })
            }
        }
    }

    /// A person's own nodes, revoked ones too (bounded: `ROWS_MAX`), and
    /// the node they chose.
    fn own_nodes(&self, owner: &str) -> CellResult<OwnNodesAnswer> {
        let rows = self.rows::<NodeRow>("SELECT id, owner, name, arch, paired_at, sealed, revoked_at FROM paired_nodes WHERE owner = ? ORDER BY paired_at, id", vec![owner.into()])?;
        let nodes = rows.into_iter().map(|r| OwnNode { id: r.id, name: r.name, arch: r.arch, paired_at: r.paired_at, revoked_at: r.revoked_at }).collect();
        let prefer = self.row::<PrefRow>("SELECT node FROM node_prefs WHERE owner = ?", vec![owner.into()])?.map(|r| r.node);
        Ok(OwnNodesAnswer { byoc: self.byoc() == Byoc::On, nodes, prefer })
    }

    pub(super) fn own_nodes_of(&self, b: OwnNodes) -> CellResult<OwnNodesAnswer> {
        self.own_nodes(&b.owner)
    }

    /// One person's node by id, its secret opened while it is live (the
    /// uplink's dial checks it; a computer's calls sign with it).
    pub(super) fn paired_node(&self, b: PairedNode) -> CellResult<Option<PairedRecord>> {
        if !pairing::is_paired_id(&b.id) {
            return Ok(None);
        }
        let Some(row) = self.node_row(&b.id)? else { return Ok(None) };
        let secret = match (&row.sealed, row.revoked_at) {
            (Some(sealed), None) => {
                let opened = crate::keys::open(&self.env, &scope(&row.id), sealed)?;
                if let Some(again) = opened.resealed {
                    self.exec("UPDATE paired_nodes SET sealed = ? WHERE id = ?", vec![again.into(), row.id.as_str().into()])?;
                }
                Some(String::from_utf8(opened.plaintext).map_err(|_| CellError::host("a node's secret is not text"))?)
            }
            _ => None,
        };
        Ok(Some(PairedRecord { id: row.id, owner: row.owner, name: row.name, arch: row.arch, revoked: row.revoked_at.is_some(), secret }))
    }

    /// Revokes a node of the person's: its secret goes, its row stays, and
    /// their choice of it with it. The router then cuts its uplink.
    pub(super) fn revoke_node(&self, b: RevokeNode) -> CellResult<OwnNodesAnswer> {
        let row = self.node_row(&b.id)?;
        if pairing::revocable(&b.owner, &b.id, row.as_ref().map(NodeRow::paired).as_ref()).map_err(refused)? {
            self.exec("UPDATE paired_nodes SET revoked_at = ?, sealed = NULL WHERE id = ?", vec![SqlStorageValue::Integer(js::now_ms()), b.id.as_str().into()])?;
            self.exec("DELETE FROM node_prefs WHERE owner = ? AND node = ?", vec![b.owner.as_str().into(), b.id.as_str().into()])?;
            // a pairing that named it and was never polled takes nothing now
            self.exec("DELETE FROM pairings WHERE node = ?", vec![b.id.as_str().into()])?;
        }
        self.own_nodes(&b.owner)
    }

    /// Where a person's new computers run: a node they may use, or the
    /// deployment's rule (`None`).
    pub(super) fn prefer_node(&self, b: PreferNode) -> CellResult<OwnNodesAnswer> {
        match &b.node {
            None => self.exec("DELETE FROM node_prefs WHERE owner = ?", vec![b.owner.as_str().into()])?,
            Some(id) => {
                let listed = self.cfg.nodes.as_ref().is_some_and(|n| n.get(id).is_some());
                let row = if listed { None } else { self.node_row(id)? };
                pairing::choosable(self.byoc(), &b.owner, id, listed, row.as_ref().map(NodeRow::paired).as_ref()).map_err(refused)?;
                self.exec(
                    "INSERT INTO node_prefs (owner, node) VALUES (?, ?) ON CONFLICT (owner) DO UPDATE SET node = excluded.node",
                    vec![b.owner.as_str().into(), id.as_str().into()],
                )?;
            }
        }
        self.own_nodes(&b.owner)
    }

    /// What placing a person's new computer needs (computer.rs `place`):
    /// the node they chose, its record when it is theirs and they may still
    /// use it, or why they may not.
    pub(super) fn choice_of(&self, b: ChoiceOf) -> CellResult<ChoiceAnswer> {
        let Some(prefer) = self.row::<PrefRow>("SELECT node FROM node_prefs WHERE owner = ?", vec![b.owner.as_str().into()])?.map(|r| r.node) else {
            return Ok(ChoiceAnswer { prefer: None, own: None, gone: None });
        };
        if !pairing::is_paired_id(&prefer) {
            return Ok(ChoiceAnswer { prefer: Some(prefer), own: None, gone: None });
        }
        let row = self.node_row(&prefer)?;
        let paired = row.as_ref().map(NodeRow::paired);
        match pairing::choosable(self.byoc(), &b.owner, &prefer, false, paired.as_ref()) {
            Ok(()) => {
                let own = self.paired_node(PairedNode { id: prefer.clone() })?;
                Ok(ChoiceAnswer { prefer: Some(prefer), own, gone: None })
            }
            Err(e) => Ok(ChoiceAnswer { prefer: Some(prefer), own: None, gone: Some(e.to_string()) }),
        }
    }
}
