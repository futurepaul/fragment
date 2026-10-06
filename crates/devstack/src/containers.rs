//! A node's computers in Docker, kept its own: several stacks (worktrees,
//! sessions, the e2e beside `xtask dev`) share one Docker daemon.
//!
//! wrangler builds each image a config names as it boots, tagged
//! `cloudflare-dev/<class>-<image>-<hash>:<build id>` with a build id of
//! that boot's own, and at its teardown (a node's stop, Ctrl-C, its exit)
//! removes `docker ps -a --filter ancestor=<tag>` for each tag it built.
//! Docker resolves that filter to the image's ID, and the same Dockerfile
//! and context build the same ID in every worktree: one node's stop
//! removed every other node's computers built from that source, mid-run.
//! So each project builds its images from Dockerfiles of its own
//! (`scope_images`): the image's own with a label naming the project
//! (`PROJECT_LABEL`) on its final stage, which makes the image's ID the
//! project's alone while every layer before it stays a cache hit.
//!
//! workerd names a Durable Object's container `workerd-<worker>-<class>-<id>`
//! (wrangler dev's namespace key is `<worker>-<class>`) and the networking
//! sidecar the container shares a network with `<that>-proxy`, from the
//! shared `cloudflare/proxy-everything` image, unlabeled. wrangler's
//! teardown removes the containers and not the sidecars, and miniflare
//! stops workerd with SIGKILL, so workerd's own cleanup (which removes
//! both) never runs: every computer a node ran left its sidecar running.
//! `remove` removes what a project's nodes left: the containers named for
//! the Durable Objects its state holds, each checked against its label.
//! Both lean on wrangler's and workerd's internals at the pinned versions
//! (docs/technical-debt-ledger.md).

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, PoisonError};

use anyhow::{bail, ensure, Context, Result};
use serde_json::Value;

/// The label on every image a project's nodes build, and so on their
/// computers' containers (a container carries its image's labels): the
/// project's absolute path.
pub const PROJECT_LABEL: &str = "dev.fragment.project";

/// Objects of one class a project's state may hold before `remove`
/// refuses to guess: an e2e run makes a few dozen computers.
const OBJECTS_MAX: usize = 4096;

/// Passes `remove` makes, each listing and removing what is left: a
/// container a killed workerd asked for a moment before it died can
/// appear after one pass listed.
const PASSES_MAX: u32 = 3;

/// One removal at a time in this process: a signal's (`on_termination`)
/// and the run's own end may meet.
static REMOVING: Mutex<()> = Mutex::new(());

/// What `remove` removed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Removed {
    pub containers: u32,
}

/// The label value naming `project`: its absolute path, one line that a
/// Dockerfile's `LABEL` carries as is (no quote, backslash or `$` for it
/// to read as syntax).
fn project_label(project: &Path) -> Result<String> {
    let abs = std::path::absolute(project).with_context(|| format!("resolve {}", project.display()))?;
    let label = abs.to_str().with_context(|| format!("{} is not UTF-8", abs.display()))?.to_string();
    ensure!(!label.contains(['\n', '\r', '"', '\\', '$']), "{label}: a project's path names its images in a Dockerfile's LABEL, which would read one of its characters as syntax");
    Ok(label)
}

/// A Dockerfile (`text`) with `label` on its final stage, the stage
/// wrangler builds (it passes no `--target`). The label is metadata: the
/// image's ID changes, and no layer does.
fn labeled_dockerfile(text: &str, label: &str) -> String {
    let last = text.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or_default();
    assert!(!last.trim_end().ends_with('\\'), "a Dockerfile's last instruction is complete, not continued onto the label");
    let mut out = String::with_capacity(text.len() + label.len() + 96);
    out.push_str(text);
    if !text.ends_with('\n') {
        out.push('\n');
    }
    out.push_str("# this node project's own image (crates/devstack/src/containers.rs)\n");
    out.push_str(&format!("LABEL {PROJECT_LABEL}=\"{label}\"\n"));
    assert!(out.starts_with(text), "the image's own Dockerfile is kept whole");
    out
}

/// Points each image of `config` (its paths absolute: `absolute_images`)
/// at a Dockerfile of the project's own, written to
/// `<project>/.wrangler/images/<image>.Dockerfile`: the image's own,
/// labeled for the project (`labeled_dockerfile`). The build context is
/// the image's own, so is its `.dockerignore`: wrangler sends the
/// Dockerfile on standard input.
pub(crate) fn scope_images(config: &mut Value, project: &Path) -> Result<()> {
    let Some(containers) = config.get_mut("containers").and_then(|c| c.as_array_mut()) else {
        return Ok(());
    };
    let label = project_label(project)?;
    let dir = images_dir(project);
    for container in containers {
        let Some(images) = container.get_mut("images").and_then(|i| i.as_object_mut()) else {
            continue;
        };
        for (name, image) in images.iter_mut() {
            ensure!(!name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'), "image name {name:?} names a file of its own");
            // a registry image is pulled whole: nothing could label it
            let source = image["dockerfile"].as_str().with_context(|| format!("image {name} names a Dockerfile (a registry image cannot be the project's own)"))?;
            ensure!(Path::new(source).is_absolute(), "image {name}'s Dockerfile {source} is absolute (absolute_images)");
            let text = fs::read_to_string(source).with_context(|| format!("read {source}"))?;
            fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
            let own = dir.join(format!("{name}.Dockerfile"));
            fs::write(&own, labeled_dockerfile(&text, &label)).with_context(|| format!("write {}", own.display()))?;
            image["dockerfile"] = Value::String(own.display().to_string());
        }
    }
    Ok(())
}

/// A Durable Object's id from the name of its file in a namespace's
/// state directory (`<64 hex>.sqlite`), and nothing for the namespace's
/// own files (`metadata.sqlite`, `-wal`, `-shm`).
fn object_id(file: &str) -> Option<&str> {
    let id = file.strip_suffix(".sqlite")?;
    (id.len() == 64 && id.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))).then_some(id)
}

/// The names workerd gives the containers of the project's Durable
/// Objects: for each container class its config names, each object of
/// that class its state holds (`v3/do/<worker>-<class>/<id>.sqlite`), the
/// container `workerd-<worker>-<class>-<id>` and its sidecar `…-proxy`.
fn owned_names(project: &Path) -> Result<BTreeSet<String>> {
    let config = crate::read_config(project)?;
    let worker = config["name"].as_str().context("a wrangler config names its Worker")?;
    let mut names = BTreeSet::new();
    for container in config["containers"].as_array().into_iter().flatten() {
        let class = container["class_name"].as_str().context("a container names its class")?;
        let namespace = format!("{worker}-{class}");
        let dir = crate::state_dir(project).join("v3/do").join(&namespace);
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            // no object of the class ever stored anything: no container
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e).with_context(|| format!("read {}", dir.display())),
        };
        let mut objects = 0;
        for entry in entries {
            let file = entry?.file_name();
            let Some(id) = file.to_str().and_then(object_id) else { continue };
            objects += 1;
            ensure!(objects <= OBJECTS_MAX, "{} holds more than {OBJECTS_MAX} objects", dir.display());
            names.insert(format!("workerd-{namespace}-{id}"));
            names.insert(format!("workerd-{namespace}-{id}-proxy"));
        }
    }
    Ok(names)
}

/// Of `listing` (each container's name and its `PROJECT_LABEL`, empty
/// when it has none), the project's: a container named for one of its
/// objects (`names`) and labeled for it, and a sidecar named for one whose
/// container is gone or is the project's. An object's id is random unless
/// it comes from a fixed name, which another project's state can share;
/// the container such a name has there carries that project's label or
/// none, and is left, as is its sidecar.
fn doomed<'a>(listing: &'a [(String, String)], names: &BTreeSet<String>, label: &str) -> Vec<&'a str> {
    let ours = |name: &str| listing.iter().find(|(n, _)| n == name).is_none_or(|(_, l)| l == label);
    let mut out = vec![];
    for (name, container_label) in listing {
        if !names.contains(name) {
            continue;
        }
        let mine = match name.strip_suffix("-proxy") {
            Some(container) => ours(container),
            None => container_label == label,
        };
        if mine {
            out.push(name.as_str());
        }
    }
    assert!(out.iter().all(|n| names.contains(*n)), "only names of the project's objects are removed");
    out
}

/// `docker <args>`'s standard output; its standard error when it fails.
fn docker(args: &[&str]) -> Result<String> {
    let out = Command::new("docker").args(args).output().with_context(|| format!("run docker {} (is Docker running?)", args.first().copied().unwrap_or_default()))?;
    if !out.status.success() {
        bail!("docker {} failed ({}): {}", args.first().copied().unwrap_or_default(), out.status, String::from_utf8_lossy(&out.stderr).trim());
    }
    String::from_utf8(out.stdout).context("docker's output is UTF-8")
}

/// Every container's name and `PROJECT_LABEL`.
fn listing() -> Result<Vec<(String, String)>> {
    let format = format!("{{{{.Names}}}}\t{{{{.Label \"{PROJECT_LABEL}\"}}}}");
    let text = docker(&["ps", "--all", "--no-trunc", "--format", &format])?;
    Ok(text.lines().filter_map(|l| l.split_once('\t')).map(|(n, l)| (n.to_string(), l.to_string())).collect())
}

/// Removes the containers the project's nodes left (`doomed`), whatever
/// left them: wrangler's teardown, a crash, a signal. Call it once its
/// nodes are gone (stopped, or `kill_nodes`): a running node makes them
/// again. Errs naming any still there after `PASSES_MAX` passes.
pub fn remove(project: &Path) -> Result<Removed> {
    let _one = REMOVING.lock().unwrap_or_else(PoisonError::into_inner);
    let names = owned_names(project)?;
    if names.is_empty() {
        return Ok(Removed { containers: 0 });
    }
    let label = project_label(project)?;
    let mut removed = 0u32;
    let mut failure = None;
    for _ in 0..PASSES_MAX {
        let all = listing()?;
        let left = doomed(&all, &names, &label);
        if left.is_empty() {
            return Ok(Removed { containers: removed });
        }
        // a name gone since the listing (workerd removing its own) fails
        // the command for that name alone; the next pass lists what is left
        let mut args = vec!["rm", "--force", "--volumes"];
        args.extend(&left);
        match docker(&args) {
            Ok(out) => removed += u32::try_from(out.lines().count()).expect("a few names"),
            Err(e) => failure = Some(e),
        }
    }
    let all = listing()?;
    let left = doomed(&all, &names, &label);
    if left.is_empty() {
        return Ok(Removed { containers: removed });
    }
    let why = failure.map(|e| format!(" (last: {e:#})")).unwrap_or_default();
    bail!("{} container(s) of {} still there after {PASSES_MAX} passes: {}{why}", left.len(), project.display(), left.join(", "))
}

/// Where a project's own Dockerfiles are (`scope_images`).
fn images_dir(project: &Path) -> PathBuf {
    project.join(".wrangler/images")
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID_A: &str = "d9414676ec33b9ed42256fb85b1fb55740ed83efa7779aea713d7c2541a0e5c0";
    const ID_B: &str = "f86d64ac5f75693b8b0cd8b06698baccabcba78faab43b8e06678409255a22ef";

    fn name(id: &str) -> String {
        format!("workerd-fragment-Computer-{id}")
    }

    fn row(name: &str, label: &str) -> (String, String) {
        (name.to_string(), label.to_string())
    }

    /// The label goes on the final stage, after the image's own text,
    /// which is kept whole; quoted, so a path with spaces stays one value.
    #[test]
    fn a_dockerfile_gets_the_projects_label_last() {
        let text = "FROM scratch AS a\nRUN true\nFROM a\nENTRYPOINT [\"/x\"]";
        let out = labeled_dockerfile(text, "/w t/target/e2e/abc/cell");
        assert!(out.starts_with(text));
        assert!(out.ends_with("\nLABEL dev.fragment.project=\"/w t/target/e2e/abc/cell\"\n"));
        assert_eq!(out.matches("LABEL").count(), 1);
        // a text ending in a newline gets no blank line before the comment
        assert!(!labeled_dockerfile("FROM scratch\n", "/p").contains("\n\n"));
    }

    #[test]
    #[should_panic(expected = "not continued onto the label")]
    fn a_continued_last_line_is_refused() {
        labeled_dockerfile("FROM scratch\nRUN true \\\n", "/p");
    }

    /// A path a Dockerfile would read as syntax is refused, not escaped.
    #[test]
    fn a_label_is_a_plain_path() {
        assert!(project_label(Path::new("/a/b c/cell")).is_ok());
        for bad in ["/a/$HOME/cell", "/a/\"q\"/cell", "/a/b\\c", "/a/b\nc"] {
            assert!(project_label(Path::new(bad)).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn object_ids_are_the_namespaces_object_files() {
        assert_eq!(object_id(&format!("{ID_A}.sqlite")), Some(ID_A));
        for not in ["metadata.sqlite", &format!("{ID_A}.sqlite-wal"), &format!("{ID_A}.sqlite-shm"), &format!("{}.sqlite", &ID_A[1..]), &format!("{}.sqlite", ID_A.to_uppercase())] {
            assert_eq!(object_id(not), None, "{not}");
        }
    }

    /// The project's state names two computers: both containers, each
    /// labeled for it, go, as does the sidecar of one whose container
    /// wrangler's teardown removed already. Another project's container is
    /// never named, and stays, whatever its label.
    #[test]
    fn a_project_removes_its_own_containers_and_sidecars() {
        let names: BTreeSet<String> = [ID_A, ID_B].iter().flat_map(|id| [name(id), format!("{}-proxy", name(id))]).collect();
        let listing = vec![
            row(&name(ID_A), "/p"),
            row(&format!("{}-proxy", name(ID_A)), ""),
            // ID_B's container is gone; its sidecar is left over
            row(&format!("{}-proxy", name(ID_B)), ""),
            row("workerd-fragment-Computer-0000000000000000000000000000000000000000000000000000000000000000", "/other"),
            row("workerd-fragment-Computer-0000000000000000000000000000000000000000000000000000000000000000-proxy", ""),
            row("minio-ci", ""),
        ];
        let doomed = doomed(&listing, &names, "/p");
        assert_eq!(doomed, [name(ID_A), format!("{}-proxy", name(ID_A)), format!("{}-proxy", name(ID_B))]);
    }

    /// An object whose id another project's state shares (an id from a
    /// fixed name): that project's container, labeled for it or for none,
    /// is not this project's, nor is its sidecar.
    #[test]
    fn a_shared_id_with_another_projects_container_is_left() {
        let names: BTreeSet<String> = [name(ID_A), format!("{}-proxy", name(ID_A))].into();
        for other in ["/other", ""] {
            let listing = vec![row(&name(ID_A), other), row(&format!("{}-proxy", name(ID_A)), "")];
            assert!(doomed(&listing, &names, "/p").is_empty(), "labeled {other:?}");
        }
    }

    /// The images of a config point at the project's own Dockerfiles,
    /// labeled for it; their contexts and build variables are unchanged.
    #[test]
    fn scoping_points_each_image_at_the_projects_dockerfile() {
        let root = std::env::temp_dir().join(format!("devstack-scope-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let project = root.join("cell");
        fs::create_dir_all(&project).unwrap();
        let source = root.join("Dockerfile");
        fs::write(&source, "FROM scratch\n").unwrap();
        let mut config = serde_json::json!({ "containers": [{ "class_name": "Computer", "images": {
            "stub": { "dockerfile": source, "build_context": root },
            "stub-next": { "dockerfile": source, "build_context": root, "build_vars": { "V": "2" } },
        } }] });
        scope_images(&mut config, &project).unwrap();
        for (image, file) in [("stub", "stub.Dockerfile"), ("stub-next", "stub-next.Dockerfile")] {
            let entry = &config["containers"][0]["images"][image];
            let own = images_dir(&project).join(file);
            assert_eq!(entry["dockerfile"], own.display().to_string());
            assert_eq!(entry["build_context"], root.display().to_string());
            assert_eq!(fs::read_to_string(&own).unwrap(), labeled_dockerfile("FROM scratch\n", &project_label(&project).unwrap()));
        }
        assert_eq!(config["containers"][0]["images"]["stub-next"]["build_vars"]["V"], "2");
        // a registry image cannot be labeled: refused, not passed through
        let mut pulled = serde_json::json!({ "containers": [{ "images": { "x": { "image": "docker.io/x:1" } } }] });
        assert!(scope_images(&mut pulled, &project).is_err());
        fs::remove_dir_all(&root).unwrap();
    }
}
