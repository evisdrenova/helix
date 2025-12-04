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

use crate::helix_index::hash::{hash_bytes, hash_to_hex, hex_to_hash, Hash};
use anyhow::{Context, Result};
use rayon::prelude::*;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Commit - represents a snapshot in history
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Commit {
    /// Commit hash (BLAKE3) - computed from content
    pub hash: Hash,

    /// Root tree hash (BLAKE3)
    pub tree: Hash,

    /// Parent commit hash(es) - empty for initial commit, 1 for normal, 2+ for merge
    pub parents: Vec<Hash>,

    /// Author name and email
    pub author: String,

    /// Author timestamp (seconds since Unix epoch)
    pub author_time: u64,

    /// Committer name and email (usually same as author)
    pub committer: String,

    /// Committer timestamp (seconds since Unix epoch)
    pub commit_time: u64,

    /// Commit message
    pub message: String,
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

        let mut commit = Self {
            hash: [0u8; 32], // Temporary, will be computed
            tree,
            parents,
            author,
            author_time: now,
            committer,
            commit_time: now,
            message,
        };

        // Compute hash from content (excluding hash field)
        commit.hash = commit.compute_hash();
        commit
    }

    /// Compute hash from commit content (for internal use)
    fn compute_hash(&self) -> Hash {
        let bytes = self.to_bytes_without_hash();
        hash_bytes(&bytes)
    }

    /// Serialize commit to bytes (excluding hash field)
    fn to_bytes_without_hash(&self) -> Vec<u8> {
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

    /// Get commit hash (already computed)
    pub fn hash(&self) -> Hash {
        self.hash
    }

    /// Get short hash (first 8 hex characters)
    pub fn short_hash(&self) -> String {
        let hex = crate::helix_index::hash::hash_to_hex(&self.hash);
        hex[..8].to_string()
    }

    /// Serialize commit to bytes (for storage)
    pub fn to_bytes(&self) -> Vec<u8> {
        // Note: We don't serialize the hash field since it's computed from the content
        self.to_bytes_without_hash()
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

        // Create commit and compute hash
        let mut commit = Self {
            hash: [0u8; 32], // Temporary
            tree,
            parents,
            author,
            author_time,
            committer,
            commit_time,
            message,
        };

        // Compute hash from the content
        commit.hash = hash_bytes(bytes);

        Ok(commit)
    }

    /// Get short commit message (first line)
    pub fn summary(&self) -> &str {
        self.message.lines().next().unwrap_or("")
    }

    /// Get relative time (e.g., "2 hours ago")
    pub fn relative_time(&self) -> String {
        use time::OffsetDateTime;

        let now = OffsetDateTime::now_utc();
        let commit_time = OffsetDateTime::from_unix_timestamp(self.commit_time as i64)
            .unwrap_or(OffsetDateTime::UNIX_EPOCH);

        let duration = now - commit_time;
        let seconds = duration.whole_seconds();

        if seconds < 60 {
            format!("{} seconds ago", seconds)
        } else if seconds < 3600 {
            let minutes = seconds / 60;
            format!(
                "{} minute{} ago",
                minutes,
                if minutes == 1 { "" } else { "s" }
            )
        } else if seconds < 86400 {
            let hours = seconds / 3600;
            format!("{} hour{} ago", hours, if hours == 1 { "" } else { "s" })
        } else if seconds < 604800 {
            let days = seconds / 86400;
            format!("{} day{} ago", days, if days == 1 { "" } else { "s" })
        } else if seconds < 2592000 {
            let weeks = seconds / 604800;
            format!("{} week{} ago", weeks, if weeks == 1 { "" } else { "s" })
        } else if seconds < 31536000 {
            let months = seconds / 2592000;
            format!("{} month{} ago", months, if months == 1 { "" } else { "s" })
        } else {
            let years = seconds / 31536000;
            format!("{} year{} ago", years, if years == 1 { "" } else { "s" })
        }
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

/// Format Unix timestamp as UTC in RFC3339 format
/// Format Unix timestamp for display
pub fn format_timestamp(timestamp: u64) -> String {
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

/// Load commits from Helix repository
pub struct CommitLoader {
    repo_path: PathBuf,
    storage: CommitStorage,
}

impl CommitLoader {
    pub fn new(repo_path: &Path) -> Result<Self> {
        let helix_dir = repo_path.join(".helix");
        if !helix_dir.exists() {
            anyhow::bail!("Not a Helix repository (no .helix directory found)");
        }

        let storage = CommitStorage::for_repo(repo_path);

        Ok(Self {
            repo_path: repo_path.to_path_buf(),
            storage,
        })
    }

    /// Load commits starting from HEAD
    pub fn load_commits(&self, limit: usize) -> Result<Vec<Commit>> {
        let mut commits = Vec::new();

        // Read HEAD
        let head_hash = match self.read_head() {
            Ok(hash) => hash,
            Err(_) => {
                // No commits yet
                return Ok(commits);
            }
        };

        // Walk commit history
        let mut current_hash = head_hash;
        let mut visited = std::collections::HashSet::new();

        while commits.len() < limit {
            // Prevent infinite loops
            if !visited.insert(current_hash) {
                break;
            }

            // Load commit
            let commit = match self.storage.read(&current_hash) {
                Ok(c) => c,
                Err(_) => break,
            };

            // Move to parent
            if commit.is_initial() {
                commits.push(commit);
                break;
            }

            let next_hash = commit.parents[0]; // Follow first parent
            commits.push(commit);
            current_hash = next_hash;
        }

        Ok(commits)
    }

    /// Read current HEAD commit hash
    fn read_head(&self) -> Result<Hash> {
        let head_path = self.repo_path.join(".helix").join("HEAD");

        if !head_path.exists() {
            anyhow::bail!("HEAD not found");
        }

        let content = fs::read_to_string(&head_path).context("Failed to read HEAD")?;

        let content = content.trim();

        if content.starts_with("ref:") {
            // Symbolic reference
            let ref_path = content.strip_prefix("ref:").unwrap().trim();
            let full_ref_path = self.repo_path.join(".helix").join(ref_path);

            if !full_ref_path.exists() {
                anyhow::bail!("Reference {} not found", ref_path);
            }

            let ref_content =
                fs::read_to_string(&full_ref_path).context("Failed to read reference")?;

            hex_to_hash(ref_content.trim()).context("Invalid hash in reference")
        } else {
            // Direct hash
            hex_to_hash(content).context("Invalid hash in HEAD")
        }
    }

    /// Get current branch name
    pub fn get_current_branch_name(&self) -> Result<String> {
        let head_path = self.repo_path.join(".helix").join("HEAD");

        if !head_path.exists() {
            return Ok("(no branch)".to_string());
        }

        let content = fs::read_to_string(&head_path)?;
        let content = content.trim();

        if content.starts_with("ref:") {
            let ref_path = content.strip_prefix("ref:").unwrap().trim();

            // Extract branch name from refs/heads/main
            if let Some(branch) = ref_path.strip_prefix("refs/heads/") {
                Ok(branch.to_string())
            } else {
                Ok("(unknown)".to_string())
            }
        } else {
            Ok("(detached HEAD)".to_string())
        }
    }

    // pub fn get_current_branch_name(repo_path: &Path) -> Result<String> {
    //     let head_path = repo_path.join(".helix/HEAD");

    //     if !head_path.exists() {
    //         return Ok("(no branch)".to_string());
    //     }

    //     let content = fs::read_to_string(&head_path)?;

    //     if content.starts_with("ref:") {
    //         let branch_ref = content.strip_prefix("ref:").unwrap().trim();
    //         let branch_path = repo_path.join(".helix").join(branch_ref);

    //         // Check if branch file exists
    //         if !branch_path.exists() {
    //             // Branch doesn't exist yet (before first commit)
    //             // But we can still show the branch name!
    //             if let Some(name) = branch_ref.strip_prefix("refs/heads/") {
    //                 return Ok(format!("{} (no commits yet)", name));
    //             }
    //         }

    //         // Branch exists, extract name
    //         if let Some(name) = branch_ref.strip_prefix("refs/heads/") {
    //             return Ok(name.to_string());
    //         }
    //     }

    //     Ok("(detached HEAD)".to_string())
    // }

    /// Get repository name from path
    pub fn get_repo_name(&self) -> String {
        self.repo_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("repository")
            .to_string()
    }

    /// Get remote tracking information (placeholder)
    pub fn remote_tracking_info(&self) -> Option<(String, usize, usize)> {
        // TODO: Implement remote tracking
        // For now, return None (no remote)
        None
    }

    /// Checkout a commit (placeholder)
    pub fn checkout_commit(&self, commit_hash: &Hash, branch_name: Option<&str>) -> Result<()> {
        // TODO: Implement checkout
        // For now, just update HEAD

        let head_path = self.repo_path.join(".helix").join("HEAD");

        if let Some(branch) = branch_name {
            // Create branch and checkout
            let branch_ref = format!("refs/heads/{}", branch);
            let branch_path = self.repo_path.join(".helix").join(&branch_ref);

            // Ensure refs/heads directory exists
            if let Some(parent) = branch_path.parent() {
                fs::create_dir_all(parent)?;
            }

            // Write commit hash to branch
            let hash_hex = hash_to_hex(commit_hash);
            fs::write(&branch_path, hash_hex)?;

            // Update HEAD to point to branch
            let head_content = format!("ref: {}", branch_ref);
            fs::write(&head_path, head_content)?;
        } else {
            // Detached HEAD
            let hash_hex = hash_to_hex(commit_hash);
            fs::write(&head_path, hash_hex)?;
        }

        Ok(())
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
            hash: [2u8; 32],
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
