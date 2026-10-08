// The deployment's operators' commands (`fragment operator …`; docs/api.md,
// Operators): an operator key of their own, and the wipe of a person.
//
// An operator key is a key the deployment's `operators` lists and no person
// holds: it signs as an operator whether or not the registry knows it, so
// a wipe of anyone, its holder included, never removes the key doing the
// wiping, and a wipe cut short is run again with it.

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use fragment_proto::wipe::{Found, WipeAsk, WipeReport, WipeState};

use crate::api::{self, Code, CodedError};
use crate::auth::Identity;

/// The environment variable naming an operator key's file, when
/// `--key-file` does not.
pub const KEY_FILE_ENV: &str = "FRAGMENT_OPERATOR_KEY_FILE";
/// Calls one `wipe --yes` makes at most: each goes as far as the platform's
/// call does (`fragment_core::wipe::CALL_BUDGET_MS`), most wipes need one
/// or two, and a cleanup its fragments' alarms finish takes a few more.
const WIPE_CALLS_MAX: u32 = 60;
/// Calls in a row that may finish no step before the wipe stops, saying
/// what it waits for: it is run again later, where it stopped.
const WIPE_STALLED_MAX: u32 = 15;
/// The pause between two calls that finished no step.
const WIPE_PAUSE: Duration = Duration::from_secs(2);

fn usage(msg: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(CodedError { code: Code::InvalidUsage, msg: msg.into() })
}

/// Makes an operator key: a new key's secret in a file of its own at
/// `path` (0600, refused if it exists), never printed. Answers its npub,
/// for the deployment's `operators`.
pub fn make_key(path: &Path, write: impl Fn(&Path, &str) -> Result<()>) -> Result<String> {
    if path.exists() {
        return Err(usage(format!("{} exists: an operator key is never overwritten (move it away first)", path.display())));
    }
    let key = Identity::generate();
    write(path, &key.secret_hex())?;
    // read back: what the file holds is the key named
    let back = read_key(path)?;
    assert_eq!(back.pubkey_hex(), key.pubkey_hex(), "the key file holds the key made");
    Ok(key.npub())
}

/// The operator key in `path`: its 64-hex secret, the file's only line.
pub fn read_key(path: &Path) -> Result<Identity> {
    let text = std::fs::read_to_string(path).with_context(|| format!("reading the operator key file {}", path.display()))?;
    Identity::from_secret_hex(text.trim()).ok_or_else(|| usage(format!("{} holds no key (64 hex: a file `fragment operator key` made)", path.display())))
}

/// Where the operator key is: `--key-file`, else `FRAGMENT_OPERATOR_KEY_FILE`
/// (`None`: this machine's own key signs).
pub fn key_file(flag: Option<&Path>, env: Option<String>) -> Option<std::path::PathBuf> {
    flag.map(Path::to_path_buf).or_else(|| env.filter(|e| !e.trim().is_empty()).map(std::path::PathBuf::from))
}

/// `GET /api/people/{person}/wipe`: the dry run.
pub fn dry_run(c: &api::Client, person: &str) -> Result<WipeReport> {
    c.call_as(c.get(&format!("/api/people/{person}/wipe"))?)
}

/// The wipe: its dry run first (whom it names), then calls until it is
/// done, each going on where the last stopped. Each call is idempotent
/// (it names the identity the dry run answered), so a lost answer is
/// asked again. `said` hears each call's report as it comes.
pub fn wipe(c: &api::Client, person: &str, mut said: impl FnMut(u32, &WipeReport)) -> Result<(WipeReport, u32)> {
    let first = dry_run(c, person)?;
    let ask = WipeAsk { confirm: first.identity.clone(), steps: None };
    let path = format!("/api/people/{}/wipe", first.identity);
    let mut stalled = 0;
    let mut last = first;
    // bounded: WIPE_CALLS_MAX calls
    for call in 1..=WIPE_CALLS_MAX {
        let report: WipeReport = c.call_as(c.post_json_by_id(&path, &ask)?)?;
        said(call, &report);
        if report.done {
            return Ok((report, call));
        }
        let moved = report.ran.iter().any(|r| r.done) || report.next != last.next;
        stalled = if moved { 0 } else { stalled + 1 };
        last = report;
        if stalled >= WIPE_STALLED_MAX {
            break;
        }
        if !moved {
            std::thread::sleep(WIPE_PAUSE);
        }
    }
    let waiting: Vec<String> = last.ran.iter().filter_map(|r| r.note.as_ref().map(|n| format!("{}: {n}", r.step))).collect();
    Err(anyhow::Error::new(CodedError {
        code: Code::Unavailable,
        msg: format!(
            "the wipe of {} stopped before its {} step ({}); run it again: it goes on where it stopped",
            last.identity,
            last.next.as_deref().unwrap_or("last"),
            if waiting.is_empty() { "no step finished".to_string() } else { waiting.join("; ") }
        ),
    }))
}

/// A report for people.
pub fn print(r: &WipeReport) {
    let state = match r.state {
        WipeState::Live => "not wiped",
        WipeState::Wiping => "being wiped",
        WipeState::Wiped => "wiped",
    };
    let next = r.next.as_deref().map(|n| format!(", next: {n}")).unwrap_or_default();
    println!("{} ({}): {state}{next}", r.identity, r.username.as_deref().unwrap_or("no username"));
    print_found(&r.found);
}

fn print_found(f: &Found) {
    println!("  sign-ins {}, keys {}, sessions {}, pictures {}", f.sign_ins, f.keys, f.sessions, f.pictures);
    if !f.agents.is_empty() {
        println!("  agents ({}): {}", f.agents.len(), f.agents.join(", "));
    }
    let listed = |what: &str, l: &fragment_proto::wipe::Listed| {
        let more = if l.more { ", …" } else { "" };
        println!("  {what} ({}): {}{more}", l.count, l.names.join(", "));
    };
    listed("fragments they own", &f.fragments);
    listed("memberships elsewhere", &f.memberships);
    if let Some(c) = &f.computer {
        let more = if c.more { "+" } else { "" };
        let snapshot = if c.snapshot { ", a snapshot" } else { "" };
        println!("  computer {} ({}): {} saves, {}{more} objects in R2{snapshot}", c.computer, c.phase.as_deref().unwrap_or("none"), c.saves, c.backups);
    }
    println!("  ledger: {}; list rows: {}", if f.ledger { "holds entries" } else { "empty" }, f.lists);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Goal: an operator key file holds a key, is made once and never
    /// overwritten, and a file that holds anything else is refused.
    /// Method: make one, make it again, read a bad one.
    #[test]
    fn an_operator_key_is_made_once() {
        let dir = std::env::temp_dir().join(format!("fragment-operator-{:016x}", rand::random::<u64>()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("operator.key");
        let write = |p: &Path, s: &str| -> Result<()> { Ok(std::fs::write(p, format!("{s}\n"))?) };
        let npub = make_key(&path, write).unwrap();
        assert!(npub.starts_with("npub1"), "{npub}");
        assert_eq!(read_key(&path).unwrap().npub(), npub);
        assert!(make_key(&path, write).is_err(), "never overwritten");
        assert_eq!(read_key(&path).unwrap().npub(), npub, "the first key stays");
        let bad = dir.join("bad.key");
        std::fs::write(&bad, "not a key").unwrap();
        assert!(read_key(&bad).is_err());
        assert!(read_key(&dir.join("missing.key")).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_key_file_is_the_flag_then_the_environment() {
        let flag = Path::new("/k/flag");
        assert_eq!(key_file(Some(flag), Some("/k/env".into())).as_deref(), Some(flag));
        assert_eq!(key_file(None, Some("/k/env".into())).as_deref(), Some(Path::new("/k/env")));
        assert_eq!(key_file(None, Some("  ".into())), None);
        assert_eq!(key_file(None, None), None);
    }
}
