// Verification logic for detecting corruption in helix.idx

use std::path::{Path, PathBuf};

use crate::helix_index::{hash, Reader};
use anyhow::Result;

/// Verification result for helix.idx integrity
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifyResult {
    Valid,     // Index is internally consistent
    Missing,   // Index doesn't exist
    WrongRepo, // Fingerprint mismatch (wrong repo)
    Corrupted, // Checksum failed or parse error
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

    /// Verify the integrity of helix.idx
    ///
    /// Checks:
    /// 1. File exists
    /// 2. Can be parsed (format valid)
    /// 3. Checksum matches (no corruption)
    /// 4. Repo fingerprint matches (not from different repo)
    pub fn verify(&self) -> Result<VerifyResult> {
        let reader = Reader::new(&self.repo_path);

        // Check existence
        if !reader.exists() {
            return Ok(VerifyResult::Missing);
        }

        // Try to read and parse (validates checksum automatically)
        let index_data = match reader.read() {
            Ok(data) => data,
            Err(_) => return Ok(VerifyResult::Corrupted),
        };

        // Verify repo fingerprint
        let current_fingerprint = hash::hash_file(&self.repo_path)?;
        if index_data.header.repo_fingerprint != current_fingerprint {
            return Ok(VerifyResult::WrongRepo);
        }

        Ok(VerifyResult::Valid)
    }

    /// Quick check if index exists and is readable
    pub fn exists(&self) -> bool {
        Reader::new(&self.repo_path).exists()
    }

    /// Get generation number (useful for checking if index is newer than expected)
    pub fn generation(&self) -> Result<u64> {
        Reader::new(&self.repo_path).generation()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::helix_index::{
        format::{Entry, Header},
        writer::Writer,
    };
    use std::fs;
    use std::path::PathBuf;
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
        fs::create_dir_all(temp_dir.path().join(".helix"))?;

        // Create valid index
        let writer = Writer::new_canonical(temp_dir.path());
        let header = Header::new(1, [0xaa; 32], 0);
        let entries = vec![Entry::new(
            PathBuf::from("test.txt"),
            1024,
            100,
            hash::ZERO_HASH,
            0,
        )];

        writer.write(&header, &entries)?;

        // Verify should pass
        let verifier = Verifier::new(temp_dir.path());
        let result = verifier.verify()?;

        assert_eq!(result, VerifyResult::Valid);

        Ok(())
    }

    #[test]
    fn test_verify_corrupted() -> Result<()> {
        let temp_dir = TempDir::new()?;
        init_test_repo(temp_dir.path())?;
        fs::create_dir_all(temp_dir.path().join(".helix"))?;

        // Create valid index
        let writer = Writer::new_canonical(temp_dir.path());
        let header = Header::new(1, [0xaa; 32], 0);
        let entries = vec![Entry::new(
            PathBuf::from("test.txt"),
            1024,
            100,
            hash::ZERO_HASH,
            0,
        )];

        writer.write(&header, &entries)?;

        // Corrupt the file
        let index_path = temp_dir.path().join(".helix/helix.idx");
        let mut contents = fs::read(&index_path)?;

        // Flip some bits in the middle (corrupt entry data)
        if contents.len() > 100 {
            contents[100] ^= 0xFF;
            contents[101] ^= 0xFF;
        }

        fs::write(&index_path, contents)?;

        // Verify should detect corruption
        let verifier = Verifier::new(temp_dir.path());
        let result = verifier.verify()?;

        assert_eq!(result, VerifyResult::Corrupted);

        Ok(())
    }

    #[test]
    fn test_verify_wrong_repo() -> Result<()> {
        let temp_dir1 = TempDir::new()?;
        let temp_dir2 = TempDir::new()?;

        init_test_repo(temp_dir1.path())?;
        init_test_repo(temp_dir2.path())?;

        fs::create_dir_all(temp_dir1.path().join(".helix"))?;

        // Create index for repo1
        let writer = Writer::new_canonical(temp_dir1.path());
        let header = Header::new(1, [0xaa; 32], 0);
        let entries = vec![];
        writer.write(&header, &entries)?;

        // Copy index file to repo2
        let index1 = temp_dir1.path().join(".helix/helix.idx");
        let index2 = temp_dir2.path().join(".helix/helix.idx");
        fs::create_dir_all(temp_dir2.path().join(".helix"))?;
        fs::copy(&index1, &index2)?;

        // Verify in repo2 should detect wrong fingerprint
        let verifier = Verifier::new(temp_dir2.path());
        let result = verifier.verify()?;

        assert_eq!(result, VerifyResult::WrongRepo);

        Ok(())
    }

    #[test]
    fn test_exists() -> Result<()> {
        let temp_dir = TempDir::new()?;
        init_test_repo(temp_dir.path())?;

        let verifier = Verifier::new(temp_dir.path());
        assert!(!verifier.exists());

        // Create index
        fs::create_dir_all(temp_dir.path().join(".helix"))?;
        let writer = Writer::new_canonical(temp_dir.path());
        let header = Header::new(1, [0xaa; 32], 0);
        writer.write(&header, &[])?;

        assert!(verifier.exists());

        Ok(())
    }

    #[test]
    fn test_generation() -> Result<()> {
        let temp_dir = TempDir::new()?;
        init_test_repo(temp_dir.path())?;
        fs::create_dir_all(temp_dir.path().join(".helix"))?;

        let writer = Writer::new_canonical(temp_dir.path());
        let header = Header::new(42, [0xaa; 32], 0);
        writer.write(&header, &[])?;

        let verifier = Verifier::new(temp_dir.path());
        assert_eq!(verifier.generation()?, 42);

        Ok(())
    }
}
