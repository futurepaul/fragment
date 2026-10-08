//! Each agent's screen, as the image names it (`BRIDGE_SCREENS_FILE`,
//! docs/bridge.md): an image whose agents each have a desktop of their own
//! (ours: Hermes gives every profile its own) lists, per agent, the RFB
//! server that is its display, and the files its runtime keeps about it:
//! the lease of who drives it (lease.rs), and the file whose time says when
//! it was last used (which the screen touches while a person watches it,
//! so a desktop being watched is never idle). Without the setting no agent
//! has a display (the stub).
//!
//! The file is `{"screens": [{"agent", "rfb", "lease"?, "activity"?}]}`,
//! the image's, written whole and renamed into place, so a read never sees
//! half of one. A missing file names no screen. One that does not read
//! keeps the screens before it (an image's bug must not take away the
//! screens it had named), and says so.

use std::collections::BTreeMap;
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;

use serde::Deserialize;

use crate::limits;
use crate::screen::Target;

/// The file is at most this many bytes: `AGENTS_MAX` entries, each three
/// paths of at most `PATH_MAX_BYTES` and a name, with room for the JSON.
pub const SCREENS_FILE_MAX_BYTES: usize = 64 * 1024;
/// A path in it is at most this long.
pub const PATH_MAX_BYTES: usize = 512;

const _: () = assert!(limits::AGENTS_MAX * (3 * PATH_MAX_BYTES + crate::screen::AGENT_MAX_BYTES + 64) < SCREENS_FILE_MAX_BYTES);

/// One agent's screen.
#[derive(Debug, Clone, PartialEq)]
pub struct Display {
    pub rfb: Target,
    pub lease: Option<PathBuf>,
    pub activity: Option<PathBuf>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    screens: Vec<Entry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    agent: String,
    rfb: String,
    #[serde(default)]
    lease: Option<String>,
    #[serde(default)]
    activity: Option<String>,
}

fn path(p: Option<String>, what: &str, agent: &str) -> Result<Option<PathBuf>, String> {
    let Some(p) = p else { return Ok(None) };
    if p.len() > PATH_MAX_BYTES || !p.starts_with('/') {
        return Err(format!("{agent}'s {what} is no absolute path of at most {PATH_MAX_BYTES} bytes"));
    }
    Ok(Some(PathBuf::from(p)))
}

/// The screens a file names, by agent.
pub fn parse(bytes: &[u8]) -> Result<BTreeMap<String, Display>, String> {
    if bytes.len() > SCREENS_FILE_MAX_BYTES {
        return Err(format!("{} bytes, past {SCREENS_FILE_MAX_BYTES}", bytes.len()));
    }
    let f: File = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    if f.screens.len() > limits::AGENTS_MAX {
        return Err(format!("{} screens; a computer runs at most {} agents", f.screens.len(), limits::AGENTS_MAX));
    }
    let mut out = BTreeMap::new();
    for e in f.screens {
        if !crate::screen::agent_name_ok(&e.agent) {
            return Err(format!("{:?} is no agent fragment's name", e.agent));
        }
        if e.rfb.len() > PATH_MAX_BYTES {
            return Err(format!("{}'s rfb is past {PATH_MAX_BYTES} bytes", e.agent));
        }
        let rfb = Target::parse(&e.rfb)?;
        let display = Display { rfb, lease: path(e.lease, "lease", &e.agent)?, activity: path(e.activity, "activity", &e.agent)? };
        if out.insert(e.agent.clone(), display).is_some() {
            return Err(format!("{} is named twice", e.agent));
        }
    }
    Ok(out)
}

/// What tells one version of the file from the next.
type Signature = Option<(u64, u64, i64, i64)>;

/// The screens file, read again when it changes.
pub struct Screens {
    path: PathBuf,
    seen: Signature,
    read_once: bool,
    screens: BTreeMap<String, Display>,
}

impl Screens {
    pub fn new(path: PathBuf) -> Screens {
        Screens { path, seen: None, read_once: false, screens: BTreeMap::new() }
    }

    pub fn get(&self, agent: &str) -> Option<&Display> {
        self.screens.get(agent)
    }

    pub fn agents(&self) -> impl Iterator<Item = &String> {
        self.screens.keys()
    }

    fn signature(&self) -> Signature {
        let m = std::fs::metadata(&self.path).ok()?;
        Some((m.ino(), m.len(), m.mtime(), m.mtime_nsec()))
    }

    /// Reads the file again if it changed since the last look (always on
    /// the first): whether the screens changed.
    pub fn refresh(&mut self) -> bool {
        let sig = self.signature();
        if self.read_once && sig == self.seen {
            return false;
        }
        self.read_once = true;
        self.seen = sig;
        let next = match sig {
            None => BTreeMap::new(),
            Some(_) => match std::fs::read(&self.path).map_err(|e| e.to_string()).and_then(|b| parse(&b)) {
                Ok(s) => s,
                Err(e) => {
                    crate::ev!("screens_file.unread", { "path": self.path.display().to_string(), "error": e, "kept": self.screens.len() });
                    return false;
                }
            },
        };
        if next == self.screens {
            return false;
        }
        crate::ev!("screens_file.read", { "agents": next.keys().collect::<Vec<_>>() });
        self.screens = next;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Valid: each agent's display and files, by agent.
    #[test]
    fn a_file_names_each_agents_screen() {
        let f = br#"{"screens": [
            {"agent": "juniper--k3x9", "rfb": "unix:/d/juniper--k3x9/bot-desktop/rfb.sock", "lease": "/d/juniper--k3x9/bot-desktop/lease.json", "activity": "/d/juniper--k3x9/bot-desktop/activity"},
            {"agent": "fred--k3x9", "rfb": "tcp:127.0.0.1:5901"}
        ]}"#;
        let s = parse(f).unwrap();
        assert_eq!(s.len(), 2);
        assert_eq!(s["juniper--k3x9"].rfb, Target::Unix("/d/juniper--k3x9/bot-desktop/rfb.sock".into()));
        assert_eq!(s["juniper--k3x9"].lease.as_deref(), Some(std::path::Path::new("/d/juniper--k3x9/bot-desktop/lease.json")));
        assert_eq!((s["fred--k3x9"].lease.clone(), s["fred--k3x9"].activity.clone()), (None, None), "a display alone: Take over is the screen's own");
        assert!(parse(br#"{"screens": []}"#).unwrap().is_empty());
    }

    /// Invalid: anything but whole entries for well-named agents, each once.
    #[test]
    fn a_file_that_is_no_screens_is_refused() {
        for bad in [
            &br#"{"screens": [{"agent": "juniper--k3x9", "rfb": "/no/scheme"}]}"#[..],
            br#"{"screens": [{"agent": "Juniper Paul", "rfb": "unix:/a"}]}"#,
            br#"{"screens": [{"agent": "juniper--k3x9", "rfb": "unix:/a", "lease": "relative/lease.json"}]}"#,
            br#"{"screens": [{"agent": "juniper--k3x9", "rfb": "unix:/a"}, {"agent": "juniper--k3x9", "rfb": "unix:/b"}]}"#,
            br#"{"screens": [{"agent": "juniper--k3x9", "rfb": "unix:/a", "colour": "red"}]}"#,
            br#"{"agents": []}"#,
            b"{",
        ] {
            assert!(parse(bad).is_err(), "{}", String::from_utf8_lossy(bad));
        }
        let many: Vec<String> = (0..=limits::AGENTS_MAX).map(|i| format!(r#"{{"agent": "a{i}--k3x9", "rfb": "unix:/a"}}"#)).collect();
        assert!(parse(format!(r#"{{"screens": [{}]}}"#, many.join(",")).as_bytes()).is_err(), "past AGENTS_MAX");
    }

    /// Replay and restart: the file read again when it changes, a missing
    /// one no screen, one that does not read keeping the screens before it.
    #[test]
    fn the_file_is_read_again_when_it_changes() {
        let dir = std::env::temp_dir().join(format!("bridge-screens-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("screens.json");
        let mut s = Screens::new(path.clone());
        assert!(!s.refresh() && s.get("juniper--k3x9").is_none(), "no file: no screen");
        std::fs::write(&path, br#"{"screens": [{"agent": "juniper--k3x9", "rfb": "unix:/a"}]}"#).unwrap();
        assert!(s.refresh() && s.get("juniper--k3x9").is_some());
        assert!(!s.refresh(), "unchanged: not read again");
        std::fs::write(&path, b"{torn").unwrap();
        assert!(!s.refresh() && s.get("juniper--k3x9").is_some(), "a file that does not read keeps the screens before it");
        std::fs::remove_file(&path).unwrap();
        assert!(s.refresh() && s.agents().next().is_none(), "removed: no screen");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
