//! Blessed templates (docs/cloudflare-v1.md, decision 40): a chat or an
//! agent fragment names its template in `fragment.json` (`template`), and
//! the platform's release serves that template's manifest and its site, so
//! one deploy of the platform updates every chat, with no copies to drift.
//! The fragment's own repo holds only its face (`meta`) and its data (an
//! agent's `SOUL.md`). A fragment that wants other code forks: its files
//! become its own, and its `fragment.json` names no template.
//!
//! Blessed templates carry no app code yet (`app.mjs`): their fragments
//! are channels and a site. The cell serves them (cell/src/plane.rs
//! installs, cell/src/serve.rs serves the site).

use fragment_core::manifest::{self, Manifest};
use crate::Template;
use sha2::{Digest, Sha256};

/// The templates the platform serves from its release.
pub const BLESSED: [&str; 1] = ["agent"];

/// A blessed template's files, when `name` is one.
pub fn template(name: &str) -> Option<Template> {
    if !BLESSED.contains(&name) {
        return None;
    }
    crate::ALL.iter().find(|(n, _)| *n == name).map(|(_, t)| *t)
}

/// A blessed template's manifest, as it runs.
pub fn manifest(name: &str) -> Result<Manifest, String> {
    let t = template(name).ok_or_else(|| format!("no blessed template {name:?} (the platform serves {})", BLESSED.join(", ")))?;
    let bytes = t.iter().find(|(p, _)| *p == "fragment.json").map(|(_, b)| *b).ok_or_else(|| format!("the {name} template has no fragment.json"))?;
    manifest::parse(bytes).map_err(|e| format!("the {name} template's fragment.json: {e}"))
}

/// Which release of a blessed template a fragment runs: a hash of its
/// files, so a platform deploy that changes them is seen at the
/// fragment's next request (`plane.rs` installs it again).
pub fn release(name: &str) -> Option<String> {
    let t = template(name)?;
    let mut h = Sha256::new();
    for (path, bytes) in t {
        h.update(path.as_bytes());
        h.update([0]);
        h.update(bytes);
        h.update([0]);
    }
    Some(hex::encode(&h.finalize()[..12]))
}

/// The file a blessed template serves for repo path `path` (`site/…`).
pub fn site_file(name: &str, path: &str) -> Option<&'static [u8]> {
    if !path.starts_with("site/") {
        return None;
    }
    template(name)?.iter().find(|(p, _)| *p == path).map(|(_, b)| *b)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every blessed template parses, names its kind, carries no app code,
    /// and has a page.
    #[test]
    fn every_blessed_template_runs() {
        for name in BLESSED {
            let Some(t) = template(name) else { panic!("{name} is blessed but not under templates/") };
            let m = manifest(name).unwrap();
            assert!(m.kind.is_some(), "{name} names its kind");
            assert!(m.template.is_none(), "{name} names no template itself");
            assert!(!t.iter().any(|(p, _)| *p == "app.mjs"), "{name} carries no app code (blessed templates do not, yet)");
            assert!(site_file(name, "site/index.html").is_some(), "{name} has a page");
            assert_eq!(release(name), release(name));
        }
        assert!(template("todo").is_none(), "a template not blessed is only ever copied");
    }
}
