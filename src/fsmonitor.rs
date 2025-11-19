/*
Async event system that watches for file changes and sends file names to index to be added
*/

use anyhow::{Context, Result};
use crossbeam_channel::{bounded, Receiver, Sender};
use dashmap::DashSet;
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

        // channel for batchin events
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

    pub fn start_watching(&mut self) -> Result<()> {
        self._watcher
            .watch(&self.repo_root, RecursiveMode::Recursive)
            .context("Failed to start watching repository")?;
        Ok(())
    }

    pub fn get_dirty(&self) -> Vec<PathBuf> {
        self.dirty.iter().map(|entry| entry.key().clone()).collect()
    }
}
