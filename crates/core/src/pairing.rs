//! Pairing a person's own sandcastle node (bring your own computer,
//! experimental: docs/self-host.md, seam 2). A device authorization, as
//! RFC 8628 has it and as `fragment login` approves a key:
//!
//! 1. The node starts a pairing (`sandcastle-node pair <platform>`,
//!    unsigned: it holds nothing yet). The platform answers a `device_code`
//!    (32 random bytes, which only the node holds, and polls with) and a
//!    `user_code` (8 letters, which a person reads and compares), good for
//!    `PAIRING_TTL_MS`.
//! 2. The person opens the platform's page for that code in their
//!    signed-in browser, checks the node's terminal shows the same code,
//!    and approves it (a form, from the platform's own origin). The
//!    platform names the node (`paired_id`: never the node's choice) and
//!    mints its secret.
//! 3. The node's next poll takes its id and secret, once: the pairing is
//!    then spent. It writes them to its config and its secret's file, and
//!    dials the uplink as that id. The secret is never shown to anyone.
//!
//! The bounds: `PENDING_MAX` pairings waiting at once and `STARTS_PER_MINUTE`
//! begun a minute (both platform-wide: a start is unsigned); a poll no
//! sooner than its interval (`slow_down` adds `SLOW_DOWN_S`); `MISSES_MAX`
//! wrong codes a person may try in `MISS_WINDOW_MS`; `NODES_MAX` live nodes
//! a person holds, `ROWS_MAX` counting revoked ones. A deployment with BYOC
//! off (`placement::Byoc`) refuses every step, saying why.
//!
//! Everything here is a decision on what the registry read
//! (`registry/nodes.rs` keeps the rows); randomness and the clock come in.

use std::fmt;

use sha2::{Digest, Sha256};

use crate::placement::{Arch, Byoc};

/// A person's own node is named `paired-` and 16 hex; no deployment node
/// may begin so (`placement::Nodes::parse`).
pub const PAIRED_PREFIX: &str = "paired-";
const PAIRED_HEX: usize = 16;
/// The letters a user code is made of (RFC 8628 §6.1: no vowels, so no
/// words; none that read as another).
pub const USER_CODE_ALPHABET: &[u8; 20] = b"BCDFGHJKLMNPQRSTVWXZ";
/// A user code's letters (two groups of four): 20^8, about 2^34.6.
pub const USER_CODE_LETTERS: usize = 8;
/// A device code's random bytes.
pub const DEVICE_CODE_BYTES: usize = 32;
/// A node's secret's random bytes (as hex: 64 bytes, past sandcastle's 32).
pub const SECRET_BYTES: usize = 32;
/// How long a pairing waits for its approval and its node's poll.
pub const PAIRING_TTL_MS: i64 = 10 * 60_000;
/// A node polls no sooner than this, at first.
pub const POLL_INTERVAL_S: u32 = 5;
/// A poll sooner than its interval adds this to it (RFC 8628's slow_down).
pub const SLOW_DOWN_S: u32 = 5;
pub const POLL_INTERVAL_MAX_S: u32 = 60;
/// Pairings waiting at once, platform-wide.
pub const PENDING_MAX: u64 = 32;
/// Pairings begun in one minute, platform-wide.
pub const STARTS_PER_MINUTE: u64 = 10;
/// Wrong codes a person may try in `MISS_WINDOW_MS`.
pub const MISSES_MAX: u64 = 5;
pub const MISS_WINDOW_MS: i64 = 10 * 60_000;
/// Live (unrevoked) nodes a person holds.
pub const NODES_MAX: u64 = 8;
/// A person's node rows, revoked ones included (a revoked node's row stays,
/// so its computers can say what became of it).
pub const ROWS_MAX: u64 = 32;
/// A node's name, as its owner reads it in settings (the machine's name).
pub const NAME_BYTES_MAX: usize = 48;

/// Why a step of pairing is refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PairError {
    /// The deployment's nodes are its operator's alone.
    ByocOff,
    /// Too many pairings wait, or began this minute.
    Busy(String),
    /// A node's name or architecture that is no name or architecture.
    Invalid(String),
    /// No pairing waits under this code: mistyped, expired, or used.
    NoSuchCode,
    /// The code expired before it was approved.
    Expired,
    /// The code was approved already.
    Used,
    /// The person tried too many wrong codes lately.
    Misses,
    /// The person holds as many nodes as they may.
    Full(String),
    /// No node of the asker's has this id (another's, or none).
    NotYours(String),
    /// The node was revoked by its owner: it dials nothing, runs nothing.
    Revoked(String),
}

impl fmt::Display for PairError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PairError::ByocOff => write!(f, "this platform pairs no personal nodes: its operator provides the nodes computers run on (FRAGMENT_BYOC is off)"),
            PairError::Busy(why) => write!(f, "{why}; try again in a minute"),
            PairError::Invalid(why) => write!(f, "{why}"),
            PairError::NoSuchCode => write!(f, "no node is waiting with that code: check it against the node's terminal (it may have expired, or been used)"),
            PairError::Expired => write!(f, "that code expired: run `sandcastle-node pair` again for a new one"),
            PairError::Used => write!(f, "that code was approved already"),
            PairError::Misses => write!(f, "too many wrong codes: wait a few minutes and try again"),
            PairError::Full(why) => write!(f, "{why}"),
            PairError::NotYours(id) => write!(f, "no node of yours is {id}"),
            PairError::Revoked(id) => write!(f, "the node {id} was revoked by its owner: pair the machine again to use it"),
        }
    }
}

/// `PAIRED_PREFIX` and 16 hex, from 8 random bytes.
pub fn paired_id(random: [u8; PAIRED_HEX / 2]) -> String {
    format!("{PAIRED_PREFIX}{}", hex::encode(random))
}

/// Whether `id` is a person's own node's.
pub fn is_paired_id(id: &str) -> bool {
    id.strip_prefix(PAIRED_PREFIX).is_some_and(|h| h.len() == PAIRED_HEX && h.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
}

/// A user code, `XXXX-XXXX` of `USER_CODE_ALPHABET`, from 8 random bytes:
/// their number in base 20 (20^8 is far below 2^64, so the bias is under
/// 2^-29).
pub fn user_code(random: [u8; 8]) -> String {
    let mut n = u64::from_be_bytes(random);
    let mut letters = Vec::with_capacity(USER_CODE_LETTERS + 1);
    for i in 0..USER_CODE_LETTERS {
        if i == USER_CODE_LETTERS / 2 {
            letters.push(b'-');
        }
        letters.push(USER_CODE_ALPHABET[(n % 20) as usize]);
        n /= 20;
    }
    String::from_utf8(letters).expect("the alphabet is ASCII")
}

/// A code as a person typed it (any case, with or without the dash or
/// spaces), as the platform keeps it; `None` when it cannot be one.
pub fn parse_user_code(typed: &str) -> Option<String> {
    if typed.len() > 32 {
        return None;
    }
    let letters: Vec<u8> = typed.bytes().filter(|b| !matches!(b, b'-' | b' ')).map(|b| b.to_ascii_uppercase()).collect();
    if letters.len() != USER_CODE_LETTERS || !letters.iter().all(|b| USER_CODE_ALPHABET.contains(b)) {
        return None;
    }
    let (a, b) = letters.split_at(USER_CODE_LETTERS / 2);
    Some(format!("{}-{}", String::from_utf8_lossy(a), String::from_utf8_lossy(b)))
}

/// A device code, as hex: what the node polls with.
pub fn device_code(random: [u8; DEVICE_CODE_BYTES]) -> String {
    hex::encode(random)
}

/// What the platform keeps of a device code: its SHA-256 (a leaked row
/// polls nothing).
pub fn device_hash(code: &str) -> Option<String> {
    (code.len() == 2 * DEVICE_CODE_BYTES && code.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))).then(|| hex::encode(Sha256::digest(code.as_bytes())))
}

/// A node's name as its owner reads it: 1 to `NAME_BYTES_MAX` bytes, no
/// control characters, trimmed.
pub fn node_name(name: &str) -> Result<String, PairError> {
    let name = name.trim();
    if name.is_empty() || name.len() > NAME_BYTES_MAX || name.chars().any(char::is_control) {
        return Err(PairError::Invalid(format!("a node's name is 1 to {NAME_BYTES_MAX} bytes, with no control characters")));
    }
    Ok(name.to_string())
}

/// A pairing as the registry keeps it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pending {
    pub user_code: String,
    pub name: String,
    pub arch: Arch,
    pub created_at: i64,
    pub expires_at: i64,
    /// The node polls no sooner than this after its last poll.
    pub interval_s: u32,
    pub polled_at: Option<i64>,
    /// Approved: the id the platform named the node, which its next poll
    /// takes with its secret.
    pub node: Option<String>,
}

/// How many pairings wait, and began in the last minute.
#[derive(Debug, Clone, Copy, Default)]
pub struct Load {
    pub pending: u64,
    pub last_minute: u64,
}

/// A new pairing for a node named `name` of `arch`, its user code made of
/// `code_random` (the caller makes another when the code is taken).
pub fn start(byoc: Byoc, load: Load, name: &str, arch: &str, code_random: [u8; 8], now: i64) -> Result<Pending, PairError> {
    if byoc == Byoc::Off {
        return Err(PairError::ByocOff);
    }
    let name = node_name(name)?;
    let arch = Arch::parse(arch).ok_or_else(|| PairError::Invalid(format!("a node's architecture is x86_64 or aarch64, not {arch:?}")))?;
    if load.pending >= PENDING_MAX {
        return Err(PairError::Busy(format!("{PENDING_MAX} nodes are waiting to be approved")));
    }
    if load.last_minute >= STARTS_PER_MINUTE {
        return Err(PairError::Busy(format!("{STARTS_PER_MINUTE} pairings began this minute")));
    }
    Ok(Pending { user_code: user_code(code_random), name, arch, created_at: now, expires_at: now + PAIRING_TTL_MS, interval_s: POLL_INTERVAL_S, polled_at: None, node: None })
}

/// What a person's count of nodes and wrong codes allows.
#[derive(Debug, Clone, Copy, Default)]
pub struct Holder {
    /// Their nodes not revoked.
    pub live: u64,
    /// Their node rows, revoked ones included.
    pub rows: u64,
    /// Their wrong codes in the last `MISS_WINDOW_MS`.
    pub misses: u64,
}

/// Whether the pairing `found` under a code a person entered may be
/// shown and approved by them. A miss (no pairing under the code) is
/// theirs to count: past `MISSES_MAX`, nothing is looked up at all.
pub fn approvable(byoc: Byoc, found: Option<&Pending>, holder: Holder, now: i64) -> Result<(), PairError> {
    if byoc == Byoc::Off {
        return Err(PairError::ByocOff);
    }
    if holder.misses >= MISSES_MAX {
        return Err(PairError::Misses);
    }
    let p = found.ok_or(PairError::NoSuchCode)?;
    if p.node.is_some() {
        return Err(PairError::Used);
    }
    if now >= p.expires_at {
        return Err(PairError::Expired);
    }
    if holder.live >= NODES_MAX {
        return Err(PairError::Full(format!("you hold {NODES_MAX} nodes: revoke one in settings first")));
    }
    if holder.rows >= ROWS_MAX {
        return Err(PairError::Full(format!("you have paired {ROWS_MAX} nodes, revoked ones included")));
    }
    Ok(())
}

/// What a node's poll finds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Polled {
    /// Not approved yet: poll again after `interval_s`.
    Pending { interval_s: u32 },
    /// Polled sooner than its interval, which grows.
    SlowDown { interval_s: u32 },
    /// Approved as `node`: its secret goes with this answer, once, and the
    /// pairing is spent.
    Approved { node: String },
    /// Not approved in time: the node starts over.
    Expired,
}

/// A node's poll of its pairing at `now`, and the pairing after it (its
/// poll's time, and its interval). An approved pairing is spent by the
/// answer (the registry deletes it with the secret it hands over), so a
/// poll replayed after finds none.
pub fn poll(p: &Pending, now: i64) -> (Polled, Pending) {
    let mut next = p.clone();
    if let Some(node) = &p.node {
        return (Polled::Approved { node: node.clone() }, next);
    }
    if now >= p.expires_at {
        return (Polled::Expired, next);
    }
    let soon = p.polled_at.is_some_and(|at| now - at < i64::from(p.interval_s) * 1000);
    next.polled_at = Some(now);
    if soon {
        next.interval_s = (p.interval_s + SLOW_DOWN_S).min(POLL_INTERVAL_MAX_S);
        return (Polled::SlowDown { interval_s: next.interval_s }, next);
    }
    (Polled::Pending { interval_s: p.interval_s }, next)
}

/// A person's own node as the registry keeps it (its secret apart).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paired {
    pub id: String,
    pub owner: String,
    pub revoked_at: Option<i64>,
}

/// Whether `by` may revoke the node `found` under the id they named:
/// their own, once (`Ok(false)`: revoked already, nothing changes). Another
/// person's node is no node of theirs: the answer does not tell it apart
/// from one that does not exist.
pub fn revocable(by: &str, id: &str, found: Option<&Paired>) -> Result<bool, PairError> {
    match found {
        Some(p) if p.owner == by => Ok(p.revoked_at.is_none()),
        _ => Err(PairError::NotYours(id.to_string())),
    }
}

/// Whether node `id`'s dial may be taken (its signature is checked after,
/// with the secret this lets the platform read): a person's live node, on
/// a deployment that pairs them.
pub fn may_dial(byoc: Byoc, id: &str, found: Option<&Paired>) -> Result<(), PairError> {
    if byoc == Byoc::Off {
        return Err(PairError::ByocOff);
    }
    match found {
        None => Err(PairError::NotYours(id.to_string())),
        Some(p) if p.revoked_at.is_some() => Err(PairError::Revoked(id.to_string())),
        Some(_) => Ok(()),
    }
}

/// Whether `by` may choose node `id` for their new computers: one of the
/// deployment's (`listed`), or a live node of theirs where BYOC is on.
pub fn choosable(byoc: Byoc, by: &str, id: &str, listed: bool, found: Option<&Paired>) -> Result<(), PairError> {
    if listed {
        return Ok(());
    }
    if !is_paired_id(id) {
        return Err(PairError::NotYours(id.to_string()));
    }
    match found {
        Some(p) if p.owner == by => may_dial(byoc, id, found),
        _ => Err(PairError::NotYours(id.to_string())),
    }
}

/// A node's secret, as hex, from its random bytes.
pub fn secret(random: [u8; SECRET_BYTES]) -> String {
    hex::encode(random)
}

#[cfg(test)]
mod tests {
    use super::*;

    const T0: i64 = 1_800_000_000_000;

    fn started() -> Pending {
        start(Byoc::On, Load::default(), " mac ", "aarch64", [7; 8], T0).unwrap()
    }

    // Goal: ids, codes and their forms: a paired id is the prefix and 16
    // hex; a user code is two groups of four of the alphabet, read back
    // whatever case or dashes it was typed with; a device code is 64 hex,
    // kept as its hash.
    #[test]
    fn names_and_codes() {
        let id = paired_id([0xab; 8]);
        assert_eq!(id, "paired-abababababababab");
        assert!(is_paired_id(&id) && crate::placement::valid_node_id(&id));
        for bad in ["paired-ABABABABABABABAB", "paired-abab", "box", "paired-abababababababab0", ""] {
            assert!(!is_paired_id(bad), "{bad}");
        }
        let code = user_code([0; 8]);
        assert_eq!(code, "BBBB-BBBB");
        let code = user_code([0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc, 0xde, 0xf0]);
        assert_eq!(code.len(), 9);
        assert!(code.bytes().enumerate().all(|(i, b)| if i == 4 { b == b'-' } else { USER_CODE_ALPHABET.contains(&b) }), "{code}");
        assert_eq!(parse_user_code(&code), Some(code.clone()));
        assert_eq!(parse_user_code(&code.to_lowercase().replace('-', " ")), Some(code.clone()));
        assert_eq!(parse_user_code(&code.replace('-', "")), Some(code.clone()));
        for bad in ["BBBB-BBB", "BBBB-BBBBB", "AAAA-AAAA", "BBBB-BBB1", "", &"B".repeat(40)] {
            assert_eq!(parse_user_code(bad), None, "{bad}");
        }
        let device = device_code([0xab; 32]);
        assert_eq!(device.len(), 64);
        let hash = device_hash(&device).unwrap();
        assert_ne!(hash, device);
        assert_eq!(device_hash(&device), Some(hash), "the same code, the same hash");
        assert_eq!(device_hash("short"), None);
        assert_eq!(device_hash(&device.to_uppercase()), None);
        assert_eq!(secret([1; 32]).len(), 64);
    }

    // Goal (valid): a pairing starts, a person may approve it, the node's
    // polls wait until then, and the approved poll names the node.
    #[test]
    fn a_pairing_starts_is_approved_and_polled() {
        let p = started();
        assert_eq!((p.name.as_str(), p.arch, p.expires_at, p.interval_s), ("mac", Arch::Aarch64, T0 + PAIRING_TTL_MS, POLL_INTERVAL_S));
        assert_eq!(approvable(Byoc::On, Some(&p), Holder::default(), T0 + 1000), Ok(()));
        let (first, p) = poll(&p, T0 + 1000);
        assert_eq!(first, Polled::Pending { interval_s: POLL_INTERVAL_S });
        let (again, p) = poll(&p, T0 + 1000 + 5000);
        assert_eq!(again, Polled::Pending { interval_s: POLL_INTERVAL_S });
        let approved = Pending { node: Some(paired_id([1; 8])), ..p };
        let (polled, _) = poll(&approved, T0 + 20_000);
        assert_eq!(polled, Polled::Approved { node: "paired-0101010101010101".into() });
    }

    // Goal (invalid): a name or an architecture that is none is refused at
    // the start; a wrong code is no pairing; and BYOC off refuses every step.
    #[test]
    fn wrong_codes_and_names_and_byoc_off() {
        assert!(matches!(start(Byoc::On, Load::default(), "", "aarch64", [0; 8], T0), Err(PairError::Invalid(_))));
        assert!(matches!(start(Byoc::On, Load::default(), &"m".repeat(NAME_BYTES_MAX + 1), "aarch64", [0; 8], T0), Err(PairError::Invalid(_))));
        assert!(matches!(start(Byoc::On, Load::default(), "a\nb", "aarch64", [0; 8], T0), Err(PairError::Invalid(_))));
        assert!(start(Byoc::On, Load::default(), "mac", "riscv64", [0; 8], T0).unwrap_err().to_string().contains("x86_64 or aarch64"));
        assert_eq!(approvable(Byoc::On, None, Holder::default(), T0), Err(PairError::NoSuchCode));
        assert_eq!(start(Byoc::Off, Load::default(), "mac", "aarch64", [0; 8], T0), Err(PairError::ByocOff));
        assert_eq!(approvable(Byoc::Off, Some(&started()), Holder::default(), T0), Err(PairError::ByocOff));
        assert!(PairError::ByocOff.to_string().contains("FRAGMENT_BYOC is off"));
    }

    // Goal: a code past its time is neither approved nor polled into
    // anything but its expiry.
    #[test]
    fn an_expired_code() {
        let p = started();
        let late = T0 + PAIRING_TTL_MS;
        assert_eq!(approvable(Byoc::On, Some(&p), Holder::default(), late), Err(PairError::Expired));
        assert_eq!(approvable(Byoc::On, Some(&p), Holder::default(), late - 1), Ok(()));
        assert_eq!(poll(&p, late).0, Polled::Expired);
    }

    // Goal (replay): an approved code is not approved again; its node's
    // answer is had once (the registry deletes the spent pairing, so the
    // replayed poll finds none: `registry/nodes.rs`); a second poll of an
    // approval names the same node, never another.
    #[test]
    fn a_used_code_is_not_approved_again() {
        let approved = Pending { node: Some(paired_id([2; 8])), ..started() };
        assert_eq!(approvable(Byoc::On, Some(&approved), Holder::default(), T0), Err(PairError::Used));
        assert_eq!(poll(&approved, T0).0, poll(&approved, T0 + 1).0);
    }

    // Goal (revoke): only its owner revokes a node, once (again changes
    // nothing); another's is no node of theirs. A revoked node's dial is
    // refused, as is any with BYOC off; it can no longer be chosen, and
    // neither can another's node, nor an id that is no node.
    #[test]
    fn revoking() {
        let mine = Paired { id: paired_id([3; 8]), owner: "id:ann".into(), revoked_at: None };
        assert_eq!(revocable("id:ann", &mine.id, Some(&mine)), Ok(true));
        assert_eq!(revocable("id:bob", &mine.id, Some(&mine)), Err(PairError::NotYours(mine.id.clone())));
        assert_eq!(revocable("id:ann", "paired-ffffffffffffffff", None), Err(PairError::NotYours("paired-ffffffffffffffff".into())));
        assert_eq!(may_dial(Byoc::On, &mine.id, Some(&mine)), Ok(()));
        assert_eq!(may_dial(Byoc::Off, &mine.id, Some(&mine)), Err(PairError::ByocOff));
        assert_eq!(choosable(Byoc::On, "id:ann", &mine.id, false, Some(&mine)), Ok(()));
        let revoked = Paired { revoked_at: Some(T0), ..mine.clone() };
        assert_eq!(revocable("id:ann", &mine.id, Some(&revoked)), Ok(false), "revoked again: nothing changes");
        assert_eq!(may_dial(Byoc::On, &mine.id, Some(&revoked)), Err(PairError::Revoked(mine.id.clone())));
        assert!(PairError::Revoked(mine.id.clone()).to_string().contains("revoked by its owner"));
        assert_eq!(may_dial(Byoc::On, &mine.id, None), Err(PairError::NotYours(mine.id.clone())));
        assert_eq!(choosable(Byoc::On, "id:ann", &mine.id, false, Some(&revoked)), Err(PairError::Revoked(mine.id.clone())));
        assert_eq!(choosable(Byoc::On, "id:bob", &mine.id, false, Some(&mine)), Err(PairError::NotYours(mine.id.clone())));
        assert_eq!(choosable(Byoc::Off, "id:ann", &mine.id, false, Some(&mine)), Err(PairError::ByocOff));
        assert_eq!(choosable(Byoc::Off, "id:ann", "box", true, None), Ok(()), "the deployment's nodes, BYOC or not");
        assert_eq!(choosable(Byoc::On, "id:ann", "nonesuch", false, None), Err(PairError::NotYours("nonesuch".into())));
    }

    // Goal (rate limits): pairings waiting and begun this minute are
    // bounded; a poll sooner than its interval slows the node down, its
    // interval growing to a ceiling; a person's wrong codes are counted
    // and, past the bound, nothing is looked up; a person holds at most so
    // many nodes.
    #[test]
    fn the_bounds() {
        let busy = start(Byoc::On, Load { pending: PENDING_MAX, last_minute: 0 }, "mac", "aarch64", [0; 8], T0).unwrap_err();
        assert!(matches!(busy, PairError::Busy(_)), "{busy}");
        assert!(matches!(start(Byoc::On, Load { pending: 0, last_minute: STARTS_PER_MINUTE }, "mac", "aarch64", [0; 8], T0), Err(PairError::Busy(_))));
        assert!(start(Byoc::On, Load { pending: PENDING_MAX - 1, last_minute: STARTS_PER_MINUTE - 1 }, "mac", "aarch64", [0; 8], T0).is_ok());
        let p = started();
        let (_, p) = poll(&p, T0);
        let (fast, p) = poll(&p, T0 + 1000);
        assert_eq!(fast, Polled::SlowDown { interval_s: POLL_INTERVAL_S + SLOW_DOWN_S });
        let (fast, p) = poll(&p, T0 + 2000);
        assert_eq!(fast, Polled::SlowDown { interval_s: POLL_INTERVAL_S + 2 * SLOW_DOWN_S });
        let (ok, _) = poll(&p, T0 + 2000 + i64::from(p.interval_s) * 1000);
        assert_eq!(ok, Polled::Pending { interval_s: p.interval_s });
        let mut q = started();
        for i in 0..40 {
            q = poll(&q, T0 + i).1;
        }
        assert_eq!(q.interval_s, POLL_INTERVAL_MAX_S);
        let fresh = started();
        assert_eq!(approvable(Byoc::On, Some(&fresh), Holder { misses: MISSES_MAX, ..Holder::default() }, T0), Err(PairError::Misses));
        assert_eq!(approvable(Byoc::On, None, Holder { misses: MISSES_MAX, ..Holder::default() }, T0), Err(PairError::Misses), "past the bound, no lookup at all");
        assert_eq!(approvable(Byoc::On, Some(&fresh), Holder { misses: MISSES_MAX - 1, ..Holder::default() }, T0), Ok(()));
        assert!(matches!(approvable(Byoc::On, Some(&fresh), Holder { live: NODES_MAX, ..Holder::default() }, T0), Err(PairError::Full(_))));
        assert!(matches!(approvable(Byoc::On, Some(&fresh), Holder { live: 1, rows: ROWS_MAX, misses: 0 }, T0), Err(PairError::Full(_))));
    }
}
