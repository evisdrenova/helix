// Verification logic for detecting drift between the helix.idx and .git/index files

use std::{
    fs,
    path::{Path, PathBuf},
};

use crate::helix_index::{
    fingerprint::generate_repo_fingerprint,
    utils::{read_git_index_checksum, system_time_to_parts},
    Reader,
};
use anyhow::{Context, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifyResult {
    Valid,            // Index is fresh and matches .git/index
    Missing,          // Index doesn't exist
    WrongRepo,        // Fingerprint mismatch (wrong repo)
    MtimeMismatch,    // .git/index mtime changed
    SizeMismatch,     // .git/index size changed
    ChecksumMismatch, // .git/index checksum changed
    Corrupted,        // Corruption detected
}

pub struct Verifier {
    repo_path: PathBuf,
}

impl Verifier {
    pub fn new(repo_path: &Path) -> Self {
        Self {
            repo_path: repo_path.to_path_buf(),
        }
    }

    pub fn verify(&self) -> Result<VerifyResult> {
        let reader = Reader::new(&self.repo_path);

        if !reader.exists() {
            return Ok(VerifyResult::Missing);
        }

        let index_data = match reader.read() {
            Ok(data) => data,
            Err(_) => return Ok(VerifyResult::Corrupted),
        };

        let current_fingerprint = generate_repo_fingerprint(&self.repo_path)?;
        if index_data.header.repo_fingerprint != current_fingerprint {
            return Ok(VerifyResult::WrongRepo);
        }

        // Get .git/index metadata
        let git_index_path = self.repo_path.join(".git/index");
        if !git_index_path.exists() {
            // .git/index doesn't exist but helix.idx does - stale
            return Ok(VerifyResult::MtimeMismatch);
        }

        let git_metadata =
            fs::metadata(&git_index_path).context("Failed to read .git/index metadata")?;

        let git_mtime = git_metadata
            .modified()
            .context("Failed to get .git/index mtime")?;

        let (git_mtime_sec, git_mtime_nsec) = system_time_to_parts(git_mtime);

        if git_mtime_sec != index_data.header.git_index_mtime_sec
            || git_mtime_nsec != index_data.header.git_index_mtime_nsec
        {
            return Ok(VerifyResult::MtimeMismatch);
        }

        let git_size = git_metadata.len();
        if git_size != index_data.header.git_index_size {
            return Ok(VerifyResult::SizeMismatch);
        }

        let git_checksum = read_git_index_checksum(&git_index_path)?;
        if git_checksum != index_data.header.git_index_checksum {
            return Ok(VerifyResult::ChecksumMismatch);
        }

        Ok(VerifyResult::Valid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::helix_index::sync::SyncEngine;
    use std::fs;
    use std::process::Command;
    use std::thread;
    use std::time::Duration;
    use tempfile::TempDir;

    fn init_test_repo(path: &Path) -> Result<()> {
        fs::create_dir_all(path.join(".git"))?;
        Command::new("git")
            .args(&["init"])
            .current_dir(path)
            .output()?;

        // Create a file and add it
        fs::write(path.join("test.txt"), "hello")?;
        Command::new("git")
            .args(&["add", "test.txt"])
            .current_dir(path)
            .output()?;

        Ok(())
    }

    #[test]
    fn test_verify_missing() -> Result<()> {
        let temp_dir = TempDir::new()?;
        init_test_repo(temp_dir.path())?;

        let verifier = Verifier::new(temp_dir.path());
        let result = verifier.verify()?;

        assert_eq!(result, VerifyResult::Missing);

        Ok(())
    }

    #[test]
    fn test_verify_valid() -> Result<()> {
        let temp_dir = TempDir::new()?;
        init_test_repo(temp_dir.path())?;

        // Sync to create helix.idx
        let syncer = SyncEngine::new(temp_dir.path());
        syncer.full_sync()?;

        let verifier = Verifier::new(temp_dir.path());
        let result = verifier.verify()?;

        assert_eq!(result, VerifyResult::Valid);

        Ok(())
    }

    #[test]
    fn test_verify_mtime_mismatch() -> Result<()> {
        let temp_dir = TempDir::new()?;
        init_test_repo(temp_dir.path())?;

        // Create helix.idx
        let syncer = SyncEngine::new(temp_dir.path());
        syncer.full_sync()?;

        // Verify it's valid
        let verifier = Verifier::new(temp_dir.path());
        assert_eq!(verifier.verify()?, VerifyResult::Valid);

        // Sleep to ensure mtime changes
        thread::sleep(Duration::from_millis(100));

        // Modify .git/index (add another file)
        fs::write(temp_dir.path().join("another.txt"), "world")?;

        Command::new("git")
            .args(&["add", "another.txt"])
            .current_dir(temp_dir.path())
            .output()?;

        // Now verify should detect mismatch
        let result = verifier.verify()?;
        assert_eq!(result, VerifyResult::MtimeMismatch);

        Ok(())
    }
}
