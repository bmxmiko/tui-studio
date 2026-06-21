//! On-disk configuration: stored OAuth credentials per provider plus the
//! cached Code Assist project id (Gemini only). Everything lives under the
//! user's config dir, e.g. `~/.config/gemini-oauth-cli/`.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// Persisted OAuth tokens. Shared shape across providers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Credentials {
    pub access_token: String,
    /// Refresh tokens let us mint new access tokens without re-consent.
    pub refresh_token: String,
    /// Unix epoch seconds at which `access_token` stops being valid.
    pub expiry: u64,
    pub token_type: String,
    pub scope: Option<String>,
}

impl Credentials {
    /// True when the access token is expired (or about to be, within 60s).
    pub fn is_expired(&self) -> bool {
        now_secs() + 60 >= self.expiry
    }
}

/// Per-provider persisted state.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct ProviderStore {
    pub credentials: Option<Credentials>,
    /// Code Assist `cloudaicompanionProject` id (Gemini only).
    #[serde(default)]
    pub project_id: Option<String>,
}

/// Everything we cache between runs, keyed by provider.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Store {
    #[serde(default)]
    pub gemini: ProviderStore,
    #[serde(default)]
    pub claude: ProviderStore,
}

pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn config_dir() -> Result<PathBuf> {
    let dir = dirs::config_dir()
        .context("could not determine the OS config directory")?
        .join("gemini-oauth-cli");
    Ok(dir)
}

fn store_path() -> Result<PathBuf> {
    Ok(config_dir()?.join("store.json"))
}

impl Store {
    pub fn load() -> Result<Self> {
        let path = store_path()?;
        if !path.exists() {
            return Ok(Store::default());
        }
        let data = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        let store = serde_json::from_str(&data).unwrap_or_default();
        Ok(store)
    }

    pub fn save(&self) -> Result<()> {
        let dir = config_dir()?;
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("creating {}", dir.display()))?;
        let path = store_path()?;
        let data = serde_json::to_string_pretty(self)?;
        std::fs::write(&path, data).with_context(|| format!("writing {}", path.display()))?;
        // Best-effort: tighten permissions so tokens aren't world-readable.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        }
        Ok(())
    }
}
