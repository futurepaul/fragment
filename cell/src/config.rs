//! The fleet's settings, from Worker variables (`cell/.dev.vars` in dev,
//! rendered `vars` at deploy), built once per isolate (`CONFIG`). Nothing about a fleet is a constant in code
//! (docs/cloudflare-v1.md, decision 4): the hostname suffix and the code.storage org
//! arrive here. The fleet's secrets do not: the host secret, the
//! code.storage key, and WorkOS's client id and API key are Secrets Store
//! bindings, read only by keys.rs.

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

/// WorkOS AuthKit (phase 4 slice B): fragment's own environment, configured
/// when its client id is bound (`secrets_store::WORKOS_CLIENT`; keys.rs
/// reads it, and its API key, as `keys::workos`).
pub struct WorkOsConfig {
    /// `WORKOS_API_URL` (default https://api.workos.com; dev and the e2e: the fake).
    pub api: String,
}

pub struct Config {
    /// `CODESTORAGE_ORG` (required) and the settings beside it.
    codestorage: CodeStorageConfig,
    /// `FRAGMENT_HOST_SUFFIX`: fragments are served from
    /// `<label>--<username>.<suffix>`. Every deployment names one.
    pub host_suffix: String,
    /// `FRAGMENT_HOST_LABEL_SUFFIX` (`--<branch>`): a branch deployment's
    /// fragments are `<label>--<username>--<branch>.<suffix>`, beside the
    /// other branches' in one zone.
    host_label_suffix: Option<String>,
    /// `FRAGMENT_COMPUTER_IMAGE`: the image a new computer is pinned to (a
    /// name in wrangler.jsonc's `containers` images). Unset, the deployment
    /// makes no computers.
    pub computer_image: Option<String>,
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
    /// `FRAGMENT_POLL_INTERVAL_S`: a busy fragment's pass, and the poll backstop (default 300).
    pub poll_interval_ms: i64,
    /// `FRAGMENT_EGRESS_LOCAL=allow`: jobs may fetch private and loopback
    /// addresses (dev and e2e fleets, which call local fakes). Never on a
    /// shared fleet.
    pub egress_local: bool,
    /// `FRAGMENT_BLOB_GRACE_S`: how long a blob no branch names is kept
    /// (default 7 days: a rollback within it still has its bytes).
    pub blob_grace_ms: i64,
    /// The shortest wait before a delivery, or a card's shot, is tried
    /// again (10 s; the wait grows with the delivery's age, and a shot's
    /// doubles), and the longest (an hour). `FRAGMENT_DELIVERY_RETRY_S`
    /// pins both: a test fleet's fixed pace.
    pub delivery_retry_s: u32,
    pub delivery_retry_max_s: u32,
    /// `AI_GATEWAY_ID`: the AI Gateway the model route calls through
    /// (models.rs): the deployment's own, named, since `default` makes a
    /// gateway that logs (spike S4).
    pub ai_gateway_id: Option<String>,
    /// `FRAGMENT_AI_URL`: dev and the e2e only. The model route POSTs the
    /// AI binding's input to `<url>/run/<model>` instead (a fake at the
    /// vendor boundary, labeled so: models.rs) and needs no gateway.
    pub ai_url: Option<String>,
    workos: Option<WorkOsConfig>,
    /// `FRAGMENT_PLATFORM_URL` (required): the platform's own origin, where
    /// sign-in and the platform session live (e.g. https://fragment.club).
    /// It is also the contact a push's VAPID token names (`sub`, RFC 8292):
    /// a push service may refuse one it cannot reach (Apple's answers 403
    /// `BadJwtToken`).
    pub platform_url: String,
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
    /// `FRAGMENT_VISION_MODEL` (the deploy config's `vision_model`): the
    /// model the route's `vision` runs, for a runtime's calls about an
    /// image (Hermes' screenshots: docs/computers.md, Models). GLM-5.3
    /// Flash unless named; one the price book does not price is refused
    /// (`fragment_core::models::vision_model`), at the deploy and here.
    pub vision_model: String,
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
        let pinned_retry_s = var(env, "FRAGMENT_DELIVERY_RETRY_S").and_then(|s| s.parse::<u32>().ok()).filter(|s| *s >= 1);
        let suffix = |name: &str| var(env, name).map(|s| s.trim_start_matches('.').to_ascii_lowercase());
        let host_suffix = suffix("FRAGMENT_HOST_SUFFIX").expect("FRAGMENT_HOST_SUFFIX names where fragments are served");
        // a branch deployment's fragments share its zone with other branches'
        let host_label_suffix = var(env, "FRAGMENT_HOST_LABEL_SUFFIX").map(|s| s.to_ascii_lowercase());
        assert!(
            host_label_suffix.as_deref().is_none_or(valid_label_suffix),
            "FRAGMENT_HOST_LABEL_SUFFIX is `--` and a branch name (^--[a-z0-9][a-z0-9-]{{0,30}}$)"
        );
        // the platform's origin is named in frames' `frame-ancestors` and
        // messages' targets (fragment_core::frames), so it is one exactly
        let platform_url = var(env, "FRAGMENT_PLATFORM_URL").map(|u| u.trim_end_matches('/').to_string()).unwrap_or_default();
        assert!(fragment_core::frames::is_origin(&platform_url), "FRAGMENT_PLATFORM_URL (required) is an origin (scheme://host[:port], lower case, no path)");
        assert!(fragment_core::frames::is_origin(&format!("https://{host_suffix}")), "FRAGMENT_HOST_SUFFIX is a host name");
        let egress_local = var(env, "FRAGMENT_EGRESS_LOCAL").as_deref() == Some("allow");
        let levers_fleet = levers_fleet(egress_local, host_label_suffix.is_some());
        let test_secret = test_secret(env, levers_fleet);
        Config {
            codestorage: {
                let org = var(env, "CODESTORAGE_ORG").unwrap_or_else(|| panic!("CODESTORAGE_ORG names the deployment's code.storage org"));
                let api =
                    var(env, "CODESTORAGE_API_URL").map(|a| a.trim_end_matches('/').to_string()).unwrap_or_else(|| fragment_core::codestorage::default_api(&org));
                let repo_prefix = var(env, "CODESTORAGE_REPO_PREFIX").unwrap_or_default();
                assert!(
                    repo_prefix.is_empty() || (repo_prefix.ends_with("--") && repo_prefix.len() <= 20 && repo_prefix.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')),
                    "CODESTORAGE_REPO_PREFIX is a branch name and `--`"
                );
                CodeStorageConfig { org, api, repo_prefix }
            },
            host_suffix,
            host_label_suffix,
            computer_image: var(env, "FRAGMENT_COMPUTER_IMAGE"),
            computer_snapshots: var(env, "FRAGMENT_COMPUTER_SNAPSHOTS").as_deref() != Some("off"),
            computer_unsaved_max_ms: var(env, "FRAGMENT_COMPUTER_UNSAVED_MAX_MS")
                .map(|v| v.parse::<i64>().ok().filter(|ms| *ms >= 0).unwrap_or_else(|| panic!("FRAGMENT_COMPUTER_UNSAVED_MAX_MS is a whole number of ms, not {v:?}")))
                .unwrap_or(fragment_core::computer::UNSAVED_MAX_MS_DEFAULT),
            poll_interval_ms: var(env, "FRAGMENT_POLL_INTERVAL_S").and_then(|s| s.parse::<i64>().ok()).filter(|s| *s >= 1).unwrap_or(300) * 1000,
            egress_local,
            blob_grace_ms: var(env, "FRAGMENT_BLOB_GRACE_S").and_then(|s| s.parse::<i64>().ok()).filter(|s| *s >= 1).unwrap_or(7 * 24 * 3600) * 1000,
            delivery_retry_s: pinned_retry_s.unwrap_or(10),
            delivery_retry_max_s: pinned_retry_s.unwrap_or(3600),
            workos: crate::keys::bound(env, fragment_core::secrets_store::WORKOS_CLIENT).then(|| WorkOsConfig {
                api: var(env, "WORKOS_API_URL").map(|u| u.trim_end_matches('/').to_string()).unwrap_or_else(|| "https://api.workos.com".into()),
            }),
            platform_url,
            default_plan: default_plan(env),
            ai_gateway_id: var(env, "AI_GATEWAY_ID").inspect(|id| {
                assert!(id != "default", "AI_GATEWAY_ID names the deployment's own gateway: `default` makes one that logs (spike S4)");
            }),
            ai_url: var(env, "FRAGMENT_AI_URL").map(|u| u.trim_end_matches('/').to_string()),
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
            vision_model: fragment_core::models::vision_model(var(env, "FRAGMENT_VISION_MODEL").as_deref(), &fragment_core::price::PriceBook::defaults())
                .unwrap_or_else(|e| panic!("FRAGMENT_VISION_MODEL: {e}")),
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

    pub fn workos(&self) -> CellResult<&WorkOsConfig> {
        self.workos
            .as_ref()
            .ok_or_else(|| CellError::new(ErrorCode::HostFailed, format!("sign-in is not configured on this fleet (no {} binding)", fragment_core::secrets_store::WORKOS_CLIENT)))
    }

    /// The platform's origin (`FRAGMENT_PLATFORM_URL`).
    pub fn platform(&self) -> String {
        self.platform_url.clone()
    }

    /// Whether `host` is the platform's own (`FRAGMENT_PLATFORM_URL`'s),
    /// which the router takes before any fragment's under the suffix.
    pub fn is_platform_host(&self, host: &str) -> bool {
        url::Url::parse(&self.platform_url).is_ok_and(|u| u.host_str().is_some_and(|h| h.eq_ignore_ascii_case(host)))
    }

    /// Whether `host` is the suffix's own name. Past `is_platform_host`, it
    /// is no one's: the platform is elsewhere.
    pub fn is_suffix(&self, host: &str) -> bool {
        self.host_suffix.eq_ignore_ascii_case(host)
    }

    pub fn codestorage(&self) -> &CodeStorageConfig {
        &self.codestorage
    }

    /// The fragment a hostname names, when it is `<label>--<username>.<suffix>`
    /// (one DNS label, so the suffix's one wildcard certificate covers every
    /// fragment). This is the only way a host becomes a fragment: an exact
    /// single label under the suffix (with a branch's mark, its own).
    pub fn fragment_of_host(&self, host: &str) -> Option<String> {
        let label = label_under(host, &self.host_suffix)?;
        let flat = match &self.host_label_suffix {
            Some(branch) => label.strip_suffix(branch.as_str())?,
            None => &label,
        };
        from_flat_name(flat)
    }

    /// The computer a hostname names (`<24 hex>--computer.<suffix>`, a
    /// branch's mark before the dot): its own origin, where its ports are.
    pub fn computer_of_host(&self, host: &str) -> Option<String> {
        let label = label_under(host, &self.host_suffix)?;
        let label = match &self.host_label_suffix {
            Some(branch) => label.strip_suffix(branch.as_str())?.to_string(),
            None => label,
        };
        fragment_proto::computer::computer_of_label(&label)
    }

    /// A computer's own origin (its ports are served there), on the
    /// platform's scheme and port; `None` for an id that is no computer's.
    pub fn computer_origin(&self, id: &str) -> Option<String> {
        let suffix = &self.host_suffix;
        let label = fragment_proto::computer::computer_label(id)?;
        let platform = url::Url::parse(&self.platform_url).ok()?;
        let port = platform.port().map(|p| format!(":{p}")).unwrap_or_default();
        Some(format!("{}://{label}{}.{suffix}{port}", platform.scheme(), self.host_label_suffix()))
    }

    /// The label a host has under the suffix (`x` of `x.<suffix>`), if it
    /// is one: such a host is a fragment's or no one's, never the platform's.
    pub fn subdomain(&self, host: &str) -> Option<String> {
        label_under(host, &self.host_suffix)
    }

    /// Where a fragment is served, given the URL a request arrived on (its
    /// scheme and port carry over).
    pub fn canonical(&self, arrived: &url::Url, name: &str) -> String {
        format!("{}/", self.origin(arrived, name))
    }

    /// A fragment's own origin as a visitor from outside reaches it, with
    /// no request to take a scheme and port from (card.rs: the renderer
    /// opens it): the platform's (`FRAGMENT_PLATFORM_URL`'s) scheme and port.
    pub fn outside_origin(&self, name: &str) -> String {
        let arrived = url::Url::parse(&self.platform_url).expect("FRAGMENT_PLATFORM_URL is an origin (checked as the isolate starts)");
        self.origin(&arrived, name)
    }

    /// A fragment's own origin, as a browser on its page names it in
    /// `Origin` (`scheme://host[:port]`).
    pub fn origin(&self, arrived: &url::Url, name: &str) -> String {
        let port = arrived.port().map(|p| format!(":{p}")).unwrap_or_default();
        let host = flat_name(name).unwrap_or_else(|| name.to_string());
        let branch = self.host_label_suffix.as_deref().unwrap_or("");
        format!("{}://{host}{branch}.{}{port}", arrived.scheme(), self.host_suffix)
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
