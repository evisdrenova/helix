use crate::index::GitIndex;

use super::format::{Entry, EntryFlags};
use super::reader::{HelixIndexData, Reader};
use super::sync::SyncEngine;
use super::verify::{Verifier, VerifyResult};
use anyhow::Result;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

pub struct HelixIndex {
    repo_path: PathBuf,
    data: HelixIndexData,
}

impl HelixIndex {
    /// Verify the current state of the Helix Index and either load or rebuild it depending on its state.
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
            VerifyResult::MtimeMismatch
            | VerifyResult::SizeMismatch
            | VerifyResult::ChecksumMismatch => {
                eprintln!("helix.idx is stale, rebuilding...");
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

    /// Rebuild the helix index from scratch using a full sync and return a new instance
    pub fn rebuild_helix_index(repo_path: &Path) -> Result<Self> {
        let syncer = SyncEngine::new(repo_path);
        syncer.full_sync()?;

        let reader = Reader::new(repo_path);
        let data = reader.read()?;

        Ok(Self {
            repo_path: repo_path.to_path_buf(),
            data,
        })
    }

    /// Incrementally refresh specific paths in the the helix index
    pub fn incremental_refresh(&mut self, changed_paths: &[PathBuf]) -> Result<()> {
        let syncer = SyncEngine::new(&self.repo_path);
        syncer.incremental_sync(changed_paths)?;

        let reader = Reader::new(&self.repo_path);
        self.data = reader.read()?;

        Ok(())
    }

    /// Full Refresh from .git/index and return existing instance
    pub fn full_refresh(&mut self) -> Result<()> {
        let syncer = SyncEngine::new(&self.repo_path);
        syncer.full_sync()?;

        let reader = Reader::new(&self.repo_path);
        self.data = reader.read()?;

        Ok(())
    }

    // Apply working tree changes to EntryFlags based on dirty paths from FSMonitor.
    ///
    /// This is responsible for setting:
    /// - MODIFIED  (working tree != index)
    /// - DELETED   (tracked in index, missing on disk)
    /// - UNTRACKED (not in index, exists on disk, not ignored)
    ///
    /// It does NOT touch:
    /// - TRACKED / STAGED (those come from SyncEngine using .git/index + HEAD)
    pub fn apply_worktree_changes(&mut self, dirty_paths: &[PathBuf]) -> Result<()> {
        // Snapshot of tracked paths from .git/index
        let git_index = GitIndex::open(&self.repo_path)?;
        let tracked: HashSet<PathBuf> =
            git_index.entries().map(|e| PathBuf::from(e.path)).collect();

        for rel_path in dirty_paths {
            let full_path = self.repo_path.join(rel_path);
            let exists = full_path.exists();

            if tracked.contains(rel_path) {
                // Tracked file: adjust MODIFIED / DELETED bits
                if let Some(entry) = self.find_entry_mut(rel_path) {
                    // Clear working-tree-related bits before recomputing
                    entry
                        .flags
                        .remove(EntryFlags::MODIFIED | EntryFlags::DELETED | EntryFlags::UNTRACKED);

                    if exists {
                        // tracked + exists + dirty => modified (unstaged)
                        entry.flags.insert(EntryFlags::MODIFIED);
                    } else {
                        // tracked + missing => deleted (unstaged)
                        entry
                            .flags
                            .insert(EntryFlags::DELETED | EntryFlags::MODIFIED);
                    }
                }
            } else {
                // Not in .git/index => candidate for UNTRACKED
                if exists {
                    let entry = self.ensure_untracked_entry(rel_path);
                    // For untracked entries, we explicitly mark them as untracked & modified.
                    entry
                        .flags
                        .remove(EntryFlags::TRACKED | EntryFlags::STAGED | EntryFlags::DELETED);
                    entry
                        .flags
                        .insert(EntryFlags::UNTRACKED | EntryFlags::MODIFIED);
                } else {
                    // Not tracked and missing on disk => nothing to keep
                    self.remove_entry_if_exists(rel_path);
                }
            }
        }

        Ok(())
    }

    /// Get all staged files. This can include tracked and untracked files.
    pub fn get_staged(&self) -> HashSet<PathBuf> {
        self.data
            .entries
            .iter()
            .filter(|e| {
                e.flags.contains(EntryFlags::STAGED) && e.flags.contains(EntryFlags::TRACKED)
            })
            .map(|e| e.path.clone())
            .collect()
    }

    /// Get all modified files. This can include both tracked and untracked files.
    pub fn get_modified(&self) -> HashSet<PathBuf> {
        self.data
            .entries
            .iter()
            .filter(|e| e.flags.contains(EntryFlags::MODIFIED))
            .map(|e| e.path.clone())
            .collect()
    }

    /// Get all deleted files
    pub fn get_deleted(&self) -> HashSet<PathBuf> {
        self.data
            .entries
            .iter()
            .filter(|e| {
                e.flags.contains(EntryFlags::DELETED) && e.flags.contains(EntryFlags::TRACKED)
            })
            .map(|e| e.path.clone())
            .collect()
    }

    /// Get all tracked files
    pub fn get_tracked(&self) -> HashSet<PathBuf> {
        self.data
            .entries
            .iter()
            .filter(|e| e.flags.contains(EntryFlags::TRACKED))
            .map(|e| e.path.clone())
            .collect()
    }

    /// Get all untracked files
    pub fn get_untracked(&self) -> HashSet<PathBuf> {
        self.data
            .entries
            .iter()
            .filter(|e| e.flags.contains(EntryFlags::UNTRACKED))
            .map(|e| e.path.clone())
            .collect()
    }

    /// Check if a file is staged
    pub fn is_staged(&self, path: &Path) -> bool {
        self.data.entries.iter().any(|e| {
            e.path == path
                && e.flags.contains(EntryFlags::STAGED)
                && e.flags.contains(EntryFlags::TRACKED)
        })
    }

    /// Returns unstaged files
    pub fn get_unstaged(&self) -> HashSet<PathBuf> {
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

    /// Get all files that need staging (for `helix add .`)
    pub fn get_files_to_add(&self) -> HashSet<PathBuf> {
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
            reserved: [0; 64],
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

        let index = HelixIndex::load_or_rebuild(temp_dir.path())?;

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
        let index1 = HelixIndex::load_or_rebuild(temp_dir.path())?;
        assert_eq!(index1.generation(), 1);

        // Second load - uses cached index
        let index2 = HelixIndex::load_or_rebuild(temp_dir.path())?;
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

        let index = HelixIndex::load_or_rebuild(temp_dir.path())?;
        let staged = index.get_staged();

        assert_eq!(staged.len(), 1);
        assert!(staged.contains(&PathBuf::from("staged.txt")));

        Ok(())
    }

    #[test]
    fn test_refresh() -> Result<()> {
        let temp_dir = TempDir::new()?;
        init_test_repo(temp_dir.path())?;

        fs::write(temp_dir.path().join("file1.txt"), "content")?;
        Command::new("git")
            .args(&["add", "file1.txt"])
            .current_dir(temp_dir.path())
            .output()?;

        let mut index = HelixIndex::load_or_rebuild(temp_dir.path())?;
        assert_eq!(index.entries().len(), 1);
        assert_eq!(index.generation(), 1);

        // Add another file
        std::thread::sleep(std::time::Duration::from_millis(10));
        fs::write(temp_dir.path().join("file2.txt"), "content")?;
        Command::new("git")
            .args(&["add", "file2.txt"])
            .current_dir(temp_dir.path())
            .output()?;

        // Refresh
        index.full_refresh()?;

        assert_eq!(index.entries().len(), 2);
        assert_eq!(index.generation(), 2);

        Ok(())
    }
}
