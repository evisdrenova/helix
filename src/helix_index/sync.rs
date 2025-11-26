/*
This file defines the sync engine that creates the helix.index from .git/index and the HEAD commit and keeps it in sync. This is the read
only index that the helix CLI command use to be very fast. It supports a full sync mode when the repo is first initialized and an incremental
sync mode as the user runs commands and makes file changes.

States:
- Tracked -> git has a history of this file either from the repo or in the last commit
    - unmodified -> the file has not been changed since the last commit
    - modified -> the file has changes since the last commit but those changes have not yet been added to the staging area (UNSTAGED)
    - staged -> the file has been added to the staging area with it's most recent changes and is ready to be committed
- untracked -> Git has no history of these files in the last commit or in the repo. These are typically new files that have been been created in the working directory but have not yet been added. This also includes files that have been explicitly ignored by .gitignore.
*/

use super::fingerprint::generate_repo_fingerprint;
use super::format::{Entry, EntryFlags, Header};
use super::reader::Reader;
use super::writer::Writer;

use crate::helix_index::utils::{read_git_index_checksum, system_time_to_parts};
use crate::index::GitIndex;

use anyhow::{Context, Result};
use rayon::prelude::*;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub struct SyncEngine {
    repo_path: PathBuf,
}

impl SyncEngine {
    pub fn new(repo_path: &Path) -> Self {
        Self {
            repo_path: repo_path.to_path_buf(),
        }
    }

    /// Full sync to rebuild the entire helix index
    /// Should only run on first run or corruption
    pub fn full_sync(&self) -> Result<()> {
        wait_for_git_lock(&self.repo_path, Duration::from_secs(5))?;

        // Read current generation, if error set it to 0 to restart generation
        let reader = Reader::new(&self.repo_path);
        let current_generation = if reader.exists() {
            reader
                .read()
                .ok()
                .map(|data| data.header.generation)
                .unwrap_or(0)
        } else {
            0
        };

        let git_index_path = self.repo_path.join(".git/index");
        if !git_index_path.exists() {
            anyhow::bail!(".git/index does not exist, either initialize a repo with `git init` or `helix init`");
        }

        let git_metadata = fs::metadata(&git_index_path)?;
        let git_mtime = git_metadata.modified()?;
        let (git_mtime_sec, git_mtime_nsec) = system_time_to_parts(git_mtime);
        let git_size = git_metadata.len();
        let git_checksum = read_git_index_checksum(&git_index_path)?;

        let index = GitIndex::open(&self.repo_path)?;

        let entries = self.build_helix_index_entries(&index)?;

        let repo_fingerprint = generate_repo_fingerprint(&self.repo_path)?;
        let header = Header::new(
            current_generation + 1,
            repo_fingerprint,
            git_mtime_sec,
            git_mtime_nsec,
            git_size,
            git_checksum,
            entries.len() as u32,
        );

        let writer = Writer::new(&self.repo_path);
        writer.write(&header, &entries)?;

        Ok(())
    }

    /// Incremental sync to only update changed entries that are tracked bia FSMonitor in the .git/index
    /// Takes in a list of changed paths from the user's working directory and then finds it in the .git/index
    /// and then creates the entry and adds it to the helix.index
    pub fn incremental_sync(&self, changed_paths: &[PathBuf]) -> Result<()> {
        wait_for_git_lock(&self.repo_path, Duration::from_secs(5))?;

        let helix_index = Reader::new(&self.repo_path);
        if !helix_index.exists() {
            return self.full_sync();
        };

        let mut index_data = helix_index.read()?;
        let git_index = GitIndex::open(&self.repo_path)?;

        let head_tree = self.load_head_tree_for_paths(changed_paths)?;

        for changed_path in changed_paths {
            match git_index.get_entry(changed_path) {
                Ok(git_entry) => {
                    if let Some(existing) = index_data
                        .entries
                        .iter_mut()
                        .find(|e| &e.path == changed_path)
                    {
                        *existing =
                            self.build_helix_entry_from_git_entry(&git_entry, &head_tree)?;
                    } else {
                        let entry =
                            self.build_helix_entry_from_git_entry(&git_entry, &head_tree)?;
                        index_data.entries.push(entry);
                    }
                }
                Err(_) => {
                    // File removed from index
                    index_data.entries.retain(|e| &e.path != changed_path);
                }
            }
        }

        let git_index_path = self.repo_path.join(".git/index");
        let git_metadata = fs::metadata(&git_index_path)?;
        let git_mtime = git_metadata.modified()?;
        let (git_mtime_sec, git_mtime_nsec) = system_time_to_parts(git_mtime);
        let git_size = git_metadata.len();
        let git_checksum = read_git_index_checksum(&git_index_path)?;

        index_data.header.generation += 1;
        index_data.header.git_index_mtime_sec = git_mtime_sec;
        index_data.header.git_index_mtime_nsec = git_mtime_nsec;
        index_data.header.git_index_size = git_size;
        index_data.header.git_index_checksum = git_checksum;
        index_data.header.entry_count = index_data.entries.len() as u32;

        let writer = Writer::new(&self.repo_path);
        writer.write(&index_data.header, &index_data.entries)?;

        Ok(())
    }

    /// Given a list of paths, create a hashmap of type Entry from the paths that are matched in the Head commit
    fn load_head_tree_for_paths(&self, paths: &[PathBuf]) -> Result<HashMap<PathBuf, Vec<u8>>> {
        let repo = gix::open(&self.repo_path)?;
        let head = match repo.head()?.peel_to_commit() {
            Ok(c) => c,
            Err(_) => return Ok(HashMap::new()),
        };

        let tree = head.tree()?;

        let mut map = HashMap::new();
        for path in paths {
            if let Ok(Some(entry)) = tree.lookup_entry_by_path(path) {
                map.insert(path.clone(), entry.id().as_bytes().to_vec());
            }
        }

        Ok(map)
    }

    fn build_helix_index_entries(&self, index: &GitIndex) -> Result<Vec<Entry>> {
        let head_tree = self.load_full_head_tree()?;

        let index_entries: Vec<_> = index.entries().collect();

        let entries: Result<Vec<Entry>> = index_entries
            .into_par_iter()
            .map(|e| self.build_helix_entry_from_git_entry(&e, &head_tree))
            .collect();

        entries
    }

    /// Build our index with files from .git/index so we can compare them against the head commit
    /// to understand the status of the files (staged, not-staged). Originally we checked for the file metadata here
    /// but that didn't scale well for large repos. There is a tiny window where the size & mtime might be stale if someone modifies files between git operations and the first helix run. FSMonitor will pick up that metadata as soon as the user starts making
    /// any changes anyways so i think it's worth the trade-off.
    fn build_helix_entry_from_git_entry(
        &self,
        index_entry: &crate::index::IndexEntry,
        head_tree: &HashMap<PathBuf, Vec<u8>>,
    ) -> Result<Entry> {
        let path = PathBuf::from(&index_entry.path);

        let mut flags = EntryFlags::TRACKED;
        let index_oid = index_entry.oid.as_bytes();

        let is_staged = head_tree
            .get(&path)
            .map(|head_oid| head_oid.as_slice() != index_oid)
            .unwrap_or(true); // Not in HEAD = new file = staged by default

        if is_staged {
            flags |= EntryFlags::STAGED;
        }

        Ok(Entry {
            path,
            size: index_entry.size as u64,
            mtime_sec: index_entry.mtime as u64,
            mtime_nsec: 0,
            flags,
            oid: *index_oid,
            reserved: [0; 64],
        })
    }

    /// Get the current repos HEAD commit and return a hashmap of all of the paths in the tree
    fn load_full_head_tree(&self) -> Result<HashMap<PathBuf, Vec<u8>>> {
        let repo = gix::open(&self.repo_path).context("Failed to open repository with gix")?;

        let commit = match repo.head()?.peel_to_commit() {
            Ok(commit) => commit,
            Err(_) => {
                return Ok(HashMap::new());
            }
        };

        let tree = commit
            .tree()
            .context("Failed to get tree from commit")?
            .to_owned();

        let mut recorder = gix::traverse::tree::Recorder::default();
        tree.traverse()
            .breadthfirst(&mut recorder)
            .context("Failed to traverse tree")?;

        let map: HashMap<PathBuf, Vec<u8>> = recorder
            .records
            .into_iter()
            .filter_map(|record| {
                // Only include blobs (files), ignoring other entries like submodules or trees
                if record.mode.is_blob() {
                    let path = PathBuf::from(record.filepath.to_string());
                    let oid_bytes = record.oid.to_owned().as_bytes().to_vec();
                    Some((path, oid_bytes))
                } else {
                    None
                }
            })
            .collect();

        Ok(map)
    }
}

fn wait_for_git_lock(repo_path: &Path, timeout: Duration) -> Result<()> {
    let lock_path = repo_path.join(".git/index.lock");
    let start = Instant::now();

    while lock_path.exists() {
        if start.elapsed() > timeout {
            anyhow::bail!("Timeout waiting for .git/index.lock");
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;
    use tempfile::TempDir;

    fn init_test_repo(path: &Path) -> Result<()> {
        fs::create_dir_all(path.join(".git"))?;
        Command::new("git")
            .args(&["init"])
            .current_dir(path)
            .output()?;

        // Configure git
        Command::new("git")
            .args(&["config", "user.name", "Test"])
            .current_dir(path)
            .output()?;
        Command::new("git")
            .args(&["config", "user.email", "test@test.com"])
            .current_dir(path)
            .output()?;

        Ok(())
    }

    #[test]
    fn test_sync_creates_index() -> Result<()> {
        let temp_dir = TempDir::new()?;
        init_test_repo(temp_dir.path())?;

        // Create and add a file
        fs::write(temp_dir.path().join("test.txt"), "hello")?;
        Command::new("git")
            .args(&["add", "test.txt"])
            .current_dir(temp_dir.path())
            .output()?;

        // Sync
        let syncer = SyncEngine::new(temp_dir.path());
        syncer.full_sync()?;

        // Verify helix.idx was created
        let reader = Reader::new(temp_dir.path());
        assert!(reader.exists());

        let data = reader.read()?;
        assert_eq!(data.header.generation, 1);
        assert_eq!(data.entries.len(), 1);
        assert_eq!(data.entries[0].path, PathBuf::from("test.txt"));
        assert!(data.entries[0].flags.contains(EntryFlags::TRACKED));
        assert!(data.entries[0].flags.contains(EntryFlags::STAGED));

        Ok(())
    }

    #[test]
    fn test_sync_increments_generation() -> Result<()> {
        let temp_dir = TempDir::new()?;
        init_test_repo(temp_dir.path())?;

        fs::write(temp_dir.path().join("test.txt"), "hello")?;
        Command::new("git")
            .args(&["add", "test.txt"])
            .current_dir(temp_dir.path())
            .output()?;

        let syncer = SyncEngine::new(temp_dir.path());

        // First sync
        syncer.full_sync()?;
        let reader = Reader::new(temp_dir.path());
        let data1 = reader.read()?;
        assert_eq!(data1.header.generation, 1);

        // Second sync
        syncer.full_sync()?;
        let data2 = reader.read()?;
        assert_eq!(data2.header.generation, 2);

        Ok(())
    }

    #[test]
    fn test_sync_detects_staged() -> Result<()> {
        let temp_dir = TempDir::new()?;
        init_test_repo(temp_dir.path())?;

        // Create file and commit
        fs::write(temp_dir.path().join("test.txt"), "hello")?;
        Command::new("git")
            .args(&["add", "test.txt"])
            .current_dir(temp_dir.path())
            .output()?;
        Command::new("git")
            .args(&["commit", "-m", "initial"])
            .current_dir(temp_dir.path())
            .output()?;

        // Modify and stage
        fs::write(temp_dir.path().join("test.txt"), "world")?;
        Command::new("git")
            .args(&["add", "test.txt"])
            .current_dir(temp_dir.path())
            .output()?;

        // Sync
        let syncer = SyncEngine::new(temp_dir.path());
        syncer.full_sync()?;

        let reader = Reader::new(temp_dir.path());
        let data = reader.read()?;

        assert_eq!(data.entries.len(), 1);
        assert!(data.entries[0].flags.contains(EntryFlags::STAGED));

        Ok(())
    }

    #[test]
    fn test_wait_for_git_lock_timeout() -> Result<()> {
        let temp_dir = TempDir::new()?;
        init_test_repo(temp_dir.path())?;

        // Create a fake lock file
        let lock_path = temp_dir.path().join(".git/index.lock");
        fs::write(&lock_path, "")?;

        // Should timeout
        let result = wait_for_git_lock(temp_dir.path(), Duration::from_millis(100));
        assert!(result.is_err());

        // Clean up
        fs::remove_file(&lock_path)?;

        Ok(())
    }
}
