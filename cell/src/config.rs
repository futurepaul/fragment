//! The fleet's settings, from Worker variables (`cell/.dev.vars` in dev,
//! rendered `vars` at deploy), built once per isolate (`CONFIG`). Nothing about a fleet is a constant in code
//! (ROADMAP decision 13): the hostname suffix and the code.storage org
//! arrive here. The fleet's secrets do not: the host secret, the
//! code.storage key, WorkOS's client id and API key, and the OpenID
//! Connect client's secret are Secrets Store bindings, read only by
//! keys.rs.

use std::sync::OnceLock;

use fragment_proto::ledger::Plan;
use fragment_proto::{flat_name, from_flat_name, ErrorCode};
use worker::Env;

use crate::error::{CellError, CellResult};

pub struct CodeStorageConfig {
    pub org: String,
    /// The API base, e.g. `https://api.<org>.code.storage`.
    pub api: String,
    /// `CODESTORAGE_REPO_PREFIX`: what this deployment's repos are named
    /// with first (a branch deployment's `<branch>--`), so deployments that
    /// share an org never share a repo.
    pub repo_prefix: String,
}

/// WorkOS (decisions 22 and 37): fragment's own environment, for Pipes'
/// connections alone, configured when its client id is bound
/// (`secrets_store::WORKOS_CLIENT`; keys.rs reads it, and its API key, as
/// `keys::workos`). Its people sign in through AuthKit's OpenID Connect
/// provider, one issuer like any other (`OidcConfig`).
pub struct WorkOsConfig {
    /// `WORKOS_API_URL` (default https://api.workos.com; dev and the e2e: the fake).
    pub api: String,
}

/// Sign-in (docs/self-host.md, seam 4): one OpenID Connect provider, any
/// of them: WorkOS AuthKit (an OAuth application's), Keycloak, Authentik,
/// Dex, ADFS, Entra, Okta. A person is keyed by `(keyed_as, sub)`. Its
/// client secret, when it has one, is the secret bound as
/// `OIDC_CLIENT_SECRET`, read only by keys.rs.
pub struct OidcConfig {
    /// `FRAGMENT_OIDC_ISSUER`: the provider's issuer, exactly as its
    /// id_tokens' `iss` says it (its metadata is at
    /// `<issuer>/.well-known/openid-configuration`).
    pub issuer: String,
    /// `FRAGMENT_OIDC_KEYED_AS`: the issuer people are keyed under (default
    /// `issuer`): AuthKit's people keep `workos:<client id>`, the name they
    /// had before sign-in was OpenID Connect, said as `workos`: the bound
    /// environment's (`fragment_core::oidc::keyed_as`).
    pub keyed_as: fragment_core::oidc::KeyedAs,
    /// `FRAGMENT_OIDC_CLIENT_ID`.
    pub client_id: String,
    /// `FRAGMENT_OIDC_SCOPES` (default `openid email profile`).
    pub scopes: String,
    /// `FRAGMENT_OIDC_CLAIMS`: which claims are the email, the name and the
    /// username (`fragment_core::oidc::ClaimMap`).
    pub claims: fragment_core::oidc::ClaimMap,
    /// `FRAGMENT_OIDC_AUTH`: how the client authenticates at the token
    /// endpoint (default: with a secret, the first of `client_secret_basic`
    /// and `client_secret_post` the provider lists; without, `none`).
    pub auth: Option<fragment_core::oidc::ClientAuth>,
}

/// `FRAGMENT_OIDC_*`, checked as the isolate starts: a deployment that
/// names an issuer and gets the rest wrong is refused at its first request.
/// Beside WorkOS (`workos`: its client id is bound), whom sign-in keys
/// people as is said, never defaulted (`fragment_core::oidc::keyed_as`).
fn oidc(env: &Env, workos: bool) -> Option<OidcConfig> {
    use fragment_core::oidc;
    let issuer = var(env, "FRAGMENT_OIDC_ISSUER")?;
    let fail = |e: oidc::OidcError| -> ! { panic!("{e}") };
    oidc::check_issuer(&issuer).unwrap_or_else(|e| fail(e));
    let keyed_as = oidc::keyed_as(&issuer, var(env, "FRAGMENT_OIDC_KEYED_AS").as_deref(), workos).unwrap_or_else(|e| fail(e));
    let client_id = var(env, "FRAGMENT_OIDC_CLIENT_ID").unwrap_or_else(|| panic!("FRAGMENT_OIDC_ISSUER needs FRAGMENT_OIDC_CLIENT_ID"));
    Some(OidcConfig {
        issuer,
        keyed_as,
        client_id,
        scopes: oidc::scopes(var(env, "FRAGMENT_OIDC_SCOPES").as_deref()).unwrap_or_else(|e| fail(e)),
        claims: oidc::ClaimMap::parse(var(env, "FRAGMENT_OIDC_CLAIMS").as_deref()).unwrap_or_else(|e| fail(e)),
        auth: var(env, "FRAGMENT_OIDC_AUTH").map(|a| oidc::ClientAuth::parse(&a).unwrap_or_else(|e| fail(e))),
    })
}

pub struct Config {
    codestorage: Option<CodeStorageConfig>,
    /// `FRAGMENT_HOST_SUFFIX`: fragments are served from `<label>--<username>.<suffix>`.
    /// Unset (dev without hostnames), they are served from `/f/<name>/`.
    pub host_suffix: Option<String>,
    /// `FRAGMENT_LEGACY_HOST_SUFFIX`: where fragments were served before the
    /// suffix changed (fragment.club, before fragment.boats): a fragment's
    /// host under it sends a browser to its host under the suffix. It counts
    /// only beside a suffix, and one that differs from it.
    legacy_host_suffix: Option<String>,
    /// `FRAGMENT_HOST_LABEL_SUFFIX` (`--<branch>`): a branch deployment's
    /// fragments are `<label>--<username>--<branch>.<suffix>`, beside the
    /// other branches' in one zone.
    host_label_suffix: Option<String>,
    /// `FRAGMENT_COMPUTER_IMAGE`: the image a new computer is pinned to (a
    /// name in wrangler.jsonc's `containers` images, or in `FRAGMENT_NODES`'
    /// images). Unset, the deployment makes no computers.
    pub computer_image: Option<String>,
    /// `FRAGMENT_NODES`: the sandcastle nodes computers are placed on, and
    /// their images by architecture (`fragment_core::placement`;
    /// docs/self-host.md, seam 2). Unset, computers run in the runtime's
    /// own containers (`ctx.container`).
    pub nodes: Option<fragment_core::placement::Nodes>,
    /// `FRAGMENT_COMPUTER_SNAPSHOTS=off`: computers sleep without a
    /// container snapshot and wake from their image and backup (local
    /// workerd takes no snapshots).
    pub computer_snapshots: bool,
    /// `FRAGMENT_COMPUTER_UNSAVED_MAX_MS` (the deploy config's
    /// `computers.unsaved_max_ms`): how long a computer whose sleep's save
    /// keeps failing stays awake before it sleeps unsaved (I5 of
    /// docs/explorations/pi-durable.md). Thirty minutes by default, a
    /// default for Paul to confirm (`fragment_core::computer`).
    pub computer_unsaved_max_ms: i64,
    /// `FRAGMENT_POLL_INTERVAL_S`: the webhook backstop (default 300).
    pub poll_interval_ms: i64,
    /// `FRAGMENT_EGRESS_LOCAL=allow`: jobs may fetch private and loopback
    /// addresses (dev and e2e fleets, which call local fakes). Never on a
    /// shared fleet.
    pub egress_local: bool,
    /// `FRAGMENT_BLOB_GRACE_S`: how long a blob no branch names is kept
    /// (default 7 days: a rollback within it still has its bytes).
    pub blob_grace_ms: i64,
    /// `FRAGMENT_PUSH_SUBJECT`: who push services may contact about this
    /// fleet's pushes (a `mailto:` or https URL, RFC 8292).
    pub push_subject: String,
    /// `FRAGMENT_DELIVERY_RETRY_S`: the shortest wait before a delivery is
    /// tried again (default 10; the wait grows with the delivery's age).
    pub delivery_retry_s: u32,
    /// `FRAGMENT_DELIVERY_RETRY_MAX_S`: the longest (default an hour, and
    /// never under the shortest; a test fleet sets both, for a fixed pace).
    pub delivery_retry_max_s: u32,
    /// `AI_GATEWAY_ID`: the AI Gateway the model route calls through
    /// (models.rs): the deployment's own, named, since `default` makes a
    /// gateway that logs (spike S4).
    pub ai_gateway_id: Option<String>,
    /// `FRAGMENT_AI_URL`: dev and the e2e only. The model route POSTs the
    /// AI binding's input to `<url>/run/<model>` instead (a fake at the
    /// vendor boundary, labeled so: models.rs) and needs no gateway.
    pub ai_url: Option<String>,
    /// `FRAGMENT_MODEL_URL` and `FRAGMENT_MODELS`: a self-hosted model
    /// upstream (docs/self-host.md, seam 3), an OpenAI-compatible server's
    /// base (with its `/v1`), and which of its models answers for each
    /// catalog id the route calls. Its key, if it takes one, is the secret
    /// bound as `MODEL_KEY` (keys.rs). For the models it maps, it wins over
    /// the AI binding, the gateway and `FRAGMENT_AI_URL`; the rest go on to
    /// them.
    pub model_upstream: Option<ModelUpstream>,
    /// `FRAGMENT_BROWSER_URL`: where preview cards are shot when it is
    /// not the `BROWSER` binding (docs/self-host.md, seam 7): a service
    /// answering the binding's routes (`/v1/devtools/browser…`) under this
    /// base, without a trailing slash. Set, it wins over the binding.
    pub browser_url: Option<String>,
    workos: Option<WorkOsConfig>,
    oidc: Option<OidcConfig>,
    /// `FRAGMENT_PLATFORM_URL`: the platform's own origin, where sign-in
    /// and the platform session live (default: the hostname suffix itself,
    /// e.g. https://fragment.club; without a suffix, the origin a request
    /// arrived on).
    pub platform_url: Option<String>,
    /// `FRAGMENT_DEFAULT_PLAN`: a new person's plan (docs/ledger.md):
    /// `guest`, the default and production's, or `seat` or
    /// `seat_always_on` (dev and the e2e: `seat`).
    pub default_plan: Plan,
    /// `FRAGMENT_OPERATORS`: identities and keys (as `parse_list` reads
    /// them) that grant credit and set plans, seats and overdrafts.
    operators: Option<Result<Vec<String>, String>>,
    /// `FRAGMENT_SIGNINS_PENDING_MAX`: sign-ins begun and not finished that
    /// the Registry keeps before it lets the oldest go (default
    /// `SIGNINS_PENDING_MAX_DEFAULT`).
    pub signins_pending_max: u64,
    /// `FRAGMENT_TEST_SECRET` (a Worker secret): the test levers
    /// (`/api/test/*`) answer requests that carry it, on a local fleet or a
    /// branch deployment only (`fragment_core::levers`). Without it they
    /// are no route, as on production.
    pub test_secret: Option<fragment_core::levers::TestSecret>,
    /// Whether this fleet has test levers (`test_secret` is set): the
    /// cells' test controls answer only on one, and only through the
    /// router's routes, which check the secret.
    pub test_hooks: bool,
    /// A branch deployment's levers reach the e2e's own things alone
    /// (`fragment_core::levers::Fleet::Branch`): its `e2e-…` fragments and
    /// its e2e people's ledgers, never the registry's, and its e2e people
    /// are made within a day's caps. A local fleet's reach everything.
    pub levers_scoped: bool,
    /// `FRAGMENT_DEPLOY_ID`: which deployment this is (the deploy sets it;
    /// `/healthz` answers it in `x-fragment-deploy`; default `dev`).
    pub deploy_id: String,
    /// `FRAGMENT_PROVIDERS`: the provider catalog (`fragment_core::catalog`),
    /// every credential a computer's guest may use: the WorkOS Pipes
    /// connections, the operator's keys (each bound as
    /// `secrets_store::operator_key` names it, and each priced, which is the
    /// price book's `keys`) and people's own keys, each with its hosts, its
    /// placements and its environment variables. None by default.
    /// `FRAGMENT_PRICE_BOOK_VERSION` (default the defaults' 1) grows with
    /// every change to a key's price, or ledgers made before keep their book.
    pub providers: fragment_core::catalog::Catalog,
    pub price_book_version: u32,
    /// `FRAGMENT_COMPUTER_INSTANCE`: the price book's name for the
    /// deployment's computer instance (default the book's own default,
    /// decision 13's 2 vCPU and 6 GiB), its awake time priced by it.
    pub computer_instance: String,
    /// `FRAGMENT_SWAP_UPSTREAM` (the e2e only): a swapped request goes here,
    /// its host in `x-fragment-upstream-host`, instead of to its host.
    pub swap_upstream: Option<String>,
}

fn var(env: &Env, name: &str) -> Option<String> {
    env.var(name).ok().map(|v| v.to_string().trim().to_string()).filter(|s| !s.is_empty())
}

/// The provider catalog (`FRAGMENT_PROVIDERS`): a deployment whose is
/// malformed is refused at its first request (the deploy checks it first).
fn providers(env: &Env) -> fragment_core::catalog::Catalog {
    var(env, "FRAGMENT_PROVIDERS").map(|v| fragment_core::catalog::Catalog::parse(&v).unwrap_or_else(|e| panic!("FRAGMENT_PROVIDERS: {e}"))).unwrap_or_default()
}

/// A self-hosted model upstream: an OpenAI-compatible server.
#[derive(Debug, Clone)]
pub struct ModelUpstream {
    /// Its base, `/v1` included, without a trailing slash.
    pub url: String,
    /// Catalog id (the price book's label) to the server's model name.
    pub models: std::collections::BTreeMap<String, String>,
}

/// `FRAGMENT_MODEL_URL` with `FRAGMENT_MODELS`, a JSON object of catalog
/// ids to the server's names. A URL without a map, a map naming no model,
/// or a malformed one is refused at the first request.
/// The nodes computers are placed on (`FRAGMENT_NODES`): a deployment
/// whose list is malformed, or that still names one node the way the spike
/// first did, is refused at its first request.
fn nodes(env: &Env, computer_image: Option<&str>) -> Option<fragment_core::placement::Nodes> {
    for gone in ["FRAGMENT_NODE_URL", "FRAGMENT_NODE_SECRET", "FRAGMENT_NODE_IMAGES"] {
        assert!(var(env, gone).is_none(), "{gone} is gone: FRAGMENT_NODES lists the nodes, each one's secret in FRAGMENT_NODE_SECRET_<ID> (docs/self-host.md, seam 2)");
    }
    let nodes = fragment_core::placement::Nodes::parse(&var(env, "FRAGMENT_NODES")?).unwrap_or_else(|e| panic!("FRAGMENT_NODES: {e}"));
    if let Some(image) = computer_image {
        assert!(nodes.image_names().any(|n| n == image), "FRAGMENT_COMPUTER_IMAGE {image:?} is none of FRAGMENT_NODES' images");
    }
    Some(nodes)
}

fn model_upstream(env: &Env) -> Option<ModelUpstream> {
    let url = var(env, "FRAGMENT_MODEL_URL")?.trim_end_matches('/').to_string();
    assert!(url.starts_with("http://") || url.starts_with("https://"), "FRAGMENT_MODEL_URL is an http(s) URL, not {url:?}");
    let map = var(env, "FRAGMENT_MODELS").unwrap_or_else(|| panic!("FRAGMENT_MODEL_URL needs FRAGMENT_MODELS: {{\"<catalog id>\": \"<the server's model>\"}}"));
    let models: std::collections::BTreeMap<String, String> = serde_json::from_str(&map).unwrap_or_else(|e| panic!("FRAGMENT_MODELS is {{\"<catalog id>\": \"<the server's model>\"}}: {e}"));
    assert!(!models.is_empty() && models.values().all(|m| !m.trim().is_empty()), "FRAGMENT_MODELS names at least one model, none empty");
    Some(ModelUpstream { url, models })
}

/// Where this fleet runs, as its levers see it (`levers::fleet_of`): its
/// fragments' hosts carry a branch's mark, or it lets jobs reach local
/// addresses (dev's and the e2e's), or neither.
fn levers_fleet(local: bool, branch: bool) -> fragment_core::levers::Fleet {
    fragment_core::levers::fleet_of(branch, local)
}

/// `FRAGMENT_TEST_SECRET`, as this fleet honours it: a local fleet's or a
/// branch deployment's. One set on a deployment of its own, or one too
/// short, is said in the log and honoured nowhere: the levers stay off,
/// and nothing else is refused.
fn test_secret(env: &Env, fleet: fragment_core::levers::Fleet) -> Option<fragment_core::levers::TestSecret> {
    use fragment_core::levers::honoured;
    let secret = env.secret("FRAGMENT_TEST_SECRET").ok().map(|s| s.to_string().trim().to_string()).filter(|s| !s.is_empty());
    match honoured(secret.as_deref(), fleet) {
        Ok(secret) => secret,
        Err(why) => {
            worker::console_error!("{}", serde_json::json!({ "event": "levers.refused", "why": why.message() }));
            None
        }
    }
}

/// `FRAGMENT_DEPLOY_ID`, or `dev` where a fleet names none.
fn deploy_id(env: &Env) -> String {
    var(env, "FRAGMENT_DEPLOY_ID").unwrap_or_else(|| "dev".into())
}

/// The isolate's settings, built from the first `env` it is handed and
/// shared by every request, cell, and queue batch it runs after that.
///
/// A cache, so its contract. Source: the deployment's Worker variables.
/// Invalidation: a deploy (or, in dev, a change to `.dev.vars`, which
/// reloads) starts new isolates, and each builds its own. Stale reads:
/// impossible, because a Worker version's variables are fixed for every
/// isolate that runs it; `from_env` checks that on every call against
/// `FRAGMENT_DEPLOY_ID`.
static CONFIG: OnceLock<Config> = OnceLock::new();

impl Config {
    /// The isolate's settings (`CONFIG`): built once, from `env`'s variables.
    pub fn from_env(env: &Env) -> &'static Config {
        let cfg = CONFIG.get_or_init(|| Config::build(env));
        // one variable read, against the 19 a build takes: variables that
        // changed under a running isolate would break the contract above
        assert_eq!(deploy_id(env), cfg.deploy_id, "a Worker variable changed under a running isolate");
        cfg
    }

    fn build(env: &Env) -> Config {
        let workos = crate::keys::bound(env, fragment_core::secrets_store::WORKOS_CLIENT).then(|| WorkOsConfig {
            api: var(env, "WORKOS_API_URL").map(|u| u.trim_end_matches('/').to_string()).unwrap_or_else(|| "https://api.workos.com".into()),
        });
        let oidc = oidc(env, workos.is_some());
        let delivery_retry_s =var(env, "FRAGMENT_DELIVERY_RETRY_S").and_then(|s| s.parse::<u32>().ok()).filter(|s| *s >= 1).unwrap_or(10);
        let suffix = |name: &str| var(env, name).map(|s| s.trim_start_matches('.').to_ascii_lowercase());
        let host_suffix = suffix("FRAGMENT_HOST_SUFFIX");
        let legacy_host_suffix = suffix("FRAGMENT_LEGACY_HOST_SUFFIX").filter(|l| host_suffix.as_ref().is_some_and(|s| s != l));
        // a branch deployment's fragments share its zone with other branches'
        let host_label_suffix = var(env, "FRAGMENT_HOST_LABEL_SUFFIX").map(|s| s.to_ascii_lowercase());
        assert!(
            host_label_suffix.as_deref().is_none_or(valid_label_suffix),
            "FRAGMENT_HOST_LABEL_SUFFIX is `--` and a branch name (^--[a-z0-9][a-z0-9-]{{0,30}}$)"
        );
        // the platform's origin is named in frames' `frame-ancestors` and
        // messages' targets (fragment_core::frames), so it is one exactly
        let platform_url = var(env, "FRAGMENT_PLATFORM_URL").map(|u| u.trim_end_matches('/').to_string());
        assert!(
            platform_url.as_deref().is_none_or(fragment_core::frames::is_origin),
            "FRAGMENT_PLATFORM_URL is an origin (scheme://host[:port], lower case, no path)"
        );
        assert!(
            host_suffix.as_deref().is_none_or(|s| fragment_core::frames::is_origin(&format!("https://{s}"))),
            "FRAGMENT_HOST_SUFFIX is a host name"
        );
        let egress_local = var(env, "FRAGMENT_EGRESS_LOCAL").as_deref() == Some("allow");
        let levers_fleet = levers_fleet(egress_local, host_label_suffix.is_some());
        let test_secret = test_secret(env, levers_fleet);
        Config {
            codestorage: var(env, "CODESTORAGE_ORG").map(|org| {
                let api =
                    var(env, "CODESTORAGE_API_URL").map(|a| a.trim_end_matches('/').to_string()).unwrap_or_else(|| fragment_core::codestorage::default_api(&org));
                let repo_prefix = var(env, "CODESTORAGE_REPO_PREFIX").unwrap_or_default();
                assert!(
                    repo_prefix.is_empty() || (repo_prefix.ends_with("--") && repo_prefix.len() <= 20 && repo_prefix.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')),
                    "CODESTORAGE_REPO_PREFIX is a branch name and `--`"
                );
                CodeStorageConfig { org, api, repo_prefix }
            }),
            host_suffix,
            legacy_host_suffix,
            host_label_suffix,
            computer_image: var(env, "FRAGMENT_COMPUTER_IMAGE"),
            nodes: nodes(env, var(env, "FRAGMENT_COMPUTER_IMAGE").as_deref()),
            computer_snapshots: var(env, "FRAGMENT_COMPUTER_SNAPSHOTS").as_deref() != Some("off"),
            computer_unsaved_max_ms: var(env, "FRAGMENT_COMPUTER_UNSAVED_MAX_MS")
                .map(|v| v.parse::<i64>().ok().filter(|ms| *ms >= 0).unwrap_or_else(|| panic!("FRAGMENT_COMPUTER_UNSAVED_MAX_MS is a whole number of ms, not {v:?}")))
                .unwrap_or(fragment_core::computer::UNSAVED_MAX_MS_DEFAULT),
            poll_interval_ms: var(env, "FRAGMENT_POLL_INTERVAL_S").and_then(|s| s.parse::<i64>().ok()).filter(|s| *s >= 1).unwrap_or(300) * 1000,
            egress_local,
            blob_grace_ms: var(env, "FRAGMENT_BLOB_GRACE_S").and_then(|s| s.parse::<i64>().ok()).filter(|s| *s >= 1).unwrap_or(7 * 24 * 3600) * 1000,
            push_subject: var(env, "FRAGMENT_PUSH_SUBJECT").unwrap_or_else(|| "mailto:webpush@fragment.invalid".into()),
            delivery_retry_s,
            delivery_retry_max_s: var(env, "FRAGMENT_DELIVERY_RETRY_MAX_S").and_then(|s| s.parse::<u32>().ok()).unwrap_or(3600).max(delivery_retry_s),
            workos,
            oidc,
            platform_url,
            default_plan: default_plan(env),
            ai_gateway_id: var(env, "AI_GATEWAY_ID").inspect(|id| {
                assert!(id != "default", "AI_GATEWAY_ID names the deployment's own gateway: `default` makes one that logs (spike S4)");
            }),
            ai_url: var(env, "FRAGMENT_AI_URL").map(|u| u.trim_end_matches('/').to_string()),
            model_upstream: model_upstream(env),
            browser_url: var(env, "FRAGMENT_BROWSER_URL").map(|u| u.trim_end_matches('/').to_string()).inspect(|u| {
                assert!(u.starts_with("http://") || u.starts_with("https://"), "FRAGMENT_BROWSER_URL is an http(s) URL, not {u:?}");
            }),
            operators: var(env, "FRAGMENT_OPERATORS").map(|l| fragment_core::npub::parse_list(&l)),
            signins_pending_max: var(env, "FRAGMENT_SIGNINS_PENDING_MAX")
                .and_then(|s| s.parse::<u64>().ok())
                .filter(|n| *n >= 1)
                // the registry counts rows as i64; a larger setting means "no cap to speak of"
                .map(|n| n.min(i64::MAX as u64))
                .unwrap_or(fragment_proto::limits::SIGNINS_PENDING_MAX_DEFAULT),
            test_hooks: test_secret.is_some(),
            levers_scoped: test_secret.is_some() && levers_fleet == fragment_core::levers::Fleet::Branch,
            test_secret,
            deploy_id: deploy_id(env),
            providers: providers(env),
            price_book_version: var(env, "FRAGMENT_PRICE_BOOK_VERSION")
                .map(|v| v.parse().unwrap_or_else(|_| panic!("FRAGMENT_PRICE_BOOK_VERSION is a whole number")))
                .unwrap_or(1),
            computer_instance: var(env, "FRAGMENT_COMPUTER_INSTANCE").unwrap_or_else(|| fragment_core::price::DEFAULT_INSTANCES[0].0.into()),
            swap_upstream: var(env, "FRAGMENT_SWAP_UPSTREAM").map(|u| u.trim_end_matches('/').to_string()),
        }
    }

    /// Whether the signer (its key, 64 hex, if it signed, and its identity)
    /// is one of the deployment's operators.
    pub fn is_operator(&self, key: Option<&str>, identity: &str) -> CellResult<bool> {
        match &self.operators {
            None => Ok(false),
            Some(Err(e)) => Err(CellError::host(format!("FRAGMENT_OPERATORS: {e}"))),
            Some(Ok(listed)) => Ok(listed.iter().any(|l| l == identity || Some(l.as_str()) == key)),
        }
    }

    /// A branch deployment's mark on its fragments' labels (`--<branch>`),
    /// or nothing.
    pub fn host_label_suffix(&self) -> &str {
        self.host_label_suffix.as_deref().unwrap_or("")
    }

    /// WorkOS, for Pipes' connections.
    pub fn workos(&self) -> CellResult<&WorkOsConfig> {
        self.workos
            .as_ref()
            .ok_or_else(|| CellError::new(ErrorCode::HostFailed, format!("connections are not configured on this fleet (no {} binding)", fragment_core::secrets_store::WORKOS_CLIENT)))
    }

    /// Who signs people in here: the OpenID Connect provider.
    pub fn signin(&self) -> CellResult<&OidcConfig> {
        self.oidc.as_ref().ok_or_else(|| CellError::new(ErrorCode::HostFailed, "sign-in is not configured on this fleet (FRAGMENT_OIDC_ISSUER)"))
    }

    /// The platform's origin, given the URL a request arrived on.
    pub fn platform(&self, arrived: &url::Url) -> String {
        if let Some(p) = &self.platform_url {
            return p.clone();
        }
        let port = arrived.port().map(|p| format!(":{p}")).unwrap_or_default();
        match &self.host_suffix {
            Some(suffix) => format!("{}://{suffix}{port}", arrived.scheme()),
            None => format!("{}://{}{port}", arrived.scheme(), arrived.host_str().unwrap_or("localhost")),
        }
    }

    /// Whether `host` is the platform's own (`FRAGMENT_PLATFORM_URL`'s, else
    /// the suffix's own name, as `platform` says), which the router takes
    /// before any fragment's under the suffix.
    pub fn is_platform_host(&self, host: &str) -> bool {
        let named = match &self.platform_url {
            Some(u) => url::Url::parse(u).ok().and_then(|u| u.host_str().map(str::to_string)),
            None => self.host_suffix.clone(),
        };
        named.is_some_and(|h| h.eq_ignore_ascii_case(host))
    }

    /// Whether `host` is the suffix's own name. Past `is_platform_host`, it
    /// is no one's: the platform is elsewhere.
    pub fn is_suffix(&self, host: &str) -> bool {
        self.host_suffix.as_deref().is_some_and(|s| s.eq_ignore_ascii_case(host))
    }

    pub fn codestorage(&self) -> CellResult<&CodeStorageConfig> {
        self.codestorage.as_ref().ok_or_else(|| {
            CellError::new(ErrorCode::HostFailed, "code.storage is not configured on this fleet (CODESTORAGE_ORG)")
        })
    }

    /// The fragment a hostname names, when it is `<label>--<username>.<suffix>`
    /// (one DNS label, so the suffix's one wildcard certificate covers every
    /// fragment). This is the only way a host becomes a fragment: an exact
    /// single label under the suffix (with a branch's mark, its own).
    pub fn fragment_of_host(&self, host: &str) -> Option<String> {
        let label = label_under(host, self.host_suffix.as_deref()?)?;
        let flat = match &self.host_label_suffix {
            Some(branch) => label.strip_suffix(branch.as_str())?,
            None => &label,
        };
        from_flat_name(flat)
    }

    /// The computer a hostname names (`<24 hex>--computer.<suffix>`, a
    /// branch's mark before the dot): its own origin, where its ports are.
    pub fn computer_of_host(&self, host: &str) -> Option<String> {
        let label = label_under(host, self.host_suffix.as_deref()?)?;
        let label = match &self.host_label_suffix {
            Some(branch) => label.strip_suffix(branch.as_str())?.to_string(),
            None => label,
        };
        fragment_proto::computer::computer_of_label(&label)
    }

    /// A computer's own origin (its ports are served there), on the
    /// platform's scheme and port.
    pub fn computer_origin(&self, id: &str) -> Option<String> {
        let suffix = self.host_suffix.as_deref()?;
        let label = fragment_proto::computer::computer_label(id)?;
        let platform = self.platform_url.as_deref().and_then(|p| url::Url::parse(p).ok());
        let scheme = platform.as_ref().map(|u| u.scheme().to_string()).unwrap_or_else(|| "https".into());
        let port = platform.and_then(|u| u.port()).map(|p| format!(":{p}")).unwrap_or_default();
        Some(format!("{scheme}://{label}{}.{suffix}{port}", self.host_label_suffix()))
    }

    /// The fragment an old host names (`<label>--<username>.<legacy
    /// suffix>`): it is served under the suffix now.
    pub fn fragment_of_legacy_host(&self, host: &str) -> Option<String> {
        from_flat_name(&label_under(host, self.legacy_host_suffix.as_deref()?)?)
    }

    /// The label a host has under the suffix or the old one (`x` of
    /// `x.<suffix>`), if it is one: such a host is a fragment's or no one's,
    /// never the platform's.
    pub fn subdomain(&self, host: &str) -> Option<String> {
        [&self.host_suffix, &self.legacy_host_suffix].into_iter().flatten().find_map(|s| label_under(host, s))
    }

    /// Where a fragment is served, given the URL a request arrived on (its
    /// scheme and port carry over).
    pub fn canonical(&self, arrived: &url::Url, name: &str) -> String {
        let origin = self.origin(arrived, name);
        match &self.host_suffix {
            Some(_) => format!("{origin}/"),
            None => format!("{origin}/f/{name}/"),
        }
    }

    /// A fragment's own origin as a visitor from outside reaches it, with
    /// no request to take a scheme and port from (card.rs: the renderer
    /// opens it): the platform's (`FRAGMENT_PLATFORM_URL`'s), else https.
    /// `None` without a suffix: fragments served by path have no origin of
    /// their own.
    pub fn outside_origin(&self, name: &str) -> Option<String> {
        self.host_suffix.as_ref()?;
        let base = self.platform_url.as_deref().unwrap_or("https://platform.invalid");
        let arrived = url::Url::parse(base).ok()?;
        Some(self.origin(&arrived, name))
    }

    /// A fragment's own origin, as a browser on its page names it in
    /// `Origin` (`scheme://host[:port]`): its host's, or, without a suffix,
    /// the one every fragment shares.
    pub fn origin(&self, arrived: &url::Url, name: &str) -> String {
        let port = arrived.port().map(|p| format!(":{p}")).unwrap_or_default();
        match &self.host_suffix {
            Some(suffix) => {
                let host = flat_name(name).unwrap_or_else(|| name.to_string());
                let branch = self.host_label_suffix.as_deref().unwrap_or("");
                format!("{}://{host}{branch}.{suffix}{port}", arrived.scheme())
            }
            None => format!("{}://{}{port}", arrived.scheme(), arrived.host_str().unwrap_or("localhost")),
        }
    }
}

/// `FRAGMENT_DEFAULT_PLAN`, `guest` when unset. Any other value is the
/// deployment's mistake, found as its first isolate starts.
fn default_plan(env: &Env) -> Plan {
    match var(env, "FRAGMENT_DEFAULT_PLAN").as_deref() {
        None | Some("guest") => Plan::Guest,
        Some("seat") => Plan::Seat,
        Some("seat_always_on") => Plan::SeatAlwaysOn,
        Some(other) => panic!("FRAGMENT_DEFAULT_PLAN is guest, seat or seat_always_on, not {other:?}"),
    }
}

/// A branch's mark on its fragments' labels: `--` and its name, so
/// `<label>--<username>--<branch>.<suffix>` stays one DNS label under the
/// zone's one wildcard certificate (docs/cloudflare-v1.md, decision 20).
fn valid_label_suffix(s: &str) -> bool {
    s.strip_prefix("--").is_some_and(|b| {
        (1..=31).contains(&b.len())
            && b.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
            && b.as_bytes()[0] != b'-'
            && !b.contains("--")
    })
}

/// The label `host` has under `suffix` (`x` of `x.<suffix>`), if it is one.
fn label_under(host: &str, suffix: &str) -> Option<String> {
    let host = host.to_ascii_lowercase();
    host.strip_suffix(suffix)?.strip_suffix('.').map(str::to_string)
}
