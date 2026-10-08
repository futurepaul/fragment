//! A fragment's host is one DNS label (docs/api.md, Names):
//! `<label>--<username>`, and a branch deployment's mark (`--<branch>`).
//! These are the two refusals that keep it one, each saying why: a label
//! too long for its owner's username, at create, and a username that
//! would leave less than `LABEL_ROOM_MIN_BYTES` for its labels, where it is
//! chosen. Neither is ever cut to fit.

use fragment_proto::limits::{HOST_LABEL_MAX_BYTES, LABEL_ROOM_MIN_BYTES};
use fragment_proto::{label_room, split_fragment_name, username_max};

/// Whether the fragment `name` (`<label>.<username>`) has a host label of
/// at most 63 bytes where hosts carry `mark`; why not, if not.
pub fn host_fits(name: &str, mark: &str) -> Result<(), String> {
    let (label, username) = split_fragment_name(name).ok_or_else(|| format!("{name:?} is not <label>.<username>"))?;
    let room = label_room(username, mark);
    if label.len() <= room {
        return Ok(());
    }
    Err(format!(
        "{label} is too long: a fragment's address, {label}--{username}{mark}, is one DNS label of at most {HOST_LABEL_MAX_BYTES} bytes \
         (this one would be {}), so a label under {username} is at most {room} bytes here",
        label.len() + "--".len() + username.len() + mark.len()
    ))
}

/// Whether the username `username` leaves its person the room every
/// username leaves for labels where hosts carry `mark`; why not, if not.
pub fn username_fits(username: &str, mark: &str) -> Result<(), String> {
    let max = username_max(mark);
    if username.len() <= max {
        return Ok(());
    }
    Err(format!(
        "{username} is too long here: a fragment's address, <label>--<username>{mark}, is one DNS label of at most {HOST_LABEL_MAX_BYTES} bytes, \
         and every username leaves {LABEL_ROOM_MIN_BYTES} of them for its labels, so a username is at most {max} bytes on this deployment"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use fragment_proto::limits::{BRANCH_MAX_BYTES, USERNAME_MAX_BYTES, USERNAME_MIN_BYTES};
    use fragment_proto::{fragment_name, host_label};

    fn label(len: usize) -> String {
        format!("a{}", "b".repeat(len - 1))
    }

    /// Goal: a create is refused exactly when its host label would pass 63
    /// bytes. Valid: the label whose host label is exactly 63, and any
    /// shorter. Invalid: one byte more, refused saying why (the 63, the
    /// address, and how long a label may be) and never cut. A branch's
    /// mark counts: the label that fits on production is refused on a
    /// branch.
    #[test]
    fn a_label_fits_its_host_or_is_refused() {
        let paul = |len: usize| fragment_name(&label(len), "paul");
        // production: 63 - "--paul" = 57
        assert_eq!(host_label(&paul(57), "").unwrap().len(), 63);
        assert_eq!(host_fits(&paul(57), ""), Ok(()));
        assert_eq!(host_fits(&paul(1), ""), Ok(()));
        let refused = host_fits(&paul(58), "").unwrap_err();
        assert!(refused.contains("at most 63 bytes") && refused.contains("(this one would be 64)") && refused.contains("at most 57 bytes"), "{refused}");
        assert!(refused.contains(&format!("{}--paul,", label(58))), "it names the whole address: {refused}");
        // a branch: its mark takes from the label's room
        assert_eq!(host_label(&paul(53), "--p5").unwrap().len(), 63);
        assert_eq!(host_fits(&paul(53), "--p5"), Ok(()));
        let refused = host_fits(&paul(57), "--p5").unwrap_err();
        assert!(refused.contains("--paul--p5,") && refused.contains("(this one would be 67)") && refused.contains("at most 53 bytes"), "{refused}");
        // the longest label and the longest username: refused anywhere
        let longest = fragment_name(&label(63), &"u".repeat(USERNAME_MAX_BYTES));
        assert!(host_fits(&longest, "").is_err());
        // a label of the room every username leaves fits every username
        for len in USERNAME_MIN_BYTES..=USERNAME_MAX_BYTES {
            assert_eq!(host_fits(&fragment_name(&label(LABEL_ROOM_MIN_BYTES), &"u".repeat(len)), ""), Ok(()), "{len}");
        }
        assert!(host_fits("todo", "").is_err(), "a bare label is no fragment's name");
    }

    /// Goal: a username is refused where it is chosen exactly when it
    /// would leave its person less than 29 bytes for labels. Valid: every
    /// username of 32 bytes or fewer where hosts carry no mark. Invalid:
    /// on a branch, one byte past `32 - mark`, refused saying why.
    #[test]
    fn a_username_leaves_room_for_its_labels() {
        let u = |len: usize| "u".repeat(len);
        assert_eq!(username_fits(&u(USERNAME_MAX_BYTES), ""), Ok(()));
        assert_eq!(username_fits(&u(28), "--p5"), Ok(()));
        let refused = username_fits(&u(29), "--p5").unwrap_err();
        assert!(refused.contains("<label>--<username>--p5") && refused.contains("leaves 29") && refused.contains("at most 28 bytes"), "{refused}");
        let mark = format!("--{}", "b".repeat(BRANCH_MAX_BYTES));
        assert_eq!(username_fits(&u(14), &mark), Ok(()));
        assert!(username_fits(&u(15), &mark).is_err());
        // every username taken leaves the room: a label of 29 fits it
        for mark in ["", "--b", "--p5", &mark] {
            for len in USERNAME_MIN_BYTES..=USERNAME_MAX_BYTES {
                let fits = host_fits(&fragment_name(&label(LABEL_ROOM_MIN_BYTES), &u(len)), mark);
                assert!(username_fits(&u(len), mark).is_err() || fits.is_ok(), "{len} {mark}: {fits:?}");
            }
        }
    }
}
