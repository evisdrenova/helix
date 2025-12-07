/*
This file defines the sync engine that handles one-time import from .git/index during 'helix init'.

After initialization, Helix operates independently:
- .helix/helix.idx is the ONLY canonical source of truth
- No ongoing sync with .git/index
- All Helix operations update .helix/helix.idx only

State model for EntryFlags:

We model three worlds:
- HEAD         (last committed state from Git)
- helix.idx    (Helix's canonical index, replaces .git/index)
- working tree (files on disk)

Bits:

- TRACKED   -> this path exists in helix.idx (was in .git/index during import)
- STAGED    -> helix.idx differs from HEAD for this path (index != HEAD)
- MODIFIED  -> working tree differs from helix.idx (working != helix.idx)
- DELETED   -> tracked in helix.idx but missing from working tree
- UNTRACKED -> not in helix.idx, but discovered via FSMonitor

This file (sync.rs) only handles the one-time import during 'helix init'.
It compares **index vs HEAD** to set TRACKED and STAGED flags.
MODIFIED / DELETED / UNTRACKED are set by FSMonitor / working-tree operations.
*/

use super::format::{Entry, EntryFlags, Header};
use super::reader::Reader;
use super::writer::Writer;

use crate::helix_index::hash;
use crate::ignore::IgnoreRules;
use crate::index::GitIndex;

use anyhow::{Context, Result};
use hash::compute_blob_oid;
use indicatif::{ProgressBar, ProgressStyle};
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

    /// One-time import from Git to create initial Helix index
    /// TODO: import other git items like refs, objects, etc.
    pub fn import_from_git(&self) -> Result<()> {
        wait_for_git_lock(&self.repo_path, Duration::from_secs(5))?;

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

        // Handle brand-new repo with no .git/index yet
        if !git_index_path.exists() {
            let header = Header::new(current_generation + 1, 0);
            let writer = Writer::new_canonical(&self.repo_path);
            writer.write(&header, &[])?;

            return Ok(());
        }

        let git_index = GitIndex::open(&self.repo_path)?;
        let entries = self.build_helix_index_entries(&git_index)?;
        let header = Header::new(current_generation + 1, entries.len() as u32);

        let writer = Writer::new_canonical(&self.repo_path);
        writer.write(&header, &entries)?;

        Ok(())
    }

    fn build_helix_index_entries(&self, git_index: &GitIndex) -> Result<Vec<Entry>> {
        let index_entries: Vec<_> = git_index.entries().collect();
        let entry_count = index_entries.len();

        if entry_count == 0 {
            return Ok(Vec::new());
        }

        let ignore_rules = IgnoreRules::load(&self.repo_path);

        let head_tree = self.load_full_head_tree()?;

        let pb = ProgressBar::new(entry_count as u64);
        pb.set_style(
            ProgressStyle::with_template(
                "{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] \
             {pos}/{len} entries ({eta})",
            )?
            .progress_chars(">-"),
        );

        // Build entries in parallel, updating the progress bar as we go
        let entries: Vec<Entry> = index_entries
            .into_par_iter()
            .map_init(
                || (pb.clone(), ignore_rules.clone()),
                |(pb, ignore_rules), e| {
                    pb.inc(1);

                    // Check if ignored
                    let path = Path::new(&e.path);
                    if ignore_rules.should_ignore(path) {
                        return None;
                    }

                    self.build_helix_entry_from_git_entry(&e, &head_tree).ok()
                },
            )
            .flatten() // Remove None values
            .collect();

        pb.finish_with_message("helix index built");

        Ok(entries)
    }

    fn build_helix_entry_from_git_entry(
        &self,
        git_index_entry: &crate::index::IndexEntry,
        head_tree: &HashMap<PathBuf, Vec<u8>>,
    ) -> Result<Entry> {
        let path = PathBuf::from(&git_index_entry.path);
        let full_path = self.repo_path.join(&path);

        let mut flags = EntryFlags::TRACKED;

        // Git's index snapshot blob-hash
        let index_git_oid = git_index_entry.oid.as_bytes();

        // Helix stores its own hash (of the Git oid bytes)
        let helix_oid = hash::hash_bytes(index_git_oid);

        // STAGED check: index vs HEAD
        // check if the hashed head oid from git head  is the same as the hashed helix oid from .git/index
        // if the same then the file is staged, if they're are different then the file is not staged
        // if it's a new file it will default be being staged
        let is_staged = head_tree
            .get(&path)
            .map(|head_git_oid| head_git_oid.as_slice() != index_git_oid)
            .unwrap_or(true);

        if is_staged {
            flags |= EntryFlags::STAGED;
        }

        let was_in_head = head_tree.contains_key(&path);

        // Always check working tree vs. index, this will catch repos with no commits yet
        if full_path.exists() && full_path.is_file() {
            let working_content = fs::read(&full_path)?;
            let working_git_oid = compute_blob_oid(&working_content);

            if &working_git_oid != index_git_oid {
                flags |= EntryFlags::MODIFIED;
            }
        } else if was_in_head {
            // Only mark DELETED if file was in HEAD
            // (Don't mark new staged files as deleted if they don't exist)
            flags |= EntryFlags::DELETED;
        }

        let (mtime_sec, file_size) = if full_path.exists() {
            let metadata = fs::metadata(&full_path)?;
            let mtime = metadata
                .modified()?
                .duration_since(std::time::UNIX_EPOCH)?
                .as_secs();
            (mtime, metadata.len())
        } else {
            (git_index_entry.mtime as u64, git_index_entry.size as u64)
        };

        Ok(Entry {
            path,
            size: file_size,
            mtime_sec,
            mtime_nsec: 0,
            flags,
            merge_conflict_stage: 0,
            file_mode: git_index_entry.file_mode,
            oid: helix_oid,
            reserved: [0; 33],
        })
    }

    /// Get the current repo's HEAD commit and return a hashmap of all paths in the tree
    fn load_full_head_tree(&self) -> Result<HashMap<PathBuf, Vec<u8>>> {
        let repo = gix::open(&self.repo_path).context("Failed to open repository with gix")?;

        let commit = match repo.head()?.peel_to_commit() {
            Ok(commit) => commit,
            Err(_) => return Ok(HashMap::new()),
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
            .into_par_iter()
            .filter_map(|record| {
                if record.mode.is_blob() {
                    let path = PathBuf::from(record.filepath.to_string());
                    let oid_bytes = record.oid.to_owned().as_bytes().to_vec();
                    Some((path, oid_bytes))
                } else {
                    None
                }
            })
            .collect();
        println!("HEAD tree has {} files", map.len());
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
    fn test_import_from_git() -> Result<()> {
        let temp_dir = TempDir::new()?;
        init_test_repo(temp_dir.path())?;

        fs::write(temp_dir.path().join("test.txt"), "hello")?;
        Command::new("git")
            .args(&["add", "test.txt"])
            .current_dir(temp_dir.path())
            .output()?;

        let syncer = SyncEngine::new(temp_dir.path());
        syncer.import_from_git()?;

        let reader = Reader::new(temp_dir.path());
        assert!(reader.exists());

        let data = reader.read()?;
        assert_eq!(data.entries.len(), 1);
        assert_eq!(data.entries[0].path, PathBuf::from("test.txt"));
        assert!(data.entries[0].flags.contains(EntryFlags::TRACKED));
        assert!(data.entries[0].flags.contains(EntryFlags::STAGED));

        Ok(())
    }

    #[test]
    fn test_import_empty_repo() -> Result<()> {
        let temp_dir = TempDir::new()?;
        init_test_repo(temp_dir.path())?;

        // No files added to Git
        let syncer = SyncEngine::new(temp_dir.path());
        syncer.import_from_git()?;

        let reader = Reader::new(temp_dir.path());
        let data = reader.read()?;

        // Should create empty index
        assert_eq!(data.entries.len(), 0);
        assert_eq!(data.header.generation, 1);

        Ok(())
    }

    #[test]
    fn test_import_increments_generation() -> Result<()> {
        let temp_dir = TempDir::new()?;
        init_test_repo(temp_dir.path())?;

        fs::write(temp_dir.path().join("test.txt"), "hello")?;
        Command::new("git")
            .args(&["add", "test.txt"])
            .current_dir(temp_dir.path())
            .output()?;

        let syncer = SyncEngine::new(temp_dir.path());

        // First import
        syncer.import_from_git()?;
        let reader = Reader::new(temp_dir.path());
        let data1 = reader.read()?;
        assert_eq!(data1.header.generation, 1);

        // Second import (re-init)
        syncer.import_from_git()?;
        let data2 = reader.read()?;
        assert_eq!(data2.header.generation, 2);

        Ok(())
    }

    #[test]
    fn test_import_detects_staged() -> Result<()> {
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

        // Import
        let syncer = SyncEngine::new(temp_dir.path());
        syncer.import_from_git()?;

        let reader = Reader::new(temp_dir.path());
        let data = reader.read()?;

        assert_eq!(data.entries.len(), 1);
        assert!(data.entries[0].flags.contains(EntryFlags::STAGED));

        Ok(())
    }

    #[test]
    fn test_import_detects_unstaged() -> Result<()> {
        let temp_dir = TempDir::new()?;
        init_test_repo(temp_dir.path())?;

        // Create, stage, and commit a file
        fs::write(temp_dir.path().join("stable.txt"), "content")?;
        Command::new("git")
            .args(&["add", "stable.txt"])
            .current_dir(temp_dir.path())
            .output()?;
        Command::new("git")
            .args(&["commit", "-m", "add stable file"])
            .current_dir(temp_dir.path())
            .output()?;

        // Import
        let syncer = SyncEngine::new(temp_dir.path());
        syncer.import_from_git()?;

        let reader = Reader::new(temp_dir.path());
        let data = reader.read()?;

        assert_eq!(data.entries.len(), 1);

        let entry = &data.entries[0];
        assert!(entry.flags.contains(EntryFlags::TRACKED));
        assert!(
            !entry.flags.contains(EntryFlags::STAGED),
            "Committed file that matches HEAD should not be staged"
        );

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
