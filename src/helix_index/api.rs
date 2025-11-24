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
    // Load helix.idx or rebuild if missing/stale
    pub fn load_or_rebuild(repo_path: &Path) -> Result<Self> {
        let verifier = Verifier::new(repo_path);

        match verifier.verify()? {
            VerifyResult::Valid => {
                // Fast path: index is fresh
                let reader = Reader::new(repo_path);
                let data = reader.read()?;

                Ok(Self {
                    repo_path: repo_path.to_path_buf(),
                    data,
                })
            }
            VerifyResult::Missing => {
                // Index doesn't exist, build it
                eprintln!("Building helix.idx for the first time...");
                Self::rebuild(repo_path)
            }
            VerifyResult::MtimeMismatch
            | VerifyResult::SizeMismatch
            | VerifyResult::ChecksumMismatch => {
                // Index is stale, rebuild
                eprintln!("helix.idx is stale, rebuilding...");
                Self::rebuild(repo_path)
            }
            VerifyResult::WrongRepo => {
                // Index is from a different repo, rebuild
                eprintln!("helix.idx is from a different repo, rebuilding...");
                Self::rebuild(repo_path)
            }
            VerifyResult::Corrupted => {
                // Index is corrupted, rebuild
                eprintln!("helix.idx is corrupted, rebuilding...");
                Self::rebuild(repo_path)
            }
        }
    }

    // Force rebuild from .git/index
    pub fn rebuild(repo_path: &Path) -> Result<Self> {
        let syncer = SyncEngine::new(repo_path);
        syncer.full_sync()?;

        let reader = Reader::new(repo_path);
        let data = reader.read()?;

        Ok(Self {
            repo_path: repo_path.to_path_buf(),
            data,
        })
    }

    /// Refresh incrementally (fast path for git add/reset)
    pub fn refresh_incremental(&mut self, changed_paths: &[PathBuf]) -> Result<()> {
        let syncer = SyncEngine::new(&self.repo_path);
        syncer.incremental_sync(changed_paths)?;

        let reader = Reader::new(&self.repo_path);
        self.data = reader.read()?;

        Ok(())
    }

    /// Full Refresh from .git/index (incremental update)
    pub fn refresh(&mut self) -> Result<()> {
        let syncer = SyncEngine::new(&self.repo_path);
        syncer.full_sync()?;

        let reader = Reader::new(&self.repo_path);
        self.data = reader.read()?;

        Ok(())
    }

    /// Get all staged files
    pub fn get_staged(&self) -> HashSet<PathBuf> {
        self.data
            .entries
            .iter()
            .filter(|e| e.flags.contains(EntryFlags::STAGED))
            .map(|e| e.path.clone())
            .collect()
    }

    /// Get all modified files
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
            .filter(|e| e.flags.contains(EntryFlags::DELETED))
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
        self.data
            .entries
            .iter()
            .any(|e| e.path == path && e.flags.contains(EntryFlags::STAGED))
    }

    /// Get current generation
    pub fn generation(&self) -> u64 {
        self.data.header.generation
    }

    /// Get all entries (for debugging)
    pub fn entries(&self) -> &[Entry] {
        &self.data.entries
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
        index.refresh()?;

        assert_eq!(index.entries().len(), 2);
        assert_eq!(index.generation(), 2);

        Ok(())
    }
}
