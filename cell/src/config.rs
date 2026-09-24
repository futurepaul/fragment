//! The fleet's settings, from Worker variables (`cell/.dev.vars` in dev,
//! rendered `vars` at deploy). Nothing about a fleet is a constant in code
//! (ROADMAP decision 13): the hostname suffix, the code.storage org, and
//! the host secret all arrive here.

use fragment_core::secrets::HOST_SECRET_MIN_BYTES;
use fragment_proto::{valid_fragment_name, ErrorCode};
use worker::Env;

use crate::error::{CellError, CellResult};

pub struct CodeStorageConfig {
    pub org: String,
    pub key_pem: String,
    /// The API base, e.g. `https://api.<org>.code.storage`.
    pub api: String,
}

/// WorkOS AuthKit (phase 4 slice B): fragment's own environment.
pub struct WorkOsConfig {
    /// `WORKOS_CLIENT_ID`: names the environment.
    pub client_id: String,
    /// `WORKOS_API_KEY`: the environment's secret key (exchanges codes).
    pub api_key: String,
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
    /// `FRAGMENT_HOST_SECRET`, then `FRAGMENT_HOST_SECRET_PREVIOUS` during a rotation.
    host_secrets: Vec<String>,
    codestorage: Option<CodeStorageConfig>,
    /// `FRAGMENT_HOST_SUFFIX`: fragments are served from `<name>.<suffix>`.
    /// Unset (dev without hostnames), they are served from `/f/<name>/`.
    pub host_suffix: Option<String>,
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
    /// `OPENROUTER_API_URL`: where AI calls go (default https://openrouter.ai; the e2e's fake).
    pub openrouter_url: String,
    workos: Option<WorkOsConfig>,
    /// `FRAGMENT_PLATFORM_URL`: the platform's own origin, where sign-in
    /// and the platform session live (default: the hostname suffix itself,
    /// e.g. https://fragment.club; without a suffix, the origin a request
    /// arrived on).
    platform_url: Option<String>,
    /// `OPENROUTER_MANAGEMENT_KEY`: mints each billing org's own OpenRouter
    /// key, its limit the org's allowance (decision 14). Unset, only a
    /// fragment's own `OPENROUTER_API_KEY` pays for AI.
    pub openrouter_management: Option<String>,
    /// `FRAGMENT_BUDGET_USD`: each person's monthly budget (default 20).
    pub budget_micros: i64,
    /// `FRAGMENT_OPERATORS`: identities and keys (as `parse_list` reads
    /// them) that may top up a budget.
    operators: Option<Result<Vec<String>, String>>,
    /// `FRAGMENT_TEST_HOOKS=allow`: dev and e2e fleets only.
    pub test_hooks: bool,
    /// `FRAGMENT_DEPLOY_ID`: which deployment this is (`cargo xtask deploy`
    /// sets it; `/healthz` answers it in `x-fragment-deploy`).
    pub deploy_id: String,
}

fn var(env: &Env, name: &str) -> Option<String> {
    env.var(name).ok().map(|v| v.to_string().trim().to_string()).filter(|s| !s.is_empty())
}

impl Config {
    pub fn from_env(env: &Env) -> Config {
        let host_secrets = ["FRAGMENT_HOST_SECRET", "FRAGMENT_HOST_SECRET_PREVIOUS"].iter().filter_map(|n| var(env, n)).collect();
        let codestorage = match (var(env, "CODESTORAGE_ORG"), var(env, "CODESTORAGE_PRIVATE_KEY")) {
            (Some(org), Some(key_pem)) => {
                let api = var(env, "CODESTORAGE_API_URL")
                    .map(|a| a.trim_end_matches('/').to_string())
                    .unwrap_or_else(|| fragment_core::codestorage::default_api(&org));
                Some(CodeStorageConfig { org, key_pem, api })
            }
            _ => None,
        };
        let host_suffix = var(env, "FRAGMENT_HOST_SUFFIX").map(|s| s.trim_start_matches('.').to_ascii_lowercase());
        let poll_interval_ms = var(env, "FRAGMENT_POLL_INTERVAL_S").and_then(|s| s.parse::<i64>().ok()).filter(|s| *s >= 1).unwrap_or(300) * 1000;
        let egress_local = var(env, "FRAGMENT_EGRESS_LOCAL").as_deref() == Some("allow");
        let blob_grace_ms = var(env, "FRAGMENT_BLOB_GRACE_S").and_then(|s| s.parse::<i64>().ok()).filter(|s| *s >= 1).unwrap_or(7 * 24 * 3600) * 1000;
        let push_subject = var(env, "FRAGMENT_PUSH_SUBJECT").unwrap_or_else(|| "mailto:webpush@fragment.invalid".into());
        let delivery_retry_s = var(env, "FRAGMENT_DELIVERY_RETRY_S").and_then(|s| s.parse::<u32>().ok()).filter(|s| *s >= 1).unwrap_or(10);
        let openrouter_url = var(env, "OPENROUTER_API_URL").map(|u| u.trim_end_matches('/').to_string()).unwrap_or_else(|| "https://openrouter.ai".into());
        let workos = match (var(env, "WORKOS_CLIENT_ID"), var(env, "WORKOS_API_KEY")) {
            (Some(client_id), Some(api_key)) => Some(WorkOsConfig {
                client_id,
                api_key,
                api: var(env, "WORKOS_API_URL").map(|u| u.trim_end_matches('/').to_string()).unwrap_or_else(|| "https://api.workos.com".into()),
            }),
            _ => None,
        };
        let platform_url = var(env, "FRAGMENT_PLATFORM_URL").map(|u| u.trim_end_matches('/').to_string());
        let openrouter_management = var(env, "OPENROUTER_MANAGEMENT_KEY");
        let budget_micros = var(env, "FRAGMENT_BUDGET_USD")
            .and_then(|v| v.parse::<f64>().ok())
            .filter(|v| v.is_finite() && *v >= 0.0)
            .map(|v| (v * fragment_core::budget::USD as f64).round() as i64)
            .unwrap_or(20 * fragment_core::budget::USD);
        let operators = var(env, "FRAGMENT_OPERATORS").map(|l| fragment_core::npub::parse_list(&l));
        let test_hooks = var(env, "FRAGMENT_TEST_HOOKS").as_deref() == Some("allow");
        let deploy_id = var(env, "FRAGMENT_DEPLOY_ID").unwrap_or_else(|| "dev".into());
        Config {
            host_secrets,
            codestorage,
            host_suffix,
            poll_interval_ms,
            egress_local,
            blob_grace_ms,
            push_subject,
            delivery_retry_s,
            openrouter_url,
            workos,
            platform_url,
            openrouter_management,
            budget_micros,
            operators,
            test_hooks,
            deploy_id,
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
            .ok_or_else(|| CellError::new(ErrorCode::HostFailed, "sign-in is not configured on this fleet (WORKOS_CLIENT_ID, WORKOS_API_KEY)"))
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

    /// The current host secret and any previous one, current first.
    pub fn host_secrets(&self) -> CellResult<Vec<&str>> {
        match self.host_secrets.first() {
            Some(s) if s.len() >= HOST_SECRET_MIN_BYTES => Ok(self.host_secrets.iter().map(String::as_str).collect()),
            Some(_) => Err(CellError::host(format!("FRAGMENT_HOST_SECRET must be at least {HOST_SECRET_MIN_BYTES} bytes"))),
            None => Err(CellError::host("FRAGMENT_HOST_SECRET is not set on this fleet; secrets cannot be stored")),
        }
    }

    pub fn codestorage(&self) -> CellResult<&CodeStorageConfig> {
        self.codestorage.as_ref().ok_or_else(|| {
            CellError::new(ErrorCode::HostFailed, "code.storage is not configured on this fleet (CODESTORAGE_ORG, CODESTORAGE_PRIVATE_KEY)")
        })
    }

    /// The fragment a hostname names, when it is `<name>.<suffix>`. celld
    /// does not vouch for `Host`, so this is the only way a host becomes a
    /// fragment: an exact single valid label under the configured suffix.
    pub fn fragment_of_host(&self, host: &str) -> Option<String> {
        let suffix = self.host_suffix.as_deref()?;
        let host = host.to_ascii_lowercase();
        let label = host.strip_suffix(suffix)?.strip_suffix('.')?;
        valid_fragment_name(label).then(|| label.to_string())
    }

    /// Where a fragment is served, given the URL a request arrived on (its
    /// scheme and port carry over).
    pub fn canonical(&self, arrived: &url::Url, name: &str) -> String {
        let port = arrived.port().map(|p| format!(":{p}")).unwrap_or_default();
        match &self.host_suffix {
            Some(suffix) => format!("{}://{name}.{suffix}{port}/", arrived.scheme()),
            None => format!("{}://{}{port}/f/{name}/", arrived.scheme(), arrived.host_str().unwrap_or("localhost")),
        }
    }
}
