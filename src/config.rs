use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};

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
    pub user: Option<UserConfig>,

    #[serde(default)]
    pub core: Option<CoreConfig>,

    // AI/LLM settings (global)
    #[serde(default)]
    pub model: Option<String>,

    #[serde(default)]
    pub api_base: Option<String>,

    #[serde(default)]
    pub api_key: Option<String>,

    #[serde(default)]
    pub message_level: MessageLevel,
}

/// Repository-specific configuration (from .helix/config.toml)
#[derive(Debug, Deserialize, Serialize, Clone, Default)]
pub struct RepoConfig {
    #[serde(default)]
    pub core: Option<CoreConfig>,

    #[serde(default)]
    pub ignore: Option<IgnoreConfig>,

    #[serde(default)]
    pub hooks: Option<HooksConfig>,

    #[serde(default, rename = "remote")]
    pub remotes: HashMap<String, RemoteConfig>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct UserConfig {
    pub name: String,
    pub email: String,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct CoreConfig {
    #[serde(default = "default_true")]
    pub auto_refresh: bool,

    #[serde(default)]
    pub editor: Option<String>,

    #[serde(default = "default_refresh_interval")]
    pub refresh_interval_secs: u64,
}

fn default_true() -> bool {
    true
}
fn default_refresh_interval() -> u64 {
    2
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct IgnoreConfig {
    #[serde(default)]
    pub patterns: Vec<String>,

    #[serde(default = "default_true")]
    pub respect_gitignore: bool,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct HooksConfig {
    #[serde(default)]
    pub pre_commit: Option<String>,

    #[serde(default)]
    pub pre_push: Option<String>,

    #[serde(default)]
    pub post_commit: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct RemoteConfig {
    pub url: String,

    #[serde(default)]
    pub fetch: Option<String>,
}

/// Merged configuration (global + repo-specific)
#[derive(Debug, Clone)]
pub struct Config {
    pub user: Option<UserConfig>,
    pub core: CoreConfig,
    pub ignore: Option<IgnoreConfig>,
    pub hooks: Option<HooksConfig>,
    pub remotes: HashMap<String, RemoteConfig>,

    // AI/LLM settings
    pub model: String,
    pub api_base: String,
    pub api_key: Option<String>,
    pub message_level: MessageLevel,
}

impl Config {
    /// Load configuration with precedence: repo > global > defaults
    pub fn load(repo_path: Option<&Path>) -> Result<Self> {
        // Load global config
        let global = Self::load_global()
            .context("Failed to load global config")?
            .unwrap_or_else(GlobalConfig::default);

        // Load repo config if repo path provided
        let repo = if let Some(path) = repo_path {
            Self::load_repo(path)?
        } else {
            None
        };

        // Merge configs
        Ok(Self::merge(global, repo))
    }

    /// Load only global config (for commands that don't need repo)
    pub fn load_global_only() -> Result<Self> {
        let global = Self::load_global()
            .context("Failed to load global config")?
            .unwrap_or_else(GlobalConfig::default);

        Ok(Self::merge(global, None))
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

    fn load_repo(repo_path: &Path) -> Result<Option<RepoConfig>> {
        let config_path = repo_path.join(".helix/config.toml");

        if !config_path.exists() {
            return Ok(None);
        }

        let content = fs::read_to_string(&config_path)
            .with_context(|| format!("Failed to read {}", config_path.display()))?;

        let config: RepoConfig =
            toml::from_str(&content).context("Failed to parse .helix/config.toml")?;

        Ok(Some(config))
    }

    fn merge(global: GlobalConfig, repo: Option<RepoConfig>) -> Self {
        // Start with global config
        let mut core = global.core.unwrap_or_else(|| CoreConfig {
            auto_refresh: true,
            editor: None,
            refresh_interval_secs: 2,
        });

        let mut ignore = None;
        let mut hooks = None;
        let mut remotes = HashMap::new();

        // Merge repo config if present
        if let Some(repo_cfg) = repo {
            // Repo core settings override global
            if let Some(repo_core) = repo_cfg.core {
                core = repo_core;
            }

            ignore = repo_cfg.ignore;
            hooks = repo_cfg.hooks;
            remotes = repo_cfg.remotes;
        }

        Self {
            user: global.user,
            core,
            ignore,
            hooks,
            remotes,
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
            user: None,
            core: Some(CoreConfig {
                auto_refresh: true,
                editor: None,
                refresh_interval_secs: 2,
            }),
            model: Some("claude-sonnet-4".to_string()),
            api_base: Some("https://api.anthropic.com".to_string()),
            api_key: None,
            message_level: MessageLevel::Normal,
        }
    }
}
