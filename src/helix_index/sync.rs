use super::fingerprint::generate_repo_fingerprint;
use super::format::{Entry, EntryFlags, Header};
use super::reader::Reader;
use super::writer::Writer;

use crate::index::GitIndex;
use crate::Oid;

use anyhow::{Context, Result};
use rayon::prelude::*;
use sha1::{Digest, Sha1};
use std::collections::HashMap;
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

    /// Full sync: rebuild the entire helix index from .git/index
    /// Only used on first run or corruption
    pub fn sync(&self) -> Result<()> {
        self.sync_with_timeout(Duration::from_secs(5))
    }

    /// Incremental sync: only update changed entries
    pub fn sync_incremental(&self, changed_paths: &[PathBuf]) -> Result<()> {
        wait_for_git_lock(&self.repo_path, Duration::from_secs(5))?;

        let reader = Reader::new(&self.repo_path);
        if !reader.exists() {
            // No index exists, do full sync
            return self.sync();
        };

        let mut index_data = reader.read()?;

        // Open git index
        let git_index = GitIndex::open(&self.repo_path)?;

        // Load HEAD tree once for staging detection
        let head_tree = self.load_head_tree()?;

        for changed_path in changed_paths {
            match git_index.get_entry(changed_path) {
                Ok(git_entry) => {
                    // Find existing entry or create new
                    if let Some(existing) = index_data
                        .entries
                        .iter_mut()
                        .find(|e| &e.path == changed_path)
                    {
                        *existing = self.build_entry_from_git(&git_entry, &head_tree)?;
                    } else {
                        let entry = self.build_entry_from_git(&git_entry, &head_tree)?;
                        index_data.entries.push(entry);
                    }
                }
                Err(_) => {
                    // File removed from index
                    index_data.entries.retain(|e| &e.path != changed_path);
                }
            }
        }

        // Update header metadata
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

        // Write atomically
        let writer = Writer::new(&self.repo_path);
        writer.write(&index_data.header, &index_data.entries)?;

        Ok(())
    }

    /// Sync with custom timeout for .git/index.lock
    pub fn sync_with_timeout(&self, timeout: Duration) -> Result<()> {
        wait_for_git_lock(&self.repo_path, timeout)?;

        // Read current generation
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

        // Get .git/index metadata
        let git_index_path = self.repo_path.join(".git/index");
        if !git_index_path.exists() {
            anyhow::bail!(".git/index does not exist");
        }

        let git_metadata = fs::metadata(&git_index_path)?;
        let git_mtime = git_metadata.modified()?;
        let (git_mtime_sec, git_mtime_nsec) = system_time_to_parts(git_mtime);
        let git_size = git_metadata.len();
        let git_checksum = read_git_index_checksum(&git_index_path)?;

        // Parse .git/index
        let index = GitIndex::open(&self.repo_path)?;

        // Build entries (optimized)
        let entries = self.build_entries_optimized(&index)?;

        // Generate header
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

        // Write atomically
        let writer = Writer::new(&self.repo_path);
        writer.write(&header, &entries)?;

        Ok(())
    }

    /// Build entries optimized: load HEAD tree once, skip modification detection
    fn build_entries_optimized(&self, index: &GitIndex) -> Result<Vec<Entry>> {
        // Load HEAD tree once (shared across all threads)
        let head_tree = self.load_head_tree()?;

        // Materialize the index entries into a Vec so Rayon can parallelize over it.
        // This still borrows from the mmap via &str, but lifetime is valid for this scope.
        let index_entries: Vec<_> = index.entries().collect();

        let entries: Result<Vec<Entry>> = index_entries
            .into_par_iter()
            .map(|e| self.build_entry_from_git(&e, &head_tree))
            .collect();

        entries
    }

    /// Build our index with files from .git/index so we can compare them against the head commit
    /// to understand the status of the files (staged, not-staged). Originally we checked for the file metadata here
    /// but that didn't scale well for large repos. There is a tiny window where the size & mtime might be astale if someone modifies files between git operations and the first helix run. FSMonitor will pick up that metadata as soon as the user starts making
    /// any changes anyways so i think it's worth the trade-off.
    fn build_entry_from_git(
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
            .unwrap_or(true); // Not in HEAD = new file = staged

        if is_staged {
            flags |= EntryFlags::STAGED;
        }

        // read the size, mtime from the index and then set the mtine_nsec to 0. See comment above func signature for more info
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

    fn load_head_tree(&self) -> Result<HashMap<PathBuf, Vec<u8>>> {
        let repo = gix::open(&self.repo_path).context("Failed to open repository with gix")?;

        // Get HEAD commit
        let commit = match repo.head()?.peel_to_commit() {
            // 1. Success arm: Peel was successful and returned the Commit object.
            Ok(commit) => commit,

            // 2. Error arm: The peel failed (e.g., HEAD is unborn, or points to a non-commit object).
            Err(_) => {
                // Unborn HEAD or failed to peel
                return Ok(HashMap::new());
            }
        };

        // Get the tree from the commit
        let tree = commit
            .tree()
            .context("Failed to get tree from commit")?
            .to_owned(); // Get an owned Tree object

        // Use a Recorder to traverse and collect all entries
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
                    // record.filename is a gix::bstr::BString, convert to PathBuf
                    let path = PathBuf::from(record.filepath.to_string());

                    // record.oid is gix::Oid, convert to Vec<u8> (its raw bytes)
                    let oid_bytes = record.oid.to_owned().as_bytes().to_vec();

                    Some((path, oid_bytes))
                } else {
                    None
                }
            })
            .collect();

        Ok(map)
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
    pub fn sync_with_timing(&self, timeout: Duration) -> Result<SyncTiming> {
        let total_start = Instant::now();

        wait_for_git_lock(&self.repo_path, timeout)?;

        // Read current generation
        let gen_start = Instant::now();
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
        let gen_time = gen_start.elapsed();

        // Get .git/index metadata
        let meta_start = Instant::now();
        let git_index_path = self.repo_path.join(".git/index");
        if !git_index_path.exists() {
            anyhow::bail!(".git/index does not exist");
        }

        let git_metadata = fs::metadata(&git_index_path)?;
        let git_mtime = git_metadata.modified()?;
        let (git_mtime_sec, git_mtime_nsec) = system_time_to_parts(git_mtime);
        let git_size = git_metadata.len();
        let git_checksum = read_git_index_checksum(&git_index_path)?;
        let meta_time = meta_start.elapsed();

        // Parse .git/index
        let index_start = Instant::now();
        let index = GitIndex::open(&self.repo_path)?;
        let index_time = index_start.elapsed();

        // Build entries (optimized)
        let build_start = Instant::now();
        let entries = self.build_entries_optimized_with_timing(&index)?;
        let build_time = build_start.elapsed();

        // Generate header
        let header_start = Instant::now();
        let repo_fingerprint = generate_repo_fingerprint(&self.repo_path)?;
        let header = Header::new(
            current_generation + 1,
            repo_fingerprint,
            git_mtime_sec,
            git_mtime_nsec,
            git_size,
            git_checksum,
            entries.0.len() as u32,
        );
        let header_time = header_start.elapsed();

        // Write atomically
        let write_start = Instant::now();
        let writer = Writer::new(&self.repo_path);
        let write_timing = writer.write_with_timing(&header, &entries.0)?;
        let write_time = write_start.elapsed();

        println!("{}", write_timing); // Print detailed write breakdown

        let total_time = total_start.elapsed();

        Ok(SyncTiming {
            total: total_time,
            generation_read: gen_time,
            metadata_read: meta_time,
            index_parse: index_time,
            build_entries: build_time,
            header_gen: header_time,
            write: write_time,
        })
    }

    fn build_entries_optimized_with_timing(
        &self,
        index: &GitIndex,
    ) -> Result<(Vec<Entry>, BuildTiming)> {
        let total_start = Instant::now();

        // Load HEAD tree once (Solution 1)
        let head_start = Instant::now();
        let head_tree = self.load_head_tree()?;
        let head_time = head_start.elapsed();

        // Collect entries into a Vec for parallel processing
        let index_entries: Vec<_> = index.entries().collect();

        let loop_start = Instant::now();
        let entries_res: Result<Vec<Entry>> = index_entries
            .into_par_iter()
            .map(|e| self.build_entry_from_git(&e, &head_tree))
            .collect();
        let entries = entries_res?;
        let loop_time = loop_start.elapsed();

        let total_time = total_start.elapsed();

        Ok((
            entries,
            BuildTiming {
                total: total_time,
                load_head_tree: head_time,
                build_loop: loop_time,
            },
        ))
    }
}

pub struct SyncTiming {
    pub total: Duration,
    pub generation_read: Duration,
    pub metadata_read: Duration,
    pub index_parse: Duration,
    pub build_entries: Duration,
    pub header_gen: Duration,
    pub write: Duration,
}

#[derive(Debug, Clone)]
pub struct BuildTiming {
    pub total: Duration,
    pub load_head_tree: Duration,
    pub build_loop: Duration,
}

impl std::fmt::Display for SyncTiming {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "Sync Timing Breakdown:")?;
        writeln!(f, "  Total:           {:>8.2?}", self.total)?;
        writeln!(f, "  Generation read: {:>8.2?}", self.generation_read)?;
        writeln!(f, "  Metadata read:   {:>8.2?}", self.metadata_read)?;
        writeln!(f, "  Index parse:     {:>8.2?}", self.index_parse)?;
        writeln!(f, "  Build entries:   {:>8.2?}", self.build_entries)?;
        writeln!(f, "  Header gen:      {:>8.2?}", self.header_gen)?;
        writeln!(f, "  Write:           {:>8.2?}", self.write)?;
        Ok(())
    }
}

fn system_time_to_parts(time: SystemTime) -> (u64, u32) {
    let duration = time.duration_since(UNIX_EPOCH).unwrap_or_default();
    (duration.as_secs(), duration.subsec_nanos())
}

fn read_git_index_checksum(path: &Path) -> Result<[u8; 20]> {
    let data = fs::read(path)?;
    if data.len() < 20 {
        anyhow::bail!(".git/index too small");
    }

    let mut checksum = [0u8; 20];
    checksum.copy_from_slice(&data[data.len() - 20..]);
    Ok(checksum)
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
