//! Fly Sprites, the part a `Computer` cell reaches through `KEYS`: make,
//! read, and delete a Sprite, and run a command on it by HTTP POST
//! (docs.fly.io/sprites/api). A Sprite here is a directory, and `exec`
//! runs its command as a local process with the Sprite's own home as
//! `HOME`, so a computer's first boot (the release's one-line install,
//! then `fragment login --pair`) really runs. The Sprite runtime's Tasks
//! API (`sprite-env curl … /v1/tasks/…`, which holds a Sprite awake) is
//! answered here: a hold is recorded, and a DELETE drops it. So are its
//! services (`sprite-env services create|delete <name>`): a process per
//! Sprite and name, started in its home, and stopped as the runtime stops
//! one (TERM, then KILL 5 s later), as every one is when the fake goes. The
//! release the install fetches (`/download/fragment-<os>-<arch>.tar.gz`)
//! is the binary the fake was started with.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::json;

use crate::http::{Handler, Request, Response, Server};

/// What the fake knows of one Sprite.
#[derive(Debug, Clone, Default)]
pub struct Sprite {
    /// Held awake (a live task).
    pub held: bool,
    pub holds: u32,
    pub releases: u32,
    /// Commands run (the Tasks API's aside), and the last input one was given.
    pub runs: u32,
    pub stdin: String,
    /// Every call that named it.
    pub calls: u32,
}

#[derive(Default)]
struct State {
    sprites: BTreeMap<String, Sprite>,
    deleted: Vec<String>,
    /// Every call is refused as a bad token's is (a lever).
    refusing: bool,
    /// Running services, by Sprite and name.
    services: BTreeMap<(String, String), Child>,
}

pub struct Sprites {
    pub url: String,
    state: Arc<Mutex<State>>,
    _server: Server,
}

fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 63 && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// Runs a command on a Sprite: the Tasks API here, anything else in its home.
fn exec(state: &Mutex<State>, home: &Path, name: &str, req: &Request) -> Response {
    let argv: Vec<&str> = req.pairs.iter().filter(|(k, _)| k == "cmd").map(|(_, v)| v.as_str()).collect();
    let Some((program, args)) = argv.split_first() else { return Response::json(400, &json!({ "error": "cmd is required" })) };
    if let ("sprite-env", ["services", verb, service, flags @ ..]) = (*program, args) {
        return services(state, home, name, verb, service, flags);
    }
    if *program == "sprite-env" {
        let method = args.iter().position(|a| *a == "-X").and_then(|i| args.get(i + 1)).copied().unwrap_or("GET");
        let mut s = state.lock().expect("sprites state");
        let sprite = s.sprites.get_mut(name).expect("exec checked the Sprite exists");
        match method {
            "PUT" | "POST" => (sprite.held, sprite.holds) = (true, sprite.holds + 1),
            "DELETE" => (sprite.held, sprite.releases) = (false, sprite.releases + 1),
            _ => {}
        }
        // the task, as the Tasks API answers it, while there is one
        return match sprite.held {
            true => Response::json(200, &json!({ "name": "fragment", "started_at": "2026-09-26T00:00:00Z", "expires_at": "2026-09-26T00:02:00Z" })),
            false => Response::json(200, &json!({ "error": "task not found" })),
        };
    }
    let stdin = if req.query.get("stdin").map(String::as_str) == Some("true") { req.body.clone() } else { Vec::new() };
    let mut s = state.lock().expect("sprites state");
    let sprite = s.sprites.get_mut(name).expect("exec checked the Sprite exists");
    sprite.runs += 1;
    if !stdin.is_empty() {
        sprite.stdin = String::from_utf8_lossy(&stdin).into_owned();
    }
    drop(s);
    let child = Command::new(program).args(args).env("HOME", home).current_dir(home).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn();
    let out = child.and_then(|mut c| {
        c.stdin.take().expect("piped").write_all(&stdin)?;
        c.wait_with_output()
    });
    match out {
        Ok(o) => Response::bytes(200, "text/plain", [o.stdout, o.stderr].concat()),
        Err(e) => Response::json(500, &json!({ "error": e.to_string() })),
    }
}

/// A Sprite's services: `create <name> --cmd C [--args a,b] [--dir D]`
/// starts one in its home; `delete <name>` stops it.
fn services(state: &Mutex<State>, home: &Path, sprite: &str, verb: &str, service: &str, flags: &[&str]) -> Response {
    let key = (sprite.to_string(), service.to_string());
    let flag = |f: &str| flags.iter().position(|a| *a == f).and_then(|i| flags.get(i + 1)).copied();
    let mut s = state.lock().expect("sprites state");
    match (verb, s.services.contains_key(&key), flag("--cmd")) {
        ("delete", true, _) => {
            let child = s.services.remove(&key).expect("a service it has");
            drop(s);
            stop(child);
            Response::json(200, &json!({}))
        }
        ("delete", false, _) => Response::json(404, &json!({ "error": "service not found" })),
        ("create", true, _) => Response::json(409, &json!({ "error": "a service has that name" })),
        ("create", false, Some(cmd)) => {
            let args = flag("--args").map_or(vec![], |a| a.split(',').collect());
            let dir = flag("--dir").map_or(home.to_path_buf(), PathBuf::from);
            let spawned = Command::new(cmd).args(args).env("HOME", home).current_dir(dir).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
            match spawned {
                Ok(child) => {
                    s.services.insert(key, child);
                    Response::json(200, &json!({ "name": service }))
                }
                Err(e) => Response::json(500, &json!({ "error": e.to_string() })),
            }
        }
        _ => Response::json(400, &json!({ "error": "services create <name> --cmd C, or delete <name>" })),
    }
}

/// Stops a service as the runtime does: TERM, then KILL 5 s later.
fn stop(mut child: Child) {
    let _ = Command::new("kill").args(["-TERM", &child.id().to_string()]).status();
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_secs(5) {
        if let Ok(Some(_)) = child.try_wait() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = child.kill();
    let _ = child.wait();
}

impl Drop for Sprites {
    fn drop(&mut self) {
        let services = std::mem::take(&mut self.state.lock().expect("sprites state").services);
        services.into_values().for_each(stop);
    }
}

impl Sprites {
    /// Serves the API with `token`, keeping Sprites under `root`, and the
    /// release made from `cli` (a `tar` of it, as a release holds it).
    pub fn start(token: &str, root: &Path, cli: &Path) -> std::io::Result<Sprites> {
        let release = root.join("release");
        std::fs::create_dir_all(&release)?;
        std::fs::copy(cli, release.join("fragment"))?;
        let made = Command::new("tar").args(["-czf", "fragment.tar.gz", "fragment"]).current_dir(&release).status()?;
        assert!(made.success(), "tar makes the release");
        let tarball = std::fs::read(release.join("fragment.tar.gz"))?;
        let (state, bearer, root): (Arc<Mutex<State>>, String, PathBuf) = (Arc::default(), format!("Bearer {token}"), root.join("sprites"));
        let st = Arc::clone(&state);
        let handler: Handler = Arc::new(move |req: &Request| {
            let parts: Vec<&str> = req.path.trim_start_matches('/').split('/').collect();
            if let ["download", file] = parts.as_slice() {
                return match file.starts_with("fragment-") && file.ends_with(".tar.gz") {
                    true => Response::bytes(200, "application/gzip", tarball.clone()),
                    false => Response::json(404, &json!({ "error": "no such release file" })),
                };
            }
            if req.header("authorization") != Some(bearer.as_str()) || st.lock().expect("sprites state").refusing {
                return Response::json(401, &json!({ "error": "Missing or invalid authentication" }));
            }
            let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap_or_default();
            let name = match parts.as_slice() {
                ["v1", "sprites"] => body["name"].as_str().unwrap_or(""),
                ["v1", "sprites", name, ..] => name,
                _ => return Response::json(404, &json!({ "error": "no such route" })),
            };
            if !valid_name(name) {
                return Response::json(400, &json!({ "error": "a Sprite's name is lowercase letters, digits, and dashes" }));
            }
            let home = root.join(name);
            let exists = match st.lock().expect("sprites state").sprites.get_mut(name) {
                Some(sprite) => {
                    sprite.calls += 1;
                    true
                }
                None => false,
            };
            let sprite = json!({ "id": format!("sprite-{name}"), "name": name, "organization": "fake", "status": "cold", "url": format!("https://{name}.sprites.app") });
            match (req.method.as_str(), &parts[2..], exists) {
                ("POST", [], true) => Response::json(409, &json!({ "error": "a Sprite has that name" })),
                ("POST", [], false) => match std::fs::create_dir_all(&home) {
                    Ok(()) => {
                        st.lock().expect("sprites state").sprites.insert(name.to_string(), Sprite::default());
                        Response::json(201, &sprite)
                    }
                    Err(e) => Response::json(500, &json!({ "error": e.to_string() })),
                },
                ("GET", [_], true) => Response::json(200, &sprite),
                ("DELETE", [_], true) => {
                    let mut s = st.lock().expect("sprites state");
                    s.sprites.remove(name);
                    s.deleted.push(name.to_string());
                    let _ = std::fs::remove_dir_all(&home);
                    Response::bytes(204, "text/plain", Vec::new())
                }
                ("POST", [_, "exec"], true) => exec(&st, &home, name, req),
                (_, _, false) => Response::json(404, &json!({ "error": "Sprite not found" })),
                _ => Response::json(404, &json!({ "error": "no such route" })),
            }
        });
        let server = Server::start(0, handler)?;
        Ok(Sprites { url: server.url.clone(), state, _server: server })
    }

    /// The Sprites there are now.
    pub fn sprites(&self) -> BTreeMap<String, Sprite> {
        self.state.lock().expect("sprites state").sprites.clone()
    }

    /// While set, every call is refused (401), as with a bad token.
    pub fn refuse(&self, refusing: bool) {
        self.state.lock().expect("sprites state").refusing = refusing;
    }

    /// The Sprites deleted, in order.
    pub fn deleted(&self) -> Vec<String> {
        self.state.lock().expect("sprites state").deleted.clone()
    }
}
