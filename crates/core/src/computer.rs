//! A fragment's own computer (docs/computers.md): the shell the platform
//! runs on it through its Sprite's exec, and how it reads the answers.
//!
//! `job.computer.exec` runs its command detached (`START`), journaled on
//! the computer's disk under `~/.fragment/exec/<id>`: the job script, its
//! deadline, the runner's pid, the output, and, once it ends, its code. The
//! id is the run's and the step's, never the attempt's, and `START` makes
//! the directory as its lock, so a second start of the same id (a retried
//! or replayed step) starts nothing: it reads the first one's result.
//! `POLL` waits on the computer up to its wait for the code, and `READ`
//! reads the output in chunks that fit `KEYS`' answer. Every answer is
//! base64 between two marker lines, read back by `answer`, so whatever a
//! Sprite's exec adds around its output is left out.

use serde::Deserialize;
use serde_json::{json, Value};

use crate::steps::ComputerExec;

pub const TIMEOUT_DEFAULT_MS: u64 = 10 * 60_000;
pub const TIMEOUT_MAX_MS: u64 = 60 * 60_000;
/// Each of stdout and stderr keeps this much; the rest is dropped.
pub const OUTPUT_MAX_BYTES: usize = 256 * 1024;
const COMMAND_MAX_BYTES: usize = 64 * 1024;
const CWD_MAX_BYTES: usize = 1024;
const ENV_MAX: usize = 64;
const ENV_VALUE_MAX_BYTES: usize = 8 * 1024;
/// How long one poll waits on the computer for the command to end (under
/// `KEYS`' 180 s for an exec, and a job step's 5 minutes).
pub const POLL_WAIT_S: u64 = 100;
/// A `READ`'s bytes: their base64 fits `KEYS`' 64 KiB answer.
pub const CHUNK_BYTES: usize = 36 * 1024;
/// A result's JSON stays this far under a step result's 1 MiB.
const RESULT_MAX_BYTES: usize = fragment_proto::limits::RESULT_MAX_BYTES - 4096;

const BEGIN: &str = "FRAGMENT-EXEC-BEGIN";
const END: &str = "FRAGMENT-EXEC-END";

/// `bash -c START fragment-exec <id> <timeout s>`, the job script on
/// stdin: starts it unless `<id>` was started before. The runner records
/// its pid, stops the command at its timeout (code 124, as timeout(1)),
/// keeps the first `OUTPUT_MAX_BYTES` of each stream, then writes the code.
pub const START: &str = r#"set -e
root="$HOME/.fragment/exec"; d="$root/$1"; mkdir -p "$root"
if mkdir "$d" 2>/dev/null; then
cat > "$d/job"; echo $(( $(date +%s) + $2 )) > "$d/deadline"
run='d=$1; echo $$ > "$d/pid"; set -m
bash "$d/job" > "$d/stdout" 2> "$d/stderr" < /dev/null & job=$!
( sleep "$2"; : > "$d/timedout"; kill -TERM -$job || kill -TERM $job; sleep 5; kill -KILL -$job || kill -KILL $job ) > /dev/null 2>&1 & watch=$!
wait $job; code=$?; kill -TERM -$watch 2> /dev/null || kill -TERM $watch
if [ -e "$d/timedout" ]; then code=124; fi
for s in stdout stderr; do if [ $(( $(wc -c < "$d/$s") )) -gt 262144 ]; then head -c 262144 "$d/$s" > "$d/$s.t"; mv "$d/$s.t" "$d/$s"; : > "$d/truncated"; fi; done
echo $code > "$d/code.t"; mv "$d/code.t" "$d/code"'
if command -v setsid > /dev/null; then detach=setsid; else detach=; fi
$detach nohup bash -c "$run" fragment-run "$d" "$2" < /dev/null > /dev/null 2>&1 &
find "$root" -mindepth 1 -maxdepth 1 -type d -mtime +30 -exec rm -rf {} + 2> /dev/null || true
fi
printf 'FRAGMENT-EXEC-BEGIN\n%s\nFRAGMENT-EXEC-END\n' "$(printf '{"state":"started"}' | base64)""#;

/// `bash -c POLL fragment-exec <id> <wait s>`: the command's state once it
/// ended, or once the wait is over (a line every few seconds meanwhile, so
/// the exec is never idle). One whose runner is gone, or that is past its
/// deadline, was interrupted (its computer stopped): it never runs again.
pub const POLL: &str = r#"d="$HOME/.fragment/exec/$1"; end=$(( $(date +%s) + $2 )); n=0
state() {
if [ -e "$d/code" ]; then t=false; if [ -e "$d/truncated" ]; then t=true; fi
printf '{"state":"done","code":%d,"out":%d,"err":%d,"truncated":%s}' "$(cat "$d/code")" $(( $(wc -c < "$d/stdout") )) $(( $(wc -c < "$d/stderr") )) $t
elif [ ! -d "$d" ]; then printf '{"state":"lost"}'
elif [ $(date +%s) -gt $(( $(cat "$d/deadline" 2> /dev/null || echo 0) + 60 )) ]; then printf '{"state":"interrupted"}'
elif [ -e "$d/pid" ] && ! kill -0 "$(cat "$d/pid")" 2> /dev/null && [ ! -e "$d/code" ]; then printf '{"state":"interrupted"}'
else printf '{"state":"running"}'; fi
}
while s=$(state); [ "$s" = '{"state":"running"}' ] && [ $(date +%s) -lt $end ]; do n=$((n + 1)); if [ $((n % 20)) = 0 ]; then echo; fi; sleep 0.5; done
printf 'FRAGMENT-EXEC-BEGIN\n%s\nFRAGMENT-EXEC-END\n' "$(printf '%s' "$s" | base64)""#;

/// `bash -c READ fragment-exec <id> <stdout|stderr> <offset> <length>`.
pub const READ: &str = r#"printf 'FRAGMENT-EXEC-BEGIN\n'; tail -c +$(( $3 + 1 )) "$HOME/.fragment/exec/$1/$2" | head -c $4 | base64; printf 'FRAGMENT-EXEC-END\n'"#;

/// What `POLL` says.
#[derive(Debug, PartialEq, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ExecState {
    Started,
    Running,
    /// Ended: its code, and how many bytes of each stream it kept.
    Done { code: i64, out: usize, err: usize, truncated: bool },
    Interrupted,
    /// No such command there (its computer was made again since).
    Lost,
}

/// The args as a job gave them, checked before anything runs.
pub fn check(e: &ComputerExec) -> Result<(), String> {
    let text = |what: &str, s: &str, max: usize| match s.len() > max || s.contains('\0') {
        true => Err(format!("job.computer.exec: {what} is at most {max} bytes, with no NUL")),
        false => Ok(()),
    };
    if e.command.trim().is_empty() {
        return Err("job.computer.exec: the command is empty".into());
    }
    text("the command", &e.command, COMMAND_MAX_BYTES)?;
    text("cwd", e.cwd.as_deref().unwrap_or(""), CWD_MAX_BYTES)?;
    if e.timeout_ms.is_some_and(|t| !(1000..=TIMEOUT_MAX_MS).contains(&t)) {
        return Err(format!("job.computer.exec: a timeout is 1 second to {} minutes", TIMEOUT_MAX_MS / 60_000));
    }
    if e.env.len() > ENV_MAX {
        return Err(format!("job.computer.exec: at most {ENV_MAX} env names"));
    }
    for (k, v) in &e.env {
        let name = k.bytes().next().is_some_and(|b| b.is_ascii_alphabetic() || b == b'_') && k.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_');
        if !name || k.len() > 128 {
            return Err(format!("job.computer.exec: env name {k:?} is letters, digits, and _, not starting with a digit"));
        }
        text(&format!("env {k}"), v, ENV_VALUE_MAX_BYTES)?;
    }
    Ok(())
}

/// Its timeout, in whole seconds.
pub fn timeout_s(e: &ComputerExec) -> u64 {
    e.timeout_ms.unwrap_or(TIMEOUT_DEFAULT_MS).div_ceil(1000)
}

/// `s` as one shell word.
pub fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// A path as the job gave it: `~`, `~/…`, or relative, under the home.
fn place(path: &str) -> String {
    match path.strip_prefix('~').filter(|rest| rest.is_empty() || rest.starts_with('/')) {
        Some(rest) => format!("\"$HOME\"{}", quote(rest)),
        None if path.starts_with('/') => quote(path),
        None => format!("\"$HOME\"/{}", quote(path)),
    }
}

/// The script a command runs as: the platform's host for the computer's
/// CLI, the job's env, its directory, then `bash -lc` with the CLI's
/// directory on its PATH (a login shell sets PATH anew).
pub fn job_script(e: &ComputerExec, host: &str) -> String {
    let mut s = format!("export FRAGMENT_HOST={}\n", quote(host));
    for (k, v) in &e.env {
        s.push_str(&format!("export {k}={}\n", quote(v)));
    }
    match &e.cwd {
        None => s.push_str("mkdir -p \"$HOME/fragment\" && cd \"$HOME/fragment\" || exit 1\n"),
        Some(cwd) => s.push_str(&format!("cd -- {} || exit 1\n", place(cwd))),
    }
    s.push_str(&format!("exec bash -lc {}\n", quote(&format!("export PATH=\"$HOME/.local/bin:$PATH\"\n{}", e.command))));
    s
}

/// The bytes between the markers of an answer, or None when it has none
/// (or they are not base64 once whatever is not base64 is left out).
pub fn answer(out: &str) -> Option<Vec<u8>> {
    let (_, rest) = out.split_once(BEGIN)?;
    let (b64, _) = rest.split_once(END)?;
    let clean: String = b64.chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '=')).collect();
    base64::Engine::decode(&base64::engine::general_purpose::STANDARD, clean).ok()
}

/// `POLL`'s answer.
pub fn state(out: &str) -> Option<ExecState> {
    serde_json::from_slice(&answer(out)?).ok()
}

/// A command's result, `{code, stdout, stderr, truncated}`, as text within
/// a step result's limit: a stream whose JSON would not fit is cut further.
pub fn result(code: i64, stdout: &[u8], stderr: &[u8], truncated: bool) -> Value {
    let (mut out, mut err) = (String::from_utf8_lossy(stdout).into_owned(), String::from_utf8_lossy(stderr).into_owned());
    let mut truncated = truncated;
    loop {
        let v = json!({ "code": code, "stdout": out, "stderr": err, "truncated": truncated });
        if v.to_string().len() <= RESULT_MAX_BYTES {
            return v;
        }
        let longer = if out.len() >= err.len() { &mut out } else { &mut err };
        let mut cut = longer.len() / 2;
        while !longer.is_char_boundary(cut) {
            cut -= 1;
        }
        longer.truncate(cut);
        truncated = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    fn exec(command: &str) -> ComputerExec {
        ComputerExec { command: command.into(), timeout_ms: None, cwd: None, env: Default::default() }
    }

    #[test]
    fn the_args_are_checked() {
        assert_eq!(check(&exec("echo hi")), Ok(()));
        assert!(check(&exec("  ")).unwrap_err().contains("empty"));
        assert!(check(&exec(&"x".repeat(COMMAND_MAX_BYTES + 1))).is_err());
        let mut e = exec("ls");
        e.timeout_ms = Some(TIMEOUT_MAX_MS + 1);
        assert!(check(&e).unwrap_err().contains("60 minutes"));
        e.timeout_ms = Some(TIMEOUT_MAX_MS);
        assert_eq!((check(&e), timeout_s(&e)), (Ok(()), 3600));
        e.env.insert("1X".into(), "v".into());
        assert!(check(&e).unwrap_err().contains("env name"));
        e.env.clear();
        e.env.insert("A_1".into(), "it's\n$(not run)".into());
        assert_eq!(check(&e), Ok(()));
        e.cwd = Some("a\0b".into());
        assert!(check(&e).is_err());
        assert_eq!(timeout_s(&exec("ls")), 600);
    }

    #[test]
    fn the_runner_keeps_what_the_platform_reads() {
        assert!(START.contains(&format!("-gt {OUTPUT_MAX_BYTES} ]; then head -c {OUTPUT_MAX_BYTES} ")));
    }

    #[test]
    fn an_answer_is_read_between_its_markers() {
        let said = format!("noise\n{BEGIN}\neyJzdGF0ZSI6\n\u{1}InJ1bm5pbmcifQ==\n{END}\nmore");
        assert_eq!(state(&said), Some(ExecState::Running), "a byte framing adds is left out");
        assert_eq!(answer("no markers"), None);
        assert_eq!(answer(&format!("{BEGIN}\n{END}")), Some(vec![]));
    }

    #[test]
    fn a_result_fits_a_step_result() {
        let r = result(3, b"out", b"err\xff", false);
        assert_eq!(r, json!({ "code": 3, "stdout": "out", "stderr": "err\u{fffd}", "truncated": false }));
        // control bytes escape to six: 256 KiB of them is 1.5 MiB of JSON
        let noisy = vec![1u8; OUTPUT_MAX_BYTES];
        let r = result(0, &noisy, &noisy, false);
        assert!(r.to_string().len() <= RESULT_MAX_BYTES && r["truncated"] == true, "{}", r.to_string().len());
    }

    /// A computer's home for one test, where the scripts run as on a Sprite.
    struct Home(PathBuf);

    impl Home {
        fn new(name: &str) -> Home {
            let dir = std::env::temp_dir().join(format!("fragment-exec-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Home(dir)
        }

        fn run(&self, script: &str, args: &[&str], stdin: &str) -> String {
            let mut child = Command::new("bash")
                .args(["-c", script, "fragment-exec"])
                .args(args)
                .env("HOME", &self.0)
                .current_dir(&self.0)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            std::io::Write::write_all(&mut child.stdin.take().unwrap(), stdin.as_bytes()).unwrap();
            let o = child.wait_with_output().unwrap();
            format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
        }

        fn start(&self, id: &str, e: &ComputerExec) -> Option<ExecState> {
            state(&self.run(START, &[id, &timeout_s(e).to_string()], &job_script(e, "https://fragment.test")))
        }

        /// Polls until it ended, as the platform does, then reads it.
        fn finish(&self, id: &str) -> Value {
            let t0 = Instant::now();
            loop {
                match state(&self.run(POLL, &[id, "5"], "")) {
                    Some(ExecState::Done { code, out, err, truncated }) => {
                        let read = |s: &str, n: usize| {
                            let mut bytes = vec![];
                            while bytes.len() < n {
                                let len = CHUNK_BYTES.min(n - bytes.len()).to_string();
                                bytes.extend(answer(&self.run(READ, &[id, s, &bytes.len().to_string(), &len], "")).unwrap());
                            }
                            bytes
                        };
                        return result(code, &read("stdout", out), &read("stderr", err), truncated);
                    }
                    Some(ExecState::Running) if t0.elapsed() < Duration::from_secs(30) => {}
                    other => panic!("{id}: {other:?}"),
                }
            }
        }

        fn path(&self, p: &str) -> PathBuf {
            self.0.join(p)
        }
    }

    impl Drop for Home {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(Path::new(&self.0));
        }
    }

    #[test]
    fn a_command_runs_once_on_the_computer() {
        let home = Home::new("once");
        let mut e = exec("n=$(cat count 2> /dev/null || echo 0); echo $((n + 1)) > count; cat count; pwd; echo \"$GREETING $FRAGMENT_HOST\"; echo oops >&2; exit 3");
        e.env.insert("GREETING".into(), "it's".into());
        assert_eq!(home.start("a1", &e), Some(ExecState::Started));
        let first = home.finish("a1");
        let dir = home.path("fragment");
        let want = format!("1\n{}\nit's https://fragment.test\n", dir.display());
        assert_eq!(first, json!({ "code": 3, "stdout": want, "stderr": "oops\n", "truncated": false }));
        // a retried or replayed step: the same id reattaches, and it does not run again
        assert_eq!(home.start("a1", &e), Some(ExecState::Started));
        assert_eq!(home.finish("a1"), first);
        assert_eq!(std::fs::read_to_string(dir.join("count")).unwrap(), "1\n");
        // a new step runs it anew
        home.start("a2", &e);
        assert_eq!(home.finish("a2")["stdout"].as_str().unwrap().lines().next(), Some("2"));
        assert_eq!(state(&home.run(POLL, &["nope", "1"], "")), Some(ExecState::Lost));
    }

    #[test]
    fn its_place_its_output_and_its_timeout() {
        let home = Home::new("limits");
        let mut e = exec("pwd; head -c 300000 /dev/zero | tr '\\0' x; head -c 10 /dev/zero | tr '\\0' y >&2");
        e.cwd = Some("~/work dir".into());
        std::fs::create_dir_all(home.path("work dir")).unwrap();
        home.start("b1", &e);
        let r = home.finish("b1");
        let out = r["stdout"].as_str().unwrap();
        assert!(out.starts_with(&format!("{}\n", home.path("work dir").display())), "{}", &out[..80]);
        assert_eq!((out.len(), r["stderr"].as_str(), r["truncated"].as_bool()), (OUTPUT_MAX_BYTES, Some("yyyyyyyyyy"), Some(true)));
        e.cwd = Some("missing".into());
        home.start("b2", &e);
        let r = home.finish("b2");
        assert!(r["code"] == 1 && r["stderr"].as_str().unwrap().contains("missing"), "{r}");
        let mut slow = exec("echo before; sleep 30; echo after");
        slow.timeout_ms = Some(1000);
        home.start("b3", &slow);
        assert_eq!(home.finish("b3"), json!({ "code": 124, "stdout": "before\n", "stderr": "", "truncated": false }));
    }

    #[test]
    fn a_command_whose_runner_died_was_interrupted() {
        let home = Home::new("died");
        home.start("c1", &exec("sleep 5"));
        let pid = || std::fs::read_to_string(home.path(".fragment/exec/c1/pid")).map(|p| p.trim().to_string()).unwrap_or_default();
        let t0 = Instant::now();
        while pid().is_empty() && t0.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(50));
        }
        // its computer stopped: the runner is gone before the command ends
        Command::new("kill").args(["-KILL", &pid()]).status().unwrap();
        assert_eq!(state(&home.run(POLL, &["c1", "1"], "")), Some(ExecState::Interrupted));
    }
}
