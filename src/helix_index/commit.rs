// Commit objects - Immutable snapshots with metadata
//
// PURE HELIX COMMITS:
// - BLAKE3 hashes (not SHA-1)
// - Zstd compression (not zlib)
// - Tree reference (root tree hash)
// - Parent commit(s) for history
// - Author/committer metadata
// - Commit message
//
// Storage: .helix/objects/commits/{BLAKE3_HASH}

use crate::helix_index::hash::{hash_bytes, Hash};
use anyhow::{Context, Result};
use rayon::prelude::*;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Commit - represents a snapshot in history
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Commit {
    pub tree: Hash,         // Root tree hash (BLAKE3)
    pub parents: Vec<Hash>, // Parent commit hash(es) - empty for initial commit, 1 for normal, 2+ for merge
    pub author: String,     // Author name and email
    pub author_time: u64,   // Author timestamp (seconds since Unix epoch)
    pub committer: String,  // Committer name and email (usually same as author)
    pub commit_time: u64,   // Committer timestamp (seconds since Unix epoch)
    pub message: String,    // Commit message
}

impl Commit {
    /// Create new commit
    pub fn new(
        tree: Hash,
        parents: Vec<Hash>,
        author: String,
        committer: String,
        message: String,
    ) -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        Self {
            tree,
            parents,
            author,
            author_time: now,
            committer,
            commit_time: now,
            message,
        }
    }

    /// Create initial commit (no parents)
    pub fn initial(tree: Hash, author: String, message: String) -> Self {
        Self::new(tree, vec![], author.clone(), author, message)
    }

    /// Create commit with one parent
    pub fn with_parent(tree: Hash, parent: Hash, author: String, message: String) -> Self {
        Self::new(tree, vec![parent], author.clone(), author, message)
    }

    /// Create merge commit (2+ parents)
    pub fn merge(tree: Hash, parents: Vec<Hash>, author: String, message: String) -> Self {
        assert!(parents.len() >= 2, "Merge commit needs 2+ parents");
        Self::new(tree, parents, author.clone(), author, message)
    }

    /// Check if this is the initial commit
    pub fn is_initial(&self) -> bool {
        self.parents.is_empty()
    }

    /// Check if this is a merge commit
    pub fn is_merge(&self) -> bool {
        self.parents.len() >= 2
    }

    /// Compute commit hash (BLAKE3)
    pub fn hash(&self) -> Hash {
        let bytes = self.to_bytes();
        hash_bytes(&bytes)
    }

    /// Serialize commit to bytes
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();

        // Tree hash (32 bytes)
        bytes.extend_from_slice(&self.tree);

        // Parent count (4 bytes)
        bytes.extend_from_slice(&(self.parents.len() as u32).to_le_bytes());

        // Parent hashes (32 bytes each)
        for parent in &self.parents {
            bytes.extend_from_slice(parent);
        }

        // Author length (2 bytes)
        bytes.extend_from_slice(&(self.author.len() as u16).to_le_bytes());
        // Author (variable)
        bytes.extend_from_slice(self.author.as_bytes());

        // Author time (8 bytes)
        bytes.extend_from_slice(&self.author_time.to_le_bytes());

        // Committer length (2 bytes)
        bytes.extend_from_slice(&(self.committer.len() as u16).to_le_bytes());
        // Committer (variable)
        bytes.extend_from_slice(self.committer.as_bytes());

        // Commit time (8 bytes)
        bytes.extend_from_slice(&self.commit_time.to_le_bytes());

        // Message length (4 bytes)
        bytes.extend_from_slice(&(self.message.len() as u32).to_le_bytes());
        // Message (variable)
        bytes.extend_from_slice(self.message.as_bytes());

        bytes
    }

    /// Deserialize commit from bytes
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < 32 + 4 {
            anyhow::bail!("Commit too short: {} bytes", bytes.len());
        }

        let mut offset = 0;

        // Tree hash (32 bytes)
        let mut tree = [0u8; 32];
        tree.copy_from_slice(&bytes[offset..offset + 32]);
        offset += 32;

        // Parent count (4 bytes)
        let parent_count = u32::from_le_bytes(bytes[offset..offset + 4].try_into()?) as usize;
        offset += 4;

        // Parent hashes
        let mut parents = Vec::with_capacity(parent_count);
        for _ in 0..parent_count {
            if offset + 32 > bytes.len() {
                anyhow::bail!("Commit ended unexpectedly while reading parents");
            }
            let mut parent = [0u8; 32];
            parent.copy_from_slice(&bytes[offset..offset + 32]);
            parents.push(parent);
            offset += 32;
        }

        // Author length (2 bytes)
        if offset + 2 > bytes.len() {
            anyhow::bail!("Commit ended unexpectedly while reading author length");
        }
        let author_len = u16::from_le_bytes(bytes[offset..offset + 2].try_into()?) as usize;
        offset += 2;

        // Author (variable)
        if offset + author_len > bytes.len() {
            anyhow::bail!("Commit ended unexpectedly while reading author");
        }
        let author = String::from_utf8(bytes[offset..offset + author_len].to_vec())?;
        offset += author_len;

        // Author time (8 bytes)
        if offset + 8 > bytes.len() {
            anyhow::bail!("Commit ended unexpectedly while reading author time");
        }
        let author_time = u64::from_le_bytes(bytes[offset..offset + 8].try_into()?);
        offset += 8;

        // Committer length (2 bytes)
        if offset + 2 > bytes.len() {
            anyhow::bail!("Commit ended unexpectedly while reading committer length");
        }
        let committer_len = u16::from_le_bytes(bytes[offset..offset + 2].try_into()?) as usize;
        offset += 2;

        // Committer (variable)
        if offset + committer_len > bytes.len() {
            anyhow::bail!("Commit ended unexpectedly while reading committer");
        }
        let committer = String::from_utf8(bytes[offset..offset + committer_len].to_vec())?;
        offset += committer_len;

        // Commit time (8 bytes)
        if offset + 8 > bytes.len() {
            anyhow::bail!("Commit ended unexpectedly while reading commit time");
        }
        let commit_time = u64::from_le_bytes(bytes[offset..offset + 8].try_into()?);
        offset += 8;

        // Message length (4 bytes)
        if offset + 4 > bytes.len() {
            anyhow::bail!("Commit ended unexpectedly while reading message length");
        }
        let message_len = u32::from_le_bytes(bytes[offset..offset + 4].try_into()?) as usize;
        offset += 4;

        // Message (variable)
        if offset + message_len > bytes.len() {
            anyhow::bail!("Commit ended unexpectedly while reading message");
        }
        let message = String::from_utf8(bytes[offset..offset + message_len].to_vec())?;

        Ok(Self {
            tree,
            parents,
            author,
            author_time,
            committer,
            commit_time,
            message,
        })
    }

    /// Get short commit message (first line)
    pub fn summary(&self) -> &str {
        self.message.lines().next().unwrap_or("")
    }

    /// Format commit for display
    pub fn format(&self, hash: &Hash) -> String {
        let hash_hex = crate::helix_index::hash::hash_to_hex(hash);
        let short_hash = &hash_hex[..8];

        format!(
            "commit {}\nAuthor: {}\nDate:   {}\n\n    {}",
            short_hash,
            self.author,
            format_timestamp(self.author_time),
            self.message.lines().collect::<Vec<_>>().join("\n    ")
        )
    }
}

/// Format Unix timestamp for display
fn format_timestamp(timestamp: u64) -> String {
    use std::time::{Duration, UNIX_EPOCH};

    let duration = Duration::from_secs(timestamp);
    let datetime = UNIX_EPOCH + duration;

    // Simple formatting (in production, use chrono)
    format!("{:?}", datetime)
}

/// Commit storage - stores commits in .helix/objects/commits/
pub struct CommitStorage {
    commits_dir: PathBuf,
}

impl CommitStorage {
    pub fn new(helix_dir: &Path) -> Self {
        Self {
            commits_dir: helix_dir.join("objects").join("commits"),
        }
    }

    pub fn for_repo(repo_path: &Path) -> Self {
        Self::new(&repo_path.join(".helix"))
    }

    /// Write commit to storage (with compression)
    pub fn write(&self, commit: &Commit) -> Result<Hash> {
        // Ensure directory exists
        fs::create_dir_all(&self.commits_dir).context("Failed to create commits directory")?;

        // Serialize commit
        let commit_bytes = commit.to_bytes();

        // Compress with Zstd
        let compressed =
            zstd::encode_all(&commit_bytes[..], 3).context("Failed to compress commit")?;

        // Compute hash
        let hash = hash_bytes(&commit_bytes);

        // Write to storage (atomic)
        let commit_path = self.commit_path(&hash);
        if commit_path.exists() {
            // Already stored (deduplication)
            return Ok(hash);
        }

        let temp_path = commit_path.with_extension("tmp");
        fs::write(&temp_path, &compressed)
            .with_context(|| format!("Failed to write commit to {:?}", temp_path))?;
        fs::rename(temp_path, &commit_path).context("Failed to rename commit file")?;

        Ok(hash)
    }

    /// Read commit from storage
    pub fn read(&self, hash: &Hash) -> Result<Commit> {
        let commit_path = self.commit_path(hash);

        if !commit_path.exists() {
            anyhow::bail!("Commit not found: {:?}", hash);
        }

        // Read compressed data
        let compressed = fs::read(&commit_path)
            .with_context(|| format!("Failed to read commit from {:?}", commit_path))?;

        // Decompress
        let commit_bytes =
            zstd::decode_all(&compressed[..]).context("Failed to decompress commit")?;

        // Deserialize
        Commit::from_bytes(&commit_bytes)
    }

    /// Check if commit exists
    pub fn exists(&self, hash: &Hash) -> bool {
        self.commit_path(hash).exists()
    }

    /// Delete commit
    pub fn delete(&self, hash: &Hash) -> Result<()> {
        let commit_path = self.commit_path(hash);
        if commit_path.exists() {
            fs::remove_file(&commit_path)
                .with_context(|| format!("Failed to delete commit {:?}", commit_path))?;
        }
        Ok(())
    }

    /// List all commits
    pub fn list_all(&self) -> Result<Vec<Hash>> {
        if !self.commits_dir.exists() {
            return Ok(Vec::new());
        }

        let mut hashes = Vec::new();

        for entry in fs::read_dir(&self.commits_dir)? {
            let entry = entry?;
            let path = entry.path();

            if path.is_file() {
                if let Some(filename) = path.file_name() {
                    if let Some(filename_str) = filename.to_str() {
                        if filename_str.len() == 64 {
                            // BLAKE3 hash in hex
                            if let Ok(hash) = crate::helix_index::hash::hex_to_hash(filename_str) {
                                hashes.push(hash);
                            }
                        }
                    }
                }
            }
        }

        Ok(hashes)
    }

    fn commit_path(&self, hash: &Hash) -> PathBuf {
        let hex = crate::helix_index::hash::hash_to_hex(hash);
        self.commits_dir.join(hex)
    }

    /// Write multiple commits in parallel (batch operation)
    pub fn write_batch(&self, commits: &[Commit]) -> Result<Vec<Hash>> {
        // Ensure directory exists
        fs::create_dir_all(&self.commits_dir).context("Failed to create commits directory")?;

        // Write all commits in parallel
        commits
            .par_iter()
            .map(|commit| self.write(commit))
            .collect()
    }

    /// Read multiple commits in parallel (batch operation)
    pub fn read_batch(&self, hashes: &[Hash]) -> Result<Vec<Commit>> {
        hashes.par_iter().map(|hash| self.read(hash)).collect()
    }

    /// Check if multiple commits exist in parallel
    pub fn exists_batch(&self, hashes: &[Hash]) -> Vec<bool> {
        hashes.par_iter().map(|hash| self.exists(hash)).collect()
    }
}

/// Batch commit operations helper
pub struct CommitBatch<'a> {
    storage: &'a CommitStorage,
}

impl<'a> CommitBatch<'a> {
    pub fn new(storage: &'a CommitStorage) -> Self {
        Self { storage }
    }

    /// Write all commits (parallel)
    pub fn write_all(&self, commits: &[Commit]) -> Result<Vec<Hash>> {
        self.storage.write_batch(commits)
    }

    /// Read all commits (parallel)
    pub fn read_all(&self, hashes: &[Hash]) -> Result<Vec<Commit>> {
        self.storage.read_batch(hashes)
    }

    /// Check existence of all commits (parallel)
    pub fn exists_all(&self, hashes: &[Hash]) -> Vec<bool> {
        self.storage.exists_batch(hashes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_commit_serialization() {
        let commit = Commit::initial(
            [1u8; 32],
            "John Doe <john@example.com>".to_string(),
            "Initial commit".to_string(),
        );

        let bytes = commit.to_bytes();
        let parsed = Commit::from_bytes(&bytes).unwrap();

        assert_eq!(parsed.tree, commit.tree);
        assert_eq!(parsed.parents, commit.parents);
        assert_eq!(parsed.author, commit.author);
        assert_eq!(parsed.message, commit.message);
    }

    #[test]
    fn test_commit_with_parent() {
        let parent_hash = [2u8; 32];
        let commit = Commit::with_parent(
            [1u8; 32],
            parent_hash,
            "John Doe <john@example.com>".to_string(),
            "Second commit".to_string(),
        );

        assert_eq!(commit.parents.len(), 1);
        assert_eq!(commit.parents[0], parent_hash);
        assert!(!commit.is_initial());
        assert!(!commit.is_merge());
    }

    #[test]
    fn test_merge_commit() {
        let parents = vec![[2u8; 32], [3u8; 32]];
        let commit = Commit::merge(
            [1u8; 32],
            parents.clone(),
            "John Doe <john@example.com>".to_string(),
            "Merge branch 'feature'".to_string(),
        );

        assert_eq!(commit.parents.len(), 2);
        assert!(!commit.is_initial());
        assert!(commit.is_merge());
    }

    #[test]
    fn test_commit_hash_deterministic() {
        let commit1 = Commit {
            tree: [1u8; 32],
            parents: vec![[2u8; 32]],
            author: "John Doe <john@example.com>".to_string(),
            author_time: 1234567890,
            committer: "John Doe <john@example.com>".to_string(),
            commit_time: 1234567890,
            message: "Test commit".to_string(),
        };

        let commit2 = commit1.clone();

        assert_eq!(commit1.hash(), commit2.hash());
    }

    #[test]
    fn test_commit_storage_write_read() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let storage = CommitStorage::new(temp_dir.path());

        let commit = Commit::initial(
            [1u8; 32],
            "John Doe <john@example.com>".to_string(),
            "Initial commit".to_string(),
        );

        let hash = storage.write(&commit)?;
        let read_commit = storage.read(&hash)?;

        assert_eq!(read_commit.tree, commit.tree);
        assert_eq!(read_commit.message, commit.message);

        Ok(())
    }

    #[test]
    fn test_commit_storage_deduplication() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let storage = CommitStorage::new(temp_dir.path());

        let commit = Commit::initial(
            [1u8; 32],
            "John Doe <john@example.com>".to_string(),
            "Initial commit".to_string(),
        );

        let hash1 = storage.write(&commit)?;
        let hash2 = storage.write(&commit)?;

        assert_eq!(hash1, hash2);

        let all_commits = storage.list_all()?;
        assert_eq!(all_commits.len(), 1);

        Ok(())
    }

    #[test]
    fn test_commit_storage_exists() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let storage = CommitStorage::new(temp_dir.path());

        let commit = Commit::initial(
            [1u8; 32],
            "John Doe <john@example.com>".to_string(),
            "Initial commit".to_string(),
        );

        let hash = storage.write(&commit)?;

        assert!(storage.exists(&hash));
        assert!(!storage.exists(&[255u8; 32]));

        Ok(())
    }

    #[test]
    fn test_commit_storage_delete() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let storage = CommitStorage::new(temp_dir.path());

        let commit = Commit::initial(
            [1u8; 32],
            "John Doe <john@example.com>".to_string(),
            "Initial commit".to_string(),
        );

        let hash = storage.write(&commit)?;
        assert!(storage.exists(&hash));

        storage.delete(&hash)?;
        assert!(!storage.exists(&hash));

        Ok(())
    }

    #[test]
    fn test_commit_batch_operations() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let storage = CommitStorage::new(temp_dir.path());
        let batch = CommitBatch::new(&storage);

        // Create commits
        let commits: Vec<Commit> = (0..10)
            .map(|i| {
                Commit::initial(
                    hash_bytes(format!("tree{}", i).as_bytes()),
                    format!("Author {} <author{}@example.com>", i, i),
                    format!("Commit {}", i),
                )
            })
            .collect();

        // Write all
        let hashes = batch.write_all(&commits)?;
        assert_eq!(hashes.len(), 10);

        // Check existence
        let exists = batch.exists_all(&hashes);
        assert!(exists.iter().all(|&e| e));

        // Read all
        let read_commits = batch.read_all(&hashes)?;
        assert_eq!(read_commits.len(), 10);

        // Verify content
        for (original, read) in commits.iter().zip(read_commits.iter()) {
            assert_eq!(original.message, read.message);
        }

        Ok(())
    }

    #[test]
    fn test_commit_summary() {
        let commit = Commit::initial(
            [1u8; 32],
            "John Doe <john@example.com>".to_string(),
            "First line\nSecond line\nThird line".to_string(),
        );

        assert_eq!(commit.summary(), "First line");
    }

    #[test]
    fn test_multiline_commit_message() -> Result<()> {
        let message = "Short summary\n\nLonger description here.\nWith multiple lines.".to_string();

        let commit = Commit::initial(
            [1u8; 32],
            "John Doe <john@example.com>".to_string(),
            message.clone(),
        );

        let bytes = commit.to_bytes();
        let parsed = Commit::from_bytes(&bytes)?;

        assert_eq!(parsed.message, message);
        assert_eq!(parsed.summary(), "Short summary");

        Ok(())
    }
}
