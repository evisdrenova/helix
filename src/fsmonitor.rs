/*
Async event system that watches for file changes and sends file names to index to be added
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
}

impl FSMonitor {
    pub fn new(repo_path: &Path) -> Result<Self> {
        let repo_root = repo_path
            .canonicalize()
            .context("Failed to canonicalize repo path")?;

        let dirty = Arc::new(DashSet::new());
        let dirty_clone = dirty.clone();
        let repo_root_clone = repo_root.clone();

        // channel for batching events that are given off by the OS when things happen to the files
        let (tx, rx): (Sender<Event>, Receiver<Event>) = bounded(1000);

        // spawns a thread
        let batch_thread =
            thread::spawn(move || Self::batch_events(rx, dirty_clone, repo_root_clone));

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
        })
    }

    pub fn start_watching_repo(&mut self) -> Result<()> {
        self._watcher
            .watch(&self.repo_root, RecursiveMode::Recursive)
            .context("Failed to start watching repository")?;
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

    // event batching thread - processes events in 10ms windows
    fn batch_events(rx: Receiver<Event>, dirty: Arc<DashSet<PathBuf>>, repo_root: PathBuf) {
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
                Self::process_events_in_batch(&batch, &dirty, &repo_root);
                batch.clear();
            }
        }
    }

    fn process_events_in_batch(events: &[Event], dirty: &DashSet<PathBuf>, repo_root: &Path) {
        for event in events {
            if !Self::is_relevant_event(&event.kind) {
                continue;
            }
            for path in &event.paths {
                if Self::path_to_ignore(path, repo_root) {
                    continue;
                }

                if let Ok(rel_path) = path.strip_prefix(repo_root) {
                    dirty.insert(rel_path.to_path_buf());
                }
            }
        }
    }

    fn is_relevant_event(kind: &EventKind) -> bool {
        match kind {
            // file content changes
            EventKind::Modify(ModifyKind::Data(DataChange::Any | DataChange::Content)) => true,

            // file metadata changes (permissions, timestamps)
            EventKind::Modify(ModifyKind::Metadata(_)) => true,

            // file creation
            EventKind::Create(CreateKind::File | CreateKind::Any) => true,

            // file deletion
            EventKind::Remove(RemoveKind::File | RemoveKind::Any) => true,

            // renames
            EventKind::Modify(ModifyKind::Name(RenameMode::Any | RenameMode::Both)) => true,

            // access events are reads, ignore
            EventKind::Access(AccessKind::Close(AccessMode::Write)) => true,
            EventKind::Access(_) => false,

            // default is to include just to be safe
            _ => true,
        }
    }

    fn path_to_ignore(repo_root: &Path, path: &Path) -> bool {
        // ignore .git directory
        if path.starts_with(repo_root.join(".git")) {
            return true;
        }

        // ignore temp files, hidden files, etc.
        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            if name.contains(".swp") {
                return true; // Vim swap files
            }
            if name.starts_with('.') && name.ends_with('~') {
                return true; // backup files
            }
            if name.starts_with("__") && name.ends_with("__") {
                return true; // python cache
            }
            if name == ".DS_Store" {
                return true; // macOS
            }
        }

        // todo: update this to read from a .helixignore file, but for now and testing this is fine to ignore this stuff
        let path_str = path.to_string_lossy();
        if path_str.contains("/node_modules/")
            || path_str.contains("/target/")
            || path_str.contains("/.venv/")
            || path_str.contains("/__pycache__/")
        {
            return true;
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
