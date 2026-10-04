//! `cargo xtask check`'s JavaScript pass: every first-party JavaScript file
//! (the shell, the cell's Worker and browser modules, the templates' site
//! and app code, the e2e's fixtures) parsed by the pinned Node, so a syntax
//! error fails the check in about a second, naming its file and line. A
//! duplicate `const` in cell/shell/shell.js once passed the check and was
//! caught only by the e2e's shell-ui section, about 40 minutes into CI.
//!
//! Which files: each `.js`, `.mjs` and `.cjs` git tracks or would add
//! (untracked and not ignored, so a new file is checked before it is
//! staged; node_modules and target are ignored), less `VENDORED`.
//!
//! How each parses: as the browser or Worker loads it. A `.mjs` is an ES
//! module, and so is a `.js`: every one the cell serves is a `<script
//! type="module">` or imported by one, except the classic scripts named in
//! `CLASSIC`. Those, and any `.cjs`, are checked as CommonJS, Node's nearest
//! to a classic script (they differ only in CommonJS's wrapper: a top-level
//! `return` passes, and a top-level `require`, `module`, `exports`,
//! `__filename` or `__dirname` is taken).
//!
//! How: `node --check` on the pinned Node (never a `node` from PATH), one
//! process per file with the source on stdin and `--input-type` named, so
//! neither the extension nor package.json's `type` decides. Node starts in
//! about 20 ms, and a few files run at once, so the repo's few dozen take
//! well under a second. One process parsing them all would start once, but
//! a module's parse error in-process (`vm.SourceTextModule`) names no
//! line; `--check`'s report does.

use std::collections::BTreeSet;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{ensure, Context, Result};
use fragment_devstack::node::Node;

/// Built or vendored JavaScript, each prefix with where it is recorded:
/// never edited here, so never checked.
pub const VENDORED: [(&str, &str); 2] = [
    ("templates/notes/site/assets/", "the notes viewer's prebuilt bundle (docs/technical-debt-ledger.md)"),
    ("cell/shell/vendor/", "split-grid, vendored unmodified (cell/shell/CREDITS.md)"),
];

/// The `.js` files loaded as classic scripts, not modules.
pub const CLASSIC: [(&str, &str); 1] = [
    ("cell/sw.js", "the service worker client.mjs registers without `type: \"module\"` (served as __sw.js, cell/src/push.rs)"),
];

/// More files than this means the walk reached something it should not
/// (an unignored node_modules): the repo has about 50.
pub const FILES_MAX: usize = 1_000;
/// The `node --check` processes run at once.
pub const PROCESSES_MAX: usize = 8;
/// The file whose duplicate `const` this pass exists for: a walk that
/// misses it is broken, not clean.
const SHELL: &str = "cell/shell/shell.js";

/// How Node is told to parse a file (`--input-type`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputType {
    Module,
    CommonJs,
}

impl InputType {
    fn flag(self) -> &'static str {
        match self {
            InputType::Module => "--input-type=module",
            InputType::CommonJs => "--input-type=commonjs",
        }
    }
}

/// How a repo path parses, by its extension and `CLASSIC`; `None` when it
/// is not JavaScript.
pub fn input_type(path: &str) -> Option<InputType> {
    if path.ends_with(".mjs") {
        Some(InputType::Module)
    } else if path.ends_with(".cjs") || CLASSIC.iter().any(|(p, _)| *p == path) {
        Some(InputType::CommonJs)
    } else if path.ends_with(".js") {
        Some(InputType::Module)
    } else {
        None
    }
}

fn vendored(path: &str) -> bool {
    VENDORED.iter().any(|(prefix, _)| path.starts_with(prefix))
}

/// The repo's JavaScript to check, sorted: what git tracks or would add
/// (and is still on disk), less `VENDORED`.
pub fn files(root: &Path) -> Result<Vec<(String, InputType)>> {
    let out = Command::new("git")
        .args(["ls-files", "-z", "--cached", "--others", "--exclude-standard"])
        .current_dir(root)
        .output()
        .context("git ls-files")?;
    ensure!(out.status.success(), "git ls-files: {}", String::from_utf8_lossy(&out.stderr));
    let listed: BTreeSet<String> = out.stdout.split(|b| *b == 0).filter(|p| !p.is_empty()).map(|p| String::from_utf8_lossy(p).into_owned()).collect();
    // a path deleted but not yet committed is listed, and is not there
    select(listed.into_iter().filter(|p| root.join(p).is_file()))
}

/// The JavaScript among `paths`, less `VENDORED`, with how each parses.
/// Each `VENDORED` prefix must still match a file and each `CLASSIC` path
/// must still be listed: a stale entry would leave a moved file unchecked
/// or checked as the wrong kind, and say nothing.
pub fn select(paths: impl Iterator<Item = String>) -> Result<Vec<(String, InputType)>> {
    let mut found = vec![];
    let mut vendored_hit = [false; VENDORED.len()];
    for path in paths {
        let Some(kind) = input_type(&path) else { continue };
        if vendored(&path) {
            for (i, (prefix, _)) in VENDORED.iter().enumerate() {
                vendored_hit[i] |= path.starts_with(prefix);
            }
            continue;
        }
        found.push((path, kind));
        ensure!(found.len() <= FILES_MAX, "more than {FILES_MAX} JavaScript files to check: is node_modules or target no longer ignored?");
    }
    for (i, (prefix, why)) in VENDORED.iter().enumerate() {
        ensure!(vendored_hit[i], "js_syntax::VENDORED names {prefix} ({why}), and no JavaScript is there: remove or move the entry");
    }
    for (path, why) in CLASSIC {
        ensure!(found.iter().any(|(p, _)| p == path), "js_syntax::CLASSIC names {path} ({why}), and it is not there: remove or move the entry");
    }
    ensure!(found.iter().any(|(p, kind)| p == SHELL && *kind == InputType::Module), "the walk did not find {SHELL}: it is broken");
    Ok(found)
}

/// Parses each file under `root` with `node --check`, a few at once, and
/// fails with every file that does not parse, each named with its line.
pub fn check(node: &Node, cache: &Path, root: &Path, files: &[(String, InputType)]) -> Result<()> {
    assert!(files.len() <= FILES_MAX, "the files to check are bounded");
    let processes = std::thread::available_parallelism().map_or(1, |n| n.get()).clamp(1, PROCESSES_MAX);
    let share = files.len().div_ceil(processes).max(1);
    let reports: Vec<Result<Vec<String>>> = std::thread::scope(|s| {
        let workers: Vec<_> = files
            .chunks(share)
            .map(|part| {
                s.spawn(move || -> Result<Vec<String>> { part.iter().filter_map(|(path, kind)| check_one(node, cache, root, path, *kind).transpose()).collect() })
            })
            .collect();
        workers.into_iter().map(|w| w.join().expect("a check's thread does not panic")).collect()
    });
    let mut failed = vec![];
    for report in reports {
        failed.extend(report?);
    }
    failed.sort();
    ensure!(failed.is_empty(), "JavaScript that does not parse (node --check on Node {}), {} file(s):\n\n{}", node.release, failed.len(), failed.join("\n\n"));
    Ok(())
}

/// One file's `node --check`: `None` when it parses, else Node's report
/// with the file named.
fn check_one(node: &Node, cache: &Path, root: &Path, path: &str, kind: InputType) -> Result<Option<String>> {
    let source = std::fs::read(root.join(path)).with_context(|| format!("read {path}"))?;
    let mut child = node
        .command(cache)?
        .args(["--check", kind.flag()])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("run {} --check for {path}", node.node.display()))?;
    // written whole, then closed: Node reads stdin to its end before it
    // parses. A node that exits first (refusing a flag) closes the pipe, and
    // its report says why.
    let mut stdin = child.stdin.take().expect("stdin is piped");
    match stdin.write_all(&source) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => {}
        Err(e) => return Err(e).with_context(|| format!("write {path} to node --check")),
    }
    drop(stdin);
    let out = child.wait_with_output().with_context(|| format!("wait for node --check of {path}"))?;
    if out.status.success() {
        Ok(None)
    } else {
        Ok(Some(located(path, &String::from_utf8_lossy(&out.stderr))))
    }
}

/// Node's report on stdin's source, naming `path` instead: its
/// `[stdin]:<line>`, the source line and the caret under the error (both
/// blank at an unexpected end of input), and the error's message, without
/// Node's warnings before it or its own stack after. A report with no such
/// line is kept whole.
fn located(path: &str, stderr: &str) -> String {
    let mut lines = stderr.lines().skip_while(|l| !l.starts_with("[stdin]:"));
    let Some(first) = lines.next() else {
        return format!("{path}: node --check failed:\n{}", stderr.trim_end());
    };
    let mut report = vec![first.replacen("[stdin]", path, 1)];
    // taken by position: the source line is the file's, whatever it holds
    report.extend(lines.by_ref().take(2).filter(|l| !l.is_empty()).map(str::to_string));
    report.extend(lines.take_while(|l| !l.starts_with("    at ")).filter(|l| !l.is_empty()).map(str::to_string));
    report.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use fragment_devstack as devstack;

    /// A path's extension and `CLASSIC` decide how it parses; anything
    /// else is not JavaScript.
    #[test]
    fn a_file_parses_as_its_extension_and_classic_list_say() {
        for (path, kind) in [
            ("cell/shell/shell.js", Some(InputType::Module)),
            ("cell/client.mjs", Some(InputType::Module)),
            ("templates/chat/site/chat.js", Some(InputType::Module)),
            ("cell/sw.js", Some(InputType::CommonJs)),
            ("templates/x/sw.js", Some(InputType::Module)),
            ("tools/config.cjs", Some(InputType::CommonJs)),
            ("cell/src/lib.rs", None),
            ("templates/todo/site/index.html", None),
            ("templates/notes/src/viewer.mjs.map", None),
            ("package.json", None),
        ] {
            assert_eq!(input_type(path), kind, "{path}");
        }
    }

    /// The walk over the repo itself finds the shell, the templates' site
    /// and app code, and the classic service worker, and leaves out the
    /// vendored bundles.
    #[test]
    fn the_walk_covers_the_shell_and_templates_and_skips_vendored_code() {
        let found = files(&devstack::repo_root()).expect("the repo's walk");
        let kind = |path: &str| found.iter().find(|(p, _)| p == path).map(|(_, k)| *k);
        assert_eq!(kind("cell/shell/shell.js"), Some(InputType::Module));
        assert_eq!(kind("templates/todo/app.mjs"), Some(InputType::Module));
        assert_eq!(kind("templates/chat/site/chat.js"), Some(InputType::Module));
        assert_eq!(kind("cell/sw.js"), Some(InputType::CommonJs));
        assert!(found.iter().all(|(p, _)| !vendored(p)), "nothing vendored is checked");
        assert!(found.iter().all(|(p, _)| !p.split('/').any(|c| c == "node_modules" || c == "target")));
        assert!(found.windows(2).all(|w| w[0].0 < w[1].0), "sorted, each once");
    }

    /// A stale `VENDORED` or `CLASSIC` entry, or a walk without the shell,
    /// fails rather than checking less.
    #[test]
    fn a_stale_list_or_a_walk_without_the_shell_is_refused() {
        let paths = |extra: &[&str]| {
            let mut all: Vec<String> = ["cell/shell/shell.js", "cell/sw.js", "templates/notes/site/assets/viewer.js", "cell/shell/vendor/split-grid.js"].map(String::from).to_vec();
            all.extend(extra.iter().map(|p| p.to_string()));
            all
        };
        let selected = select(paths(&["templates/todo/app.mjs", "README.md"]).into_iter()).expect("a whole list");
        assert_eq!(selected.iter().map(|(p, _)| p.as_str()).collect::<Vec<_>>(), ["cell/shell/shell.js", "cell/sw.js", "templates/todo/app.mjs"]);
        let without = |gone: &str| select(paths(&[]).into_iter().filter(|p| p != gone));
        assert!(without("cell/sw.js").is_err(), "CLASSIC names a file that is gone");
        assert!(without("cell/shell/vendor/split-grid.js").is_err(), "VENDORED names a directory with no JavaScript");
        assert!(without("cell/shell/shell.js").is_err(), "the walk missed the shell");
        let many = (0..=FILES_MAX).map(|i| format!("gen/{i}.js"));
        assert!(select(paths(&[]).into_iter().chain(many)).is_err(), "more than FILES_MAX");
    }

    /// Node's report on stdin is narrowed to the file, its line, the caret
    /// and the message; one without a location is kept whole.
    #[test]
    fn a_report_names_the_file_and_line_not_stdin() {
        let stderr = "(node:20958) Warning: Failed to load the ES module: [stdin]. Make sure to set \"type\": \"module\".\n(Use `node --trace-warnings ...` to show where the warning was created)\n[stdin]:2\nconst a = 2;\n      ^\n\nSyntaxError: Identifier 'a' has already been declared\n    at checkSyntax (node:internal/main/check_syntax:84:5)\n    at node:internal/main/check_syntax:45:5\n\nNode.js v24.21.0\n";
        assert_eq!(located("cell/shell/shell.js", stderr), "cell/shell/shell.js:2\nconst a = 2;\n      ^\nSyntaxError: Identifier 'a' has already been declared");
        // a source line shaped as a stack frame is still the source line
        let frame_like = "[stdin]:7\n    at = at + ;\n              ^\n\nSyntaxError: Unexpected token ';'\n    at checkSyntax (node:internal/main/check_syntax:84:5)\n";
        assert_eq!(located("a.js", frame_like), "a.js:7\n    at = at + ;\n              ^\nSyntaxError: Unexpected token ';'");
        let end_of_input = "[stdin]:3\n\n\n\nSyntaxError: Unexpected end of input\n    at checkSyntax (node:internal/main/check_syntax:84:5)\n";
        assert_eq!(located("b.mjs", end_of_input), "b.mjs:3\nSyntaxError: Unexpected end of input");
        assert_eq!(located("x.js", "node: bad option: --input-type=nope\n"), "x.js: node --check failed:\nnode: bad option: --input-type=nope");
    }

    /// The pass on the real pinned Node: each kind of file that does not
    /// parse fails the check, named with its line, and a file that parses
    /// only as its own kind passes as that kind. Method: a scratch
    /// directory of good and broken files (a duplicate `const`, as once in
    /// the shell; an export of nothing; strict mode in a module; an
    /// `import` in a classic script), checked together and then only the
    /// good ones. Fetches the pinned Node into target/tools if it is not
    /// there (`cargo xtask check` has by the time its tests run).
    #[test]
    fn a_file_that_does_not_parse_fails_the_check_naming_its_line() {
        let root = devstack::repo_root();
        let node = devstack::node::locate(&root.join(devstack::TOOLS_DIR)).expect("the pinned Node");
        let cache = root.join(devstack::CACHE_DIR);
        let dir = std::env::temp_dir().join(format!("xtask-js-syntax-{}", devstack::random_hex(6)));
        let good: [(&str, InputType, &str); 4] = [
            ("site/app.js", InputType::Module, "import { x } from \"./lib.mjs\";\nexport const y = x + 1;\nawait Promise.resolve(y);\n"),
            ("lib.mjs", InputType::Module, "export const x = 1;\n"),
            // sloppy mode, which no module may be
            ("sw.js", InputType::CommonJs, "var n = 010;\nwith (self) { n += 1; }\n"),
            ("conf.cjs", InputType::CommonJs, "module.exports = { a: 1 };\n"),
        ];
        let broken: [(&str, InputType, &str); 4] = [
            ("site/shell.js", InputType::Module, "import { x } from \"../lib.mjs\";\nconst a = x;\nconst a = 2;\n"),
            ("lib/broken.mjs", InputType::Module, "const ok = 1;\nexport { nope };\n"),
            ("site/strict.js", InputType::Module, "export const n = 1;\nlet m = 010;\n"),
            ("classic.js", InputType::CommonJs, "self.onpush = () => {};\nimport \"./lib.mjs\";\n"),
        ];
        for (path, _, source) in good.iter().chain(&broken) {
            std::fs::create_dir_all(dir.join(path).parent().unwrap()).unwrap();
            std::fs::write(dir.join(path), source).unwrap();
        }
        let list = |set: &[(&str, InputType, &str)]| set.iter().map(|(p, k, _)| (p.to_string(), *k)).collect::<Vec<_>>();

        check(&node, &cache, &dir, &list(&good)).expect("the good files parse, each as its kind");
        let all = [list(&good), list(&broken)].concat();
        let err = check(&node, &cache, &dir, &all).expect_err("the broken files fail").to_string();
        assert!(err.starts_with(&format!("JavaScript that does not parse (node --check on Node {}), 4 file(s):", node.release)), "{err}");
        for (at, message) in [
            ("site/shell.js:3\nconst a = 2;", "SyntaxError: Identifier 'a' has already been declared"),
            ("lib/broken.mjs:2", "SyntaxError: Export 'nope' is not defined in module"),
            ("site/strict.js:2", "SyntaxError: Octal literals are not allowed in strict mode."),
            ("classic.js:2", "SyntaxError: Cannot use import statement outside a module"),
        ] {
            assert!(err.contains(at), "{at} in:\n{err}");
            assert!(err.contains(message), "{message} in:\n{err}");
        }
        assert!(!err.contains("[stdin]") && !err.contains("node:internal"), "named by path, without Node's stack:\n{err}");
        assert!(good.iter().all(|(p, _, _)| !err.contains(&format!("{p}:"))), "only the broken files are named:\n{err}");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
