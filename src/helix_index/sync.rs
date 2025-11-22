use super::fingerprint::generate_repo_fingerprint;
use super::format::{Entry, EntryFlags, Header};
use super::reader::Reader;
use super::writer::Writer;
use crate::index::Index;
use crate::Oid;
use anyhow::{Context, Result};
use sha1::{Digest, Sha1};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

// TODO: maybe in a later version see if we can be smarter here about what we need to re-write is there is some drift
// thinking of a LSM type of algorithm where can we just update certain entries
// although it might be simpler to just re-write the entire thing??

pub struct SyncEngine {
    repo_path: PathBuf,
}

impl SyncEngine {
    pub fn new(repo_path: &Path) -> Self {
        Self {
            repo_path: repo_path.to_path_buf(),
        }
    }

    /// Sync helix.idx from .git/index
    ///
    /// This is the core rebuild operation. It:
    /// 1. Checks for .git/index.lock (waits if needed)
    /// 2. Reads .git/index
    /// 3. Builds entries with status flags
    /// 4. Writes helix.idx atomically
    pub fn sync(&self) -> Result<()> {
        self.sync_with_timeout(Duration::from_secs(5))
    }

    /// Sync with custom timeout for .git/index.lock
    ///
    pub fn sync_with_timeout(&self, timeout: Duration) -> Result<()> {
        // Wait for any concurrent git operation to finish
        wait_for_git_lock(&self.repo_path, timeout)?;

        let reader = Reader::new(&self.repo_path);

        // if reader exists, read it, otherwise create it and set the generation to 0
        let current_generation = if reader.exists() {
            reader
                .read()
                .ok()
                .map(|data| data.header.generation)
                .unwrap_or(0)
        } else {
            0
        };

        // Get .git/index metadata
        let git_index_path = self.repo_path.join(".git/index");
        if !git_index_path.exists() {
            anyhow::bail!(".git/index does not exist");
        }

        let git_metadata =
            fs::metadata(&git_index_path).context("Failed to read .git/index metadata")?;

        let git_mtime = git_metadata.modified()?;
        let (git_mtime_sec, git_mtime_nsec) = system_time_to_parts(git_mtime);
        let git_size = git_metadata.len();

        // Read .git/index checksum (last 20 bytes)
        let git_data = fs::read(&git_index_path)?;
        let mut git_checksum = [0u8; 20];
        if git_data.len() >= 20 {
            git_checksum.copy_from_slice(&git_data[git_data.len() - 20..]);
        }

        // Parse .git/index
        let index = Index::open(&self.repo_path).context("Failed to open .git/index")?;

        // Build entries
        let entries = self.build_entries(&index)?;

        // Generate header
        let repo_fingerprint = generate_repo_fingerprint(&self.repo_path)?;
        let header = Header::new(
            current_generation + 1, // Increment generation
            repo_fingerprint,
            git_mtime_sec,
            git_mtime_nsec,
            git_size,
            git_checksum,
            entries.len() as u32,
        );

        // Write atomically
        let writer = Writer::new(&self.repo_path);
        writer.write(&header, &entries)?;

        Ok(())
    }

    /// Build entries from .git/index
    fn build_entries(&self, index: &Index) -> Result<Vec<Entry>> {
        let mut entries = Vec::new();

        // Get all tracked files from index
        for index_entry in index.entries() {
            let path = PathBuf::from(&index_entry.path);
            let full_path = self.repo_path.join(&path);

            let mut flags = EntryFlags::TRACKED;

            // Check if file is staged (compare with HEAD)
            if self.is_staged(&path, &index_entry.oid)? {
                flags |= EntryFlags::STAGED;
            }

            // Check if file exists and get metadata
            let (size, mtime_sec, mtime_nsec) = if full_path.exists() {
                let metadata = fs::metadata(&full_path)?;
                let mtime = metadata.modified()?;
                let (sec, nsec) = system_time_to_parts(mtime);

                // Check if modified (compare file hash with index)
                if self.is_modified(&full_path, &index_entry.oid)? {
                    flags |= EntryFlags::MODIFIED;
                }

                (metadata.len(), sec, nsec)
            } else {
                // File deleted
                flags |= EntryFlags::DELETED;
                (0, 0, 0)
            };

            entries.push(Entry {
                path,
                size,
                mtime_sec,
                mtime_nsec,
                flags,
                oid: *index_entry.oid.as_bytes(),
                reserved: [0; 64],
            });
        }

        // TODO: Add untracked files (would need to scan working tree)
        // For V1, we'll just track what's in .git/index

        Ok(entries)
    }

    /// Check if a file is staged (index differs from HEAD)
    /// TODO: replace this with gix instead of git2
    fn is_staged(&self, path: &Path, index_oid: &Oid) -> Result<bool> {
        use git2::Repository;

        let repo = Repository::open(&self.repo_path)?;

        // Try to get HEAD
        let head = match repo.head() {
            Ok(h) => h,
            Err(_) => return Ok(true), // No HEAD yet, everything is staged
        };

        let commit = head.peel_to_commit()?;
        let tree = commit.tree()?;

        // Look up path in HEAD tree
        match tree.get_path(path) {
            Ok(entry) => {
                let head_oid = entry.id();
                // If index OID != HEAD OID, file is staged
                Ok(index_oid.as_bytes() != head_oid.as_bytes())
            }
            Err(_) => {
                // File not in HEAD, so it's a new file (staged)
                Ok(true)
            }
        }
    }

    /// Check if a file is modified (working tree differs from index)
    ///     /// TODO: replace this with gix instead of git2
    fn is_modified(&self, file_path: &Path, index_oid: &Oid) -> Result<bool> {
        // Hash the current file
        let current_oid = hash_file_git_compatible(file_path)?;

        // Compare with index
        Ok(current_oid != index_oid.as_bytes())
    }
}

fn system_time_to_parts(time: SystemTime) -> (u64, u32) {
    let duration = time.duration_since(UNIX_EPOCH).unwrap_or_default();
    (duration.as_secs(), duration.subsec_nanos())
}

fn hash_file_git_compatible(path: &Path) -> Result<Vec<u8>> {
    let contents = fs::read(path).with_context(|| format!("Failed to read {}", path.display()))?;

    // Git hashes with format: "blob <size>\0<contents>"
    let header = format!("blob {}\0", contents.len());

    let mut hasher = Sha1::new();
    hasher.update(header.as_bytes());
    hasher.update(&contents);
    let result = hasher.finalize();

    Ok(result.to_vec())
}

/// Wait for .git/index.lock to be released
fn wait_for_git_lock(repo_path: &Path, timeout: Duration) -> Result<()> {
    let lock_path = repo_path.join(".git/index.lock");
    let start = Instant::now();

    while lock_path.exists() {
        if start.elapsed() > timeout {
            anyhow::bail!("Timeout waiting for .git/index.lock to be released");
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
        syncer.sync()?;

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
        syncer.sync()?;
        let reader = Reader::new(temp_dir.path());
        let data1 = reader.read()?;
        assert_eq!(data1.header.generation, 1);

        // Second sync
        syncer.sync()?;
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
        syncer.sync()?;

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
