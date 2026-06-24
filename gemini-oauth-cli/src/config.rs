//! On-disk configuration: stored OAuth credentials per provider plus the
//! cached Code Assist project id (Gemini only). Everything lives under the
//! user's config dir, e.g. `~/.config/gemini-oauth-cli/`.

use anyhow::{Context, Result};
use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// Which backend to talk to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProviderKind {
    #[default]
    Gemini,
    Claude,
}

impl ProviderKind {
    pub fn name(self) -> &'static str {
        match self {
            ProviderKind::Gemini => "Gemini",
            ProviderKind::Claude => "Claude",
        }
    }
    pub fn default_model(self) -> &'static str {
        match self {
            ProviderKind::Gemini => "gemini-2.5-flash",
            ProviderKind::Claude => "claude-sonnet-4-5",
        }
    }
}

/// A "Gem": a reusable, named assistant/persona (system instruction plus
/// optional defaults). Local equivalent of the Gemini app's Gems.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Gem {
    pub system: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub provider: Option<ProviderKind>,
    #[serde(default)]
    pub temperature: Option<f32>,
    #[serde(default)]
    pub thinking: Option<i32>,
}

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

/// In-flight OAuth state, persisted between a headless `login --no-browser`
/// (which generates the URL) and the later `login --code` (which exchanges it).
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct PendingAuth {
    /// PKCE verifier (Claude). Empty for the Gemini client-secret flow.
    pub verifier: String,
    pub state: String,
    pub redirect_uri: String,
}

/// Per-provider persisted state.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct ProviderStore {
    pub credentials: Option<Credentials>,
    /// Code Assist `cloudaicompanionProject` id (Gemini only).
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub pending: Option<PendingAuth>,
}

/// Everything we cache between runs, keyed by provider.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Store {
    #[serde(default)]
    pub gemini: ProviderStore,
    #[serde(default)]
    pub claude: ProviderStore,
    /// User-defined Gems (personas), keyed by name.
    #[serde(default)]
    pub gems: BTreeMap<String, Gem>,
}

/// Extract an authorization `code` (and optional `state`) from whatever the
/// user pasted: a full redirect URL, a `code#state` string, or a bare code.
pub fn extract_code(input: &str) -> (String, Option<String>) {
    let s = input.trim();
    // Full URL or query fragment containing code=...
    if let Some(q) = s.split('?').nth(1).or_else(|| s.contains("code=").then_some(s)) {
        let params: std::collections::HashMap<String, String> =
            url::form_urlencoded::parse(q.as_bytes()).into_owned().collect();
        if let Some(code) = params.get("code") {
            return (code.clone(), params.get("state").cloned());
        }
    }
    // "code#state" form (Claude console copy page).
    if let Some((code, state)) = s.split_once('#') {
        return (code.to_string(), Some(state.to_string()));
    }
    (s.to_string(), None)
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
