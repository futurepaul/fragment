//! The SimpleX side of the spike: a local SMP server and simplex-chat CLI
//! clients in Docker, each client driven through its WebSocket bot API
//! (`simplex-chat -p`). Std, serde_json, anyhow and tungstenite only, so
//! the e2e lane (`spikes/simplex/lane.rs`) can include this file as is.
//!
//! The bot API: the client sends `{"corrId": "<n>", "cmd": "<chat command>"}`
//! and the core answers `{"corrId": "<n>", "resp": {...}}`; anything it
//! says on its own (a message arrived, a contact connected) comes as
//! `{"resp": {...}}` with no `corrId`.

#![allow(dead_code)]

use std::collections::VecDeque;
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};

/// The SMP server image, pinned: v7.0.1, the multi-arch index's digest
/// (Docker Hub, 2026-10-05).
pub const SMP_IMAGE: &str =
    "simplexchat/smp-server:v7.0.1@sha256:7d825822839a9d5ee9e9a99563d77a319816c7a8e0b4b7bf4dcebe448897af54";
/// The client image `docker/chat.Dockerfile` builds (simplex-chat v7.0.3,
/// by checksum).
pub const CHAT_IMAGE: &str = "simplex-spike-chat:7.0.3";
/// The SMP server's name on the lab's network: a domain, as its image's
/// entrypoint wants one with a dot.
pub const SMP_HOST: &str = "smp.spike.test";

fn docker(args: &[&str]) -> Result<String> {
    let out = Command::new("docker").args(args).output().context("running docker")?;
    if !out.status.success() {
        bail!("docker {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// A lab: one Docker network, one SMP server, and clients, all named
/// `<prefix>-…`, with their state in `dir` on the host.
pub struct Lab {
    pub prefix: String,
    pub dir: PathBuf,
    /// `smp://<fingerprint>@smp.spike.test`, once the server is up.
    pub server: String,
}

/// One simplex-chat client in the lab.
#[derive(Clone)]
pub struct ClientSpec {
    /// Its container is `<prefix>-<name>`, its database `<dir>/<name>/db_*`.
    pub name: String,
    /// The host port its bot API is published on (127.0.0.1).
    pub port: u16,
    /// `--create-bot-display-name` (a bot) or `--user-display-name`.
    pub bot: bool,
}

impl Lab {
    /// Builds the client image (cached after the first build), then starts
    /// the network and the SMP server, and waits for its fingerprint.
    pub fn up(prefix: &str, dir: &Path, docker_dir: &Path) -> Result<Lab> {
        let dockerfile = docker_dir.join("chat.Dockerfile");
        docker(&["build", "-q", "-f", &dockerfile.to_string_lossy(), "-t", CHAT_IMAGE, &docker_dir.to_string_lossy()])?;
        std::fs::create_dir_all(dir.join("smp/conf"))?;
        std::fs::create_dir_all(dir.join("smp/logs"))?;
        let net = format!("{prefix}-net");
        let _ = docker(&["network", "create", &net]);
        let smp = format!("{prefix}-smp");
        let _ = docker(&["rm", "-f", &smp]);
        let conf = format!("{}:/etc/opt/simplex", dir.join("smp/conf").display());
        let logs = format!("{}:/var/opt/simplex", dir.join("smp/logs").display());
        docker(&[
            "run", "-d", "--name", &smp, "--network", &net, "--network-alias", SMP_HOST,
            "-e", &format!("ADDR={SMP_HOST}"), "-e", "WEB_MANUAL=1", "-v", &conf, "-v", &logs, SMP_IMAGE,
        ])?;
        let t0 = Instant::now();
        let fp_file = dir.join("smp/conf/fingerprint");
        let fingerprint = loop {
            if let Ok(fp) = std::fs::read_to_string(&fp_file) {
                if !fp.trim().is_empty() {
                    break fp.trim().to_string();
                }
            }
            if t0.elapsed() > Duration::from_secs(60) {
                bail!("the SMP server wrote no fingerprint in 60 s: {}", docker(&["logs", &smp]).unwrap_or_default());
            }
            std::thread::sleep(Duration::from_millis(200));
        };
        // listening once its log says so
        let t0 = Instant::now();
        while !docker_logs(&smp).contains("Serving SMP protocol") {
            if t0.elapsed() > Duration::from_secs(60) {
                bail!("the SMP server is not serving after 60 s");
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        Ok(Lab { prefix: prefix.into(), dir: dir.into(), server: format!("smp://{fingerprint}@{SMP_HOST}") })
    }

    pub fn container(&self, name: &str) -> String {
        format!("{}-{name}", self.prefix)
    }

    /// The client's database directory on the host.
    pub fn db_dir(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }

    /// Starts a client (its database kept across starts), and connects to
    /// its bot API.
    pub fn start(&self, c: &ClientSpec) -> Result<Api> {
        let data = self.db_dir(&c.name);
        std::fs::create_dir_all(&data)?;
        let ctr = self.container(&c.name);
        let _ = docker(&["rm", "-f", &ctr]);
        let vol = format!("{}:/data", data.display());
        let publish = format!("127.0.0.1:{}:5226", c.port);
        let net = format!("{}-net", self.prefix);
        let who = if c.bot { "--create-bot-display-name" } else { "--user-display-name" };
        docker(&[
            "run", "-d", "--name", &ctr, "--network", &net, "-e", "API_PUBLIC_PORT=5226", "-p", &publish, "-v", &vol,
            CHAT_IMAGE, "-d", "/data/db", "-s", &self.server, "-p", "5225", who, &c.name, "-y",
        ])?;
        Api::connect(&format!("ws://127.0.0.1:{}", c.port), Duration::from_secs(60))
            .with_context(|| format!("{ctr}'s bot API: {}", docker_logs(&ctr)))
    }

    /// Stops a client: SIGTERM, then its container is removed (its
    /// database stays on the host).
    pub fn stop(&self, name: &str) -> Result<()> {
        let ctr = self.container(name);
        docker(&["stop", "-t", "10", &ctr])?;
        docker(&["rm", "-f", &ctr])?;
        Ok(())
    }

    /// Kills a client at once (SIGKILL: no shutdown of its own), and removes
    /// its container (its database stays on the host, as the kill left it).
    pub fn kill(&self, name: &str) -> Result<()> {
        let ctr = self.container(name);
        docker(&["kill", "-s", "KILL", &ctr])?;
        docker(&["rm", "-f", &ctr])?;
        Ok(())
    }

    pub fn down(&self, names: &[&str]) {
        for n in names {
            let _ = docker(&["rm", "-f", &self.container(n)]);
        }
        let _ = docker(&["rm", "-f", &self.container("smp")]);
        let _ = docker(&["network", "rm", &format!("{}-net", self.prefix)]);
    }
}

pub fn docker_logs(container: &str) -> String {
    Command::new("docker")
        .args(["logs", container])
        .output()
        .map(|o| format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)))
        .unwrap_or_default()
}

/// A client's bot API.
pub struct Api {
    ws: WebSocket<MaybeTlsStream<TcpStream>>,
    next: u64,
    /// What the core said on its own, in order, with when it was read.
    pub events: VecDeque<(Instant, Value)>,
}

impl Api {
    /// Connects, retrying until `within` passes (the core opens its
    /// databases and runs migrations before it listens).
    pub fn connect(url: &str, within: Duration) -> Result<Api> {
        let t0 = Instant::now();
        loop {
            match tungstenite::connect(url) {
                Ok((ws, _)) => {
                    if let MaybeTlsStream::Plain(s) = ws.get_ref() {
                        s.set_nodelay(true)?;
                    }
                    return Ok(Api { ws, next: 1, events: VecDeque::new() });
                }
                Err(e) if t0.elapsed() > within => bail!("connecting to {url}: {e}"),
                Err(_) => std::thread::sleep(Duration::from_millis(250)),
            }
        }
    }

    fn set_timeout(&mut self, t: Option<Duration>) -> Result<()> {
        if let MaybeTlsStream::Plain(s) = self.ws.get_ref() {
            s.set_read_timeout(t)?;
        }
        Ok(())
    }

    /// One frame, or `None` when `timeout` passes first.
    fn read(&mut self, timeout: Duration) -> Result<Option<Value>> {
        self.set_timeout(Some(timeout.max(Duration::from_millis(1))))?;
        loop {
            match self.ws.read() {
                Ok(Message::Text(t)) => return Ok(Some(serde_json::from_str(t.as_str()).context("a frame that is not JSON")?)),
                Ok(Message::Ping(_) | Message::Pong(_) | Message::Frame(_)) => continue,
                Ok(Message::Binary(_)) => continue,
                Ok(Message::Close(c)) => bail!("the bot API closed: {c:?}"),
                Err(tungstenite::Error::Io(e)) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {
                    return Ok(None)
                }
                Err(e) => return Err(anyhow!("reading the bot API: {e}")),
            }
        }
    }

    /// Sends a chat command and returns the core's answer to it (its
    /// `resp`); events read meanwhile are kept in `events`.
    pub fn cmd(&mut self, cmd: &str) -> Result<Value> {
        let id = self.next.to_string();
        self.next += 1;
        self.ws.send(Message::text(json!({ "corrId": id, "cmd": cmd }).to_string()))?;
        let t0 = Instant::now();
        loop {
            let left = Duration::from_secs(60).checked_sub(t0.elapsed()).ok_or_else(|| anyhow!("no answer to `{cmd}` in 60 s"))?;
            let Some(v) = self.read(left)? else { continue };
            if v["corrId"].as_str() == Some(id.as_str()) {
                return Ok(v["resp"].clone());
            }
            self.events.push_back((Instant::now(), v["resp"].clone()));
        }
    }

    /// The next event `pred` accepts, within `timeout` (earlier events it
    /// does not accept stay queued, in order).
    pub fn wait(&mut self, timeout: Duration, pred: impl Fn(&Value) -> bool) -> Result<(Instant, Value)> {
        if let Some(i) = self.events.iter().position(|(_, e)| pred(e)) {
            return Ok(self.events.remove(i).expect("the position is in range"));
        }
        let t0 = Instant::now();
        loop {
            let Some(left) = timeout.checked_sub(t0.elapsed()) else {
                bail!("no such event in {timeout:?}; queued: {}", self.event_types().join(", "))
            };
            if let Some(v) = self.read(left)? {
                let at = Instant::now();
                let resp = v["resp"].clone();
                if pred(&resp) {
                    return Ok((at, resp));
                }
                self.events.push_back((at, resp));
            }
        }
    }

    /// Reads whatever arrives for `d`, keeping it in `events`.
    pub fn drain(&mut self, d: Duration) -> Result<()> {
        let t0 = Instant::now();
        while let Some(left) = d.checked_sub(t0.elapsed()) {
            if let Some(v) = self.read(left)? {
                self.events.push_back((Instant::now(), v["resp"].clone()));
            }
        }
        Ok(())
    }

    pub fn event_types(&self) -> Vec<String> {
        self.events.iter().map(|(_, e)| type_of(e).to_string()).collect()
    }
}

/// `docker stats` for one container, once: its memory and CPU now.
pub fn docker_stats(container: &str) -> String {
    docker(&["stats", "--no-stream", "--format", "mem {{.MemUsage}} cpu {{.CPUPerc}}", container]).unwrap_or_else(|e| e.to_string())
}

/// A response's or event's `type` (the bot API's tag), wherever this
/// version puts it.
pub fn type_of(v: &Value) -> &str {
    v["type"].as_str().or_else(|| v["Right"]["type"].as_str()).or_else(|| v["Left"]["type"].as_str()).unwrap_or("?")
}
