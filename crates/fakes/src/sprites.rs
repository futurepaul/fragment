//! Fly Sprites, the part a `Computer` cell reaches through `KEYS`: make,
//! read, and delete a Sprite, and run a command on it by HTTP POST
//! (docs.fly.io/sprites/api). A Sprite here is a directory, and `exec`
//! runs its command as a local process with the Sprite's own home as
//! `HOME`, so a computer's first boot (the release's one-line install,
//! then `fragment login --pair`) really runs. The Sprite runtime's Tasks
//! API (`sprite-env curl … /v1/tasks/…`, which holds a Sprite awake) is
//! answered here: a hold is recorded, and a DELETE drops it. The release
//! the install fetches (`/download/fragment-<os>-<arch>.tar.gz`) is the
//! binary the fake was started with.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

use serde_json::json;

use crate::http::{Handler, Request, Response, Server};

/// What the fake knows of one Sprite.
#[derive(Debug, Clone, Default)]
pub struct Sprite {
    /// Held awake (a live task).
    pub held: bool,
    pub holds: u32,
    pub releases: u32,
    /// Commands run (the Tasks API's aside), and the last one's input.
    pub runs: u32,
    pub stdin: String,
}

#[derive(Default)]
struct State {
    sprites: BTreeMap<String, Sprite>,
    deleted: Vec<String>,
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
    if *program == "sprite-env" {
        let method = args.iter().position(|a| *a == "-X").and_then(|i| args.get(i + 1)).copied().unwrap_or("GET");
        let mut s = state.lock().expect("sprites state");
        let sprite = s.sprites.get_mut(name).expect("exec checked the Sprite exists");
        match method {
            "PUT" | "POST" => (sprite.held, sprite.holds) = (true, sprite.holds + 1),
            "DELETE" => (sprite.held, sprite.releases) = (false, sprite.releases + 1),
            _ => {}
        }
        return Response::json(200, &json!({}));
    }
    let stdin = if req.query.get("stdin").map(String::as_str) == Some("true") { req.body.clone() } else { Vec::new() };
    let mut s = state.lock().expect("sprites state");
    let sprite = s.sprites.get_mut(name).expect("exec checked the Sprite exists");
    (sprite.runs, sprite.stdin) = (sprite.runs + 1, String::from_utf8_lossy(&stdin).into_owned());
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
            if req.header("authorization") != Some(bearer.as_str()) {
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
            let exists = st.lock().expect("sprites state").sprites.contains_key(name);
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

    /// The Sprites deleted, in order.
    pub fn deleted(&self) -> Vec<String> {
        self.state.lock().expect("sprites state").deleted.clone()
    }
}
