/*
Async event system that watches for file changes and tracks:
 - files that are modified in working tree
 - canonical index changes (.helix/helix.idx)
 - git index changes (.git/index) for interop
 - triggers cache invalidation when needed
*/

use anyhow::{Context, Result};
use crossbeam_channel::{bounded, Receiver, Sender};
use dashmap::DashSet;
use notify::event::{
    AccessKind, AccessMode, CreateKind, DataChange, ModifyKind, RemoveKind, RenameMode,
};
use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crate::ignore::IgnoreRules;

// file system monitor that tracks changes to files in a repository
pub struct FSMonitor {
    _watcher: RecommendedWatcher, // watches for file events
    dirty: Arc<DashSet<PathBuf>>, // lockfree reads and writes hashset to track which files are dirty/have been modified
    repo_root: PathBuf,
    _batch_thread: thread::JoinHandle<()>,
    ignore_rules: Arc<IgnoreRules>,
    index_dirty: Arc<DashSet<PathBuf>>, // track index changes (both helix and git)
    cache_invalidator: Arc<Mutex<Option<Box<dyn Fn() + Send + Sync>>>>, // callback when canonical index changes
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

        let cache_invalidator = Arc::new(Mutex::new(None));
        let cache_invalidator_clone = cache_invalidator.clone();

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
                cache_invalidator_clone,
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
            cache_invalidator,
        })
    }

    pub fn start_watching_repo(&mut self) -> Result<()> {
        self._watcher
            .watch(&self.repo_root, RecursiveMode::Recursive)
            .context("Failed to start watching repository")?;

        // Watch helix canonical index
        let helix_index = self.repo_root.join(".helix/helix.idx");
        if helix_index.exists() {
            self._watcher
                .watch(&helix_index, RecursiveMode::NonRecursive)
                .context("Failed to watch .helix/helix.idx")?;
        }

        // Watch git index for interop
        let git_index = self.repo_root.join(".git/index");
        if git_index.exists() {
            self._watcher
                .watch(&git_index, RecursiveMode::NonRecursive)
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

    /// Check if any index changed (helix or git)
    pub fn index_changed(&self) -> bool {
        !self.index_dirty.is_empty()
    }

    /// Check if specifically the helix canonical index changed
    pub fn helix_index_changed(&self) -> bool {
        self.index_dirty
            .iter()
            .any(|entry| entry.key().ends_with(".helix/helix.idx"))
    }

    /// Check if specifically the git index changed
    pub fn git_index_changed(&self) -> bool {
        self.index_dirty
            .iter()
            .any(|entry| entry.key().ends_with(".git/index"))
    }

    pub fn clear_index_flag(&self) {
        self.index_dirty.clear();
    }

    /// Set callback to be invoked when canonical helix index changes
    /// This is useful for triggering cache invalidation/rebuilds
    pub fn set_cache_invalidator<F>(&self, callback: F)
    where
        F: Fn() + Send + Sync + 'static,
    {
        let mut invalidator = self.cache_invalidator.lock().unwrap();
        *invalidator = Some(Box::new(callback));
    }

    /// Clear the cache invalidator callback
    pub fn clear_cache_invalidator(&self) {
        let mut invalidator = self.cache_invalidator.lock().unwrap();
        *invalidator = None;
    }

    // event batching thread - processes events in 10ms windows
    fn batch_events(
        rx: Receiver<Event>,
        dirty: Arc<DashSet<PathBuf>>,
        repo_root: PathBuf,
        ignore_rules: Arc<IgnoreRules>,
        index_dirty: Arc<DashSet<PathBuf>>,
        cache_invalidator: Arc<Mutex<Option<Box<dyn Fn() + Send + Sync>>>>,
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
                    &cache_invalidator,
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
        cache_invalidator: &Mutex<Option<Box<dyn Fn() + Send + Sync>>>,
    ) {
        let mut helix_index_changed = false;

        for event in events {
            if !Self::is_relevant_event(&event.kind) {
                continue;
            }

            for path in &event.paths {
                // Check for helix canonical index changes
                if path.ends_with(".helix/helix.idx") {
                    index_dirty.insert(PathBuf::from(".helix/helix.idx"));
                    helix_index_changed = true;
                    continue;
                }

                // Check for git index changes (for interop)
                if path.ends_with(".git/index") {
                    index_dirty.insert(PathBuf::from(".git/index"));
                    continue;
                }

                // Track working tree file changes
                if let Ok(rel_path) = path.strip_prefix(repo_root) {
                    if ignore_rules.should_ignore(rel_path) {
                        continue;
                    }
                    dirty.insert(rel_path.to_path_buf());
                }
            }
        }

        // Trigger cache invalidation if helix index changed
        if helix_index_changed {
            if let Some(invalidator) = cache_invalidator.lock().unwrap().as_ref() {
                invalidator();
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
        println!("dirty files {:?}", dirty_files);
        let file_name = Path::new("test.txt");
        assert!(dirty_files.contains(&file_name.to_path_buf()));

        // Clear and verify
        monitor.clear_dirty();
        assert_eq!(monitor.dirty_count(), 0);

        Ok(())
    }

    #[test]
    fn test_helix_index_detection() -> Result<()> {
        let temp_dir = tempfile::tempdir()?;
        let repo_path = temp_dir.path();

        // Create .helix directory
        fs::create_dir(repo_path.join(".helix"))?;

        let mut monitor = FSMonitor::new(repo_path)?;
        monitor.start_watching_repo()?;

        thread::sleep(Duration::from_millis(100));

        // Simulate index change
        let index_path = repo_path.join(".helix/helix.idx");
        fs::write(&index_path, b"fake index")?;

        thread::sleep(Duration::from_millis(150));

        assert!(
            monitor.helix_index_changed(),
            "Should detect helix index change"
        );
        assert!(monitor.index_changed(), "Should report index changed");

        Ok(())
    }

    #[test]
    fn test_cache_invalidator_callback() -> Result<()> {
        use std::sync::atomic::{AtomicBool, Ordering};

        let temp_dir = tempfile::tempdir()?;
        let repo_path = temp_dir.path();

        fs::create_dir(repo_path.join(".helix"))?;

        let mut monitor = FSMonitor::new(repo_path)?;

        // Set callback
        let called = Arc::new(AtomicBool::new(false));
        let called_clone = called.clone();
        monitor.set_cache_invalidator(move || {
            called_clone.store(true, Ordering::SeqCst);
        });

        monitor.start_watching_repo()?;
        thread::sleep(Duration::from_millis(100));

        // Trigger index change
        let index_path = repo_path.join(".helix/helix.idx");
        fs::write(&index_path, b"fake index")?;

        thread::sleep(Duration::from_millis(150));

        assert!(
            called.load(Ordering::SeqCst),
            "Callback should have been called"
        );

        Ok(())
    }

    #[test]
    fn test_built_in_ignore_patterns() {
        let rules = IgnoreRules {
            patterns: IgnoreRules::built_in_patterns(),
        };

        // Should ignore .git subdirectories
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

        // Should ignore helix cache directory
        assert!(rules.should_ignore(Path::new(".helix/cache/data.bin")));

        // Should ignore temp files during write
        assert!(rules.should_ignore(Path::new(".helix/helix.idx.new")));

        // Should NOT ignore helix.idx itself (we watch this explicitly)
        assert!(!rules.should_ignore(Path::new(".helix/helix.idx")));

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
}
