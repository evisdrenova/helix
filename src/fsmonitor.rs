/*
Async event system that watches for file changes and tracks:
 - files that are modified
 - staging changes
*/

use anyhow::{Context, Result};
use crossbeam_channel::{bounded, Receiver, Sender};
use dashmap::DashSet;
use notify::event::{
    AccessKind, AccessMode, CreateKind, DataChange, ModifyKind, RemoveKind, RenameMode,
};
use notify::{event, Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

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

                    if Self::path_to_ignore(repo_root, path) {
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

    fn path_to_ignore(repo_root: &Path, path: &Path) -> bool {
        // ignore .git directory
        if path.starts_with(repo_root.join(".git")) && !path.ends_with(".git/index") {
            return true;
        }

        // Ignore common editor temp files
        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            if name.starts_with('.') && (name.ends_with(".swp") || name.ends_with('~')) {
                return true;
            }
            if name == ".DS_Store" {
                return true;
            }
        }

        false
    }
}

pub struct IgnoreRules {
    patterns: Vec<String>,
}

impl IgnoreRules {
    pub fn load(repo_path: &Path) -> Self {
        use std::fs::File;
        use std::io::{BufRead, BufReader};

        let mut patterns = vec![
            "target/".to_string(),
            "node_modules/".to_string(),
            "__pycache__/".to_string(),
            ".venv/".to_string(),
            "dist/".to_string(),
            "build/".to_string(),
        ];

        let gitignore_path = repo_path.join(".gitignore");
        if let Ok(file) = File::open(gitignore_path) {
            let reader = BufReader::new(file);
            for line in reader.lines().flatten() {
                let line = line.trim();
                if !line.is_empty() && !line.starts_with('#') {
                    patterns.push(line.to_string());
                }
            }
        }

        Self { patterns }
    }

    // todo: clean up the ignore, shoudl be deried from the helix.toml local config
    pub fn should_ignore(&self, path: &Path) -> bool {
        let path_str = path.to_string_lossy();

        for pattern in &self.patterns {
            if pattern.ends_with('/') {
                if path_str.contains(pattern) || path_str.starts_with(pattern) {
                    return true;
                }
            } else if pattern.starts_with('*') {
                if let Some(ext) = pattern.strip_prefix('*') {
                    if path_str.ends_with(ext) {
                        return true;
                    }
                }
            } else {
                if path_str.contains(pattern) {
                    return true;
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
            .args(&["init", repo_path.to_str().unwrap()])
            .output()?;

        let mut monitor = FSMonitor::new(repo_path)?;
        monitor.start_watching_repo()?;

        // Give it time to start
        thread::sleep(Duration::from_millis(100));

        // Create a file
        let test_file = repo_path.join("test.txt");
        fs::write(&test_file, "hello")?;

        // Give FSMonitor time to detect change
        thread::sleep(Duration::from_millis(50));

        // Check if file is marked as dirty
        let dirty_files = monitor.get_dirty_files();
        assert!(
            !dirty_files.is_empty(),
            "Should have detected file creation"
        );

        println!("Dirty files: {:?}", dirty_files);

        // Clear and verify
        monitor.clear_dirty();
        assert_eq!(monitor.dirty_count(), 0);

        Ok(())
    }

    #[test]
    fn test_should_ignore() {
        let repo_root = Path::new("/repo");

        // Should ignore
        assert!(FSMonitor::path_to_ignore(
            repo_root,
            &Path::new("/repo/.git/index"),
        ));
        assert!(FSMonitor::path_to_ignore(
            repo_root,
            &Path::new("/repo/file.swp"),
        ));
        assert!(FSMonitor::path_to_ignore(
            repo_root,
            &Path::new("/repo/.DS_Store"),
        ));
        assert!(FSMonitor::path_to_ignore(
            repo_root,
            &Path::new("/repo/node_modules/package/file.js")
        ));

        // Should not ignore
        assert!(!FSMonitor::path_to_ignore(
            repo_root,
            &Path::new("/repo/src/main.rs")
        ));
        assert!(!FSMonitor::path_to_ignore(
            repo_root,
            &Path::new("/repo/README.md")
        ));
    }
}
