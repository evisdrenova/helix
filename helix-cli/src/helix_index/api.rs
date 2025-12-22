use crate::helix_index::Writer;

use super::format::{Entry, EntryFlags};
use super::reader::{HelixIndex, Reader};
use super::sync::SyncEngine;
use super::verify::{Verifier, VerifyResult};
use anyhow::Result;
use helix_protocol::hash;
use rayon::prelude::*;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

pub struct HelixIndexData {
    repo_path: PathBuf,
    data: HelixIndex,
}

impl HelixIndexData {
    /// Verify the current state of the Helix Index. If it is in a valid state, then load the index.
    /// If it is in an invalid state then rebuild it and load it.
    pub fn load_or_rebuild(repo_path: &Path) -> Result<Self> {
        let verifier = Verifier::new(repo_path);

        match verifier.verify()? {
            VerifyResult::Valid => {
                let reader = Reader::new(repo_path);
                let data = reader.read()?;

                Ok(Self {
                    repo_path: repo_path.to_path_buf(),
                    data,
                })
            }
            VerifyResult::Missing => {
                eprintln!("Building helix.idx for the first time...");
                Self::rebuild_helix_index(repo_path)
            }
            VerifyResult::WrongRepo => {
                eprintln!("helix.idx is from a different repo, rebuilding...");
                Self::rebuild_helix_index(repo_path)
            }
            VerifyResult::Corrupted => {
                eprintln!("helix.idx is corrupted, rebuilding...");
                Self::rebuild_helix_index(repo_path)
            }
        }
    }

    /// Rebuild the helix index from Git (used for recovery or first-time init)
    /// TODO: what happens if the git index is no longer up to date? or if it was never there to begin with? what is the back up plan? i don't think rebuilding from git should be the first option here.
    pub fn rebuild_helix_index(repo_path: &Path) -> Result<Self> {
        let syncer = SyncEngine::new(repo_path);
        syncer.import_from_git()?;

        let reader = Reader::new(repo_path);
        let data = reader.read()?;

        Ok(Self {
            repo_path: repo_path.to_path_buf(),
            data,
        })
    }

    /// Reload the helix index from disk
    /// Use this after operations that modify .helix/helix.idx (like helix add, helix commit)
    pub fn reload(&mut self) -> Result<()> {
        let reader = Reader::new(&self.repo_path);
        self.data = reader.read()?;
        Ok(())
    }

    /// Persist changes to disk
    ///
    /// This writes the index to .helix/helix.idx with:
    /// - Incremented generation counter
    /// - Updated entry count
    /// - Computed checksum
    /// - fsync for durability
    pub fn persist(&mut self) -> Result<()> {
        // Increment generation
        self.data.header.generation += 1;

        // Update entry count
        self.data.header.entry_count = self.data.entries.len() as u32;

        // Write to disk
        let writer = Writer::new_canonical(&self.repo_path);
        writer.write(&self.data.header, &self.data.entries)?;

        Ok(())
    }

    /// Apply working tree changes to EntryFlags based on dirty paths from FSMonitor.
    /// dirty paths have some sort of change at the path
    ///
    /// This is responsible for setting:
    /// - MODIFIED  (working tree != helix.idx)
    /// - DELETED   (tracked in helix.idx, missing on disk)
    /// - UNTRACKED (not in helix.idx, exists on disk, not ignored)
    ///
    /// It does NOT touch:
    /// - TRACKED / STAGED (those come from SyncEngine using .git/index + HEAD during import)
    pub fn apply_worktree_changes(&mut self, dirty_paths: &[PathBuf]) -> Result<()> {
        // Build map of tracked paths -> entry index
        let index_by_path: HashMap<PathBuf, usize> = if self.data.entries.len() > 1000 {
            self.data
                .entries
                .par_iter()
                .enumerate()
                .filter(|(_, e)| e.flags.contains(EntryFlags::TRACKED))
                .map(|(i, e)| (e.path.clone(), i))
                .collect()
        } else {
            self.data
                .entries
                .iter()
                .enumerate()
                .filter(|(_, e)| e.flags.contains(EntryFlags::TRACKED))
                .map(|(i, e)| (e.path.clone(), i))
                .collect()
        };

        for rel_path in dirty_paths {
            let full_path = self.repo_path.join(rel_path);
            let exists = full_path.exists();

            if let Some(&idx) = index_by_path.get(rel_path) {
                // Tracked file: adjust MODIFIED / DELETED bits on a single indexed entry
                let entry = &mut self.data.entries[idx];

                entry
                    .flags
                    .remove(EntryFlags::MODIFIED | EntryFlags::DELETED | EntryFlags::UNTRACKED);

                if exists {
                    entry.flags.insert(EntryFlags::MODIFIED);
                } else {
                    entry
                        .flags
                        .insert(EntryFlags::DELETED | EntryFlags::MODIFIED);
                }
            } else {
                // Not in helix.idx → candidate UNTRACKED
                if exists {
                    let entry = self.ensure_untracked_entry(rel_path);
                    entry
                        .flags
                        .remove(EntryFlags::TRACKED | EntryFlags::STAGED | EntryFlags::DELETED);
                    entry
                        .flags
                        .insert(EntryFlags::UNTRACKED | EntryFlags::MODIFIED);
                } else {
                    // Not tracked and missing → nothing to keep
                    self.remove_entry_if_exists(rel_path);
                }
            }
        }

        Ok(())
    }

    pub fn stage_file(&mut self, path: &Path) -> Result<()> {
        // Find the entry
        let entry = self
            .entries_mut()
            .iter_mut()
            .find(|e| e.path == path)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Cannot stage '{}': file is not tracked. Use 'helix add' to track it first.",
                    path.display()
                )
            })?;

        // Add STAGED flag
        entry.flags.insert(EntryFlags::STAGED);

        Ok(())
    }

    pub fn stage_files(&mut self, paths: &[&Path]) -> Result<()> {
        for path in paths {
            self.stage_file(path)?;
        }
        Ok(())
    }

    pub fn stage_all(&mut self) -> Result<()> {
        for entry in self.entries_mut() {
            entry.flags.insert(EntryFlags::STAGED);
        }
        Ok(())
    }

    pub fn unstage_file(&mut self, path: &Path) -> Result<()> {
        // Find the entry
        let entry = self
            .entries_mut()
            .iter_mut()
            .find(|e| e.path == path)
            .ok_or_else(|| {
                anyhow::anyhow!("Cannot unstage '{}': file is not tracked.", path.display())
            })?;

        // Remove STAGED flag
        entry.flags.remove(EntryFlags::STAGED);

        Ok(())
    }

    /// Unstage multiple files at once
    pub fn unstage_files(&mut self, paths: &[&Path]) -> Result<()> {
        for path in paths {
            self.unstage_file(path)?;
        }
        Ok(())
    }

    pub fn unstage_all(&mut self) -> Result<()> {
        for entry in self.entries_mut() {
            entry.flags.remove(EntryFlags::STAGED);
        }
        Ok(())
    }

    /// Get all staged files. This can include tracked and untracked files.
    pub fn get_staged(&self) -> HashSet<PathBuf> {
        if self.data.entries.len() > 1000 {
            self.data
                .entries
                .par_iter()
                .filter(|e| {
                    e.flags.contains(EntryFlags::STAGED) && e.flags.contains(EntryFlags::TRACKED)
                })
                .map(|e| e.path.clone())
                .collect()
        } else {
            self.data
                .entries
                .iter()
                .filter(|e| {
                    e.flags.contains(EntryFlags::STAGED) && e.flags.contains(EntryFlags::TRACKED)
                })
                .map(|e| e.path.clone())
                .collect()
        }
    }

    /// Get all modified files. This can include both tracked and untracked files.
    pub fn get_modified(&self) -> HashSet<PathBuf> {
        if self.data.entries.len() > 1000 {
            self.data
                .entries
                .par_iter()
                .filter(|e| e.flags.contains(EntryFlags::MODIFIED))
                .map(|e| e.path.clone())
                .collect()
        } else {
            self.data
                .entries
                .iter()
                .filter(|e| e.flags.contains(EntryFlags::MODIFIED))
                .map(|e| e.path.clone())
                .collect()
        }
    }

    /// Get all deleted files
    pub fn get_deleted(&self) -> HashSet<PathBuf> {
        if self.data.entries.len() > 1000 {
            self.data
                .entries
                .par_iter()
                .filter(|e| {
                    e.flags.contains(EntryFlags::DELETED) && e.flags.contains(EntryFlags::TRACKED)
                })
                .map(|e| e.path.clone())
                .collect()
        } else {
            self.data
                .entries
                .iter()
                .filter(|e| {
                    e.flags.contains(EntryFlags::DELETED) && e.flags.contains(EntryFlags::TRACKED)
                })
                .map(|e| e.path.clone())
                .collect()
        }
    }

    /// Get all tracked files
    pub fn get_tracked(&self) -> HashSet<PathBuf> {
        if self.data.entries.len() > 1000 {
            self.data
                .entries
                .par_iter()
                .filter(|e| e.flags.contains(EntryFlags::TRACKED))
                .map(|e| e.path.clone())
                .collect()
        } else {
            self.data
                .entries
                .iter()
                .filter(|e| e.flags.contains(EntryFlags::TRACKED))
                .map(|e| e.path.clone())
                .collect()
        }
    }

    /// Get all untracked files
    pub fn get_untracked(&self) -> HashSet<PathBuf> {
        if self.data.entries.len() > 1000 {
            self.data
                .entries
                .par_iter()
                .filter(|e| e.flags.contains(EntryFlags::UNTRACKED))
                .map(|e| e.path.clone())
                .collect()
        } else {
            self.data
                .entries
                .iter()
                .filter(|e| e.flags.contains(EntryFlags::UNTRACKED))
                .map(|e| e.path.clone())
                .collect()
        }
    }

    /// Check if a file is staged
    pub fn is_staged(&self, path: &Path) -> bool {
        if self.data.entries.len() > 1000 {
            self.data.entries.par_iter().any(|e| {
                e.path == path
                    && e.flags.contains(EntryFlags::STAGED)
                    && e.flags.contains(EntryFlags::TRACKED)
            })
        } else {
            self.data.entries.iter().any(|e| {
                e.path == path
                    && e.flags.contains(EntryFlags::STAGED)
                    && e.flags.contains(EntryFlags::TRACKED)
            })
        }
    }

    /// Returns unstaged files
    pub fn get_unstaged(&self) -> HashSet<PathBuf> {
        if self.data.entries.len() > 1000 {
            self.data
                .entries
                .par_iter()
                .filter(|e| {
                    e.flags.contains(EntryFlags::TRACKED)
                        && e.flags.contains(EntryFlags::MODIFIED)
                        && !e.flags.contains(EntryFlags::STAGED)
                })
                .map(|e| e.path.clone())
                .collect()
        } else {
            self.data
                .entries
                .iter()
                .filter(|e| {
                    e.flags.contains(EntryFlags::TRACKED)
                        && e.flags.contains(EntryFlags::MODIFIED)
                        && !e.flags.contains(EntryFlags::STAGED)
                })
                .map(|e| e.path.clone())
                .collect()
        }
    }

    /// Get all files that need staging
    pub fn get_files_to_add(&self) -> HashSet<PathBuf> {
        if self.data.entries.len() > 1000 {
            self.data
                .entries
                .par_iter()
                .filter(|e| {
                    // New file not tracked yet
                    e.flags.contains(EntryFlags::UNTRACKED)

            // Modified tracked file not staged
            || (e.flags.contains(EntryFlags::TRACKED)
                && e.flags.contains(EntryFlags::MODIFIED)
                && !e.flags.contains(EntryFlags::STAGED))

            // Deleted tracked file not staged
            || (e.flags.contains(EntryFlags::TRACKED)
                && e.flags.contains(EntryFlags::DELETED)
                && !e.flags.contains(EntryFlags::STAGED))
                })
                .map(|e| e.path.clone())
                .collect()
        } else {
            self.data
                .entries
                .iter()
                .filter(|e| {
                    // New file not tracked yet
                    e.flags.contains(EntryFlags::UNTRACKED)

            // Modified tracked file not staged
            || (e.flags.contains(EntryFlags::TRACKED)
                && e.flags.contains(EntryFlags::MODIFIED)
                && !e.flags.contains(EntryFlags::STAGED))

            // Deleted tracked file not staged
            || (e.flags.contains(EntryFlags::TRACKED)
                && e.flags.contains(EntryFlags::DELETED)
                && !e.flags.contains(EntryFlags::STAGED))
                })
                .map(|e| e.path.clone())
                .collect()
        }
    }

    /// Get current generation
    pub fn generation(&self) -> u64 {
        self.data.header.generation
    }

    pub fn is_tracked(&self, path: &Path) -> bool {
        if self.data.entries.len() > 1000 {
            self.data
                .entries
                .par_iter()
                .any(|e| e.path == path && e.flags.contains(EntryFlags::TRACKED))
        } else {
            self.data
                .entries
                .iter()
                .any(|e| e.path == path && e.flags.contains(EntryFlags::TRACKED))
        }
    }

    /// Get all entries (for debugging)
    pub fn entries(&self) -> &[Entry] {
        &self.data.entries
    }

    pub fn entries_mut(&mut self) -> &mut Vec<Entry> {
        &mut self.data.entries
    }

    fn remove_entry_if_exists(&mut self, path: &Path) {
        self.data.entries.retain(|e| e.path != path);
    }

    fn ensure_untracked_entry(&mut self, path: &Path) -> &mut Entry {
        // see if an entry already exists
        if let Some(pos) = self.data.entries.iter().position(|e| e.path == path) {
            return &mut self.data.entries[pos];
        }

        self.data.entries.push(Entry {
            path: path.to_path_buf(),
            size: 0,
            mtime_sec: 0,
            mtime_nsec: 0,
            flags: EntryFlags::empty(),
            oid: hash::ZERO_HASH,
            merge_conflict_stage: 0,
            file_mode: 0o100644,
            reserved: [0; 33],
        });

        let len = self.data.entries.len();
        &mut self.data.entries[len - 1]
    }
}

#[cfg(test)]
mod tests {
    use helix_protocol::hash::hash_bytes;

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

    fn create_test_entry(path: &str, flags: EntryFlags) -> Entry {
        Entry {
            path: PathBuf::from(path),
            oid: [0u8; 32],
            flags,
            size: 100,
            mtime_sec: 1234567890,
            mtime_nsec: 0,
            file_mode: 0o100644,
            merge_conflict_stage: 0,
            reserved: [0u8; 33],
        }
    }

    #[test]
    fn test_load_or_rebuild_first_time() -> Result<()> {
        let temp_dir = TempDir::new()?;
        init_test_repo(temp_dir.path())?;

        fs::write(temp_dir.path().join("test.txt"), "hello")?;
        Command::new("git")
            .args(&["add", "test.txt"])
            .current_dir(temp_dir.path())
            .output()?;

        let index = HelixIndexData::load_or_rebuild(temp_dir.path())?;

        assert_eq!(index.generation(), 1);
        assert_eq!(index.entries().len(), 1);

        Ok(())
    }

    #[test]
    fn test_load_or_rebuild_cached() -> Result<()> {
        let temp_dir = TempDir::new()?;
        init_test_repo(temp_dir.path())?;

        fs::write(temp_dir.path().join("test.txt"), "hello")?;
        Command::new("git")
            .args(&["add", "test.txt"])
            .current_dir(temp_dir.path())
            .output()?;

        // First load - builds index
        let index1 = HelixIndexData::load_or_rebuild(temp_dir.path())?;
        assert_eq!(index1.generation(), 1);

        // Second load - uses existing index (no rebuild needed)
        let index2 = HelixIndexData::load_or_rebuild(temp_dir.path())?;
        assert_eq!(index2.generation(), 1); // Same generation

        Ok(())
    }

    #[test]
    fn test_get_staged() -> Result<()> {
        let temp_dir = TempDir::new()?;
        init_test_repo(temp_dir.path())?;

        fs::write(temp_dir.path().join("staged.txt"), "content")?;
        Command::new("git")
            .args(&["add", "staged.txt"])
            .current_dir(temp_dir.path())
            .output()?;

        let index = HelixIndexData::load_or_rebuild(temp_dir.path())?;
        let staged = index.get_staged();

        assert_eq!(staged.len(), 1);
        assert!(staged.contains(&PathBuf::from("staged.txt")));

        Ok(())
    }

    #[test]
    fn test_reload() -> Result<()> {
        let temp_dir = TempDir::new()?;
        init_test_repo(temp_dir.path())?;

        fs::write(temp_dir.path().join("file1.txt"), "content")?;
        Command::new("git")
            .args(&["add", "file1.txt"])
            .current_dir(temp_dir.path())
            .output()?;

        let mut index = HelixIndexData::load_or_rebuild(temp_dir.path())?;
        assert_eq!(index.entries().len(), 1);
        assert_eq!(index.generation(), 1);

        // Simulate external update to helix.idx (e.g., another helix command)
        std::thread::sleep(std::time::Duration::from_millis(10));

        // Add another file via Git (simulating helix add)
        fs::write(temp_dir.path().join("file2.txt"), "content")?;
        Command::new("git")
            .args(&["add", "file2.txt"])
            .current_dir(temp_dir.path())
            .output()?;

        // Import again (simulating what helix add would do)
        let syncer = SyncEngine::new(temp_dir.path());
        syncer.import_from_git()?;

        // Reload to see the changes
        index.reload()?;

        assert_eq!(index.entries().len(), 2);
        assert_eq!(index.generation(), 2);

        Ok(())
    }

    #[test]
    fn test_apply_worktree_changes_tracked() -> Result<()> {
        let temp_dir = TempDir::new()?;
        init_test_repo(temp_dir.path())?;

        // Create and commit a file
        fs::write(temp_dir.path().join("tracked.txt"), "v1")?;
        Command::new("git")
            .args(&["add", "tracked.txt"])
            .current_dir(temp_dir.path())
            .output()?;
        Command::new("git")
            .args(&["commit", "-m", "initial"])
            .current_dir(temp_dir.path())
            .output()?;

        let mut index = HelixIndexData::load_or_rebuild(temp_dir.path())?;

        // Modify the file in working tree
        fs::write(temp_dir.path().join("tracked.txt"), "v2")?;

        // Apply worktree changes
        index.apply_worktree_changes(&[PathBuf::from("tracked.txt")])?;

        // Should be marked as MODIFIED
        let modified = index.get_modified();
        assert_eq!(modified.len(), 1);
        assert!(modified.contains(&PathBuf::from("tracked.txt")));

        Ok(())
    }

    #[test]
    fn test_apply_worktree_changes_untracked() -> Result<()> {
        let temp_dir = TempDir::new()?;
        init_test_repo(temp_dir.path())?;

        let mut index = HelixIndexData::load_or_rebuild(temp_dir.path())?;

        // Create untracked file
        fs::write(temp_dir.path().join("untracked.txt"), "content")?;

        // Apply worktree changes
        index.apply_worktree_changes(&[PathBuf::from("untracked.txt")])?;

        // Should be marked as UNTRACKED
        let untracked = index.get_untracked();
        assert_eq!(untracked.len(), 1);
        assert!(untracked.contains(&PathBuf::from("untracked.txt")));

        Ok(())
    }

    #[test]
    fn test_entries_mut() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        init_test_repo(repo_path)?;

        let mut index = HelixIndexData::load_or_rebuild(repo_path)?;

        // Initially empty
        assert_eq!(index.entries_mut().len(), 0);

        // Add an entry
        index.entries_mut().push(Entry {
            path: PathBuf::from("test.txt"),
            size: 100,
            mtime_sec: 1234567890,
            mtime_nsec: 0,
            flags: EntryFlags::TRACKED | EntryFlags::STAGED,
            oid: hash_bytes(b"test"),
            file_mode: 0o100644,
            merge_conflict_stage: 0,
            reserved: [0u8; 33],
        });

        assert_eq!(index.entries_mut().len(), 1);

        Ok(())
    }

    #[test]
    fn test_persist() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        init_test_repo(repo_path)?;

        let mut index = HelixIndexData::load_or_rebuild(repo_path)?;
        let gen1 = index.generation();

        // Add an entry
        index.entries_mut().push(Entry {
            path: PathBuf::from("test.txt"),
            size: 100,
            mtime_sec: 1234567890,
            mtime_nsec: 0,
            flags: EntryFlags::TRACKED | EntryFlags::STAGED,
            oid: hash_bytes(b"test"),
            file_mode: 0o100644,
            merge_conflict_stage: 0,
            reserved: [0u8; 33],
        });

        // Persist
        index.persist()?;

        // Reload
        let reader = Reader::new(repo_path);
        let data = reader.read()?;

        // Generation should have incremented
        assert_eq!(data.header.generation, gen1 + 1);

        // Entry should be there
        assert_eq!(data.entries.len(), 1);
        assert_eq!(data.entries[0].path, PathBuf::from("test.txt"));

        Ok(())
    }

    #[test]
    fn test_persist_increments_generation() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        init_test_repo(repo_path)?;

        let mut index = HelixIndexData::load_or_rebuild(repo_path)?;
        let gen1 = index.generation();

        // Persist without changes
        index.persist()?;

        let gen2 = index.generation();
        assert_eq!(gen2, gen1 + 1);

        // Persist again
        index.persist()?;

        let gen3 = index.generation();
        assert_eq!(gen3, gen2 + 1);

        Ok(())
    }

    #[test]
    fn test_persist_updates_entry_count() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        init_test_repo(repo_path)?;

        let mut index = HelixIndexData::load_or_rebuild(repo_path)?;

        // Add entries
        for i in 0..5 {
            index.entries_mut().push(Entry {
                path: PathBuf::from(format!("file{}.txt", i)),
                size: 100,
                mtime_sec: 1234567890,
                mtime_nsec: 0,
                flags: EntryFlags::TRACKED,
                oid: hash_bytes(format!("content{}", i).as_bytes()),
                file_mode: 0o100644,
                merge_conflict_stage: 0,
                reserved: [0u8; 33],
            });
        }

        // Persist
        index.persist()?;

        // Reload and check
        let reader = Reader::new(repo_path);
        let data = reader.read()?;

        assert_eq!(data.header.entry_count, 5);
        assert_eq!(data.entries.len(), 5);

        Ok(())
    }

    #[test]
    fn test_repo_path() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        init_test_repo(repo_path)?;

        let index = HelixIndexData::load_or_rebuild(repo_path)?;

        assert_eq!(index.repo_path, repo_path);

        Ok(())
    }

    #[test]
    fn test_stage_file_not_tracked() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        init_test_repo(repo_path)?;

        let mut index = HelixIndexData::load_or_rebuild(repo_path)?;

        // Try to stage untracked file
        let result = index.stage_file(Path::new("untracked.txt"));

        // Should error
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not tracked"));
        Ok(())
    }

    #[test]
    fn test_unstage_file() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        init_test_repo(repo_path)?;

        let mut index = HelixIndexData::load_or_rebuild(repo_path)?;

        // Add a staged file
        index.entries_mut().push(create_test_entry(
            "test.txt",
            EntryFlags::TRACKED | EntryFlags::STAGED,
        ));

        // Unstage it
        index.unstage_file(Path::new("test.txt"))?;

        // Verify STAGED flag removed but TRACKED remains
        let entry = &index.entries()[0];
        assert!(entry.flags.contains(EntryFlags::TRACKED));
        assert!(!entry.flags.contains(EntryFlags::STAGED));

        Ok(())
    }

    #[test]
    fn test_stage_multiple_files() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        init_test_repo(repo_path)?;

        let mut index = HelixIndexData::load_or_rebuild(repo_path)?;
        // Add multiple tracked files
        index
            .entries_mut()
            .push(create_test_entry("file1.txt", EntryFlags::TRACKED));
        index
            .entries_mut()
            .push(create_test_entry("file2.txt", EntryFlags::TRACKED));
        index
            .entries_mut()
            .push(create_test_entry("file3.txt", EntryFlags::TRACKED));

        // Stage multiple
        index.stage_files(&[Path::new("file1.txt"), Path::new("file2.txt")])?;

        // Verify
        assert!(index.entries()[0].flags.contains(EntryFlags::STAGED));
        assert!(index.entries()[1].flags.contains(EntryFlags::STAGED));
        assert!(!index.entries()[2].flags.contains(EntryFlags::STAGED));

        Ok(())
    }

    #[test]
    fn test_stage_all() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        init_test_repo(repo_path)?;

        let mut index = HelixIndexData::load_or_rebuild(repo_path)?;

        // Add multiple tracked files
        index
            .entries_mut()
            .push(create_test_entry("file1.txt", EntryFlags::TRACKED));
        index
            .entries_mut()
            .push(create_test_entry("file2.txt", EntryFlags::TRACKED));
        index
            .entries_mut()
            .push(create_test_entry("file3.txt", EntryFlags::TRACKED));

        // Stage all
        index.stage_all()?;

        // Verify all staged
        for entry in index.entries() {
            assert!(entry.flags.contains(EntryFlags::STAGED));
        }

        Ok(())
    }

    #[test]
    fn test_unstage_all() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        init_test_repo(repo_path)?;

        let mut index = HelixIndexData::load_or_rebuild(repo_path)?;

        // Add multiple staged files
        index.entries_mut().push(create_test_entry(
            "file1.txt",
            EntryFlags::TRACKED | EntryFlags::STAGED,
        ));
        index.entries_mut().push(create_test_entry(
            "file2.txt",
            EntryFlags::TRACKED | EntryFlags::STAGED,
        ));

        // Unstage all
        index.unstage_all()?;

        // Verify all unstaged but still tracked
        for entry in index.entries() {
            assert!(entry.flags.contains(EntryFlags::TRACKED));
            assert!(!entry.flags.contains(EntryFlags::STAGED));
        }

        Ok(())
    }

    #[test]
    fn test_stage_already_staged() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        init_test_repo(repo_path)?;

        let mut index = HelixIndexData::load_or_rebuild(repo_path)?;

        // Add already staged file
        index.entries_mut().push(create_test_entry(
            "test.txt",
            EntryFlags::TRACKED | EntryFlags::STAGED,
        ));

        // Stage again (should be idempotent)
        index.stage_file(Path::new("test.txt"))?;

        // Verify still staged
        let entry = &index.entries()[0];
        assert!(entry.flags.contains(EntryFlags::STAGED));

        Ok(())
    }

    #[test]
    fn test_unstage_not_staged() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        init_test_repo(repo_path)?;

        let mut index = HelixIndexData::load_or_rebuild(repo_path)?;

        // Add unstaged file
        index
            .entries_mut()
            .push(create_test_entry("test.txt", EntryFlags::TRACKED));

        // Unstage (should be idempotent)
        index.unstage_file(Path::new("test.txt"))?;

        // Verify still not staged
        let entry = &index.entries()[0];
        assert!(!entry.flags.contains(EntryFlags::STAGED));

        Ok(())
    }
}
