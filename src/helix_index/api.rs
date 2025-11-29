use super::format::{Entry, EntryFlags};
use super::reader::{HelixIndex, Reader};
use super::sync::SyncEngine;
use super::verify::{Verifier, VerifyResult};
use anyhow::Result;
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

    /// Get all entries (for debugging)
    pub fn entries(&self) -> &[Entry] {
        &self.data.entries
    }

    fn find_entry_mut(&mut self, path: &Path) -> Option<&mut Entry> {
        self.data.entries.iter_mut().find(|e| e.path == path)
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
            oid: [0; 20], // we don't need a real OID for untracked entries
            merge_conflict_stage: 0,
            file_mode: 0o100644,
            reserved: [0; 57],
        });

        let len = self.data.entries.len();
        &mut self.data.entries[len - 1]
    }
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
}
