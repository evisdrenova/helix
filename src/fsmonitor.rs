/*
Async event system that watches for file changes and sends file names to index to be added
*/

use anyhow::{Context, Result};
use crossbeam_channel::{bounded, Receiver, Sender};
use dashmap::DashSet;
use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

pub struct FSMonitor {
    _watcher: RecommendedWatcher,
    dirty: Arc<DashSet<PathBuf>>,
    repo_root: PathBuf,
    _batch_thread: thread::JoinHandle<()>,
}
