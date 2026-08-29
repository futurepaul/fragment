//! Boot configuration. Every value comes from the environment, is validated
//! once here, and then travels inside the immutable [`Config`] struct for the
//! rest of the process lifetime. Invalid or unknown configuration exits
//! non-zero with one human-readable reason instead of guessing defaults.

use std::collections::{BTreeSet, HashMap};
use std::net::SocketAddr;

use crate::error::BootError;

/// Default body ceiling from the spec (64 MiB).
pub const DEFAULT_MAX_BLOB_BYTES: u64 = 67_108_864;

/// Hard ceiling on `BLOBSD_MAX_BLOB_BYTES`. Above this the single-shot
/// conditional PUT staging design (stage file, bounded local disk) stops being
/// the right tool; raising it is a deliberate spec-level decision, not a
/// config tweak.
pub const MAX_MAX_BLOB_BYTES: u64 = 1_073_741_824;

/// Below this an upload endpoint cannot carry a real cell asset; it only
/// invites foot-gunned configs where every request trips size enforcement.
const MIN_MAX_BLOB_BYTES: u64 = 1024;

/// Entropy floor for the internal bearer token: below this the token does not
/// buy privacy against casual scraping of logs and process listings anyway,
/// so run-time behavior (auth required) would rest on fake secrecy.
const MIN_INTERNAL_TOKEN_LEN: usize = 16;

/// Recognized `BLOBSD_*` environment names. Anything else prefixed
/// `BLOBSD_` fails boot: a typo'd variable silently falling back to default
/// behavior is exactly how an instance ends up public without meaning to be.
pub const KNOWN_BLOBSD_VARS: [&str; 10] = [
    "BLOBSD_LISTEN",
    "BLOBSD_DATA_DIR",
    "BLOBSD_PUBLIC_URL",
    "BLOBSD_MAX_BLOB_BYTES",
    "BLOBSD_ALLOW_NPUBS",
    "BLOBSD_INTERNAL_TOKEN",
    "BLOBSD_PUBLIC_GET",
    "BLOBSD_BUCKET",
    "BLOBSD_BACKEND",
    "BLOBSD_FS_ROOT",
];

/// Which object store backs the blob tier. `S3` is production (and anything
/// with an S3-compatible endpoint); `Fs` stores blobs as plain files under a
/// local root so dev and CI run the whole stack as native processes — no
/// container, no MinIO, same handlers and descriptor store either way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    S3,
    Fs,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub listen: SocketAddr,
    pub data_dir: String,
    /// Object key prefix inside the bucket, always `blobs/<sha256>`.
    pub bucket_prefix: &'static str,
    pub backend: Backend,
    /// Local filesystem root for `Backend::Fs` (unused by `Backend::S3`).
    pub fs_root: String,
    pub bucket: String,
    pub s3_endpoint: String,
    pub aws_access_key_id: String,
    pub aws_secret_access_key: String,
    pub aws_region: String,
    /// Exact string the auth event's `x` tag must equal. Deliberately kept
    /// byte-exact (no normalization): the x-tag comparison is equality, and a
    /// canonicalized copy would diverge from what signers saw in docs.
    pub public_url: String,
    pub max_blob_bytes: u64,
    /// Allowlisted signer pubkeys as lowercase hex, decoded from npubs at
    /// boot so request-path comparisons are plain fixed-width hex matches.
    pub allow_hex_pubkeys: BTreeSet<String>,
    /// true when BLOBSD_ALLOW_NPUBS contains "*": any verified event pubkey
    /// may store (dev/test; production sets explicit npubs).
    pub allow_all: bool,
    pub internal_token: String,
    pub public_get: bool,
}

impl Config {
    /// Read and validate the full configuration from the process environment.
    pub fn from_env() -> Result<Config, BootError> {
        let vars: HashMap<String, String> = std::env::vars().collect();
        Self::from_map(&vars)
    }

    /// The map-driven core behind [`Self::from_env`], also the test seam:
    /// injecting a map keeps tests free of process-global env races.
    pub fn from_map(vars: &HashMap<String, String>) -> Result<Config, BootError> {
        // Unknown-variable refusal first, so a typo never hides behind a
        // later, more confusing failure.
        let mut unknown: Vec<&str> = Vec::new();
        for name in vars.keys() {
            if let Some(stripped) = name.strip_prefix("BLOBSD_") {
                if stripped.is_empty() {
                    unknown.push("BLOBSD_");
                } else if !KNOWN_BLOBSD_VARS.contains(&name.as_str()) {
                    unknown.push(name.as_str());
                }
            }
        }
        if !unknown.is_empty() {
            unknown.sort_unstable();
            return Err(BootError::Config(format!(
                "unknown BLOBSD_* variables: {} (recognized: {})",
                unknown.join(", "),
                KNOWN_BLOBSD_VARS.join(", ")
            )));
        }

        let get = |name: &str| -> Result<String, BootError> {
            match vars.get(name) {
                Some(value) if !value.trim().is_empty() => Ok(value.clone()),
                _ => Err(BootError::Config(format!(
                    "{name} is required and must not be empty"
                ))),
            }
        };

        let backend = match vars.get("BLOBSD_BACKEND").map(String::as_str) {
            None | Some("s3") => Backend::S3,
            Some("fs") => Backend::Fs,
            Some(other) => {
                return Err(BootError::Config(format!(
                    "BLOBSD_BACKEND must be s3 or fs (got {other:?})"
                )))
            }
        };

        let listen = get("BLOBSD_LISTEN")?
            .parse::<SocketAddr>()
            .map_err(|e| BootError::Config(format!("BLOBSD_LISTEN: {e}")))?;

        let data_dir = get("BLOBSD_DATA_DIR")?;

        let public_url = get("BLOBSD_PUBLIC_URL")?;
        validate_public_url(&public_url)?;

        // S3 wiring applies to the s3 backend only; the fs backend takes a
        // local root instead and must not demand cloud credentials.
        let (fs_root, bucket, s3_endpoint, aws_access_key_id, aws_secret_access_key, aws_region) =
            match backend {
                Backend::Fs => {
                    let root = get("BLOBSD_FS_ROOT")?;
                    if root.contains("..") || !root.starts_with('/') {
                        return Err(BootError::Config(
                            "BLOBSD_FS_ROOT must be an absolute path without '..'".to_string(),
                        ));
                    }
                    (root, String::new(), String::new(), String::new(), String::new(), String::new())
                }
                Backend::S3 => {
                    let bucket = get("BLOBSD_BUCKET")?;
                    validate_bucket_name(&bucket)?;
                    let s3_endpoint = get("S3_ENDPOINT")?;
                    if !(s3_endpoint.starts_with("http://") || s3_endpoint.starts_with("https://")) {
                        return Err(BootError::Config(
                            "S3_ENDPOINT must start with http:// or https://".to_string(),
                        ));
                    }
                    let aws_access_key_id = get("AWS_ACCESS_KEY_ID")?;
                    let aws_secret_access_key = get("AWS_SECRET_ACCESS_KEY")?;
                    let aws_region = get("AWS_REGION")?;
                    if aws_region.is_empty()
                        || aws_region.len() > 64
                        || !aws_region
                            .chars()
                            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
                    {
                        return Err(BootError::Config(
                            "AWS_REGION must be 1-64 chars of [a-z0-9-]".to_string(),
                        ));
                    }
                    (
                        String::new(),
                        bucket,
                        s3_endpoint,
                        aws_access_key_id,
                        aws_secret_access_key,
                        aws_region,
                    )
                }
            };

        let max_blob_bytes_raw = match vars.get("BLOBSD_MAX_BLOB_BYTES") {
            // Unset takes the specced default; set-but-empty is an error so a
            // wiped CI secret never masquerades as a default.
            None => DEFAULT_MAX_BLOB_BYTES,
            Some(raw) => raw
                .trim()
                .parse::<u64>()
                .map_err(|_| BootError::Config("BLOBSD_MAX_BLOB_BYTES: not an integer".into()))?,
        };
        if !(MIN_MAX_BLOB_BYTES..=MAX_MAX_BLOB_BYTES).contains(&max_blob_bytes_raw) {
            return Err(BootError::Config(format!(
                "BLOBSD_MAX_BLOB_BYTES must be between {MIN_MAX_BLOB_BYTES} and {MAX_MAX_BLOB_BYTES}"
            )));
        }

        // Fetched directly (not through `get`) so an explicitly EMPTY value
        // reaches the dedicated allowlist error below instead of the generic
        // required-variable error.
        let allow_csv = vars.get("BLOBSD_ALLOW_NPUBS").map(String::as_str).ok_or(
            BootError::Config("BLOBSD_ALLOW_NPUBS is required".to_string()),
        )?;
        let mut allow_hex_pubkeys = BTreeSet::new();
        let mut allow_all = false;
        for npub in allow_csv.split(',') {
            let npub = npub.trim();
            if npub.is_empty() {
                continue;
            }
            if npub == "*" {
                // Dev/test affordance: any verified event pubkey may store.
                // Ledgered as accepted coarseness; production sets explicit npubs.
                allow_all = true;
                continue;
            }
            let hex_pubkey = crate::auth::npub_to_hex(npub)
                .map_err(|e| BootError::Config(format!("BLOBSD_ALLOW_NPUBS: {npub}: {e}")))?;
            allow_hex_pubkeys.insert(hex_pubkey);
        }
        if allow_hex_pubkeys.is_empty() && !allow_all {
            // Fail closed: an empty allowlist would silently brick every
            // write while serving reads fine — half-alive fleet state nobody
            // wants to discover from CLI errors three weeks later.
            return Err(BootError::Config(
                "BLOBSD_ALLOW_NPUBS must contain at least one valid npub".to_string(),
            ));
        }

        let internal_token = get("BLOBSD_INTERNAL_TOKEN")?;
        if internal_token.len() < MIN_INTERNAL_TOKEN_LEN {
            return Err(BootError::Config(format!(
                "BLOBSD_INTERNAL_TOKEN must be at least {MIN_INTERNAL_TOKEN_LEN} characters"
            )));
        }

        let public_get = match vars.get("BLOBSD_PUBLIC_GET").map(String::as_str) {
            Some("true") | Some("1") => true,
            Some("false") | Some("0") => false,
            _ => {
                return Err(BootError::Config(
                    "BLOBSD_PUBLIC_GET must be true or false".to_string(),
                ))
            }
        };

        Ok(Config {
            listen,
            data_dir,
            bucket_prefix: "blobs",
            backend,
            fs_root,
            bucket,
            s3_endpoint,
            aws_access_key_id,
            aws_secret_access_key,
            aws_region,
            public_url,
            max_blob_bytes: max_blob_bytes_raw,
            allow_hex_pubkeys,
            allow_all,
            internal_token,
            public_get,
        })
    }

    /// Staging directory for in-flight uploads, derived from `data_dir`.
    pub fn staging_dir(&self) -> String {
        format!("{}/staging", self.data_dir)
    }

}

fn validate_public_url(url: &str) -> Result<(), BootError> {
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err(BootError::Config(
            "BLOBSD_PUBLIC_URL must start with http:// or https://".to_string(),
        ));
    }
    if url.ends_with('/') {
        // The x-tag match is exact string equality; documents tell operators
        // to configure the origin bare, so a trailing slash means the signer
        // and the server disagree about what was configured.
        return Err(BootError::Config(
            "BLOBSD_PUBLIC_URL must not end with '/' (x tags compare exact strings)".to_string(),
        ));
    }
    if url.len() > 2048 || url.contains(['?', '#', ' ']) || !url.is_ascii() {
        return Err(BootError::Config(
            "BLOBSD_PUBLIC_URL must be plain ASCII origin[/path], <= 2048 chars, no '?'/'#'"
                .to_string(),
        ));
    }
    Ok(())
}

fn validate_bucket_name(name: &str) -> Result<(), BootError> {
    let ok_len = (3..=63).contains(&name.len());
    let ok_chars = name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '.');
    let ok_edges = name
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphanumeric())
        && name.chars().last().is_some_and(|c| c.is_ascii_alphanumeric());
    if !(ok_len && ok_chars && ok_edges) {
        return Err(BootError::Config(format!(
            "BLOBSD_BUCKET {name:?} is not an S3 bucket name (3-63 chars [a-z0-9.-], alphanumeric edges)"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_vars() -> HashMap<String, String> {
        let mut vars = HashMap::new();
        vars.insert("BLOBSD_LISTEN".to_string(), "127.0.0.1:8090".to_string());
        vars.insert("BLOBSD_DATA_DIR".to_string(), "/tmp/blobsd-test".to_string());
        vars.insert(
            "BLOBSD_PUBLIC_URL".to_string(),
            "https://blobsd.example.com".to_string(),
        );
        vars.insert("BLOBSD_BUCKET".to_string(), "fragment-dev".to_string());
        vars.insert(
            "S3_ENDPOINT".to_string(),
            "http://127.0.0.1:9000".to_string(),
        );
        vars.insert("AWS_ACCESS_KEY_ID".to_string(), "minio".to_string());
        vars.insert("AWS_SECRET_ACCESS_KEY".to_string(), "miniosecret".to_string());
        vars.insert("AWS_REGION".to_string(), "us-east-1".to_string());
        // A real test npub generated by tests/auth.rs fixtures' keypair.
        vars.insert(
            "BLOBSD_ALLOW_NPUBS".to_string(),
            crate::auth::tests_fixtures::test_npub(),
        );
        vars.insert(
            "BLOBSD_INTERNAL_TOKEN".to_string(),
            "dev-internal-token-0123456789".to_string(),
        );
        vars.insert("BLOBSD_PUBLIC_GET".to_string(), "true".to_string());
        vars
    }

    #[test]
    fn accepts_valid_config_with_default_limit() {
        let cfg = Config::from_map(&base_vars()).expect("valid base config");
        assert_eq!(cfg.max_blob_bytes, DEFAULT_MAX_BLOB_BYTES);
        assert_eq!(cfg.bucket_prefix, "blobs");
        // Ingress check echoed back: exactly one allowlisted key, lowercase hex.
        assert_eq!(cfg.allow_hex_pubkeys.len(), 1);
        let only = cfg.allow_hex_pubkeys.iter().next().expect("one entry");
        assert_eq!(only.len(), 64);
        assert!(
            only.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()),
            "allowlist keys are normalized to lowercase hex"
        );
    }

    #[test]
    fn rejects_unknown_blobsd_var_by_name() {
        let mut vars = base_vars();
        vars.insert("BLOBSD_LISSEN".to_string(), "1.2.3.4:99".to_string());
        let err = Config::from_map(&vars).expect_err("typo must fail boot");
        let text = err.to_string();
        assert!(text.contains("BLOBSD_LISSEN"), "names the offender: {text}");
        assert!(text.contains("BLOBSD_LISTEN"), "lists the recognized set");
    }

    #[test]
    fn rejects_missing_required_var() {
        let mut vars = base_vars();
        vars.remove("BLOBSD_BUCKET");
        let err = Config::from_map(&vars).expect_err("missing var must fail");
        assert!(err.to_string().contains("BLOBSD_BUCKET"));
    }

    #[test]
    fn rejects_bad_values_explicitly() {
        // Sequential mutations of the same map keep these checks honest
        // against process-global state-free parsing.
        struct Case {
            label: &'static str,
            mutate: fn(&mut HashMap<String, String>),
            expect_fragment: &'static str,
        }
        let cases = [
            Case {
                label: "bad listen",
                mutate: |v| {
                    v.insert("BLOBSD_LISTEN".into(), "not-an-address".into());
                },
                expect_fragment: "BLOBSD_LISTEN",
            },
            Case {
                label: "trailing-slash public url",
                mutate: |v| {
                    v.insert("BLOBSD_PUBLIC_URL".into(), "https://blobsd.example.com/".into());
                },
                expect_fragment: "must not end with '/'",
            },
            Case {
                label: "public_get garbage",
                mutate: |v| {
                    v.insert("BLOBSD_PUBLIC_GET".into(), "yes".into());
                },
                expect_fragment: "true or false",
            },
            Case {
                label: "oversize limit",
                mutate: |v| {
                    v.insert("BLOBSD_MAX_BLOB_BYTES".into(), (MAX_MAX_BLOB_BYTES * 2).to_string());
                },
                expect_fragment: "between",
            },
            Case {
                label: "empty allowlist",
                mutate: |v| {
                    v.insert("BLOBSD_ALLOW_NPUBS".into(), "".into());
                },
                expect_fragment: "at least one valid npub",
            },
            Case {
                label: "malformed npub",
                mutate: |v| {
                    v.insert("BLOBSD_ALLOW_NPUBS".into(), "npub1zzzz-not-really".into());
                },
                expect_fragment: "BLOBSD_ALLOW_NPUBS",
            },
            Case {
                label: "short internal token",
                mutate: |v| {
                    v.insert("BLOBSD_INTERNAL_TOKEN".into(), "tiny".into());
                },
                expect_fragment: "at least 16",
            },
        ];
        for case in cases {
            let mut vars = base_vars();
            (case.mutate)(&mut vars);
            let err = Config::from_map(&vars)
                .map(|_| ())
                .expect_err(case.label);
            assert!(
                err.to_string().contains(case.expect_fragment),
                "{}: wanted {:?} in {:?}",
                case.label,
                case.expect_fragment,
                err.to_string()
            );
        }
    }

    #[test]
    fn explicit_max_blob_bytes_overrides_default() {
        let mut vars = base_vars();
        vars.insert("BLOBSD_MAX_BLOB_BYTES".to_string(), "4096".to_string());
        let cfg = Config::from_map(&vars).expect("valid override");
        assert_eq!(cfg.max_blob_bytes, 4096);
    }
}
