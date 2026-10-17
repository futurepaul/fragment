//! Guards over the cell's source that its types alone cannot state.

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::Path;

    /// Every `.rs` file under `cell/src`, by its path there, with its text.
    fn cell_sources() -> BTreeMap<String, String> {
        let root = crate::devstack::repo_root().join("cell/src");
        let mut files = BTreeMap::new();
        let mut dirs = vec![root.clone()];
        // bounded: the cell's own tree
        while let Some(dir) = dirs.pop() {
            for entry in std::fs::read_dir(&dir).expect("cell/src reads") {
                let path = entry.expect("an entry").path();
                if path.is_dir() {
                    dirs.push(path);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    let rel = path.strip_prefix(&root).expect("under cell/src").to_string_lossy().into_owned();
                    files.insert(rel, std::fs::read_to_string(&path).expect("a source file"));
                }
            }
        }
        assert!(files.len() > 20 && files.contains_key("auth.rs"), "the cell's sources were found");
        files
    }

    /// Where `needle` occurs in the cell, by file, ignoring comments.
    fn uses(files: &BTreeMap<String, String>, needle: &str) -> BTreeMap<String, usize> {
        let mut found = BTreeMap::new();
        for (path, text) in files {
            let n = text.lines().filter(|l| !l.trim_start().starts_with("//")).filter(|l| l.contains(needle)).count();
            if n > 0 {
                found.insert(path.clone(), n);
            }
        }
        found
    }

    fn only(file: &str, n: usize) -> BTreeMap<String, usize> {
        BTreeMap::from([(file.to_string(), n)])
    }

    /// Goal: a browser round trip finishes only in the browser that began
    /// it (docs/api.md, Sign-in; audit H3), and the cell's types say so:
    /// `auth::Bound` has private fields, so no other module can make one,
    /// and what finishes a round trip takes one. This holds what the types
    /// cannot: that `auth.rs` makes a `Bound` in `trip_bound` alone, from
    /// the request's own cookie; that its fields stay private; that no
    /// other file reads a round trip's cookie or compares states; and that
    /// the finished round trips (`Exchange`, `computer::SignedIn`) are made
    /// only from a `Bound` and are never `Deserialize`. Method: the cell's
    /// source, read.
    #[test]
    fn a_round_trip_is_bound_only_by_its_request() {
        let files = cell_sources();
        let auth = &files["auth.rs"];
        assert!(
            auth.contains("pub(crate) struct Bound {\n    trip: Trip,\n    state: String,\n}"),
            "Bound's fields are private to auth.rs, and it derives nothing (no Clone, no Deserialize)"
        );
        assert!(!auth.contains("#[derive(Clone)]\npub(crate) struct Bound") && !auth.contains("Deserialize)]\npub(crate) struct Bound"), "Bound derives nothing");
        assert_eq!(uses(&files, "Bound { trip"), only("auth.rs", 1), "a Bound is made once, in trip_bound");
        let made_at = auth.find("Bound { trip").expect("made");
        let trip_bound = auth.find("pub(crate) fn trip_bound(req: &Request").expect("trip_bound reads the request");
        let next_fn = auth[trip_bound + 1..].find("\nfn ").or_else(|| auth[trip_bound + 1..].find("\npub")).map(|i| i + trip_bound + 1).unwrap_or(auth.len());
        assert!((trip_bound..next_fn).contains(&made_at), "inside trip_bound");
        assert_eq!(uses(&files, "same_state("), only("auth.rs", 1), "states are compared in trip_bound alone");
        assert_eq!(uses(&files, ".cookie()"), only("auth.rs", 3), "a round trip's cookie is set, spent and read in auth.rs alone");
        for cookie in ["\"fragment_login\"", "\"fragment_switch\"", "\"fragment_own_key\""] {
            assert!(uses(&files, cookie).is_empty(), "{cookie} is named by fragment_core::roundtrip alone");
        }
        // the finished round trips: made from a Bound, never decoded
        let calls = &files["registry/calls.rs"];
        assert!(calls.contains("#[derive(Serialize)]\n#[serde(rename_all = \"camelCase\")]\npub(crate) struct Exchange {\n    state: String,"), "Exchange is Serialize only, its fields private");
        assert_eq!(uses(&files, "Exchange { state"), only("registry/calls.rs", 1), "an Exchange is made in Exchange::of alone");
        assert!(calls.contains("pub(crate) fn of(bound: crate::auth::Bound,"), "from a Bound");
        let computer = &files["computer.rs"];
        assert!(computer.contains("#[derive(serde::Serialize)]\npub(crate) struct SignedIn {"), "SignedIn is Serialize only");
        assert_eq!(uses(&files, "SignedIn { computer"), only("computer.rs", 1), "a SignedIn is made in SignedIn::of alone");
        assert!(computer.contains("pub(crate) fn of(bound: crate::auth::Bound,"), "from a Bound");
        assert_eq!(uses(&files, "\"computer/signed-in\""), only("computer.rs", 2), "the computer's signed-in is asked through signed_in alone (and answered by the computer)");
        assert!(Path::new(&crate::devstack::repo_root().join("crates/core/src/roundtrip.rs")).exists());
    }
}
