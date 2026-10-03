//! Blessed templates (docs/cloudflare-v1.md, decision 40): a chat or an
//! agent fragment names its template in `fragment.json` (`template`), and
//! the platform's release serves that template's manifest, its site, and
//! its app code (`app.mjs`, with `applib/`), so one deploy of the platform
//! updates every chat, with no copies to drift. The fragment's own repo
//! holds only its face (`meta`) and its data (an agent's `SOUL.md`). A
//! fragment that wants other code forks: its files become its own, and its
//! `fragment.json` names no template.
//!
//! The cell installs a blessed fragment's code from here (cell/src/plane.rs:
//! the release's code under the release's identity, `loader_id`, so a
//! platform deploy that changes it starts a fresh worker, as a new commit
//! would) and serves its site (cell/src/serve.rs).

use std::collections::BTreeMap;
use std::sync::OnceLock;

use fragment_core::manifest::{self, Manifest};
use fragment_proto::limits;
use crate::Template;
use sha2::{Digest, Sha256};

/// The templates the platform serves from its release.
pub const BLESSED: [&str; 2] = ["agent", "chat"];

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
/// files, so a platform deploy that changes any of them is seen at the
/// fragment's next request (cell/src/plane.rs installs it again). The files
/// are constants of this build, so each hash is made once per isolate.
pub fn release(name: &str) -> Option<String> {
    static RELEASES: OnceLock<BTreeMap<&'static str, String>> = OnceLock::new();
    let releases = RELEASES.get_or_init(|| BLESSED.iter().filter_map(|n| Some((*n, hash(template(n)?)))).collect());
    releases.get(name).cloned()
}

fn hash(t: Template) -> String {
    let mut h = Sha256::new();
    for (path, bytes) in t {
        h.update(path.as_bytes());
        h.update([0]);
        h.update(bytes);
        h.update([0]);
    }
    hex::encode(&h.finalize()[..12])
}

/// The file a blessed template serves for repo path `path` (`site/…`).
pub fn site_file(name: &str, path: &str) -> Option<&'static [u8]> {
    if !path.starts_with("site/") {
        return None;
    }
    template(name)?.iter().find(|(p, _)| *p == path).map(|(_, b)| *b)
}

/// Whether `path` is app code: what a fragment on a blessed template may
/// not carry of its own (it forks to), and what the release serves it.
pub fn is_code(path: &str) -> bool {
    path == "app.mjs" || (path.starts_with("applib/") && (path.ends_with(".mjs") || path.ends_with(".js")))
}

/// A blessed template's app code as the cell installs it: `app.mjs`, its
/// `applib/` modules by path, and its identity, `blessed:<name>@<release>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Code {
    pub id: String,
    pub source: &'static str,
    pub modules: BTreeMap<String, &'static str>,
}

/// Why a template's code is no code the cell installs (a build that
/// carries it fails its tests: `every_blessed_template_runs`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodeFault {
    NotUtf8(&'static str),
    TooManyModules(usize),
    TooLarge(usize),
    /// `applib/` without an `app.mjs` to import it.
    LibWithoutApp,
}

impl CodeFault {
    pub fn message(&self) -> String {
        match self {
            CodeFault::NotUtf8(p) => format!("{p} is not UTF-8"),
            CodeFault::TooManyModules(n) => format!("applib/ has {n} modules; the limit is {}", limits::APPLIB_FILES_MAX),
            CodeFault::TooLarge(n) => format!("app.mjs and applib/ are {n} bytes; the limit is {}", limits::APP_MODULES_MAX_BYTES),
            CodeFault::LibWithoutApp => "applib/ without an app.mjs".into(),
        }
    }
}

/// A blessed template's app code, `None` when it carries none (its
/// fragments are channels and a site), held to an app's own limits.
pub fn code(name: &str) -> Result<Option<Code>, CodeFault> {
    let Some(t) = template(name) else { return Ok(None) };
    let text = |path: &'static str, bytes: &'static [u8]| std::str::from_utf8(bytes).map_err(|_| CodeFault::NotUtf8(path));
    let mut source = None;
    let mut modules = BTreeMap::new();
    let mut total = 0usize;
    for (path, bytes) in t.iter().filter(|(p, _)| is_code(p)) {
        total += bytes.len();
        match *path {
            "app.mjs" => source = Some(text(path, bytes)?),
            lib => {
                modules.insert(lib.to_string(), text(path, bytes)?);
            }
        }
    }
    if modules.len() > limits::APPLIB_FILES_MAX {
        return Err(CodeFault::TooManyModules(modules.len()));
    }
    if total > limits::APP_MODULES_MAX_BYTES || source.is_some_and(|s| s.len() > limits::SOURCE_MAX_BYTES) {
        return Err(CodeFault::TooLarge(total));
    }
    match source {
        None if modules.is_empty() => Ok(None),
        None => Err(CodeFault::LibWithoutApp),
        Some(source) => {
            let release = release(name).expect("a template found here has a release");
            Ok(Some(Code { id: format!("blessed:{name}@{release}"), source, modules }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every blessed template parses, names its kind, has a page, and its
    /// code, when it carries some, is within an app's limits and runs the
    /// operations its manifest declares (a chat's push, from its trigger).
    #[test]
    fn every_blessed_template_runs() {
        for name in BLESSED {
            let Some(t) = template(name) else { panic!("{name} is blessed but not under templates/") };
            let m = manifest(name).unwrap();
            assert!(m.kind.is_some(), "{name} names its kind");
            assert!(m.template.is_none(), "{name} names no template itself");
            assert!(site_file(name, "site/index.html").is_some(), "{name} has a page");
            assert_eq!(release(name), release(name));
            assert_eq!(release(name), Some(hash(t)), "the cached release is the files' hash");
            match code(name).unwrap_or_else(|e| panic!("{name}: {}", e.message())) {
                Some(c) => {
                    assert_eq!(c.id, format!("blessed:{name}@{}", release(name).unwrap()), "its code's identity is the release");
                    assert!(c.source.contains("export class App"), "{name}'s app.mjs exports App");
                    assert!(!m.operations.is_empty(), "{name} declares the operations its code runs");
                    for op in m.operations.keys() {
                        assert!(c.source.contains(&format!("{op}(")), "{name}'s app.mjs has a method for {op}");
                    }
                    for trigger in &m.triggers {
                        assert!(m.operations.contains_key(&trigger.run), "{name}'s trigger runs an operation it declares");
                    }
                }
                None => assert!(m.operations.is_empty() && m.triggers.is_empty(), "{name} declares no operations without code"),
            }
        }
        assert!(code("chat").unwrap().is_some(), "the chat carries its push (decision 9)");
        assert!(template("todo").is_none(), "a template not blessed is only ever copied");
        assert_eq!(code("todo"), Ok(None), "and its code is no release's");
    }

    /// Goal: what counts as code is what a fragment on a template may not
    /// carry of its own. Method: app.mjs and applib modules are code; data,
    /// a page's script, and a module outside applib/ are not.
    #[test]
    fn code_is_app_mjs_and_applib() {
        for p in ["app.mjs", "applib/x.mjs", "applib/a/b.js"] {
            assert!(is_code(p), "{p}");
        }
        for p in ["SOUL.md", "site/chat.js", "lib/x.mjs", "applib/notes.md", "app.js", "x/app.mjs"] {
            assert!(!is_code(p), "{p}");
        }
    }

    #[test]
    fn faults_say_why() {
        assert!(CodeFault::TooManyModules(65).message().contains("64"));
        assert!(CodeFault::NotUtf8("app.mjs").message().contains("app.mjs"));
    }
}
