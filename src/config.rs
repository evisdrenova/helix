use anyhow::{Context, Result};
use serde::Deserialize;
use std::{fs, path::PathBuf};

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "lowercase")]
pub enum MessageLevel {
    Quiet,   // one-line subject only
    Normal,  // subject + short body
    Verbose, // subject + detailed body
}

#[derive(Debug, Deserialize, Clone)]
pub struct Config {
    pub model: String,
    pub api_key: String,
    pub message_level: MessageLevel,
}

impl Config {
    pub fn load() -> Result<Self> {
        let mut path: PathBuf = dirs::home_dir().context("could not find home directory")?;
        path.push(".helix.toml");

        let s = fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;

        let cfg: Config = toml::from_str(&s).context("failed to parse .helix.toml")?;

        Ok(cfg)
    }
}
