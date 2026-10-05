//! `cargo xtask <command>`: the repo's tooling, in Rust.
//!
//!   build            build cell/ and agent/ for wasm32 (worker-build 0.8.5; in
//!                    parallel once worker-build has fetched its tools: build.rs)
//!   dev [--clean] [--runtime wrangler|celld] [--lan]
//!                    build, then run the stack in the foreground under
//!                    `wrangler dev`: the cell on :8790 (fragments at
//!                    <label>--<username>.fragment.localhost:8790), the
//!                    code.storage fake on :8792 (or a store already running:
//!                    CODESTORAGE_API_URL; or macrofiche, started there:
//!                    MACROFICHE_BIN; `dev_codestore`), the Workers AI fake on :8796
//!                    behind the model route, and the agents' Worker beside it
//!                    (their turns spend their owner's ledger; new people are
//!                    seats with the month's included credit). Sign-in is the
//!                    WorkOS fake's AuthKit on :8795 (Pipes on :8794), or
//!                    with FRAGMENT_SIGNIN=oidc the
//!                    OpenID Connect fake on :8798, or a real provider
//!                    (FRAGMENT_OIDC_ISSUER: `dev_signin`). FRAGMENT_DEV_PORT
//!                    moves the cell and its fakes (`dev_ports`). --lan serves
//!                    it to the home network as an intranet (on celld; lan.rs,
//!                    docs/self-host-lan.md): https://<zone> through a TLS
//!                    front door, DNS for the zone, a private CA, Dex
//!   try <template> [name]
//!                    on the running dev stack: a fragment from a template
//!                    (todo, inbox, notes), scaffolded under target/devstack/try
//!                    so nothing lands in the repo; prints what to open and paste
//!   e2e [args...]    build, then run crates/e2e (args pass through: --only <section>[,...],
//!                    --except <section>[,...], or --shard <k>/<n> (the table's, as CI
//!                    splits it); --summary <file> writes the run's summary; --rehearse
//!                    keeps the hosted lane's rules on the local node). The build
//!                    builds the computer images ahead of the node beside the Rust.
//!                    CI splits it in two steps, so the cache saves between them:
//!                    --build-only (builds, runs nothing), then --no-build (runs
//!                    what the build left)
//!   e2e-summary <dir>
//!                    CI's `e2e` check: the shards' summaries in <dir> make the
//!                    suite exactly once, and every check passed (summary.rs)
//!   e2e --hosted --config <file> --branch <name> [--only … | --except …]
//!       [--dry-run | --sweep] [--max-paid-calls <n>]
//!                    the suite against that branch deployment on its real vendors
//!                    (crates/e2e/src/hosted.rs); --dry-run prints its plan
//!   check            every first-party JavaScript file parses (node --check;
//!                    xtask/src/js_syntax.rs), then host tests and clippy,
//!                    warnings denied
//!   secret set <name> --config <file> [--from-file <path>]
//!   secret gen <name> --config <file>
//!   secret list --config <file>
//!                    the deployment's secrets in its account's Cloudflare
//!                    Secrets Store: set one (wrangler's hidden prompt, standard
//!                    input, or a file once), make a host secret, or list names
//!                    and times; --local <state dir> for wrangler's local store
//!                    instead (xtask/src/secret.rs)
//!   deploy --config <file> [--branch <name>]
//!                    build and deploy to Cloudflare from a deployment's config
//!                    (deploy/example.jsonc), once its secrets are all in the
//!                    store: a branch gets a complete copy of its own at
//!                    <branch>.<zone> (xtask/src/deploy.rs)
//!   teardown --config <file> --branch <name>
//!                    remove a branch deployment (irreversible)
//!
//! dev, e2e, secret, deploy and teardown run wrangler and npm on the pinned Node,
//! and check runs `node --check` on it, fetched into target/tools on first
//! use, never a `node` from PATH (crates/devstack/src/node.rs;
//! FRAGMENT_NODE names another).
//!
//! fragment.club runs on celld from the `celld` branch (the tag celld-final)
//! until the cutover (docs/cloudflare-v1.md, decision 35): `deploy` never
//! reaches it.

use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};
use fragment_devstack as devstack;

mod build;
mod deploy;
mod dns;
mod js_syntax;
mod lan;
mod secret;
mod summary;

/// The dev stack's ports (`FRAGMENT_DEV_PORT` moves them all, so two stacks
/// share a machine): the cell, then its fakes above it.
const DEV_PORT: u16 = 8790;
const DEV_CODESTORAGE_OFFSET: u16 = 2;
const DEV_WORKOS_OFFSET: u16 = 4;
/// The WorkOS fake's AuthKit (its OpenID Connect provider), at a domain of
/// its own as AuthKit's is.
const DEV_AUTHKIT_OFFSET: u16 = 5;
/// The Workers AI fake behind the model route (`FRAGMENT_AI_URL`): dev
/// never calls real models.
const DEV_AI_OFFSET: u16 = 6;
/// The OpenID Connect fake (`FRAGMENT_SIGNIN=oidc`).
const DEV_OIDC_OFFSET: u16 = 8;
/// The WorkOS fake's environment in dev, and its OAuth application.
const DEV_WORKOS_CLIENT: &str = "client_fragment_dev";
const DEV_WORKOS_KEY: &str = "sk_test_fragment_dev";
const DEV_WORKOS_APP: &str = "client_fragment_dev_app";
const DEV_WORKOS_APP_SECRET: &str = "sk_app_fragment_dev";
/// The OpenID Connect fake's client in dev.
const DEV_OIDC_CLIENT: &str = "fragment-dev";
const DEV_OIDC_SECRET: &str = "oidc-secret-fragment-dev";
const DEV_ORG: &str = "fragment-dev";

/// The dev stack's base port: `FRAGMENT_DEV_PORT`, else `DEV_PORT`; its
/// fakes take the ports above it, so it leaves room for them.
fn dev_port() -> Result<u16> {
    match std::env::var("FRAGMENT_DEV_PORT") {
        Err(_) => Ok(DEV_PORT),
        Ok(p) => match p.parse::<u16>() {
            Ok(port) if (1024..=u16::MAX - DEV_OIDC_OFFSET).contains(&port) => Ok(port),
            _ => bail!("FRAGMENT_DEV_PORT is a port from 1024 to {}, not {p:?}", u16::MAX - DEV_OIDC_OFFSET),
        },
    }
}

/// A command as errors show it: the program and its arguments only
/// (`{cmd:?}` would print the environment set on it, where a deploy may
/// hand wrangler its token).
fn shown(cmd: &Command) -> String {
    std::iter::once(cmd.get_program()).chain(cmd.get_args()).map(|a| a.to_string_lossy()).collect::<Vec<_>>().join(" ")
}

fn run(cmd: &mut Command) -> Result<()> {
    let shown = shown(cmd);
    let status = cmd.status().with_context(|| format!("could not start {shown}"))?;
    if !status.success() {
        bail!("{shown} failed: {status}");
    }
    Ok(())
}

/// cell/ and agent/ for wasm32 (build.rs).
fn build() -> Result<()> {
    build::workers()
}

fn dev(args: &[String]) -> Result<()> {
    // the home network as an intranet (lan.rs): its front door is before celld
    let lan_mode = args.iter().any(|a| a == "--lan");
    // the runtime: wrangler's local workerd, or celld (docs/self-host.md, seam 1)
    let celld = match args.iter().position(|a| a == "--runtime").and_then(|i| args.get(i + 1)).map(String::as_str) {
        None => lan_mode,
        Some("wrangler") if lan_mode => bail!("--lan runs on celld: drop --runtime wrangler"),
        Some("wrangler") => false,
        Some("celld") => true,
        Some(other) => bail!("--runtime is wrangler or celld, not {other}"),
    };
    if celld {
        // before the build: a missing CELLD_BIN stops the run at once
        devstack::celld::CelldTools::locate()?;
    }
    let port = dev_port()?;
    // the pinned Node and node_modules first: a refused FRAGMENT_NODE stops
    // the run before a build
    let tools = devstack::Tools::locate()?;
    build()?;
    let clean = args.iter().any(|a| a == "--clean");
    if clean {
        // the node's state, before the fleet seeds its local store again
        devstack::clear_state(&devstack::cell_dir())?;
    }
    let store = dev_codestore(port, clean)?;
    let read = |var: &str| -> Result<Option<String>> {
        match std::env::var_os(var) {
            Some(path) => Ok(Some(std::fs::read_to_string(&path).with_context(|| format!("reading {}", Path::new(&path).display()))?.trim().to_string())),
            None => Ok(None),
        }
    };
    // on the LAN, Dex signs people in, and nothing stands in for WorkOS
    let lan = if lan_mode { Some(lan::start(port)?) } else { None };
    let (workos, authkit, _workos_fake) = match &lan {
        Some(_) => (None, None, None),
        None => {
            let (w, authkit, fake) = dev_workos(port, &read)?;
            (Some(w), authkit, fake)
        }
    };
    let ai = fragment_fakes::workers_ai::WorkersAi::start(port + DEV_AI_OFFSET)?;
    let (oidc, signin_label, _oidc_fake) = match &lan {
        Some(l) => (l.oidc.clone(), format!("Dex at {}", l.oidc.issuer), None),
        None => dev_signin(port, &read, authkit)?,
    };
    let (model_upstream, nodes) = self_host(&read)?;
    // computers run on the nodes when there are some, else in local Docker;
    // with neither, the stack runs without computers, and says so
    // (celld runs no containers of its own: its computers are a node's)
    let docker = !celld && nodes.is_none() && Command::new(std::env::var("WRANGLER_DOCKER_BIN").unwrap_or_else(|_| "docker".into())).arg("info").stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success());
    let computers = nodes.is_some() || docker;
    let computer_image = dev_computer_image(nodes.as_ref())?;
    let mut fleet = devstack::Fleet {
        host_secret: devstack::dev_secret("host-secret", || devstack::random_hex(32))?,
        codestorage_org: store.org.clone(),
        codestorage_key_pem: store.key_pem.clone(),
        codestorage_url: store.url.clone(),
        host_suffix: Some(lan.as_ref().map_or_else(|| "fragment.localhost".to_string(), |l| l.settings.zone.clone())),
        legacy_host_suffix: None,
        host_label_suffix: None,
        // No webhooks reach dev fragments (the CLI's refresh and this poll do).
        poll_interval_s: 10,
        egress_local: true,
        job_retry_delay_s: 2,
        blob_grace_s: None,
        // text and images: the Workers AI fake (dev never calls a real model)
        ai_url: Some(ai.url.clone()),
        ai_gateway: None,
        // a dev person is a seat, with the month's included credit
        default_plan: Some("seat".into()),
        delivery_retry_s: None,
        workos,
        oidc: Some(oidc),
        // the CLI's host: sign-in and approvals happen where it points (on
        // the LAN, the front door's https origin)
        platform_url: Some(lan.as_ref().map_or_else(|| format!("http://127.0.0.1:{port}"), |l| l.settings.platform_url())),
        operators: None,
        signins_pending_max: None,
        test_secret: None,
        computer_image: computers.then(|| computer_image.clone()),
        computer_snapshots: false,
        providers: None,
        operator_key_values: vec![],
        swap_upstream: None,
        model_upstream,
        nodes,
        containers: docker,
        byoc: byoc()?,
        browser_url: None,
        // celld has no Secrets Store: the cell's shim stands in (seam 12)
        secrets: if celld { devstack::Secrets::Shim } else { devstack::Secrets::Store },
    };
    // preview cards: wrangler's workerd has Browser Rendering's local mode;
    // on celld, which has no `browser` binding, the renderer shoots them
    let renderer = match celld {
        true => Some(dev_renderer(&fleet, lan.as_ref().map(lan::Lan::renderer_defaults))?),
        false => None,
    };
    fleet.browser_url = renderer.as_ref().map(|(r, _)| r.url.clone());
    // its secrets go to wrangler's local store under cell/.wrangler/state
    fleet.configure(&tools, &devstack::cell_dir())?;
    // the agents' Worker runs beside it, as a deployment runs it, bound to
    // the same host secret
    devstack::AgentFleet { fragment_api: format!("http://127.0.0.1:{port}"), agent_url: format!("http://127.0.0.1:{port}"), test_hooks: false }
        .configure(&devstack::agent_dir(), &fleet)?;
    let opts = devstack::NodeOptions {
        project: devstack::cell_dir(),
        port,
        with: vec![devstack::agent_dir()],
        log_dir: devstack::repo_root().join("target/devstack"),
        // wrangler's own debug logs, when asked for
        node_logs: std::env::var_os("FRAGMENT_NODE_LOGS").is_some(),
        // in the terminal's group: Ctrl-C stops it with xtask
        own_group: false,
    };
    enum Running {
        Wrangler(devstack::Node),
        Celld(devstack::celld::CelldNode),
    }
    let (running, base, log, took) = if celld {
        let tools = devstack::celld::CelldTools::locate()?;
        let copts = devstack::celld::CelldOptions {
            project: opts.project.clone(),
            with: opts.with.clone(),
            port: opts.port,
            log_dir: opts.log_dir.clone(),
            // the cell's own fetches (Dex's discovery, keys, token) trust the LAN's root
            extra_ca_file: lan.as_ref().map(|l| l.ca_file.clone()),
            // in the terminal's group: Ctrl-C stops it with xtask
            own_group: false,
        };
        let (n, took) = devstack::celld::CelldNode::start(&tools, &copts)?;
        let (base, log) = (n.base.clone(), n.log.clone());
        (Running::Celld(n), base, log, took)
    } else {
        let (n, took) = devstack::Node::start(&tools, &opts)?;
        let (base, log) = (n.base.clone(), n.log.clone());
        (Running::Wrangler(n), base, log, took)
    };
    println!("fragment dev: {base} (ready in {took:.1?}; Ctrl-C stops it)");
    println!("  runtime:      {}", if celld { "celld (CELLD_BIN)" } else { "wrangler dev (workerd)" });
    println!("  node log:     {}", log.display());
    match &lan {
        Some(l) => lan::banner(l).iter().for_each(|line| println!("{line}")),
        None => println!("  fragments:    http://<label>--<username>.fragment.localhost:{port}/"),
    }
    println!("  agents:       {base}/api/agents (beside it; signed)");
    println!("  code.storage: {} ({})", store.url, store.label);
    match std::env::var("FRAGMENT_MODEL_URL") {
        Ok(u) => println!("  models:       {u} (a self-hosted model server)"),
        Err(_) => println!("  models:       {} (the Workers AI fake: echoes, never a real model)", ai.url),
    }
    match (std::env::var(devstack::sandcastle::NODES_FILE_VAR), docker) {
        (Ok(f), _) => println!("  computers:    the sandcastle nodes {f} lists, new ones on its image {computer_image}{}", if byoc()? { ", and people's own (FRAGMENT_BYOC=on: sandcastle-node pair <platform>)" } else { "" }),
        (Err(_), true) => println!("  computers:    local Docker (the stub image)"),
        (Err(_), false) => println!("  computers:    none (no {}, and {})", devstack::sandcastle::NODES_FILE_VAR, if celld { "celld runs no containers" } else { "Docker is not reachable" }),
    }

    match &renderer {
        Some((r, said)) => println!("  cards:        {said} (the renderer at {})", r.url),
        None => println!("  cards:        Browser Rendering's local mode (wrangler's Chrome for Testing)"),
    }
    match &lan {
        Some(l) => {
            println!("  sign-in:      {}/ via {signin_label}", l.settings.platform_url());
            println!("  the CLI:      FRAGMENT_HOST={} fragment login   (it trusts the root once this machine does)", l.settings.platform_url());
        }
        None => {
            println!("  sign-in:      http://127.0.0.1:{port}/ via {signin_label}");
            println!("  try one:      cargo xtask try todo | inbox   (in another terminal)");
        }
    }
    let status = match running {
        Running::Wrangler(n) => n.wait()?,
        Running::Celld(n) => n.wait()?,
    };
    println!("the node exited: {status}");
    // the code store started with the stack stops with it
    if let Some(m) = store.macrofiche {
        m.stop()?;
    }
    Ok(())
}

/// The dev stack's renderer on celld (docs/self-host.md, seam 7): the
/// pinned chrome-headless-shell (`target/tools`, fetched on first use, or
/// `FRAGMENT_BROWSER_ZIP`), its pages let reach the fleet's fragments'
/// origins alone, served on this box at the platform URL's port (the node,
/// or an edge before it) unless `FRAGMENT_BROWSER_UPSTREAM` (`ip:port`)
/// names where. `FRAGMENT_BROWSER_CA_FILE` names a private CA's PEM the
/// browser trusts, for fragments on https under it. On the LAN (`lan`),
/// each defaults to the front door's: its address and the root it serves
/// under. With it, what the banner says the browser is.
fn dev_renderer(fleet: &devstack::Fleet, lan: Option<(std::net::SocketAddr, PathBuf)>) -> Result<(devstack::rendering::Rendering, String)> {
    let suffix = fleet.host_suffix.as_deref().context("cards need fragments on hosts of their own (a host suffix)")?;
    let platform = fleet.platform_url.as_deref().context("cards need the platform's URL: its scheme and port are the fragments'")?;
    let upstream = match std::env::var("FRAGMENT_BROWSER_UPSTREAM") {
        Ok(u) => Some(u.parse().with_context(|| format!("FRAGMENT_BROWSER_UPSTREAM is ip:port, not {u:?}"))?),
        Err(_) => lan.as_ref().map(|(door, _)| *door),
    };
    let origins = devstack::rendering::Origins::of_platform(suffix, platform, upstream).map_err(anyhow::Error::msg)?;
    let browser = devstack::browser::locate(&devstack::repo_root().join(devstack::TOOLS_DIR))?;
    let opts = devstack::rendering::Options {
        browser: browser.bin,
        state: devstack::repo_root().join(format!("target/devstack/renderer-{}", origins.port)),
        origins,
        ca_file: std::env::var_os("FRAGMENT_BROWSER_CA_FILE").map(PathBuf::from).or(lan.map(|(_, ca)| ca)),
        port: 0,
    };
    Ok((devstack::rendering::Rendering::start(opts)?, browser.version))
}

/// The dev stack's code store, as it runs (docs/self-host.md, seam 5).
struct DevStore {
    url: String,
    org: String,
    key_pem: String,
    /// What the banner says it is.
    label: String,
    /// The fake, serving on this process's threads.
    _fake: Option<fragment_fakes::codestorage::CodeStorage>,
    /// macrofiche, when the stack started it.
    macrofiche: Option<devstack::codestore::Macrofiche>,
}

/// The code store by configuration (`devstack::codestore::CodeStore::from_env`):
/// the fake on its port, its repos in `target/devstack/codestorage.json`
/// (the default); a store already running (`CODESTORAGE_API_URL`,
/// `CODESTORAGE_ORG`, `CODESTORAGE_PRIVATE_KEY_FILE`), which `--clean`
/// never touches; or macrofiche (`MACROFICHE_BIN`) on the fake's port, its
/// repos in `target/devstack/macrofiche`, its org key made there on first
/// run. A cell remembers each fragment's repo: switching stores takes
/// `--clean`.
fn dev_codestore(port: u16, clean: bool) -> Result<DevStore> {
    use devstack::codestore::{CodeStore, Macrofiche, MacroficheOptions};
    let dir = devstack::repo_root().join("target/devstack");
    match CodeStore::from_env()? {
        CodeStore::Fake => {
            let state = dir.join("codestorage.json");
            if clean {
                let _ = std::fs::remove_file(&state);
            }
            let key = devstack::dev_secret("codestorage-org-key.pem", fragment_fakes::codestorage::generate_org_key_pem)?;
            let fake = fragment_fakes::codestorage::CodeStorage::start(fragment_fakes::codestorage::Options {
                org: DEV_ORG.into(),
                org_key_pem: Some(key.clone()),
                state_file: Some(state),
                port: port + DEV_CODESTORAGE_OFFSET,
                ..Default::default()
            })?;
            Ok(DevStore { url: fake.url.clone(), org: DEV_ORG.into(), key_pem: key, label: "the fake".into(), _fake: Some(fake), macrofiche: None })
        }
        CodeStore::External(x) => Ok(DevStore { url: x.url, org: x.org, key_pem: x.key_pem, label: "an external store: CODESTORAGE_API_URL".into(), _fake: None, macrofiche: None }),
        CodeStore::Macrofiche(bin) => {
            let data = dir.join("macrofiche");
            if clean {
                let _ = std::fs::remove_dir_all(&data);
            }
            let key = devstack::dev_secret("macrofiche-org-key.pem", fragment_fakes::codestorage::generate_org_key_pem)?;
            let opts = MacroficheOptions { dir: data.clone(), port: port + DEV_CODESTORAGE_OFFSET, org: DEV_ORG.into(), key_pem: key.clone(), log_dir: dir };
            let m = Macrofiche::start(&bin, &opts)?;
            let label = format!("macrofiche {}, its repos in {}, its log {}", bin.display(), data.display(), m.log.display());
            Ok(DevStore { url: m.url.clone(), org: DEV_ORG.into(), key_pem: key, label, _fake: None, macrofiche: Some(m) })
        }
    }
}

/// WorkOS for dev: a real environment when its files are named
/// (`WORKOS_CLIENT_ID_FILE`, `WORKOS_API_KEY_FILE`), with its AuthKit for
/// sign-in when its domain and OAuth application are named too
/// (`WORKOS_AUTHKIT_DOMAIN`, `WORKOS_OAUTH_CLIENT_ID_FILE`,
/// `WORKOS_OAUTH_CLIENT_SECRET_FILE`; the application's redirect URIs must
/// include `http://127.0.0.1:<port>/auth/callback`); else the fake, Pipes
/// and AuthKit both. Answers Pipes' settings, and AuthKit's as sign-in.
fn dev_workos(port: u16, read: ReadFile) -> Result<(devstack::WorkOsVars, AuthKit, Option<fragment_fakes::workos::WorkOs>)> {
    let var = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
    match (read("WORKOS_CLIENT_ID_FILE")?, read("WORKOS_API_KEY_FILE")?) {
        (Some(client_id), Some(api_key)) => {
            let authkit = match (var("WORKOS_AUTHKIT_DOMAIN"), read("WORKOS_OAUTH_CLIENT_ID_FILE")?, read("WORKOS_OAUTH_CLIENT_SECRET_FILE")?) {
                (Some(domain), Some(app), Some(secret)) => Some((devstack::OidcVars::authkit(&format!("https://{domain}"), &app, Some(&secret)), format!("WorkOS AuthKit at https://{domain}"))),
                (None, None, None) => None,
                _ => bail!("AuthKit's sign-in names WORKOS_AUTHKIT_DOMAIN, WORKOS_OAUTH_CLIENT_ID_FILE and WORKOS_OAUTH_CLIENT_SECRET_FILE together"),
            };
            Ok((devstack::WorkOsVars { client_id, api_key, api_url: None }, authkit, None))
        }
        _ => {
            let fake = fragment_fakes::workos::WorkOs::start_on(port + DEV_WORKOS_OFFSET, port + DEV_AUTHKIT_OFFSET, DEV_WORKOS_CLIENT, DEV_WORKOS_KEY, DEV_WORKOS_APP, DEV_WORKOS_APP_SECRET)?;
            let authkit = devstack::OidcVars::authkit(&fake.authkit.url, DEV_WORKOS_APP, Some(DEV_WORKOS_APP_SECRET));
            let label = format!("{} (the WorkOS fake's AuthKit: any email)", fake.authkit.url);
            Ok((devstack::WorkOsVars { client_id: DEV_WORKOS_CLIENT.into(), api_key: DEV_WORKOS_KEY.into(), api_url: Some(fake.url.clone()) }, Some((authkit, label)), Some(fake)))
        }
    }
}

/// Sign-in for dev (docs/self-host.md, seam 4), one OpenID Connect
/// provider: a real one (`FRAGMENT_OIDC_ISSUER`, with
/// `FRAGMENT_OIDC_CLIENT_ID`, the secret's file
/// `FRAGMENT_OIDC_CLIENT_SECRET_FILE`, and optionally `FRAGMENT_OIDC_SCOPES`,
/// `FRAGMENT_OIDC_CLAIMS`, `FRAGMENT_OIDC_AUTH`, `FRAGMENT_OIDC_KEYED_AS`;
/// its redirect URIs must include `http://127.0.0.1:<port>/auth/callback`),
/// else with `FRAGMENT_SIGNIN=oidc` the strict fake (any email or
/// username), else WorkOS's AuthKit (`authkit`: the fake's, or a real
/// environment's). Answers the settings, and how the summary names them.
fn dev_signin(port: u16, read: ReadFile, authkit: AuthKit) -> Result<(devstack::OidcVars, String, Option<fragment_fakes::oidc::Oidc>)> {
    let var = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
    if let Some(issuer) = var("FRAGMENT_OIDC_ISSUER") {
        let vars = devstack::OidcVars {
            client_id: var("FRAGMENT_OIDC_CLIENT_ID").context("FRAGMENT_OIDC_ISSUER needs FRAGMENT_OIDC_CLIENT_ID")?,
            client_secret: read("FRAGMENT_OIDC_CLIENT_SECRET_FILE")?,
            scopes: var("FRAGMENT_OIDC_SCOPES"),
            claims: var("FRAGMENT_OIDC_CLAIMS"),
            auth: var("FRAGMENT_OIDC_AUTH"),
            // dev's people are dev's: beside WorkOS's Pipes, they are the issuer's unless said
            keyed_as: var("FRAGMENT_OIDC_KEYED_AS").or_else(|| Some(issuer.clone())),
            issuer,
        };
        let label = format!("the OpenID Connect provider {}", vars.issuer);
        return Ok((vars, label, None));
    }
    match var("FRAGMENT_SIGNIN").as_deref() {
        None | Some("workos") => {
            let (vars, label) = authkit.context("a real WorkOS environment signs people in through its AuthKit: name WORKOS_AUTHKIT_DOMAIN, WORKOS_OAUTH_CLIENT_ID_FILE and WORKOS_OAUTH_CLIENT_SECRET_FILE")?;
            Ok((vars, label, None))
        }
        Some("oidc") => {
            let fake = fragment_fakes::oidc::Oidc::start_on(port + DEV_OIDC_OFFSET, DEV_OIDC_CLIENT, DEV_OIDC_SECRET)?;
            let vars = devstack::OidcVars {
                issuer: fake.url.clone(),
                client_id: DEV_OIDC_CLIENT.into(),
                client_secret: Some(DEV_OIDC_SECRET.into()),
                scopes: None,
                claims: None,
                auth: None,
                keyed_as: Some(fake.url.clone()),
            };
            let label = format!("{} (the OpenID Connect fake)", fake.url);
            Ok((vars, label, Some(fake)))
        }
        Some(other) => bail!("FRAGMENT_SIGNIN is workos or oidc, not {other:?}"),
    }
}

/// The self-hosted lane's settings for dev (docs/self-host.md), each from
/// the environment, secrets from files: a model server
/// (`FRAGMENT_MODEL_URL`, `FRAGMENT_MODELS`, `FRAGMENT_MODEL_KEY_FILE`) and
/// the sandcastle nodes computers are placed on (`FRAGMENT_NODES_FILE`:
/// `FRAGMENT_NODES`' JSON, each node with its `secret_file`).
type ReadFile<'a> = &'a dyn Fn(&str) -> Result<Option<String>>;

/// AuthKit's settings as sign-in, and how the dev summary names them.
type AuthKit = Option<(devstack::OidcVars, String)>;

fn self_host(read: ReadFile) -> Result<(Option<devstack::ModelUpstreamVars>, Option<devstack::sandcastle::NodesVars>)> {
    let var = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
    let model = match var("FRAGMENT_MODEL_URL") {
        Some(url) => Some(devstack::ModelUpstreamVars {
            url,
            models: var("FRAGMENT_MODELS").context("FRAGMENT_MODEL_URL needs FRAGMENT_MODELS: {\"<catalog id>\": \"<the server's model>\"}")?,
            key: read("FRAGMENT_MODEL_KEY_FILE")?,
        }),
        None => None,
    };
    if let Some(gone) = ["FRAGMENT_NODE_URL", "FRAGMENT_NODE_SECRET_FILE", "FRAGMENT_NODE_IMAGES"].into_iter().find(|k| var(k).is_some()) {
        bail!("{gone} is gone: {} names a file listing the nodes (docs/self-host.md, Running it)", devstack::sandcastle::NODES_FILE_VAR);
    }
    let nodes = match var(devstack::sandcastle::NODES_FILE_VAR) {
        Some(file) => Some(devstack::sandcastle::NodesFile::read(std::path::Path::new(&file))?),
        None => None,
    };
    Ok((model, nodes))
}

/// The image a new computer is pinned to: `FRAGMENT_COMPUTER_IMAGE`, one of
/// the images the node list names (`hermes`, for a stack whose agents are
/// Hermes: docs/self-host.md, S3), else the stub. Local Docker runs the
/// stub alone.
fn dev_computer_image(nodes: Option<&devstack::sandcastle::NodesVars>) -> Result<String> {
    let Some(image) = std::env::var("FRAGMENT_COMPUTER_IMAGE").ok().filter(|v| !v.trim().is_empty()) else {
        return Ok("stub".into());
    };
    let Some(nodes) = nodes else {
        bail!("FRAGMENT_COMPUTER_IMAGE needs {}, whose images it names: local Docker runs the stub alone", devstack::sandcastle::NODES_FILE_VAR);
    };
    let listed = fragment_core::placement::Nodes::parse(&nodes.nodes, fragment_core::placement::Byoc::On).map_err(|e| anyhow::anyhow!("{e}"))?;
    if !listed.image_names().any(|n| n == image) {
        bail!("FRAGMENT_COMPUTER_IMAGE {image:?} is none of the images {} names ({})", devstack::sandcastle::NODES_FILE_VAR, listed.image_names().collect::<Vec<_>>().join(", "));
    }
    Ok(image)
}

/// `FRAGMENT_BYOC` (on or off; off unless said): whether the stack's
/// people may pair sandcastle nodes of their own (docs/self-host.md, seam 2,
/// Bring your own computer). On needs `FRAGMENT_NODES_FILE`, whose images
/// such a node runs.
fn byoc() -> Result<bool> {
    let v = std::env::var("FRAGMENT_BYOC").ok();
    Ok(fragment_core::placement::Byoc::parse(v.as_deref()).map_err(anyhow::Error::msg)? == fragment_core::placement::Byoc::On)
}

/// The templates `try` scaffolds.
const TRY_TEMPLATES: [&str; 3] = ["todo", "inbox", "notes"];

fn try_template(args: &[String]) -> Result<()> {
    let usage = || format!("usage: cargo xtask try <{}> [name]", TRY_TEMPLATES.join("|"));
    let tpl = args.first().ok_or_else(|| anyhow::anyhow!(usage()))?;
    if !TRY_TEMPLATES.contains(&tpl.as_str()) {
        bail!("{}", usage());
    }
    let port = dev_port()?;
    if TcpStream::connect(("127.0.0.1", port)).is_err() {
        bail!("nothing answers on :{port}: start `cargo xtask dev` in another terminal, wait for `ready`, then try again");
    }
    let root = devstack::repo_root();
    run(Command::new("cargo").args(["build", "--quiet", "--manifest-path"]).arg(root.join("Cargo.toml")).args(["-p", "fragment-cli"]))?;
    let cli = root.join("target/debug/fragment");
    let host = format!("http://127.0.0.1:{port}");
    let name = args.get(1).cloned().unwrap_or_else(|| format!("{tpl}-{}", devstack::random_hex(2)));
    let dir = root.join("target/devstack/try");
    std::fs::create_dir_all(&dir)?;
    // your CLI key must be a person's on the dev fleet (phase 4: people sign in)
    let who = Command::new(&cli).args(["whoami"]).env("FRAGMENT_HOST", &host).output()?;
    if !who.status.success() {
        bail!(
            "your CLI key is no one's on the dev stack yet. Once:\n  FRAGMENT_HOST={host} {} login\n(the dev stack's sign-in fakes take any email)",
            cli.display()
        );
    }
    let out = Command::new(&cli).args(["init", &name, "--template", tpl]).env("FRAGMENT_HOST", &host).current_dir(&dir).output()?;
    let text = String::from_utf8_lossy(&out.stdout);
    print!("{text}");
    eprint!("{}", String::from_utf8_lossy(&out.stderr));
    if !out.status.success() {
        bail!("fragment init failed (a name already taken? pass another: cargo xtask try {tpl} <name>)");
    }
    let line = |prefix: &str| text.lines().find_map(|l| l.strip_prefix(prefix)).map(str::trim).unwrap_or("").to_string();
    let alias = format!("alias fragment='FRAGMENT_HOST={host} {}'", cli.display());
    println!("\nnext:");
    println!("  open it      {}", line("share link:"));
    if tpl == "inbox" {
        let hook = line("webhook URL:");
        println!("  post to it   curl -s -X POST '{hook}' -H 'content-type: application/json' -d '{{\"payload\":{{\"text\":\"hi\"}}}}'");
        println!("  or a page    curl -s -X POST '{hook}' -H 'content-type: application/json' -d '{{\"payload\":{{\"url\":\"https://example.com\"}}}}'");
    }
    println!("  the CLI      {alias}");
    println!("               fragment runs {name} | fragment channel {name} | fragment call {name} list");
    println!("  change it    edit {}, then: fragment deploy {name} --dir {}", dir.join(&name).display(), dir.join(&name).display());
    Ok(())
}

fn e2e(args: &[String]) -> Result<()> {
    // the hosted lane: a branch deployment, read from its config; it needs
    // the CLI and the suite, and no Worker built here (a dry run, the suite alone)
    if args.iter().any(|a| a == "--hosted") {
        let suite_args = deploy::hosted_e2e_args(args)?;
        match suite_args.iter().any(|a| a == "--dry-run") {
            true => build_suite()?,
            false => build_native()?,
        }
        return run(Command::new(devstack::repo_root().join(E2E_BIN)).args(suite_args));
    }
    // CI's split: `--build-only` builds (and the cache is saved after it,
    // so a red run still saves), then `--no-build` runs what it left
    let build_only = args.iter().any(|a| a == "--build-only");
    let no_build = args.iter().any(|a| a == "--no-build");
    if build_only && no_build {
        bail!("--build-only and --no-build are two steps of one run, not one");
    }
    let suite_args: Vec<&String> = args.iter().filter(|a| *a != "--build-only" && *a != "--no-build").collect();
    if build_only && !suite_args.is_empty() {
        bail!("--build-only builds what every run needs, and takes no suite arguments");
    }
    // the pinned Node and node_modules, before the build; the suite finds
    // them in place
    devstack::Tools::locate()?;
    if !no_build {
        build_e2e()?;
    }
    if build_only {
        return Ok(());
    }
    run(Command::new(devstack::repo_root().join(E2E_BIN)).args(suite_args))
}

/// The suite alone, for this machine.
fn build_suite() -> Result<()> {
    let manifest = devstack::repo_root().join("Cargo.toml");
    run(Command::new("cargo").args(["build", "--quiet", "--release", "--manifest-path"]).arg(&manifest).args(["-p", "fragment-e2e"]))
}

/// The suite, as `build_e2e` leaves it.
const E2E_BIN: &str = "target/release/fragment-e2e";
/// What the e2e runs: the workers (build.rs: in parallel once worker-build
/// has its tools), the CLI (the e2e drives it too) and the suite beside
/// them, and the computer images the node's boot builds, built ahead
/// beside them all so its build finds every layer cached. A node on celld
/// builds none (its computers are a sandcastle node's: docs/self-host.md,
/// seam 2), so neither does its run, nor needs Docker.
fn build_e2e() -> Result<()> {
    let t0 = std::time::Instant::now();
    let celld = std::env::var("FRAGMENT_E2E_RUNTIME").as_deref() == Ok("celld");
    let built = std::thread::scope(|s| {
        let images = s.spawn(move || if celld { Ok(()) } else { build::images() });
        let native = s.spawn(build_native);
        let workers = build::workers();
        workers.and(native.join().expect("the native build does not panic")).and(images.join().expect("the images' build does not panic"))
    });
    println!("built the workers, the CLI, the suite and the images in {:.1?}", t0.elapsed());
    built
}

/// The CLI and the suite, for this machine.
fn build_native() -> Result<()> {
    let t0 = std::time::Instant::now();
    let manifest = devstack::repo_root().join("Cargo.toml");
    run(Command::new("cargo").args(["build", "--quiet", "--release", "--manifest-path"]).arg(&manifest).args(["-p", "fragment-cli"]))?;
    build_suite()?;
    println!("built the CLI and the suite in {:.1?}", t0.elapsed());
    Ok(())
}

/// A merge conflict's markers (git's diff3 style too), at the start of a
/// line: none may be committed.
fn conflict_marker(line: &str) -> bool {
    ["<<<<<<< ", "||||||| ", ">>>>>>> "].iter().any(|m| line.starts_with(m)) || line == "======="
}

/// Fails on a tracked text file holding conflict markers: a rebase that
/// committed one compiles when it lands in docs or a lockfile.
fn no_conflict_markers(root: &Path) -> Result<()> {
    let out = Command::new("git").args(["ls-files", "-z"]).current_dir(root).output().context("git ls-files")?;
    anyhow::ensure!(out.status.success(), "git ls-files: {}", String::from_utf8_lossy(&out.stderr));
    let mut found = vec![];
    for path in out.stdout.split(|b| *b == 0).filter(|p| !p.is_empty()) {
        let path = String::from_utf8_lossy(path).into_owned();
        // a file that is not text (a font, an image) holds no markers
        let Ok(text) = std::fs::read_to_string(root.join(&path)) else { continue };
        let at = text.lines().enumerate().filter(|(_, l)| conflict_marker(l)).map(|(i, _)| format!("{path}:{}", i + 1));
        found.extend(at);
    }
    anyhow::ensure!(found.is_empty(), "conflict markers are committed:\n{}", found.join("\n"));
    Ok(())
}

/// Every first-party JavaScript file parses, on the pinned Node (fetched
/// into target/tools on first use; no node_modules needed): seconds here,
/// where the e2e would find it most of an hour in (xtask/src/js_syntax.rs).
fn javascript_parses(root: &Path) -> Result<()> {
    let node = devstack::node::locate(&root.join(devstack::TOOLS_DIR))?;
    let started = std::time::Instant::now();
    let files = js_syntax::files(root)?;
    js_syntax::check(&node, &root.join(devstack::CACHE_DIR), root, &files)?;
    println!("JavaScript: {} files parse (node --check on Node {}, {:.1?})", files.len(), node.release, started.elapsed());
    Ok(())
}

fn check() -> Result<()> {
    let root = devstack::repo_root();
    no_conflict_markers(&root)?;
    let read = |path: &str| std::fs::read_to_string(root.join(path)).with_context(|| format!("read {path}"));
    let copies = ["cli/GUIDE.md", "README.md", "cell/shell/shell.js"].map(|path| read(path).map(|text| (path, text)));
    let copies: Vec<(&str, String)> = copies.into_iter().collect::<Result<_>>()?;
    skill_installs_release(&read("cli/SKILL.md")?, &read(".github/workflows/release.yml")?, &copies)?;
    javascript_parses(&root)?;
    run(Command::new("cargo").args(["test", "--workspace", "--all-features"]).current_dir(&root))?;
    run(Command::new("cargo")
        .args(["clippy", "--workspace", "--all-targets", "--all-features", "--", "-D", "warnings"])
        .current_dir(&root))?;
    run(Command::new("cargo")
        .args(["clippy", "--target", "wasm32-unknown-unknown", "--", "-D", "warnings"])
        .current_dir(Path::new(&devstack::cell_dir())))?;
    run(Command::new("cargo")
        .args(["clippy", "--target", "wasm32-unknown-unknown", "--", "-D", "warnings"])
        .current_dir(Path::new(&devstack::agent_dir())))
}

/// Where the one-line install fetches the CLI from.
const RELEASES: &str = "https://github.com/futurepaul/fragment/releases/latest/download/";
/// Each target the release builds, and its asset's name: the `uname -s` and
/// `uname -m` of the machines it runs on, which the install command names.
const RELEASE_ASSETS: [(&str, &str); 3] =
    [("aarch64-apple-darwin", "Darwin-arm64"), ("x86_64-apple-darwin", "Darwin-x86_64"), ("x86_64-unknown-linux-musl", "Linux-x86_64")];

/// cli/SKILL.md (`fragment skill`) is a skill, and its one-line install,
/// which the shell's settings, GUIDE.md, and the README copy, fetches
/// the latest release's `fragment-$(uname -s)-$(uname -m).tar.gz`: the
/// release workflow (`.github/workflows/release.yml`) must build exactly
/// those assets, each a tarball of `fragment` alone. Otherwise a renamed
/// asset or a stale copy breaks the install, and nothing says so until
/// someone runs it.
fn skill_installs_release(skill: &str, workflow: &str, copies: &[(&str, String)]) -> Result<()> {
    anyhow::ensure!(skill.starts_with("---\nname: fragment\ndescription: "), "cli/SKILL.md opens with a skill's name and description");
    let install = skill.lines().map(str::trim).find(|l| l.contains(RELEASES)).context("cli/SKILL.md shows no install command")?;
    let fetched = format!("{RELEASES}fragment-$(uname -s)-$(uname -m).tar.gz | tar -xzf - -C ~/.local/bin");
    anyhow::ensure!(install.ends_with(&fetched), "the install command unpacks {fetched}, not: {install}");
    for (path, text) in copies {
        anyhow::ensure!(text.contains(install), "{path} does not show cli/SKILL.md's install command: {install}");
    }
    let field = |key: &str| workflow.lines().filter_map(|l| l.trim().strip_prefix(key)).collect::<Vec<_>>();
    let built: Vec<(&str, &str)> = field("target: ").into_iter().zip(field("asset: ")).collect();
    anyhow::ensure!(built == RELEASE_ASSETS, "the release builds {built:?}, not the assets the install fetches: {RELEASE_ASSETS:?}");
    let packed = r#"tar -czf "$RUNNER_TEMP/fragment-${{ matrix.asset }}.tar.gz" -C "target/${{ matrix.target }}/release" fragment"#;
    anyhow::ensure!(workflow.contains(packed), "each release asset is a tarball of `fragment` alone: {packed}");
    Ok(())
}

/// `cargo xtask node-images [--tag <tag>] [--engine <dir>]`: the computer
/// images a sandcastle node on this machine runs (docs/self-host.md,
/// Running it): the cell's (the stub) and our Hermes image, built for this
/// machine's architecture into local Docker as `fragment-<name>:<tag>`
/// (default `local`), then loaded into the engine whose sockets are in
/// `<dir>` (default `SANDCASTLE_ENGINE_DIR`, else /var/lib/sandcastle). It
/// prints the `images` a node list (`FRAGMENT_NODES_FILE`) names them by.
/// Run with Docker reachable (the docker group) and the engine's socket
/// this user's.
fn node_images(args: &[String]) -> Result<()> {
    let mut tag = "local".to_string();
    let mut engine = std::env::var_os("SANDCASTLE_ENGINE_DIR").map(std::path::PathBuf::from).unwrap_or_else(|| "/var/lib/sandcastle".into());
    let mut rest = args.iter();
    // bounded: the arguments
    while let Some(a) = rest.next() {
        match a.as_str() {
            "--tag" => tag = rest.next().context("--tag names a tag")?.clone(),
            "--engine" => engine = rest.next().context("--engine names the engine's directory")?.into(),
            other => bail!("usage: cargo xtask node-images [--tag <tag>] [--engine <dir>], not {other}"),
        }
    }
    anyhow::ensure!(!tag.is_empty() && tag.len() <= 64 && tag.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-' || b == b'_'), "a tag is 1 to 64 of A-Z, a-z, 0-9, '.', '-' and '_', not {tag:?}");
    let mut images = devstack::sandcastle::cell_images(&devstack::cell_dir())?;
    images.retain(|name, _| name == "stub");
    images.insert("hermes".into(), devstack::sandcastle::hermes_image(None));
    let t0 = std::time::Instant::now();
    let built = devstack::sandcastle::build_images(&images, &tag)?;
    println!("built {} in {:.1?}", built.iter().map(|(_, t)| t.as_str()).collect::<Vec<_>>().join(", "), t0.elapsed());
    let tags: Vec<String> = built.iter().map(|(_, t)| t.clone()).collect();
    devstack::sandcastle::load_images(&engine, &tags, &devstack::repo_root().join("target/devstack/images"))?;
    let listed: serde_json::Map<String, serde_json::Value> = built.iter().map(|(name, t)| (name.clone(), serde_json::json!(format!("docker.io/library/{t}")))).collect();
    println!("the node list's images (FRAGMENT_NODES_FILE), for {}:", std::env::consts::ARCH);
    println!("  \"images\": {}", serde_json::Value::Object(listed));
    Ok(())
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("build") => build(),
        Some("dev") => dev(&args[1..]),
        Some("try") => try_template(&args[1..]),
        Some("e2e") => e2e(&args[1..]),
        Some("e2e-summary") => summary::e2e_summary(&args[1..]),
        Some("check") => check(),
        Some("secret") => secret::secret(&args[1..]),
        Some("node-images") => node_images(&args[1..]),
        Some("deploy") => deploy::deploy(&args[1..]),
        Some("teardown") => deploy::teardown(&args[1..]),
        _ => bail!("usage: cargo xtask build | dev [--clean] [--runtime wrangler|celld] [--lan] | try <template> [name] | e2e [--build-only | --no-build] [--only | --except <section>[,...] | --shard <k>/<n>] [--summary <file>] [--rehearse] | e2e --hosted --config <file> --branch <name> [--dry-run | --sweep] | e2e-summary <dir> | check | secret set <name> | gen <name> | list --config <file> | node-images [--tag <tag>] [--engine <dir>] | deploy --config <file> [--branch <name>] | teardown --config <file> --branch <name>"),
    }
}

#[cfg(test)]
mod tests {
    /// Git's markers, the diff3 base's included, are caught at a line's
    /// start; text that only mentions one, or a longer rule, is not.
    #[test]
    fn conflict_markers_are_caught_at_a_line_start() {
        for line in ["<<<<<<< HEAD", "||||||| parent of e8dd007 (e2e: …)", "=======", ">>>>>>> e8dd007 (e2e: …)"] {
            assert!(super::conflict_marker(line), "{line}");
        }
        for line in ["  <<<<<<< indented", "a ======= b", "========", "the `<<<<<<< ` marker", ">>>>>>>"] {
            assert!(!super::conflict_marker(line), "{line}");
        }
    }

    /// The repo's own skill, release workflow, and copies agree; a renamed
    /// asset, a stale copy, or a skill without its frontmatter does not.
    #[test]
    fn the_install_fetches_what_the_release_builds() {
        let read = |path: &str| std::fs::read_to_string(super::devstack::repo_root().join(path)).unwrap();
        let (skill, workflow) = (read("cli/SKILL.md"), read(".github/workflows/release.yml"));
        let copies = [("README.md", read("README.md"))];
        assert!(super::skill_installs_release(&skill, &workflow, &copies).is_ok());
        assert!(super::skill_installs_release(&skill, &workflow.replace("asset: Darwin-arm64", "asset: Darwin-aarch64"), &copies).is_err());
        assert!(super::skill_installs_release(&skill, &workflow, &[("README.md", read("README.md").replace("uname -m", "arch"))]).is_err());
        assert!(super::skill_installs_release(&skill.replacen("name: fragment", "title: fragment", 1), &workflow, &copies).is_err());
    }

    #[test]
    fn a_failed_command_never_shows_its_environment() {
        let mut cmd = std::process::Command::new("wrangler");
        cmd.args(["dev", "--port", "8790"]).env("FRAGMENT_KEYS_HOST_SECRET", "not-a-real-secret");
        assert_eq!(super::shown(&cmd), "wrangler dev --port 8790");
    }
}
