use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(rename_all = "lowercase")]
pub enum MessageLevel {
    Quiet,   // one-line subject only
    Normal,  // subject + short body
    Verbose, // subject + detailed body
}

impl Default for MessageLevel {
    fn default() -> Self {
        MessageLevel::Normal
    }
}

/// Global configuration (from ~/.helix.toml)
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct GlobalConfig {
    #[serde(default)]
    pub model: Option<String>,

    #[serde(default)]
    pub api_base: Option<String>,

    #[serde(default)]
    pub api_key: Option<String>,

    #[serde(default)]
    pub message_level: MessageLevel,
}

/// Repository-specific configuration (from helix.toml)
#[derive(Debug, Deserialize, Serialize, Clone, Default)]
pub struct RepoConfig {
    #[serde(default)]
    pub ignore: Option<IgnoreConfig>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct IgnoreConfig {
    #[serde(default)]
    pub patterns: Vec<String>,
}

/// Merged configuration (global + repo-specific)
#[derive(Debug, Clone)]
pub struct Config {
    pub model: String,
    pub api_base: String,
    pub api_key: Option<String>,
    pub message_level: MessageLevel,
}

impl Config {
    /// Load configuration with precedence: repo > global > defaults
    pub fn load() -> Result<Self> {
        // Load global config
        let global = Self::load_global()
            .context("Failed to load global config")?
            .unwrap_or_else(GlobalConfig::default);

        // Merge configs
        Ok(Self::merge(global))
    }

    fn load_global() -> Result<Option<GlobalConfig>> {
        let mut path = dirs::home_dir().context("Could not find home directory")?;
        path.push(".helix.toml");

        if !path.exists() {
            return Ok(None);
        }

        let content = fs::read_to_string(&path)
            .with_context(|| format!("Failed to read {}", path.display()))?;

        let config: GlobalConfig =
            toml::from_str(&content).context("Failed to parse .helix.toml")?;

        Ok(Some(config))
    }
    fn merge(global: GlobalConfig) -> Self {
        Self {
            model: global
                .model
                .unwrap_or_else(|| "claude-sonnet-4".to_string()),
            api_base: global
                .api_base
                .unwrap_or_else(|| "https://api.anthropic.com".to_string()),
            api_key: global.api_key,
            message_level: global.message_level,
        }
    }
}

impl Default for GlobalConfig {
    fn default() -> Self {
        Self {
            model: Some("claude-sonnet-4".to_string()),
            api_base: Some("https://api.anthropic.com".to_string()),
            api_key: None,
            message_level: MessageLevel::Normal,
        }
    }
}
