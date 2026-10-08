//! Which of its computer's agents the bridge runs, when the image says so
//! (`BRIDGE_AGENTS_FILE`, docs/bridge.md). A computer's agents may change
//! while it runs (docs/computers.md). An image whose runtime must prepare
//! an agent before its first turn (ours writes a Hermes profile) lists the
//! agents it has made ready in a file, and the bridge runs only those of
//! `GET /api/computer`'s, in the platform's order. Without the setting it
//! runs every agent the platform lists (the stub).
//!
//! The file is `{"agents": ["juniper.paul", …]}`, the host's, written whole
//! and renamed into place, so a read never sees half of one. A missing
//! file is no agent ready. One that does not read keeps the set before it
//! (a host's bug must not stop the agents it had made ready), and says so.

use std::collections::BTreeSet;
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;

use serde::Deserialize;

use crate::limits;
use crate::runtime::Agent;

/// The file is at most this many bytes: `AGENTS_MAX` names, each a
/// fragment's name (at most 128 bytes), with room for the JSON.
pub const READY_FILE_MAX_BYTES: usize = 16 * 1024;
/// A name in it is at most this long (a fragment's `<label>.<username>`).
pub const READY_NAME_MAX_BYTES: usize = 128;

const _: () = assert!(limits::AGENTS_MAX * (READY_NAME_MAX_BYTES + 4) < READY_FILE_MAX_BYTES);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    agents: Vec<String>,
}

/// The agents a ready file names.
pub fn parse(bytes: &[u8]) -> Result<BTreeSet<String>, String> {
    if bytes.len() > READY_FILE_MAX_BYTES {
        return Err(format!("{} bytes, past {READY_FILE_MAX_BYTES}", bytes.len()));
    }
    let f: File = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    if f.agents.len() > limits::AGENTS_MAX {
        return Err(format!("{} agents; a computer runs at most {}", f.agents.len(), limits::AGENTS_MAX));
    }
    if let Some(bad) = f.agents.iter().find(|a| a.is_empty() || a.len() > READY_NAME_MAX_BYTES || a.chars().any(char::is_whitespace)) {
        return Err(format!("{bad:?} is no agent fragment's name"));
    }
    Ok(f.agents.into_iter().collect())
}

/// The platform's agents that are ready, in the platform's order.
pub fn gate(agents: Vec<Agent>, ready: &BTreeSet<String>) -> Vec<Agent> {
    agents.into_iter().filter(|a| ready.contains(&a.fragment)).collect()
}

/// What tells one version of the file from the next: a rename makes a new
/// inode, and a write in place a new length or time.
type Signature = Option<(u64, u64, i64, i64)>;

/// The ready file, read again when it changes.
pub struct Ready {
    path: PathBuf,
    seen: Signature,
    read_once: bool,
    agents: BTreeSet<String>,
}

impl Ready {
    pub fn new(path: PathBuf) -> Ready {
        Ready { path, seen: None, read_once: false, agents: BTreeSet::new() }
    }

    pub fn agents(&self) -> &BTreeSet<String> {
        &self.agents
    }

    fn signature(&self) -> Signature {
        let m = std::fs::metadata(&self.path).ok()?;
        Some((m.ino(), m.len(), m.mtime(), m.mtime_nsec()))
    }

    /// Reads the file again if it changed since the last look (always on
    /// the first): whether the set of ready agents changed.
    pub fn refresh(&mut self) -> bool {
        let sig = self.signature();
        if self.read_once && sig == self.seen {
            return false;
        }
        self.read_once = true;
        self.seen = sig;
        let next = match sig {
            None => BTreeSet::new(),
            Some(_) => match std::fs::read(&self.path).map_err(|e| e.to_string()).and_then(|b| parse(&b)) {
                Ok(set) => set,
                Err(e) => {
                    crate::ev!("agents_file.unread", { "path": self.path.display().to_string(), "error": e, "kept": self.agents.len() });
                    return false;
                }
            },
        };
        if next == self.agents {
            return false;
        }
        crate::ev!("agents_file.read", { "ready": next.iter().collect::<Vec<_>>() });
        self.agents = next;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(label: &str) -> Agent {
        Agent { fragment: format!("{label}.paul"), identity: format!("npub1{label}"), name: label.into(), owner: "npub1paul".into(), credentials: vec![] }
    }

    /// Valid: the file names the agents ready; the gate keeps the
    /// platform's order and drops an agent not ready yet, and one the
    /// platform no longer lists.
    #[test]
    fn the_gate_runs_the_ready_agents_in_the_platforms_order() {
        let ready = parse(br#"{"agents": ["maple.paul", "juniper.paul", "gone.paul"]}"#).unwrap();
        let got = gate(vec![agent("juniper"), agent("oak"), agent("maple")], &ready);
        assert_eq!(got.iter().map(|a| a.fragment.as_str()).collect::<Vec<_>>(), ["juniper.paul", "maple.paul"]);
        assert!(gate(vec![agent("juniper")], &BTreeSet::new()).is_empty(), "nothing ready, nothing run");
    }

    /// Invalid: past its bounds, not the shape, or names that are no
    /// fragment's.
    #[test]
    fn a_file_out_of_shape_is_refused() {
        let many: Vec<String> = (0..=limits::AGENTS_MAX).map(|i| format!("a{i}.paul")).collect();
        let too_many = serde_json::to_vec(&serde_json::json!({ "agents": many })).unwrap();
        for bad in [&b"not json"[..], br#"{"agents": "juniper.paul"}"#, br#"{"agents": [], "more": 1}"#, br#"{"agents": [""]}"#, br#"{"agents": ["a b"]}"#, &too_many[..]] {
            assert!(parse(bad).is_err(), "{}", String::from_utf8_lossy(bad));
        }
        assert!(parse(&vec![b' '; READY_FILE_MAX_BYTES + 1]).is_err());
    }

    /// Replay and restart: the file is read again only when it changes; a
    /// rewrite with the same agents changes nothing; one that does not read
    /// keeps the set before it; a removed file is no agent ready; a new
    /// reader (a restarted bridge) reads it whole at once.
    #[test]
    fn the_file_is_read_again_when_it_changes() {
        crate::log::set_quiet(true);
        let dir = std::env::temp_dir().join(format!("bridge-ready-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("agents.json");
        let write = |text: &str| {
            let tmp = dir.join("agents.json.tmp");
            std::fs::write(&tmp, text).unwrap();
            std::fs::rename(&tmp, &path).unwrap();
        };
        let mut r = Ready::new(path.clone());
        assert!(!r.refresh() && r.agents().is_empty(), "no file: no agent ready");
        write(r#"{"agents": ["juniper.paul"]}"#);
        assert!(r.refresh());
        assert_eq!(r.agents().iter().collect::<Vec<_>>(), ["juniper.paul"]);
        assert!(!r.refresh(), "unchanged: not read again");
        write(r#"{"agents": ["juniper.paul"]}"#);
        assert!(!r.refresh(), "the same agents again: no change");
        write(r#"{"agents": ["juniper.paul", "maple.paul"]}"#);
        assert!(r.refresh() && r.agents().len() == 2);
        write("{half");
        assert!(!r.refresh() && r.agents().len() == 2, "a file that does not read keeps the set before it");
        let mut restarted = Ready::new(path.clone());
        write(r#"{"agents": ["maple.paul"]}"#);
        assert!(restarted.refresh() && restarted.agents().iter().collect::<Vec<_>>() == ["maple.paul"]);
        std::fs::remove_file(&path).unwrap();
        assert!(restarted.refresh() && restarted.agents().is_empty(), "removed: no agent ready");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
