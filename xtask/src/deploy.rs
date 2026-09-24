//! `cargo xtask deploy <fleet> [--nodes]`: ships to a hosted fleet, read
//! from `fleets/<fleet>.json` (ROADMAP decision 13: nothing about a fleet is
//! a constant). Without `--nodes` it deploys the cell: builds `cell/`,
//! renders its `vars` from the fleet file and the secret files it names,
//! writes the deployment to the bucket with `celld deploy`, and waits until
//! the fleet answers with it. `--nodes` rolls the Machines to a new node
//! image (`fleets/Dockerfile`), one at a time.
//!
//! `cargo xtask fleet <fleet> <celld command...>` runs an operator command
//! of the fork's celld (`diagnose`, `cell list`, `queue info Q`, ...)
//! against the fleet's bucket with its credentials.
//!
//! Secret values are read from files and never printed: celld's output is
//! redacted, and the staged project that holds them is deleted.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use fragment_devstack as devstack;
use serde::Deserialize;

#[derive(Deserialize)]
struct Fleet {
    /// Where the platform answers (the fleet's own host).
    url: String,
    fly: Fly,
    bucket: Bucket,
    /// Worker variables with plain values.
    vars: BTreeMap<String, String>,
    /// Worker variables whose values are the contents of these files.
    secret_vars: BTreeMap<String, String>,
    /// The hosted e2e's inputs.
    e2e: Option<E2e>,
}

#[derive(Deserialize)]
struct E2e {
    /// A file holding the e2e's secret key (hex); a person on the fleet
    /// approved it once (`/cli?key=<npub>`, signed in).
    key: String,
    /// A file holding an OpenRouter key for the live AI check (optional).
    openrouter_key: Option<String>,
}

#[derive(Deserialize)]
struct Fly {
    app: String,
    org: String,
    region: String,
    size: String,
    memory: String,
    volume: String,
    /// A file holding a Fly API token for the org.
    token: String,
}

#[derive(Deserialize)]
struct Bucket {
    url: String,
    endpoint: String,
    region: String,
    /// A file of `AWS_ACCESS_KEY_ID` and `AWS_SECRET_ACCESS_KEY` lines
    /// (`NAME=value` or `NAME: value`).
    credentials: String,
}

/// How long the fleet may take to adopt a deployment (nodes poll every 30 s).
const ADOPT_TIMEOUT: Duration = Duration::from_secs(120);
/// Consecutive answers with the new deployment before it counts as adopted
/// (requests spread over both Machines).
const ADOPT_STREAK: usize = 8;

fn expand(path: &str) -> PathBuf {
    match path.strip_prefix("~/") {
        Some(rest) => PathBuf::from(std::env::var_os("HOME").expect("HOME is set")).join(rest),
        None => PathBuf::from(path),
    }
}

fn read_secret(path: &str) -> Result<String> {
    let p = expand(path);
    let text = std::fs::read_to_string(&p).with_context(|| format!("reading the secret file {}", p.display()))?;
    let text = text.trim().to_string();
    if text.is_empty() {
        bail!("the secret file {} is empty", p.display());
    }
    Ok(text)
}

/// `NAME=value` or `NAME: value` lines → (name, value).
fn parse_env_lines(text: &str) -> BTreeMap<String, String> {
    text.lines()
        .filter_map(|l| {
            let l = l.trim();
            let (k, v) = l.split_once('=').or_else(|| l.split_once(':'))?;
            let k = k.trim();
            (!k.is_empty() && k.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')).then(|| (k.to_string(), v.trim().to_string()))
        })
        .collect()
}

fn load(name: &str) -> Result<Fleet> {
    let path = devstack::repo_root().join("fleets").join(format!("{name}.json"));
    let text = std::fs::read_to_string(&path).with_context(|| format!("no fleet file {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("{} does not parse", path.display()))
}

/// The wrangler config with `vars` added after its opening brace (JSONC
/// allows the comments around it).
fn with_vars(wrangler: &str, vars: &BTreeMap<String, String>) -> Result<String> {
    let at = wrangler.find('{').context("the wrangler config has no object")?;
    Ok(format!("{}{{\n  \"vars\": {},{}", &wrangler[..at], serde_json::to_string(vars)?, &wrangler[at + 1..]))
}

fn deploy_id() -> String {
    let rev = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .current_dir(devstack::repo_root())
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    let dirty = Command::new("git")
        .args(["status", "--porcelain", "--untracked-files=no"])
        .current_dir(devstack::repo_root())
        .output()
        .is_ok_and(|o| !o.stdout.is_empty());
    let at = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    format!("{rev}{}-{at}", if dirty { "+dirty" } else { "" })
}

fn redact(text: &str, secrets: &[String]) -> String {
    secrets.iter().filter(|s| s.len() >= 8).fold(text.to_string(), |t, s| t.replace(s.as_str(), "<redacted>"))
}

pub fn run(args: &[String]) -> Result<()> {
    let usage = "usage: cargo xtask deploy <fleet> [--nodes]";
    let (name, nodes) = match args {
        [name] => (name, false),
        [name, flag] if flag == "--nodes" => (name, true),
        _ => bail!(usage),
    };
    let fleet = load(name)?;
    if nodes {
        deploy_nodes(name, &fleet)
    } else {
        deploy_cell(name, &fleet)
    }
}

/// The bucket's credentials, from the fleet's credentials file.
fn bucket_keys(fleet: &Fleet) -> Result<(String, String)> {
    let creds = parse_env_lines(&read_secret(&fleet.bucket.credentials)?);
    match (creds.get("AWS_ACCESS_KEY_ID"), creds.get("AWS_SECRET_ACCESS_KEY")) {
        (Some(id), Some(key)) => Ok((id.clone(), key.clone())),
        _ => bail!("{} lacks AWS_ACCESS_KEY_ID or AWS_SECRET_ACCESS_KEY", fleet.bucket.credentials),
    }
}

/// `celld <args> --bucket … --endpoint … --region …` with the bucket's keys.
fn celld(fleet: &Fleet, args: &[&str]) -> Result<Command> {
    let tools = devstack::Tools::locate()?;
    let (id, key) = bucket_keys(fleet)?;
    let mut cmd = Command::new(&tools.celld);
    cmd.args(args)
        .args(["--bucket", &fleet.bucket.url, "--endpoint", &fleet.bucket.endpoint, "--region", &fleet.bucket.region])
        .env("AWS_ACCESS_KEY_ID", id)
        .env("AWS_SECRET_ACCESS_KEY", key)
        .env("CELLD_ESBUILD", &tools.esbuild);
    Ok(cmd)
}

/// Runs `cmd`, printing its output with every secret redacted.
fn run_redacted(mut cmd: Command, secrets: &[String]) -> Result<()> {
    let out = cmd.output().context("could not start celld")?;
    print!("{}", redact(&String::from_utf8_lossy(&out.stdout), secrets));
    eprint!("{}", redact(&String::from_utf8_lossy(&out.stderr), secrets));
    if !out.status.success() {
        bail!("celld failed: {}", out.status);
    }
    Ok(())
}

/// The environment `fragment-e2e --hosted` reads for `fleet`: paths to
/// secret files, never their values.
pub fn hosted_e2e_env(name: &str) -> Result<Vec<(String, String)>> {
    let fleet = load(name)?;
    let e2e = fleet.e2e.as_ref().with_context(|| format!("fleets/{name}.json has no e2e block"))?;
    let path = |p: &str| expand(p).to_string_lossy().into_owned();
    let mut env = vec![
        ("FRAGMENT_E2E_HOSTED".to_string(), fleet.url.clone()),
        ("FRAGMENT_E2E_SUFFIX".to_string(), fleet.vars.get("FRAGMENT_HOST_SUFFIX").cloned().unwrap_or_default()),
        ("FRAGMENT_E2E_KEY_FILE".to_string(), path(&e2e.key)),
    ];
    if let Some(k) = &e2e.openrouter_key {
        env.push(("FRAGMENT_E2E_OPENROUTER_KEY_FILE".into(), path(k)));
    }
    if let (Some(org), Some(key)) = (fleet.vars.get("CODESTORAGE_ORG"), fleet.secret_vars.get("CODESTORAGE_PRIVATE_KEY")) {
        let api = fleet.vars.get("CODESTORAGE_API_URL").cloned().unwrap_or_else(|| format!("https://api.{org}.code.storage"));
        env.push(("FRAGMENT_E2E_CODESTORAGE_ORG".into(), org.clone()));
        env.push(("FRAGMENT_E2E_CODESTORAGE_API".into(), api));
        env.push(("FRAGMENT_E2E_CODESTORAGE_KEY_FILE".into(), path(key)));
    }
    Ok(env)
}

pub fn operate(args: &[String]) -> Result<()> {
    let [name, rest @ ..] = args else { bail!("usage: cargo xtask fleet <fleet> <celld command...>") };
    if rest.is_empty() {
        bail!("usage: cargo xtask fleet <fleet> <celld command...> (diagnose, cell list, queue info <queue>, ...)");
    }
    let fleet = load(name)?;
    let args: Vec<&str> = rest.iter().map(String::as_str).collect();
    run_redacted(celld(&fleet, &args)?, &[bucket_keys(&fleet)?.1])
}

fn deploy_cell(name: &str, fleet: &Fleet) -> Result<()> {
    crate::build()?;
    let mut vars = fleet.vars.clone();
    let mut secrets = vec![];
    for (k, path) in &fleet.secret_vars {
        let v = read_secret(path)?;
        secrets.push(v.clone());
        vars.insert(k.clone(), v);
    }
    let id = deploy_id();
    vars.insert("FRAGMENT_DEPLOY_ID".into(), id.clone());
    secrets.push(bucket_keys(fleet)?.1);

    let stage = devstack::repo_root().join("target/fleets").join(name).join("cell");
    let _ = std::fs::remove_dir_all(&stage);
    devstack::stage_project(&stage)?;
    let result = (|| -> Result<()> {
        let config = stage.join("wrangler.jsonc");
        let rendered = with_vars(&std::fs::read_to_string(&config)?, &vars)?;
        // the staged copy holds secrets: owner-only, before they are written
        std::fs::set_permissions(&config, std::os::unix::fs::PermissionsExt::from_mode(0o600))?;
        std::fs::write(&config, rendered)?;
        println!("deploying the cell to {name} ({}) as {id}", fleet.bucket.url);
        let stage = stage.to_str().context("a UTF-8 path")?;
        run_redacted(celld(fleet, &["deploy", stage])?, &secrets)
    })();
    std::fs::remove_dir_all(&stage).with_context(|| format!("remove the staged project {} (it holds secrets)", stage.display()))?;
    result?;
    wait_for(&format!("{}/healthz", fleet.url.trim_end_matches('/')), &id)
}

/// Polls `url` until `ADOPT_STREAK` answers in a row carry deployment `id`.
fn wait_for(url: &str, id: &str) -> Result<()> {
    let client = reqwest::blocking::Client::builder().timeout(Duration::from_secs(10)).build()?;
    let started = Instant::now();
    let mut streak = 0;
    let mut last = String::new();
    while started.elapsed() < ADOPT_TIMEOUT {
        match client.get(url).send() {
            Ok(r) => {
                last = r.headers().get("x-fragment-deploy").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
                streak = if last == id { streak + 1 } else { 0 };
            }
            Err(e) => {
                last = e.to_string();
                streak = 0;
            }
        }
        if streak >= ADOPT_STREAK {
            println!("the fleet serves {id} ({:.0?})", started.elapsed());
            return Ok(());
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    bail!("the fleet did not adopt {id} within {ADOPT_TIMEOUT:?} (last answer: {last})")
}

/// fly.toml for the fleet, rendered from its file.
fn fly_toml(fleet: &Fleet) -> String {
    let f = &fleet.fly;
    format!(
        r#"# Rendered by `cargo xtask deploy` from fleets/<fleet>.json; not edited by hand.
app = "{app}"
primary_region = "{region}"
kill_signal = "SIGTERM"
# celld drains within CELLD_SHUTDOWN_TOTAL_MS (40 s by default)
kill_timeout = "60s"

[env]
  CELLD_BUCKET = "{bucket}"
  S3_ENDPOINT = "{endpoint}"
  AWS_REGION = "{bucket_region}"
  # a Worker's fetch reaches public addresses only: the private network
  # carries the internal listener (docs/phase-3.md slice E)
  CELLD_EGRESS_PUBLIC_ONLY = "1"

[mounts]
  source = "celld_data"
  destination = "/data"
  initial_size = "{volume}"

[http_service]
  internal_port = 8080
  force_https = true
  auto_stop_machines = "off"
  auto_start_machines = false
  min_machines_running = 2
  [http_service.concurrency]
    type = "connections"
    soft_limit = 4000
    hard_limit = 5000
  [[http_service.checks]]
    grace_period = "30s"
    interval = "15s"
    method = "GET"
    path = "/.well-known/celld/health"
    timeout = "5s"

[[restart]]
  policy = "always"

[deploy]
  strategy = "rolling"
  max_unavailable = 1

[[vm]]
  size = "{size}"
  memory = "{memory}"
"#,
        app = f.app,
        region = f.region,
        bucket = fleet.bucket.url,
        endpoint = fleet.bucket.endpoint,
        bucket_region = fleet.bucket.region,
        volume = f.volume,
        size = f.size,
        memory = f.memory,
    )
}

/// Builds the node image with the local Docker (for linux/amd64: OrbStack
/// or Docker Desktop), pushes it to Fly's registry, and rolls the Machines
/// to it. Fly's remote builders are not used: their push to the registry
/// is refused for this org's token (flyctl 0.3.145), while a push from here
/// with the same token is accepted.
fn deploy_nodes(name: &str, fleet: &Fleet) -> Result<()> {
    let root = devstack::repo_root();
    let dir = root.join("target/fleets").join(name);
    std::fs::create_dir_all(&dir)?;
    let config = dir.join("fly.toml");
    std::fs::write(&config, fly_toml(fleet))?;
    let token = read_secret(&fleet.fly.token)?;
    let rev = deploy_id();
    let tag = format!("registry.fly.io/{}:node-{}-{}", fleet.fly.app, &devstack::CELLD_FORK_REV[..7], rev.replace('+', "-"));
    println!("building {tag} (celld {})", &devstack::CELLD_FORK_REV[..7]);
    crate::run(
        Command::new("docker")
            .args(["build", "--platform", "linux/amd64", "-f", "fleets/Dockerfile"])
            .args(["--build-arg", &format!("CELLD_REV={}", devstack::CELLD_FORK_REV), "-t", &tag, "."])
            .current_dir(&root),
    )?;
    // a Docker config of its own, so the token never lands in the user's
    let docker = dir.join("docker-config");
    let _ = std::fs::remove_dir_all(&docker);
    std::fs::create_dir_all(&docker)?;
    let pushed = (|| -> Result<()> {
        use std::io::Write;
        let mut login = Command::new("docker")
            .args(["login", "registry.fly.io", "-u", "x", "--password-stdin"])
            .env("DOCKER_CONFIG", &docker)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .spawn()
            .context("could not start docker login")?;
        login.stdin.take().context("docker login's stdin")?.write_all(token.as_bytes())?;
        if !login.wait()?.success() {
            bail!("docker login to registry.fly.io failed");
        }
        crate::run(Command::new("docker").args(["push", &tag]).env("DOCKER_CONFIG", &docker))
    })();
    std::fs::remove_dir_all(&docker).with_context(|| format!("remove {} (it holds a registry login)", docker.display()))?;
    pushed?;
    println!("rolling {} ({}) to {tag}", fleet.fly.app, fleet.fly.org);
    crate::run(
        Command::new("flyctl")
            .arg("deploy")
            .arg("--config")
            .arg(&config)
            .args(["--image", &tag, "--ha=false", "--wait-timeout", "10m"])
            .current_dir(&root)
            .env("FLY_API_TOKEN", token),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vars_go_inside_the_config() {
        let vars = BTreeMap::from([("A".to_string(), "line1\nline2".to_string())]);
        let out = with_vars("// a comment\n{\n  \"name\": \"fragment\"\n}\n", &vars).unwrap();
        assert_eq!(out, "// a comment\n{\n  \"vars\": {\"A\":\"line1\\nline2\"},\n  \"name\": \"fragment\"\n}\n");
    }

    #[test]
    fn credentials_in_either_form() {
        let got = parse_env_lines("AWS_ACCESS_KEY_ID=tid_1\n  AWS_SECRET_ACCESS_KEY: tsec_2 \nnot a line\nlower=x\n");
        assert_eq!(got.get("AWS_ACCESS_KEY_ID").map(String::as_str), Some("tid_1"));
        assert_eq!(got.get("AWS_SECRET_ACCESS_KEY").map(String::as_str), Some("tsec_2"));
        assert_eq!(got.len(), 2);
    }

    #[test]
    fn secrets_never_print() {
        let secret = "0123456789abcdef".to_string();
        assert_eq!(redact("vars: 0123456789abcdef ok", &[secret]), "vars: <redacted> ok");
    }
}
