/*
Verification lgoic for detecting drift between the helix.idx and .git/index files
*/

use std::{
    fs,
    path::{Path, PathBuf},
};

use crate::helix_index::{fingerprint::generate_repo_fingerprint, Reader};
use anyhow::{Context, Result};
use std::time::SystemTime;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifyResult {
    /// Index is fresh and matches .git/index
    Valid,

    /// Index doesn't exist
    Missing,

    /// Fingerprint mismatch (wrong repo)
    WrongRepo,

    /// .git/index mtime changed
    MtimeMismatch,

    /// .git/index size changed
    SizeMismatch,

    /// .git/index checksum changed
    ChecksumMismatch,

    /// Corruption detected
    Corrupted,
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

        // Tier 4: Checksum check (optional, more expensive)
        let git_checksum = compute_git_index_checksum(&git_index_path)?;
        if git_checksum != index_data.header.git_index_checksum {
            return Ok(VerifyResult::ChecksumMismatch);
        }

        Ok(VerifyResult::Valid)
    }
}

fn system_time_to_parts(time: SystemTime) -> (u64, u32) {
    use std::time::UNIX_EPOCH;

    let duration = time.duration_since(UNIX_EPOCH).unwrap_or_default();
    (duration.as_secs(), duration.subsec_nanos())
}

fn compute_git_index_checksum(path: &Path) -> Result<[u8; 20]> {
    use sha1::{Digest, Sha1};

    let data = fs::read(path).context("Failed to read .git/index")?;

    // Git index has SHA-1 checksum in last 20 bytes
    // We want the checksum OF the index (including its own checksum)
    // So we just take the last 20 bytes
    if data.len() < 20 {
        anyhow::bail!(".git/index too small");
    }

    let mut checksum = [0u8; 20];
    checksum.copy_from_slice(&data[data.len() - 20..]);

    Ok(checksum)
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
        syncer.sync()?;

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
        syncer.sync()?;

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

        println!("the index {:?}", result);

        assert_eq!(result, VerifyResult::MtimeMismatch);

        Ok(())
    }
}
