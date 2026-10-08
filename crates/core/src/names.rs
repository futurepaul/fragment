//! A fragment's host is one DNS label (docs/api.md, Names): its name,
//! `<label>--<suffix>`, and a branch deployment's mark (`--<branch>`).
//! This is the refusal that keeps it one, saying why: a label too long for
//! this deployment's mark, at create. It is never cut to fit.

use fragment_proto::label_room;
use fragment_proto::limits::HOST_LABEL_MAX_BYTES;

/// Whether the fragment `name` (`<label>--<suffix>`) has a host label of
/// at most 63 bytes where hosts carry `mark`; why not, if not.
pub fn host_fits(name: &str, mark: &str) -> Result<(), String> {
    let (label, _) = fragment_proto::split_fragment_name(name).ok_or_else(|| format!("{name:?} is not <label>--<suffix>"))?;
    let room = label_room(mark);
    if label.len() <= room {
        return Ok(());
    }
    Err(format!(
        "{label} is too long: a fragment's address, {name}{mark}, is one DNS label of at most {HOST_LABEL_MAX_BYTES} bytes \
         (this one would be {}), so a label is at most {room} bytes here",
        name.len() + mark.len()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use fragment_proto::limits::{BRANCH_MAX_BYTES, LABEL_ROOM_MIN_BYTES};
    use fragment_proto::{fragment_name, host_label};

    fn named(len: usize) -> String {
        fragment_name(&format!("a{}", "b".repeat(len - 1)), [1, 2, 3])
    }

    /// Goal: a create is refused exactly when its host label would pass 63
    /// bytes. Valid: the label whose host label is exactly 63, and any
    /// shorter. Invalid: one byte more, refused saying why (the 63, the
    /// address, and how long a label may be) and never cut. A branch's
    /// mark counts: the label that fits on production is refused on a
    /// branch.
    #[test]
    fn a_label_fits_its_host_or_is_refused() {
        // production: 63 - "--k3x9" = 57, the longest label there is
        assert_eq!(host_label(&named(57), "").unwrap().len(), 63);
        assert_eq!(host_fits(&named(57), ""), Ok(()));
        assert_eq!(host_fits(&named(1), ""), Ok(()));
        // a branch: its mark takes from the label's room
        assert_eq!(host_label(&named(53), "--p5").unwrap().len(), 63);
        assert_eq!(host_fits(&named(53), "--p5"), Ok(()));
        let refused = host_fits(&named(54), "--p5").unwrap_err();
        assert!(refused.contains("at most 63 bytes") && refused.contains("(this one would be 64)") && refused.contains("at most 53 bytes"), "{refused}");
        assert!(refused.contains(&format!("{}--p5,", named(54))), "it names the whole address: {refused}");
        let refused = host_fits(&named(57), "--p5").unwrap_err();
        assert!(refused.contains("(this one would be 67)"), "{refused}");
        // a label of the room every deployment leaves fits every branch
        for branch in 1..=BRANCH_MAX_BYTES {
            assert_eq!(host_fits(&named(LABEL_ROOM_MIN_BYTES), &format!("--{}", "b".repeat(branch))), Ok(()), "{branch}");
        }
        assert!(host_fits("todo", "").is_err(), "a bare label is no fragment's name");
    }
}
