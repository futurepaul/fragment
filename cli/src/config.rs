//! CLI config: ~/.config/fragment/config.json (secret key + host, perms 0600).

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const DEFAULT_HOST: &str = "http://127.0.0.1:8789";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// hex-encoded nostr secp256k1 secret key
    pub secret_key: String,
    /// base URL of the celld public listener
    pub host: String,
}

pub fn config_path() -> Result<PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| anyhow!("cannot locate home directory"))?;
    Ok(home.join(".config").join("fragment").join("config.json"))
}

/// Load the config; Ok(None) if the file does not exist.
pub fn load() -> Result<Option<Config>> {
    let path = config_path()?;
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            let cfg: Config = serde_json::from_str(&text)
                .with_context(|| format!("invalid config file {}", path.display()))?;
            Ok(Some(cfg))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("cannot read {}", path.display())),
    }
}

/// Write the config with 0600 perms (it holds the secret key).
pub fn save(cfg: &Config) -> Result<PathBuf> {
    let path = config_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text = serde_json::to_string_pretty(cfg)? + "\n";

    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&path)?;
        f.write_all(text.as_bytes())?;
        // if the file already existed with looser perms, enforce 0600
        let mut perms = std::fs::metadata(&path)?.permissions();
        <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::set_mode(&mut perms, 0o600);
        std::fs::set_permissions(&path, perms)?;
    }
    #[cfg(not(unix))]
    {
        std::fs::write(&path, text)?;
    }
    Ok(path)
}

/// Host resolution: --host flag > config file > FRAGMENT_HOST env > default.
pub fn resolve_host(flag: Option<&str>, cfg: Option<&Config>) -> String {
    if let Some(h) = flag {
        return h.trim_end_matches('/').to_string();
    }
    if let Some(c) = cfg {
        return c.host.trim_end_matches('/').to_string();
    }
    if let Ok(h) = std::env::var("FRAGMENT_HOST") {
        if !h.is_empty() {
            return h.trim_end_matches('/').to_string();
        }
    }
    DEFAULT_HOST.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_resolution_order() {
        let cfg = Config {
            secret_key: "ab".into(),
            host: "http://cfg-host/".into(),
        };
        assert_eq!(resolve_host(Some("http://flag/"), Some(&cfg)), "http://flag");
        assert_eq!(resolve_host(None, Some(&cfg)), "http://cfg-host");
        // env/default branches depend on the process env; only assert default
        // when FRAGMENT_HOST is absent.
        if std::env::var("FRAGMENT_HOST").is_err() {
            assert_eq!(resolve_host(None, None), DEFAULT_HOST);
        }
    }
}
