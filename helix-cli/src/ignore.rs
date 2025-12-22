use globset::{Glob, GlobSet, GlobSetBuilder};
use serde::Deserialize;
use std::env;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use crate::init::HelixConfig;

/// Ignore rules from multiple sources with clear precedence:
/// 1. Built-in patterns (always apply)
/// 2. .gitignore (repo-level git rules)
/// 3. helix.toml (repo-level helix rules)
/// 4. ~/.helix.toml (user-level helix rules)

#[derive(Debug, Clone)]
pub struct IgnoreRules {
    globset: GlobSet,
}

impl IgnoreRules {
    pub fn load(repo_path: &Path) -> Self {
        let mut builder = GlobSetBuilder::new();

        Self::add_built_in_patterns(&mut builder);
        Self::add_gitignore_patterns(&mut builder, repo_path);
        Self::add_helix_repo_patterns(&mut builder, repo_path);
        Self::add_helix_global_patterns(&mut builder);

        let globset = builder.build().unwrap_or_else(|_| {
            // Fallback: just use built-in patterns
            let mut fallback = GlobSetBuilder::new();
            Self::add_built_in_patterns(&mut fallback);
            fallback.build().unwrap()
        });

        Self { globset }
    }

    /// Built-in patterns that always apply
    /// These cover Helix internal files
    fn add_built_in_patterns(builder: &mut GlobSetBuilder) {
        // Git internal directory
        Self::add_pattern(builder, ".git");
        Self::add_pattern(builder, ".git/**");

        // Helix internal directory
        Self::add_pattern(builder, ".helix");
        Self::add_pattern(builder, ".helix/**");

        // Helix cache directory
        Self::add_pattern(builder, ".helix/cache");
        Self::add_pattern(builder, ".helix/cache/**");

        // Temporary files during index writes
        Self::add_pattern(builder, "**/.helix.idx.new");
        Self::add_pattern(builder, ".helix.idx.new");
    }

    /// Load patterns from .gitignore
    fn add_gitignore_patterns(builder: &mut GlobSetBuilder, repo_path: &Path) {
        let gitignore_path = repo_path.join(".gitignore");

        if let Ok(file) = File::open(gitignore_path) {
            let reader = BufReader::new(file);
            for line in reader.lines().flatten() {
                let line = line.trim();

                // Skip empty lines and comments
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }

                Self::add_pattern(builder, line);
            }
        }
    }

    /// Load patterns from helix.toml (repo-level)
    fn add_helix_repo_patterns(builder: &mut GlobSetBuilder, repo_path: &Path) {
        let config_path = repo_path.join("helix.toml");
        Self::add_helix_toml_patterns(builder, &config_path);
    }

    /// Load patterns from ~/.helix.toml (global)
    fn add_helix_global_patterns(builder: &mut GlobSetBuilder) {
        if let Ok(home) = env::var("HOME") {
            let config_path = Path::new(&home).join(".helix.toml");
            Self::add_helix_toml_patterns(builder, &config_path);
        }
    }

    fn add_helix_toml_patterns(builder: &mut GlobSetBuilder, path: &Path) {
        if !path.exists() {
            return;
        }

        let Ok(contents) = std::fs::read_to_string(path) else {
            return;
        };

        let Ok(cfg) = toml::from_str::<HelixConfig>(&contents) else {
            return;
        };

        for pattern in cfg.ignore.patterns {
            Self::add_pattern(builder, &pattern);
        }
    }

    /// Add a pattern to the builder, normalizing it for proper matching
    fn add_pattern(builder: &mut GlobSetBuilder, pattern: &str) {
        let pattern = pattern.trim();

        if pattern.is_empty() {
            return;
        }

        // Normalize the pattern for both files and directories
        let normalized_patterns = Self::normalize_pattern(pattern);

        for normalized in normalized_patterns {
            if let Ok(glob) = Glob::new(&normalized) {
                builder.add(glob);
            }
        }
    }

    /// Normalize a pattern to handle various formats
    /// Returns multiple patterns to match both files and directories
    fn normalize_pattern(pattern: &str) -> Vec<String> {
        let mut patterns = Vec::new();

        // Remove leading "./" if present
        let pattern = pattern.strip_prefix("./").unwrap_or(pattern);

        // Handle different pattern formats
        if pattern.ends_with('/') {
            // "target/" -> match directory
            let base = pattern.trim_end_matches('/');
            patterns.push(format!("{}", base));
            patterns.push(format!("{}/**", base));
        } else if pattern.ends_with("/*") {
            // ".helix/*" -> match directory contents
            let base = pattern.strip_suffix("/*").unwrap();
            patterns.push(format!("{}", base));
            patterns.push(format!("{}/**", base));
        } else if pattern.ends_with("/**") {
            // ".git/**" -> match directory and contents
            let base = pattern.strip_suffix("/**").unwrap();
            patterns.push(format!("{}", base));
            patterns.push(format!("{}/**", base));
        } else if pattern.contains('/') {
            // "src/test" -> match as path
            patterns.push(pattern.to_string());
            // Also match as directory
            patterns.push(format!("{}/**", pattern));
        } else if pattern.starts_with("*.") {
            // "*.log" -> extension pattern (already correct)
            patterns.push(pattern.to_string());
        } else {
            // "node_modules" or "HEAD" -> match as file or directory
            // Match exact name
            patterns.push(pattern.to_string());
            // Match as directory
            patterns.push(format!("{}/**", pattern));
            // Match anywhere in tree
            patterns.push(format!("**/{}", pattern));
            patterns.push(format!("**/{}/**", pattern));
        }

        patterns
    }

    /// Check if a path should be ignored
    pub fn should_ignore(&self, path: &Path) -> bool {
        self.globset.is_match(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_pattern_directory_slash() {
        let patterns = IgnoreRules::normalize_pattern("target/");
        assert!(patterns.contains(&"target".to_string()));
        assert!(patterns.contains(&"target/**".to_string()));
    }

    #[test]
    fn test_normalize_pattern_glob_star() {
        let patterns = IgnoreRules::normalize_pattern(".helix/*");
        assert!(patterns.contains(&".helix".to_string()));
        assert!(patterns.contains(&".helix/**".to_string()));
    }

    #[test]
    fn test_normalize_pattern_double_star() {
        let patterns = IgnoreRules::normalize_pattern(".git/**");
        assert!(patterns.contains(&".git".to_string()));
        assert!(patterns.contains(&".git/**".to_string()));
    }

    #[test]
    fn test_normalize_pattern_simple_name() {
        let patterns = IgnoreRules::normalize_pattern("HEAD");
        assert!(patterns.contains(&"HEAD".to_string()));
        assert!(patterns.contains(&"HEAD/**".to_string()));
        assert!(patterns.contains(&"**/HEAD".to_string()));
        assert!(patterns.contains(&"**/HEAD/**".to_string()));
    }

    #[test]
    fn test_normalize_pattern_extension() {
        let patterns = IgnoreRules::normalize_pattern("*.log");
        assert!(patterns.contains(&"*.log".to_string()));
    }

    #[test]
    fn test_should_ignore_helix_directory() {
        let mut builder = GlobSetBuilder::new();
        IgnoreRules::add_pattern(&mut builder, ".helix");
        IgnoreRules::add_pattern(&mut builder, ".helix/*");
        let rules = IgnoreRules {
            globset: builder.build().unwrap(),
        };

        assert!(rules.should_ignore(Path::new(".helix")));
        assert!(rules.should_ignore(Path::new(".helix/HEAD")));
        assert!(rules.should_ignore(Path::new(".helix/helix.idx")));
        assert!(rules.should_ignore(Path::new(".helix/cache/data.bin")));
    }

    #[test]
    fn test_should_ignore_git_directory() {
        let mut builder = GlobSetBuilder::new();
        IgnoreRules::add_pattern(&mut builder, ".git");
        let rules = IgnoreRules {
            globset: builder.build().unwrap(),
        };

        assert!(rules.should_ignore(Path::new(".git")));
        assert!(rules.should_ignore(Path::new(".git/HEAD")));
        assert!(rules.should_ignore(Path::new(".git/objects/abc123")));
    }

    #[test]
    fn test_should_ignore_file_by_name() {
        let mut builder = GlobSetBuilder::new();
        IgnoreRules::add_pattern(&mut builder, "HEAD");
        let rules = IgnoreRules {
            globset: builder.build().unwrap(),
        };

        assert!(rules.should_ignore(Path::new("HEAD")));
        assert!(rules.should_ignore(Path::new(".helix/HEAD")));
        assert!(rules.should_ignore(Path::new("subdir/HEAD")));
    }

    #[test]
    fn test_should_ignore_extension() {
        let mut builder = GlobSetBuilder::new();
        IgnoreRules::add_pattern(&mut builder, "*.log");
        let rules = IgnoreRules {
            globset: builder.build().unwrap(),
        };

        assert!(rules.should_ignore(Path::new("debug.log")));
        assert!(rules.should_ignore(Path::new("logs/error.log")));
        assert!(!rules.should_ignore(Path::new("logfile.txt")));
    }

    #[test]
    fn test_should_not_ignore_normal_files() {
        let mut builder = GlobSetBuilder::new();
        IgnoreRules::add_built_in_patterns(&mut builder);
        let rules = IgnoreRules {
            globset: builder.build().unwrap(),
        };

        assert!(!rules.should_ignore(Path::new("src/main.rs")));
        assert!(!rules.should_ignore(Path::new("README.md")));
        assert!(!rules.should_ignore(Path::new("Cargo.toml")));
    }

    #[test]
    fn test_should_ignore_helix_toml_config() {
        let temp_dir = tempfile::tempdir().unwrap();
        let repo_path = temp_dir.path();

        // Create helix.toml with custom patterns
        std::fs::write(
            repo_path.join("helix.toml"),
            r#"
[ignore]
patterns = [
    "*.tmp",
    "cache/",
    "node_modules"
]
"#,
        )
        .unwrap();

        let rules = IgnoreRules::load(repo_path);

        assert!(rules.should_ignore(Path::new("file.tmp")));
        assert!(rules.should_ignore(Path::new("cache/data.db")));
        assert!(rules.should_ignore(Path::new("node_modules/pkg/index.js")));
    }

    #[test]
    fn test_should_ignore_gitignore_patterns() {
        let temp_dir = tempfile::tempdir().unwrap();
        let repo_path = temp_dir.path();

        // Create .gitignore
        std::fs::write(
            repo_path.join(".gitignore"),
            "*.log\ntarget/\n# Comment\n\n.DS_Store\n",
        )
        .unwrap();

        let rules = IgnoreRules::load(repo_path);

        assert!(rules.should_ignore(Path::new("debug.log")));
        assert!(rules.should_ignore(Path::new("target/debug/main")));
        assert!(rules.should_ignore(Path::new(".DS_Store")));
    }

    #[test]
    fn test_pattern_with_leading_dot_slash() {
        let patterns = IgnoreRules::normalize_pattern("./target/");
        assert!(patterns.contains(&"target".to_string()));
        assert!(patterns.contains(&"target/**".to_string()));
    }

    #[test]
    fn test_built_in_patterns_ignore_helix_internals() {
        let mut builder = GlobSetBuilder::new();
        IgnoreRules::add_built_in_patterns(&mut builder);
        let rules = IgnoreRules {
            globset: builder.build().unwrap(),
        };

        // Should ignore .helix directory
        assert!(rules.should_ignore(Path::new(".helix")));
        assert!(rules.should_ignore(Path::new(".helix/HEAD")));
        assert!(rules.should_ignore(Path::new(".helix/helix.idx")));
        assert!(rules.should_ignore(Path::new(".helix/cache/data")));
        assert!(rules.should_ignore(Path::new(".helix/objects/abc123")));

        // Should ignore .git directory
        assert!(rules.should_ignore(Path::new(".git")));
        assert!(rules.should_ignore(Path::new(".git/HEAD")));
        assert!(rules.should_ignore(Path::new(".git/index")));

        // Should NOT ignore helix.toml (repo config)
        assert!(!rules.should_ignore(Path::new("helix.toml")));

        // Should NOT ignore normal files
        assert!(!rules.should_ignore(Path::new("src/main.rs")));
        assert!(!rules.should_ignore(Path::new("README.md")));
    }
}
