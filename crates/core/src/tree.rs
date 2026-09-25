//! A pin's tree index (the cell's `tree` table, plane.rs) follows its
//! branch by the changes alone: when a pin moves, the files added or
//! changed are upserted and the ones gone are deleted, so the rows written
//! scale with the change, not with the tree.

use std::collections::{BTreeMap, BTreeSet};

use crate::codestorage::TreeEntry;

/// A file as the index holds it: what a change is judged by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Indexed {
    pub size: u64,
    pub mode: String,
    pub last_commit: String,
}

/// What moving a pin changes in its index.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct TreeDiff<'a> {
    /// Files added or changed, in the listing's order: one row each to upsert.
    pub upserts: Vec<&'a TreeEntry>,
    /// Paths the index holds that the listing does not: one row each to delete.
    pub removed: Vec<&'a str>,
}

impl TreeDiff<'_> {
    /// Every path the move changed, as the change feed and file triggers
    /// see them: the upserts, then the removals.
    pub fn paths(&self) -> Vec<String> {
        let mut out: Vec<String> = self.upserts.iter().map(|e| e.path.clone()).collect();
        out.extend(self.removed.iter().map(|p| p.to_string()));
        out
    }
}

/// The index's changes from `prior` (its rows now) to `listing` (the new
/// commit's tree). A file whose size, mode, or last commit differs is
/// changed: the last commit alone names its bytes at that path.
pub fn diff<'a>(prior: &'a BTreeMap<String, Indexed>, listing: &'a [TreeEntry]) -> TreeDiff<'a> {
    let upserts: Vec<&TreeEntry> = listing
        .iter()
        .filter(|e| match prior.get(&e.path) {
            Some(held) => held.size != e.size || held.mode != e.mode || held.last_commit != e.last_commit_sha,
            None => true,
        })
        .collect();
    let listed: BTreeSet<&str> = listing.iter().map(|e| e.path.as_str()).collect();
    let removed: Vec<&str> = prior.keys().map(String::as_str).filter(|p| !listed.contains(p)).collect();
    assert!(upserts.len() <= listing.len(), "an upsert is a listed file");
    assert!(removed.len() <= prior.len(), "a removal is a held file");
    TreeDiff { upserts, removed }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str, last_commit: &str) -> TreeEntry {
        TreeEntry { path: path.into(), size: 10, mode: "100644".into(), last_commit_sha: last_commit.into() }
    }

    fn held(listing: &[TreeEntry]) -> BTreeMap<String, Indexed> {
        listing.iter().map(|e| (e.path.clone(), Indexed { size: e.size, mode: e.mode.clone(), last_commit: e.last_commit_sha.clone() })).collect()
    }

    /// Goal: a one-file commit on a large tree writes one row (the audit's
    /// case: 10,000 row writes for a one-file commit on 5,000 files).
    /// Method: a 5,000-file index, then listings with one file changed,
    /// added, removed, re-moded, and nothing changed.
    #[test]
    fn a_move_writes_only_what_changed() {
        let before: Vec<TreeEntry> = (0..5000).map(|i| entry(&format!("data/{i:04}.txt"), "c1")).collect();
        let prior = held(&before);

        let mut changed = before.clone();
        changed[42].last_commit_sha = "c2".into();
        let d = diff(&prior, &changed);
        assert_eq!((d.upserts.len(), d.removed.len()), (1, 0));
        assert_eq!(d.paths(), vec!["data/0042.txt".to_string()]);

        let mut added = before.clone();
        added.push(entry("new.md", "c2"));
        let d = diff(&prior, &added);
        assert_eq!((d.upserts.len(), d.removed.len(), d.paths()), (1, 0, vec!["new.md".to_string()]));

        let removed: Vec<TreeEntry> = before.iter().filter(|e| e.path != "data/4999.txt").cloned().collect();
        let d = diff(&prior, &removed);
        assert_eq!((d.upserts.len(), d.removed), (0, vec!["data/4999.txt"]));

        let mut moded = before.clone();
        moded[7].mode = "100755".into();
        assert_eq!(diff(&prior, &moded).upserts.len(), 1, "a mode change is a change");

        assert_eq!(diff(&prior, &before), TreeDiff::default(), "the same tree writes nothing");
    }

    /// Goal: the first pin writes the whole tree, and a branch emptied
    /// removes it all. Method: an empty index against a listing, and back.
    #[test]
    fn from_and_to_nothing() {
        let listing = vec![entry("a", "c1"), entry("b", "c1")];
        let empty = BTreeMap::new();
        let d = diff(&empty, &listing);
        assert_eq!((d.upserts.len(), d.removed.len()), (2, 0));
        let prior = held(&listing);
        let d = diff(&prior, &[]);
        assert_eq!(d.paths(), vec!["a".to_string(), "b".to_string()]);
        assert_eq!((d.upserts.len(), d.removed), (0, vec!["a", "b"]));
    }
}
