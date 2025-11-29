use serde::Deserialize;
use std::env;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

// Ignore rules from multiple sources with clear precedence:
/// 1. Built-in patterns (always apply)
/// 2. .gitignore (repo-level git rules)
/// 3. .helix/config.toml (repo-level helix rules)
/// 4. ~/.helix.toml (user-level helix rules)
pub struct IgnoreRules {
    pub patterns: Vec<IgnorePattern>,
}

#[derive(Debug, Clone)]
pub enum IgnorePattern {
    /// Directory pattern: "target/"
    Directory(String),
    /// Extension pattern: "*.log"
    Extension(String),
    /// Exact substring match: "node_modules"
    Substring(String),
}

#[derive(Debug, Default, Deserialize)]
struct HelixConfig {
    #[serde(default)]
    ignore: IgnoreSection,
}

#[derive(Debug, Default, Deserialize)]
pub struct IgnoreSection {
    #[serde(default)]
    patterns: Vec<String>,
}

impl IgnoreRules {
    pub fn load(repo_path: &Path) -> Self {
        let mut patterns = Vec::new();

        // Load in order of precedence
        patterns.extend(Self::built_in_patterns());
        patterns.extend(Self::load_gitignore(repo_path));
        patterns.extend(Self::load_helix_repo_config(repo_path));
        patterns.extend(Self::load_helix_global_config());

        Self { patterns }
    }

    /// Built-in patterns that always apply
    /// These cover common build artifacts and system files
    pub fn built_in_patterns() -> Vec<IgnorePattern> {
        vec![
            // Git internal (except .git/index which we track explicitly)
            IgnorePattern::Directory(".git/objects/".to_string()),
            IgnorePattern::Directory(".git/refs/".to_string()),
            IgnorePattern::Directory(".git/logs/".to_string()),
            // Build directories
            IgnorePattern::Directory("target/".to_string()),
            IgnorePattern::Directory("node_modules/".to_string()),
            IgnorePattern::Directory("__pycache__/".to_string()),
            IgnorePattern::Directory(".venv/".to_string()),
            IgnorePattern::Directory("dist/".to_string()),
            IgnorePattern::Directory("build/".to_string()),
            // Editor temporary files
            IgnorePattern::Extension(".swp".to_string()),
            IgnorePattern::Extension(".swo".to_string()),
            IgnorePattern::Extension("~".to_string()),
            IgnorePattern::Substring(".DS_Store".to_string()),
            // Helix's own cache directory (but NOT helix.idx which we watch)
            IgnorePattern::Directory(".helix/cache/".to_string()),
            IgnorePattern::Substring(".helix.idx.new".to_string()), // Temp file during write
        ]
    }

    /// Load patterns from .gitignore
    fn load_gitignore(repo_path: &Path) -> Vec<IgnorePattern> {
        let gitignore_path = repo_path.join(".gitignore");
        let mut patterns = Vec::new();

        if let Ok(file) = File::open(gitignore_path) {
            let reader = BufReader::new(file);
            for line in reader.lines().flatten() {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }

                patterns.push(Self::parse_pattern(line));
            }
        }

        patterns
    }

    /// Load patterns from .helix/config.toml (repo-level)
    fn load_helix_repo_config(repo_path: &Path) -> Vec<IgnorePattern> {
        let config_path = repo_path.join(".helix/config.toml");
        Self::load_helix_toml(&config_path)
    }

    /// Load patterns from ~/.helix.toml (global)
    fn load_helix_global_config() -> Vec<IgnorePattern> {
        let home = match env::var("HOME") {
            Ok(h) => h,
            Err(_) => return Vec::new(),
        };

        let config_path = Path::new(&home).join(".helix.toml");
        Self::load_helix_toml(&config_path)
    }

    fn load_helix_toml(path: &Path) -> Vec<IgnorePattern> {
        if !path.exists() {
            return Vec::new();
        }

        let Ok(contents) = std::fs::read_to_string(path) else {
            return Vec::new();
        };

        let Ok(cfg) = toml::from_str::<HelixConfig>(&contents) else {
            return Vec::new();
        };

        cfg.ignore
            .patterns
            .into_iter()
            .map(|p| Self::parse_pattern(&p))
            .collect()
    }

    /// Parse a pattern string into the appropriate type
    fn parse_pattern(s: &str) -> IgnorePattern {
        if s.ends_with('/') {
            IgnorePattern::Directory(s.to_string())
        } else if s.starts_with('*') {
            // Extract extension: "*.log" -> ".log"
            IgnorePattern::Extension(s.strip_prefix('*').unwrap_or(s).to_string())
        } else {
            IgnorePattern::Substring(s.to_string())
        }
    }

    /// Check if a path should be ignored
    pub fn should_ignore(&self, path: &Path) -> bool {
        let path_str = path.to_string_lossy();

        for pattern in &self.patterns {
            match pattern {
                IgnorePattern::Directory(dir) => {
                    // Match if path contains the directory
                    // e.g., "target/" matches "target/debug/main"
                    if path_str.starts_with(dir.as_str()) || path_str.contains(dir.as_str()) {
                        return true;
                    }
                }
                IgnorePattern::Extension(ext) => {
                    // Match file extension
                    // e.g., ".swp" matches "file.swp"
                    if path_str.ends_with(ext.as_str()) {
                        return true;
                    }
                }
                IgnorePattern::Substring(substr) => {
                    // Match substring anywhere in path
                    // e.g., "node_modules" matches "lib/node_modules/pkg"
                    if path_str.contains(substr.as_str()) {
                        return true;
                    }
                }
            }
        }

        false
    }
}
