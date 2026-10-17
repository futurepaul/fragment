//! A fenced agent's user and sandbox (docs/cloudflare-v1.md, decision 63,
//! proposed; docs/computers.md, "A fenced agent in our Hermes image").
//!
//! A fenced agent stays in its profile under the one gateway; what walls it
//! off is Hermes' own per-profile terminal and the kernel's users and
//! namespaces. Its profile's terminal is Hermes' `ssh` backend (hermes.rs,
//! `profile_config`), which carries its file tools and `execute_code` too,
//! to the computer's loopback sshd as a Linux user of its own; sshd's
//! `ForceCommand` (`hermes-boot fence-shell`) joins each session to the
//! agent's sandbox: a user, network and mount namespace held open by a
//! process of its user's, with its own loopback and no other interface, its
//! `tmp` on `/tmp` and a `/dev/shm` of its own. Its work directory is its
//! user's and the `hermes` group's; every other agent's, and Hermes' home,
//! are closed to it, and `/proc` hides other users' processes from it.
//!
//! The plan is pure (`number`, `sshd_config`, `sandbox_argv`,
//! `session_argv`); `Fences` carries it out, as root, and keeps it while the
//! computer runs (a sandbox or sshd that ends is started again).

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use fragment_bridge::ev;
use fragment_bridge::runtime::Agent;

/// Each fenced agent's user number, by its agent fragment: the computer's
/// (root's), under `/data`, so a start after a save gives the agent the
/// uid its work belongs to.
pub const USERS_FILE: &str = "/data/fragment-fence/users.json";
/// The first fence user's uid (the `hermes` user is 10000).
pub const UID_BASE: u32 = 20_000;
/// User numbers, at most: a computer runs at most 16 agents, and a number
/// is kept for its agent while the file keeps it.
pub const USERS_MAX: u32 = 256;
/// The computer's sshd, on loopback alone.
pub const SSH_PORT: u16 = 2222;
/// sshd's host key, the computer's (root's), under `/data`: Hermes' `ssh`
/// backend remembers the first key it meets (`StrictHostKeyChecking
/// accept-new`, in the `hermes` user's home, which a snapshot may keep), so
/// it never changes under it.
pub const HOST_KEY: &str = "/data/fragment-fence/ssh_host_ed25519";
/// This boot's sshd files (its host key, the gateway's key, each fence
/// user's authorized key, its config): under the boot's runtime files,
/// emptied at every start.
pub const SSH_DIR: &str = "/var/lib/fragment-run/ssh";
/// The key the gateway's user (Hermes' `ssh` backend) signs in with.
pub const CLIENT_KEY: &str = "/var/lib/fragment-run/ssh/client";
/// Each sandbox's holder, by its user's uid (`<uid>.pid`), which the
/// session joins.
pub const SANDBOX_DIR: &str = "/var/lib/fragment-run/fence";
/// What sshd runs for every session of a fence user.
pub const SHELL_COMMAND: &str = "/opt/fragment/bin/hermes-boot fence-shell";
/// The tools off for a fenced agent (Hermes' `agent.disabled_toolsets`):
/// each reads files or reaches services in the gateway's own process, as
/// the `hermes` user, past the sandbox. `cronjob` is off for every agent
/// (decision 38).
pub const DISABLED_TOOLSETS: [&str; 10] = ["cronjob", "browser", "vision", "video", "computer_use", "desktop_ui", "project", "kanban", "connections", "catalog"];

/// A fence user's name.
pub fn user_name(number: u32) -> String {
    format!("fence-{number}")
}

/// A fence user's uid (and its own group's gid).
pub fn uid(number: u32) -> u32 {
    UID_BASE + number
}

/// The user number of `agent`: the one it has, else the least one free;
/// `None` when every number is taken. Pure: the caller keeps the map.
pub fn number(users: &BTreeMap<String, u32>, agent: &str) -> Option<u32> {
    if let Some(n) = users.get(agent) {
        return Some(*n);
    }
    (1..=USERS_MAX).find(|n| !users.values().any(|taken| taken == n))
}

/// What a fenced agent's profile is told of its terminal (hermes.rs,
/// `profile_config`): the user its commands run as, the key the gateway
/// signs in with, and sshd's port.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fence {
    pub user: String,
    /// Its uid, whose its work is.
    pub uid: u32,
    pub key: PathBuf,
    pub port: u16,
}

/// sshd's config: loopback alone; fence users alone, by the key under
/// `authorized/<user>`; no password, no root, no forwarding of any kind;
/// every session through `SHELL_COMMAND`, an sftp request too (the
/// command then is the server's, which it runs in the sandbox).
pub fn sshd_config(port: u16, dir: &Path, host_key: &Path) -> String {
    let (d, k) = (dir.display(), host_key.display());
    format!(
        "# Written by hermes-boot at every boot: the fenced agents' way in (fence.rs).\n\
         ListenAddress 127.0.0.1:{port}\n\
         HostKey {k}\n\
         PidFile {d}/sshd.pid\n\
         AuthorizedKeysFile {d}/authorized/%u\n\
         PasswordAuthentication no\n\
         KbdInteractiveAuthentication no\n\
         PermitRootLogin no\n\
         AllowUsers fence-*\n\
         DisableForwarding yes\n\
         PermitTunnel no\n\
         PermitUserEnvironment no\n\
         UsePAM no\n\
         PrintMotd no\n\
         Subsystem sftp /usr/lib/openssh/sftp-server\n\
         ForceCommand {SHELL_COMMAND}\n"
    )
}

/// The holder of a fenced agent's sandbox, as its user (`uid`): a user
/// namespace mapping it as root inside (so it may set up the rest), a
/// network namespace whose loopback it brings up (no other interface), and
/// a mount namespace with its `tmp` on `/tmp` and a `/dev/shm` of its own;
/// then it sleeps, holding them open for its sessions.
pub fn sandbox_argv(uid: u32, tmp: &Path) -> Vec<String> {
    let script = format!("ip link set lo up && mount --bind {} /tmp && mount -t tmpfs -o mode=1777,size=64m tmpfs /dev/shm && exec sleep infinity", shell_quote(&tmp.display().to_string()));
    ["setpriv", &format!("--reuid={uid}"), &format!("--regid={uid}"), "--clear-groups", "--", "unshare", "-r", "-n", "-m", "--propagation", "private", "--", "sh", "-c", &script].iter().map(|s| s.to_string()).collect()
}

/// A session's command, as its user: joined to the sandbox `holder` holds
/// (its user, network and mount namespaces; its credentials kept, so it is
/// its user outside, root only inside), group-writable files by default
/// (the gateway's group shares its work), running what the client asked
/// (`original`) or a login shell.
pub fn session_argv(holder: u32, original: Option<&str>) -> Vec<String> {
    let run = match original {
        Some(cmd) => format!("umask 007; {cmd}"),
        None => "umask 007; exec bash -l".to_string(),
    };
    ["nsenter", "--preserve-credentials", "-t", &holder.to_string(), "-U", "-n", "-m", "--", "/bin/bash", "-c", &run].iter().map(|s| s.to_string()).collect()
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// `hermes-boot fence-shell`: sshd's `ForceCommand` for a fence user. The
/// sandbox is its own uid's (never a name it is told), and its holder must
/// be a process of that uid; with none, the session is refused.
pub fn shell() -> ! {
    // SAFETY: getuid never fails.
    let me = unsafe { libc::getuid() };
    let refuse = |why: &str| -> ! {
        eprintln!("fence-shell: {why}");
        std::process::exit(1)
    };
    let holder: u32 = std::fs::read_to_string(format!("{SANDBOX_DIR}/{me}.pid")).ok().and_then(|t| t.trim().parse().ok()).unwrap_or_else(|| refuse("this user has no sandbox"));
    let owner = std::fs::metadata(format!("/proc/{holder}")).map(|m| std::os::unix::fs::MetadataExt::uid(&m)).unwrap_or_else(|_| refuse("its sandbox has ended"));
    if owner != me {
        refuse("its sandbox is not this user's");
    }
    let original = std::env::var("SSH_ORIGINAL_COMMAND").ok().filter(|c| !c.is_empty());
    let argv = session_argv(holder, original.as_deref());
    let err = Command::new(&argv[0]).args(&argv[1..]).exec();
    refuse(&format!("nsenter: {err}"))
}

/// The fenced agents of this computer, carried out and kept as root: their
/// users, their work's owners, their sandboxes, and the sshd they come in
/// by. One a boot.
pub struct Fences {
    /// The `hermes` user's uid and gid.
    hermes: (u32, u32),
    users: BTreeMap<String, u32>,
    /// Each fenced agent's sandbox holder, by its agent fragment.
    holders: BTreeMap<String, Child>,
    sshd: Option<Child>,
    /// sshd's keys and config written, `/proc` hidden, and the shared
    /// directories closed: done once a boot, at its first fenced agent.
    ready: bool,
}

impl Fences {
    pub fn new(hermes: (u32, u32)) -> Fences {
        let users = std::fs::read_to_string(USERS_FILE).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        Fences { hermes, users, holders: BTreeMap::new(), sshd: None, ready: false }
    }

    /// What `agent`'s profile is told of its terminal, when it is fenced
    /// and its user is made.
    pub fn of(&self, agent: &Agent) -> Option<Fence> {
        let n = self.users.get(&agent.fragment).filter(|_| agent.fenced)?;
        Some(Fence { user: user_name(*n), uid: uid(*n), key: PathBuf::from(CLIENT_KEY), port: SSH_PORT })
    }

    /// Fences each fenced agent of `agents` (its user, its work given to it,
    /// its sandbox) and lets each other go (its work the `hermes` user's
    /// again, its sandbox ended): at a boot, and as their fences change.
    /// Before a fenced agent's profile is written, so it names its user.
    pub fn apply(&mut self, agents: &[Agent]) {
        if agents.iter().any(|a| a.fenced) && !self.ready {
            self.ready = self.prepare();
        }
        for a in agents {
            if a.fenced && self.ready {
                self.fence(a);
            } else if !a.fenced {
                self.let_go(a);
            }
        }
    }

    /// An agent gone from this computer: its sandbox ends; its user and its
    /// number stay (its work may come back with it).
    pub fn removed(&mut self, agent: &Agent) {
        self.end_sandbox(&agent.fragment);
    }

    /// Each tick: a sandbox or the sshd that ended is started again.
    pub fn keep(&mut self, agents: &[Agent]) {
        if !self.ready {
            return;
        }
        let ended: Vec<String> = self.holders.iter_mut().filter_map(|(f, c)| matches!(c.try_wait(), Ok(Some(_))).then(|| f.clone())).collect();
        for f in ended {
            self.holders.remove(&f);
            ev!("fence.sandbox_ended", { "agent": f });
            if let Some(a) = agents.iter().find(|a| a.fragment == f && a.fenced) {
                self.fence(a);
            }
        }
        if self.sshd.as_mut().is_none_or(|c| !matches!(c.try_wait(), Ok(None))) && agents.iter().any(|a| a.fenced) {
            self.sshd = spawn_sshd();
        }
    }

    /// sshd's keys and config, `/proc` hiding other users' processes from a
    /// fence user (root's group sees them all: the boot watches the gateway
    /// and each sandbox, and a container's root has no `CAP_SYS_PTRACE` to
    /// see past `hidepid`), and the directories every agent shares closed to
    /// fence users. Whether it is done.
    fn prepare(&mut self) -> bool {
        let dir = Path::new(SSH_DIR);
        let made = (|| -> std::io::Result<()> {
            std::fs::create_dir_all(dir.join("authorized"))?;
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755))?;
            std::fs::create_dir_all(SANDBOX_DIR)?;
            std::fs::set_permissions(SANDBOX_DIR, std::fs::Permissions::from_mode(0o755))?;
            std::fs::create_dir_all("/run/sshd")?;
            let host_key = Path::new(HOST_KEY);
            std::fs::create_dir_all(host_key.parent().expect("the host key has a directory"))?;
            std::fs::set_permissions(host_key.parent().expect("the host key has a directory"), std::fs::Permissions::from_mode(0o700))?;
            keygen(host_key)?;
            keygen(Path::new(CLIENT_KEY))?;
            crate::chown(Path::new(CLIENT_KEY), Some(self.hermes));
            std::fs::write(dir.join("sshd_config"), sshd_config(SSH_PORT, dir, host_key))?;
            Ok(())
        })();
        if let Err(e) = made {
            ev!("fence.unprepared", { "error": e.to_string() });
            return false;
        }
        let hidden = Command::new("mount").args(["-o", "remount,hidepid=invisible,gid=0", "/proc"]).status();
        // the directories every agent shares: no fence user lists /data/work
        // or enters Hermes' home (every profile's `.env`, every desktop's
        // sockets); each other agent's work is its own (`close_others`).
        // A fresh computer has no /data/work yet: made here, as each agent's
        // work would make it (root's), so it is never made open after
        let _ = std::fs::create_dir_all(crate::hermes::WORK);
        for (path, mode) in [(crate::hermes::WORK, 0o711), ("/data/hermes", 0o700)] {
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
        }
        self.sshd = spawn_sshd();
        ev!("fence.prepared", { "hidepid": hidden.map(|s| s.success()).unwrap_or(false), "sshd": self.sshd.is_some() });
        self.sshd.is_some()
    }

    fn fence(&mut self, a: &Agent) {
        let Some(n) = number(&self.users, &a.fragment) else {
            ev!("fence.no_user", { "agent": a.fragment, "why": "every user number is taken" });
            return;
        };
        if self.users.insert(a.fragment.clone(), n) != Some(n) {
            self.save_users();
        }
        let (name, id) = (user_name(n), uid(n));
        if let Err(e) = make_user(&name, id, &crate::hermes::home_dir(&a.fragment)) {
            ev!("fence.no_user", { "agent": a.fragment, "error": e });
            return;
        }
        give_work(&a.fragment, (id, self.hermes.1));
        let pub_key = std::fs::read_to_string(format!("{CLIENT_KEY}.pub")).unwrap_or_default();
        let _ = std::fs::write(Path::new(SSH_DIR).join("authorized").join(&name), pub_key);
        if !self.holders.contains_key(&a.fragment) {
            match spawn_sandbox(id, &crate::hermes::tmp_dir(&a.fragment)) {
                Ok(child) => {
                    let _ = std::fs::write(format!("{SANDBOX_DIR}/{id}.pid"), child.id().to_string());
                    ev!("fence.sandbox", { "agent": a.fragment, "user": name, "holder": child.id() });
                    self.holders.insert(a.fragment.clone(), child);
                }
                Err(e) => ev!("fence.sandbox_failed", { "agent": a.fragment, "user": name, "error": e.to_string() }),
            }
        }
    }

    fn let_go(&mut self, a: &Agent) {
        let Some(n) = self.users.get(&a.fragment).copied() else { return };
        self.end_sandbox(&a.fragment);
        let _ = std::fs::remove_file(Path::new(SSH_DIR).join("authorized").join(user_name(n)));
        let work = crate::hermes::work_dir(&a.fragment);
        if owner_of(&work) == Some(uid(n)) {
            let _ = Command::new("chown").args(["-R", &format!("{}:{}", self.hermes.0, self.hermes.1)]).arg(&work).status();
            let _ = std::fs::set_permissions(&work, std::fs::Permissions::from_mode(0o700));
            ev!("fence.let_go", { "agent": a.fragment, "user": user_name(n) });
        }
    }

    fn end_sandbox(&mut self, agent: &str) {
        if let Some(mut child) = self.holders.remove(agent) {
            let _ = child.kill();
            let _ = child.wait();
            if let Some(n) = self.users.get(agent) {
                let _ = std::fs::remove_file(format!("{SANDBOX_DIR}/{}.pid", uid(*n)));
            }
        }
    }

    fn save_users(&self) {
        let _ = std::fs::create_dir_all(Path::new(USERS_FILE).parent().expect("the users file has a directory"));
        let _ = std::fs::write(USERS_FILE, serde_json::to_string(&self.users).expect("a map of numbers serializes"));
        let _ = std::fs::set_permissions(USERS_FILE, std::fs::Permissions::from_mode(0o600));
    }
}

/// Closes each unfenced agent's work to fence users (0700, the `hermes`
/// user's), when any agent is fenced: its files are its own, and its
/// parent (0711) is listed by no one.
pub fn close_others(agents: &[Agent]) {
    if !agents.iter().any(|a| a.fenced) {
        return;
    }
    for a in agents.iter().filter(|a| !a.fenced) {
        let _ = std::fs::set_permissions(crate::hermes::work_dir(&a.fragment), std::fs::Permissions::from_mode(0o700));
    }
}

fn keygen(path: &Path) -> std::io::Result<()> {
    if path.exists() {
        return Ok(());
    }
    let out = Command::new("ssh-keygen").args(["-q", "-t", "ed25519", "-N", "", "-f"]).arg(path).stdin(Stdio::null()).output()?;
    if !out.status.success() {
        return Err(std::io::Error::other(String::from_utf8_lossy(&out.stderr).to_string()));
    }
    Ok(())
}

fn spawn_sshd() -> Option<Child> {
    let config = format!("{SSH_DIR}/sshd_config");
    match Command::new("/usr/sbin/sshd").args(["-D", "-e", "-f", &config]).stdin(Stdio::null()).spawn() {
        Ok(c) => Some(c),
        Err(e) => {
            ev!("fence.sshd_failed", { "error": e.to_string() });
            None
        }
    }
}

fn spawn_sandbox(uid: u32, tmp: &Path) -> std::io::Result<Child> {
    std::fs::create_dir_all(tmp)?;
    let argv = sandbox_argv(uid, tmp);
    Command::new(&argv[0]).args(&argv[1..]).stdin(Stdio::null()).stdout(Stdio::null()).spawn()
}

/// The fence user `name` (`uid`, and a group of its own by that id), made
/// if missing: no password (`*`, so sshd takes its key), no sudo, never in
/// the `hermes` group.
fn make_user(name: &str, uid: u32, home: &Path) -> Result<(), String> {
    let exists = Command::new("id").arg("-u").arg(name).stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success());
    if exists {
        return Ok(());
    }
    let id = uid.to_string();
    let group = Command::new("groupadd").args(["-g", &id, name]).output().map_err(|e| e.to_string())?;
    if !group.status.success() && !String::from_utf8_lossy(&group.stderr).contains("already exists") {
        return Err(String::from_utf8_lossy(&group.stderr).to_string());
    }
    let user = Command::new("useradd").args(["-u", &id, "-g", &id, "-M", "-s", "/bin/bash", "-p", "*", "-d"]).arg(home).arg(name).output().map_err(|e| e.to_string())?;
    if !user.status.success() {
        return Err(String::from_utf8_lossy(&user.stderr).to_string());
    }
    Ok(())
}

/// A fenced agent's work given to its user, the `hermes` group sharing it
/// (2770 on its directories, group-writable files): its directories made
/// first (a new agent's, before its profile is written), wholly given once,
/// as it is fenced (its owner is its user already after), and its own
/// directories' modes said again each time.
fn give_work(agent: &str, (uid, gid): (u32, u32)) {
    let work = crate::hermes::work_dir(agent);
    let dirs = [work.clone(), crate::hermes::home_dir(agent), crate::hermes::tmp_dir(agent), work.join("browser-profile")];
    for d in &dirs {
        let _ = std::fs::create_dir_all(d);
    }
    if owner_of(&work) != Some(uid) {
        let owner = format!("{uid}:{gid}");
        let _ = Command::new("chown").args(["-R", &owner]).arg(&work).status();
        let _ = Command::new("chmod").args(["-R", "u+rwX,g+rwX,o-rwx"]).arg(&work).status();
        let _ = Command::new("find").arg(&work).args(["-type", "d", "-exec", "chmod", "g+s", "{}", "+"]).status();
        ev!("fence.work_given", { "agent": agent, "uid": uid });
    }
    for d in &dirs {
        crate::chown(d, Some((uid, gid)));
        let _ = std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o2770));
    }
}

fn owner_of(path: &Path) -> Option<u32> {
    std::fs::metadata(path).ok().map(|m| std::os::unix::fs::MetadataExt::uid(&m))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Valid and replay: an agent keeps its number; a new one takes the
    /// least free; none past the last.
    #[test]
    fn an_agent_keeps_its_user_number() {
        let mut users = BTreeMap::new();
        assert_eq!(number(&users, "polish--k3x9"), Some(1));
        users.insert("polish--k3x9".to_string(), 1);
        assert_eq!(number(&users, "polish--k3x9"), Some(1), "asked again, its own");
        assert_eq!(number(&users, "research--k3x9"), Some(2));
        users.insert("research--k3x9".to_string(), 3);
        assert_eq!(number(&users, "design--k3x9"), Some(2), "the least free, not the next");
        let full: BTreeMap<String, u32> = (1..=USERS_MAX).map(|n| (format!("a{n}--k3x9"), n)).collect();
        assert_eq!(number(&full, "one-more--k3x9"), None);
        assert_eq!((user_name(1), uid(1)), ("fence-1".to_string(), 20_001));
    }

    /// sshd lets in fence users alone, on loopback, by key, forwarding
    /// nothing, every session through the fence shell.
    #[test]
    fn sshd_lets_in_fence_users_alone() {
        let c = sshd_config(2222, Path::new("/var/lib/fragment-run/ssh"), Path::new(HOST_KEY));
        for line in [
            "ListenAddress 127.0.0.1:2222\n",
            "HostKey /data/fragment-fence/ssh_host_ed25519\n",
            "AuthorizedKeysFile /var/lib/fragment-run/ssh/authorized/%u\n",
            "PasswordAuthentication no\n",
            "PermitRootLogin no\n",
            "AllowUsers fence-*\n",
            "DisableForwarding yes\n",
            "PermitTunnel no\n",
            "ForceCommand /opt/fragment/bin/hermes-boot fence-shell\n",
        ] {
            assert!(c.contains(line), "{line}");
        }
        assert!(!c.contains("0.0.0.0") && !c.contains("PermitEmptyPasswords yes"));
    }

    /// The sandbox: as its user, mapped root inside; its own network (its
    /// loopback up) and mounts (its tmp on /tmp); a session joins all three
    /// keeping its credentials, files group-writable.
    #[test]
    fn a_session_joins_its_users_sandbox() {
        let s = sandbox_argv(20_001, Path::new("/data/work/polish--k3x9/tmp"));
        assert_eq!(&s[..5], ["setpriv", "--reuid=20001", "--regid=20001", "--clear-groups", "--"]);
        assert!(s.windows(4).any(|w| w == ["unshare", "-r", "-n", "-m"]));
        let script = s.last().unwrap();
        assert!(script.starts_with("ip link set lo up && mount --bind '/data/work/polish--k3x9/tmp' /tmp") && script.ends_with("exec sleep infinity"), "{script}");
        let joined = session_argv(42, Some("ls -la"));
        assert_eq!(&joined[..8], ["nsenter", "--preserve-credentials", "-t", "42", "-U", "-n", "-m", "--"]);
        assert_eq!(joined.last().unwrap(), "umask 007; ls -la");
        assert_eq!(session_argv(42, None).last().unwrap(), "umask 007; exec bash -l");
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
    }

    /// What a fenced agent loses: every tool that reads in the gateway's
    /// own process, and the platform's cron, as every agent does.
    #[test]
    fn a_fenced_agent_loses_the_tools_that_reach_past_its_sandbox() {
        for t in ["cronjob", "browser", "vision", "computer_use", "desktop_ui"] {
            assert!(DISABLED_TOOLSETS.contains(&t), "{t}");
        }
        for kept in ["terminal", "file", "code_execution", "web", "memory", "skills"] {
            assert!(!DISABLED_TOOLSETS.contains(&kept), "{kept}");
        }
    }
}
