//! The fleet's settings, from Worker variables (`cell/.dev.vars` in dev,
//! rendered `vars` at deploy), built once per isolate (`CONFIG`). Nothing about a fleet is a constant in code
//! (ROADMAP decision 13): the hostname suffix and the code.storage org
//! arrive here. The fleet's secrets do not: the host secret, the
//! code.storage key, the WorkOS API key, and the OpenRouter management key
//! live in the node's environment, used through `KEYS` (keys.rs).

use std::sync::OnceLock;

use fragment_proto::{flat_name, from_flat_name, ErrorCode};
use worker::Env;

use crate::error::{CellError, CellResult};

pub struct CodeStorageConfig {
    pub org: String,
    /// The API base, e.g. `https://api.<org>.code.storage`.
    pub api: String,
}

/// WorkOS AuthKit (phase 4 slice B): fragment's own environment.
pub struct WorkOsConfig {
    /// `WORKOS_CLIENT_ID`: names the environment (its API key is the node's).
    pub client_id: String,
    /// `WORKOS_API_URL` (default https://api.workos.com; dev and the e2e: the fake).
    pub api: String,
}

impl WorkOsConfig {
    /// Who vouches for a person's subject: this environment. A person is
    /// keyed by `(issuer, subject)`, so finite.computer's login (another
    /// environment) is another issuer (docs/finite-integration.md).
    pub fn issuer(&self) -> String {
        format!("workos:{}", self.client_id)
    }
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
    /// `OPENROUTER_API_URL`: where AI calls go (default https://openrouter.ai; the e2e's fake).
    pub openrouter_url: String,
    workos: Option<WorkOsConfig>,
    /// `FRAGMENT_PLATFORM_URL`: the platform's own origin, where sign-in
    /// and the platform session live (default: the hostname suffix itself,
    /// e.g. https://fragment.club; without a suffix, the origin a request
    /// arrived on).
    platform_url: Option<String>,
    /// `FRAGMENT_BUDGET_USD`: each person's monthly budget (default 20).
    pub budget_micros: i64,
    /// `FRAGMENT_OPERATORS`: identities and keys (as `parse_list` reads
    /// them) that may top up a budget.
    operators: Option<Result<Vec<String>, String>>,
    /// `FRAGMENT_SIGNINS_PENDING_MAX`: sign-ins begun and not finished that
    /// the Registry keeps before it lets the oldest go (default
    /// `SIGNINS_PENDING_MAX_DEFAULT`).
    pub signins_pending_max: u64,
    /// `FRAGMENT_COMPUTER_TICK_S` (default 60): how long a computer is held
    /// awake, and charged, at a time; `FRAGMENT_COMPUTER_IDLE_S` (default
    /// 300): how long it stays awake after its fragment's last page closes.
    pub computer_tick_ms: i64,
    pub computer_idle_ms: i64,
    /// `FRAGMENT_CLI_RELEASE_URL`: where a computer's one-line install
    /// fetches `fragment-<os>-<arch>.tar.gz` (default the latest release).
    pub cli_release_url: String,
    /// `FRAGMENT_TEST_HOOKS=allow`: dev and e2e fleets only.
    pub test_hooks: bool,
    /// `FRAGMENT_DEPLOY_ID`: which deployment this is (`cargo xtask deploy`
    /// sets it; `/healthz` answers it in `x-fragment-deploy`).
    pub deploy_id: String,
}

fn var(env: &Env, name: &str) -> Option<String> {
    env.var(name).ok().map(|v| v.to_string().trim().to_string()).filter(|s| !s.is_empty())
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
/// rebuilds) starts new isolates, and each builds its own. Stale reads:
/// impossible, because celld writes the variables into an isolate's `env`
/// once, as literals, when it builds the isolate from its deployment's
/// config (`build_env`), and never changes them under it; `from_env`
/// checks that on every call against `FRAGMENT_DEPLOY_ID`.
static CONFIG: OnceLock<Config> = OnceLock::new();

impl Config {
    /// The isolate's settings (`CONFIG`): built once, from `env`'s variables.
    pub fn from_env(env: &Env) -> &'static Config {
        let cfg = CONFIG.get_or_init(|| Config::build(env));
        // one variable read, against the 18 a build takes: variables that
        // changed under a running isolate would break the contract above
        assert_eq!(deploy_id(env), cfg.deploy_id, "celld changed a Worker variable under a running isolate");
        cfg
    }

    fn build(env: &Env) -> Config {
        let delivery_retry_s = var(env, "FRAGMENT_DELIVERY_RETRY_S").and_then(|s| s.parse::<u32>().ok()).filter(|s| *s >= 1).unwrap_or(10);
        let suffix = |name: &str| var(env, name).map(|s| s.trim_start_matches('.').to_ascii_lowercase());
        let host_suffix = suffix("FRAGMENT_HOST_SUFFIX");
        let legacy_host_suffix = suffix("FRAGMENT_LEGACY_HOST_SUFFIX").filter(|l| host_suffix.as_ref().is_some_and(|s| s != l));
        Config {
            codestorage: var(env, "CODESTORAGE_ORG").map(|org| {
                let api =
                    var(env, "CODESTORAGE_API_URL").map(|a| a.trim_end_matches('/').to_string()).unwrap_or_else(|| fragment_core::codestorage::default_api(&org));
                CodeStorageConfig { org, api }
            }),
            host_suffix,
            legacy_host_suffix,
            poll_interval_ms: var(env, "FRAGMENT_POLL_INTERVAL_S").and_then(|s| s.parse::<i64>().ok()).filter(|s| *s >= 1).unwrap_or(300) * 1000,
            egress_local: var(env, "FRAGMENT_EGRESS_LOCAL").as_deref() == Some("allow"),
            blob_grace_ms: var(env, "FRAGMENT_BLOB_GRACE_S").and_then(|s| s.parse::<i64>().ok()).filter(|s| *s >= 1).unwrap_or(7 * 24 * 3600) * 1000,
            push_subject: var(env, "FRAGMENT_PUSH_SUBJECT").unwrap_or_else(|| "mailto:webpush@fragment.invalid".into()),
            delivery_retry_s,
            delivery_retry_max_s: var(env, "FRAGMENT_DELIVERY_RETRY_MAX_S").and_then(|s| s.parse::<u32>().ok()).unwrap_or(3600).max(delivery_retry_s),
            openrouter_url: var(env, "OPENROUTER_API_URL").map(|u| u.trim_end_matches('/').to_string()).unwrap_or_else(|| "https://openrouter.ai".into()),
            workos: var(env, "WORKOS_CLIENT_ID").map(|client_id| WorkOsConfig {
                client_id,
                api: var(env, "WORKOS_API_URL").map(|u| u.trim_end_matches('/').to_string()).unwrap_or_else(|| "https://api.workos.com".into()),
            }),
            platform_url: var(env, "FRAGMENT_PLATFORM_URL").map(|u| u.trim_end_matches('/').to_string()),
            budget_micros: var(env, "FRAGMENT_BUDGET_USD")
                .and_then(|v| v.parse::<f64>().ok())
                .filter(|v| v.is_finite() && *v >= 0.0)
                .map(|v| (v * fragment_core::budget::USD as f64).round() as i64)
                .unwrap_or(20 * fragment_core::budget::USD),
            operators: var(env, "FRAGMENT_OPERATORS").map(|l| fragment_core::npub::parse_list(&l)),
            signins_pending_max: var(env, "FRAGMENT_SIGNINS_PENDING_MAX")
                .and_then(|s| s.parse::<u64>().ok())
                .filter(|n| *n >= 1)
                // the registry counts rows as i64; a larger setting means "no cap to speak of"
                .map(|n| n.min(i64::MAX as u64))
                .unwrap_or(fragment_proto::limits::SIGNINS_PENDING_MAX_DEFAULT),
            computer_tick_ms: var(env, "FRAGMENT_COMPUTER_TICK_S").and_then(|s| s.parse::<i64>().ok()).filter(|s| *s >= 1).unwrap_or(60) * 1000,
            computer_idle_ms: var(env, "FRAGMENT_COMPUTER_IDLE_S").and_then(|s| s.parse::<i64>().ok()).filter(|s| *s >= 0).unwrap_or(300) * 1000,
            cli_release_url: var(env, "FRAGMENT_CLI_RELEASE_URL")
                .map(|u| u.trim_end_matches('/').to_string())
                .unwrap_or_else(|| "https://github.com/futurepaul/fragment/releases/latest/download".into()),
            test_hooks: var(env, "FRAGMENT_TEST_HOOKS").as_deref() == Some("allow"),
            deploy_id: deploy_id(env),
        }
    }

    /// Whether the signer (its key, 64 hex, if it signed, and its identity)
    /// may top up budgets.
    pub fn is_operator(&self, key: Option<&str>, identity: &str) -> CellResult<bool> {
        match &self.operators {
            None => Ok(false),
            Some(Err(e)) => Err(CellError::host(format!("FRAGMENT_OPERATORS: {e}"))),
            Some(Ok(listed)) => Ok(listed.iter().any(|l| l == identity || Some(l.as_str()) == key)),
        }
    }

    pub fn workos(&self) -> CellResult<&WorkOsConfig> {
        self.workos
            .as_ref()
            .ok_or_else(|| CellError::new(ErrorCode::HostFailed, "sign-in is not configured on this fleet (WORKOS_CLIENT_ID)"))
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

    /// The platform's origin as a computer's CLI reaches it: no request
    /// names one, so the fleet's (`FRAGMENT_PLATFORM_URL`, or its suffix).
    pub fn computer_platform(&self) -> CellResult<String> {
        match (&self.platform_url, &self.host_suffix) {
            (Some(p), _) => Ok(p.clone()),
            (None, Some(suffix)) => Ok(format!("https://{suffix}")),
            (None, None) => Err(CellError::new(ErrorCode::HostFailed, "computers need FRAGMENT_PLATFORM_URL or FRAGMENT_HOST_SUFFIX")),
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
    /// fragment). celld does not vouch for `Host`, so this is the only way a
    /// host becomes a fragment: an exact single label under the suffix.
    pub fn fragment_of_host(&self, host: &str) -> Option<String> {
        from_flat_name(&label_under(host, self.host_suffix.as_deref()?)?)
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

    /// A fragment's own origin, as a browser on its page names it in
    /// `Origin` (`scheme://host[:port]`): its host's, or, without a suffix,
    /// the one every fragment shares.
    pub fn origin(&self, arrived: &url::Url, name: &str) -> String {
        let port = arrived.port().map(|p| format!(":{p}")).unwrap_or_default();
        match &self.host_suffix {
            Some(suffix) => {
                let host = flat_name(name).unwrap_or_else(|| name.to_string());
                format!("{}://{host}.{suffix}{port}", arrived.scheme())
            }
            None => format!("{}://{}{port}", arrived.scheme(), arrived.host_str().unwrap_or("localhost")),
        }
    }
}

/// The label `host` has under `suffix` (`x` of `x.<suffix>`), if it is one.
fn label_under(host: &str, suffix: &str) -> Option<String> {
    let host = host.to_ascii_lowercase();
    host.strip_suffix(suffix)?.strip_suffix('.').map(str::to_string)
}
