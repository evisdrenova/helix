/*
Async event system that watches for file changes and tracks:
 - files that are modified
 - staging changes
 -
*/

use anyhow::{Context, Result};
use crossbeam_channel::{bounded, Receiver, Sender};
use dashmap::DashSet;
use notify::event::{
    AccessKind, AccessMode, CreateKind, DataChange, ModifyKind, RemoveKind, RenameMode,
};
use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use serde::Deserialize;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use std::{env, thread};

// file system monitor that tracks changes to files in a repository
pub struct FSMonitor {
    _watcher: RecommendedWatcher, // watches for file events
    dirty: Arc<DashSet<PathBuf>>, // lockfree reads and writes hashset to track which files are dirty/have been modified
    repo_root: PathBuf,
    _batch_thread: thread::JoinHandle<()>,
    ignore_rules: Arc<IgnoreRules>,
    index_dirty: Arc<DashSet<PathBuf>>, // track index changes like staging changes
}

impl FSMonitor {
    pub fn new(repo_path: &Path) -> Result<Self> {
        let repo_root = repo_path
            .canonicalize()
            .context("Failed to canonicalize repo path")?;

        let dirty = Arc::new(DashSet::new());
        let dirty_clone = dirty.clone();
        let repo_root_clone = repo_root.clone();

        let ignore_rules = Arc::new(IgnoreRules::load(&repo_root));
        let ignore_rules_clone = ignore_rules.clone();

        let index_dirty = Arc::new(DashSet::new());
        let index_dirty_clone = index_dirty.clone();

        // channel for batching events that are given off by the OS when things happen to the files
        let (tx, rx): (Sender<Event>, Receiver<Event>) = bounded(1000);

        // spawns a thread
        let batch_thread = thread::spawn(move || {
            Self::batch_events(
                rx,
                dirty_clone,
                repo_root_clone,
                ignore_rules_clone,
                index_dirty_clone,
            )
        });

        // creates file watcher
        let watcher = RecommendedWatcher::new(
            move |res: notify::Result<Event>| {
                if let Ok(event) = res {
                    let _ = tx.send(event);
                }
            },
            Config::default().with_poll_interval(Duration::from_millis(100)),
        )?;

        Ok(Self {
            _watcher: watcher,
            dirty,
            repo_root,
            _batch_thread: batch_thread,
            ignore_rules,
            index_dirty,
        })
    }

    pub fn start_watching_repo(&mut self) -> Result<()> {
        self._watcher
            .watch(&self.repo_root, RecursiveMode::Recursive)
            .context("Failed to start watching repository")?;

        // watch the index specifically for staging changes
        let index_path = self.repo_root.join(".git/index");
        if index_path.exists() {
            self._watcher
                .watch(&index_path, RecursiveMode::NonRecursive)
                .context("Failed to watch .git/index")?;
        }

        Ok(())
    }

    pub fn get_dirty_files(&self) -> Vec<PathBuf> {
        self.dirty.iter().map(|entry| entry.key().clone()).collect()
    }

    pub fn dirty_count(&self) -> usize {
        self.dirty.len()
    }

    pub fn is_dirty(&self, path: &Path) -> bool {
        self.dirty.contains(path)
    }

    pub fn clear_dirty(&self) {
        self.dirty.clear();
    }

    pub fn clear_single_path(&self, path: &Path) {
        self.dirty.remove(path);
    }

    pub fn index_changed(&self) -> bool {
        !self.index_dirty.is_empty()
    }

    pub fn clear_index_flag(&self) {
        self.index_dirty.clear();
    }

    // event batching thread - processes events in 10ms windows
    fn batch_events(
        rx: Receiver<Event>,
        dirty: Arc<DashSet<PathBuf>>,
        repo_root: PathBuf,
        ignore_rules: Arc<IgnoreRules>,
        index_dirty: Arc<DashSet<PathBuf>>,
    ) {
        let batch_interval = Duration::from_millis(10);
        let mut batch = Vec::new();

        loop {
            // collect events
            let deadline = std::time::Instant::now() + batch_interval;

            while let Ok(event) = rx.recv_deadline(deadline) {
                batch.push(event);

                // if we hit the deadline, process batch
                if std::time::Instant::now() >= deadline {
                    break;
                }
            }

            if !batch.is_empty() {
                Self::process_events_in_batch(
                    &batch,
                    &dirty,
                    &repo_root,
                    &ignore_rules,
                    &index_dirty,
                );
                batch.clear();
            }
        }
    }

    fn process_events_in_batch(
        events: &[Event],
        dirty: &DashSet<PathBuf>,
        repo_root: &Path,
        ignore_rules: &IgnoreRules,
        index_dirty: &DashSet<PathBuf>,
    ) {
        for event in events {
            if !Self::is_relevant_event(&event.kind) {
                continue;
            }

            for path in &event.paths {
                if path.ends_with(".git/index") {
                    index_dirty.insert(PathBuf::from(".git/index"));
                    continue;
                }

                if let Ok(rel_path) = path.strip_prefix(repo_root) {
                    if ignore_rules.should_ignore(rel_path) {
                        continue;
                    }
                    dirty.insert(rel_path.to_path_buf());
                }
            }
        }
    }

    fn is_relevant_event(kind: &EventKind) -> bool {
        match kind {
            EventKind::Modify(ModifyKind::Data(DataChange::Any | DataChange::Content)) => true,
            EventKind::Modify(ModifyKind::Metadata(_)) => true,
            EventKind::Create(CreateKind::File | CreateKind::Any) => true,
            EventKind::Remove(RemoveKind::File | RemoveKind::Any) => true,
            EventKind::Modify(ModifyKind::Name(RenameMode::Any | RenameMode::Both)) => true,
            EventKind::Access(AccessKind::Close(AccessMode::Write)) => true,
            EventKind::Access(_) => false,
            _ => true,
        }
    }
}
// Ignore rules from multiple sources with clear precedence:
/// 1. Built-in patterns (always apply)
/// 2. .gitignore (repo-level git rules)
/// 3. .helix/config.toml (repo-level helix rules)
/// 4. ~/.helix.toml (user-level helix rules)
pub struct IgnoreRules {
    patterns: Vec<IgnorePattern>,
}

#[derive(Debug, Clone)]
enum IgnorePattern {
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
struct IgnoreSection {
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
    fn built_in_patterns() -> Vec<IgnorePattern> {
        vec![
            // Git internal (except .git/index which we track explicitly)
            IgnorePattern::Directory(".git/".to_string()),
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
            // Helix's own cache (don't watch our own index!)
            IgnorePattern::Directory(".helix/".to_string()),
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_fsmonitor_basic() -> Result<()> {
        let temp_dir = tempfile::tempdir()?;
        let repo_path = temp_dir.path();

        // Initialize a git repo
        std::process::Command::new("git")
            .args(&["init"])
            .current_dir(repo_path)
            .output()?;

        let mut monitor = FSMonitor::new(repo_path)?;
        monitor.start_watching_repo()?;

        // Give it time to start
        thread::sleep(Duration::from_millis(100));

        // Create a file
        let test_file = repo_path.join("test.txt");
        fs::write(&test_file, "hello")?;

        // Give FSMonitor time to detect change
        thread::sleep(Duration::from_millis(100));

        // Check if file is marked as dirty
        let dirty_files = monitor.get_dirty_files();
        assert!(
            !dirty_files.is_empty(),
            "Should have detected file creation"
        );
        println!("dirty fails {:?}", dirty_files);
        let file_name = Path::new("test.txt");
        assert!(dirty_files.contains(&file_name.to_path_buf()));

        // Clear and verify
        monitor.clear_dirty();
        assert_eq!(monitor.dirty_count(), 0);

        Ok(())
    }

    #[test]
    fn test_built_in_ignore_patterns() {
        let rules = IgnoreRules {
            patterns: IgnoreRules::built_in_patterns(),
        };

        // Should ignore .git directory (except .git/index)
        assert!(rules.should_ignore(Path::new(".git/objects/abc123")));
        assert!(rules.should_ignore(Path::new(".git/refs/heads/main")));

        // Should ignore build directories
        assert!(rules.should_ignore(Path::new("target/debug/main")));
        assert!(rules.should_ignore(Path::new("node_modules/package/index.js")));
        assert!(rules.should_ignore(Path::new("__pycache__/module.pyc")));

        // Should ignore editor temp files
        assert!(rules.should_ignore(Path::new(".foo.swp")));
        assert!(rules.should_ignore(Path::new("file.txt~")));
        assert!(rules.should_ignore(Path::new(".DS_Store")));

        // Should ignore helix cache
        assert!(rules.should_ignore(Path::new(".helix/helix.idx")));

        // Should NOT ignore normal files
        assert!(!rules.should_ignore(Path::new("src/main.rs")));
        assert!(!rules.should_ignore(Path::new("README.md")));
        assert!(!rules.should_ignore(Path::new("Cargo.toml")));
    }

    #[test]
    fn test_gitignore_loading() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();

        // Create .gitignore
        fs::write(
            repo.join(".gitignore"),
            "# Comment\n\n*.log\nbuild/\ntemp\n",
        )
        .unwrap();

        let rules = IgnoreRules::load(repo);

        assert!(rules.should_ignore(Path::new("debug.log")));
        assert!(rules.should_ignore(Path::new("build/output.txt")));
        assert!(rules.should_ignore(Path::new("temp")));
        assert!(!rules.should_ignore(Path::new("normal.txt")));
    }

    #[test]
    fn test_helix_repo_config() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();

        // Create .helix/config.toml
        fs::create_dir(repo.join(".helix")).unwrap();
        fs::write(
            repo.join(".helix/config.toml"),
            r#"
[ignore]
patterns = ["*.tmp", "cache/", "ignored_file"]
"#,
        )
        .unwrap();

        let rules = IgnoreRules::load(repo);

        assert!(rules.should_ignore(Path::new("file.tmp")));
        assert!(rules.should_ignore(Path::new("cache/data.db")));
        assert!(rules.should_ignore(Path::new("ignored_file")));
        assert!(!rules.should_ignore(Path::new("normal.txt")));
    }

    #[test]
    fn test_combined_ignore_sources() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();

        // .gitignore
        fs::write(repo.join(".gitignore"), "*.git_ignored\n").unwrap();

        // .helix/config.toml
        fs::create_dir(repo.join(".helix")).unwrap();
        fs::write(
            repo.join(".helix/config.toml"),
            r#"
[ignore]
patterns = ["*.helix_ignored"]
"#,
        )
        .unwrap();

        let rules = IgnoreRules::load(repo);

        // From .gitignore
        assert!(rules.should_ignore(Path::new("file.git_ignored")));

        // From .helix/config.toml
        assert!(rules.should_ignore(Path::new("file.helix_ignored")));

        // Built-in
        assert!(rules.should_ignore(Path::new("target/debug/main")));

        // Normal file
        assert!(!rules.should_ignore(Path::new("src/main.rs")));
    }

    #[test]
    fn test_pattern_parsing() {
        // Directory pattern
        let dir_pattern = IgnoreRules::parse_pattern("target/");
        assert!(matches!(dir_pattern, IgnorePattern::Directory(_)));

        // Extension pattern
        let ext_pattern = IgnoreRules::parse_pattern("*.log");
        assert!(matches!(ext_pattern, IgnorePattern::Extension(_)));

        // Substring pattern
        let sub_pattern = IgnoreRules::parse_pattern("node_modules");
        assert!(matches!(sub_pattern, IgnorePattern::Substring(_)));
    }

    #[test]
    fn test_directory_pattern_matching() {
        let rules = IgnoreRules {
            patterns: vec![IgnorePattern::Directory("target/".to_string())],
        };

        assert!(rules.should_ignore(Path::new("target/debug/main")));
        assert!(rules.should_ignore(Path::new("target/release/lib.so")));
        assert!(rules.should_ignore(Path::new("src/target/nested"))); // Contains "target/"
        assert!(!rules.should_ignore(Path::new("src/retarget.rs"))); // "target" but not "target/"
    }

    #[test]
    fn test_extension_pattern_matching() {
        let rules = IgnoreRules {
            patterns: vec![IgnorePattern::Extension(".swp".to_string())],
        };

        assert!(rules.should_ignore(Path::new(".file.swp")));
        assert!(rules.should_ignore(Path::new("main.rs.swp")));
        assert!(!rules.should_ignore(Path::new("swap.txt")));
    }

    #[test]
    fn test_substring_pattern_matching() {
        let rules = IgnoreRules {
            patterns: vec![IgnorePattern::Substring("temp".to_string())],
        };

        assert!(rules.should_ignore(Path::new("temp")));
        assert!(rules.should_ignore(Path::new("temp/file.txt")));
        assert!(rules.should_ignore(Path::new("my_temp_file.txt")));
        assert!(rules.should_ignore(Path::new("src/template.rs"))); // Contains "temp"
        assert!(!rules.should_ignore(Path::new("src/main.rs")));
    }

    #[test]
    fn test_git_index_not_ignored() {
        let rules = IgnoreRules {
            patterns: IgnoreRules::built_in_patterns(),
        };

        // .git/index should NOT be in the ignore patterns
        // It's handled specially in the event processing
        assert!(rules.should_ignore(Path::new(".git/objects/abc")));
        assert!(rules.should_ignore(Path::new(".git/refs/heads/main")));

        // These should be caught by the .git/ directory pattern
        // but .git/index is explicitly excluded in process_events_in_batch
    }
}
