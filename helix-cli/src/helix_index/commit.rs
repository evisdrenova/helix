use anyhow::{Context, Result};
use chrono::DateTime;
use helix_protocol::hash::{hash_bytes, hash_to_hex, hex_to_hash, Hash};
use helix_protocol::message::ObjectType;
use helix_protocol::storage::FsObjectStore;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Commit - represents a snapshot in history
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Commit {
    pub commit_hash: Hash,  // hash of the entire commit objects
    pub tree_hash: Hash,    // hash of the entire root tree
    pub parents: Vec<Hash>, // Parent commit hash(es) - empty for initial commit, 1 for normal, 2+ for merge
    pub author: String,     // Author name and email
    pub author_time: u64,   // Author timestamp (seconds since Unix epoch)
    pub commit_time: u64,   // Committer timestamp (seconds since Unix epoch)
    pub message: String,    // Commit message
}

impl Commit {
    /// Create new commit
    pub fn new(tree_hash: Hash, parents: Vec<Hash>, author: String, message: String) -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let mut commit = Self {
            commit_hash: [0u8; 32],
            tree_hash,
            parents,
            author,
            author_time: now,
            commit_time: now,
            message,
        };

        // Compute hash from content (excluding hash field)
        commit.commit_hash = commit.compute_hash();
        commit
    }

    /// Compute hash from commit content
    pub fn compute_hash(&self) -> Hash {
        let bytes = self.to_bytes_without_hash();
        hash_bytes(&bytes)
    }

    /// Serialize commit to bytes (excluding hash field)
    fn to_bytes_without_hash(&self) -> Vec<u8> {
        let mut bytes = Vec::new();

        // Tree hash (32 bytes)
        bytes.extend_from_slice(&self.tree_hash);

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
        Self::new(tree, vec![], author, message)
    }

    /// Create commit with one parent
    pub fn with_parent(tree: Hash, parent: Hash, author: String, message: String) -> Self {
        Self::new(tree, vec![parent], author, message)
    }

    /// Create merge commit (2+ parents)
    pub fn merge(tree: Hash, parents: Vec<Hash>, author: String, message: String) -> Self {
        assert!(parents.len() >= 2, "Merge commit needs 2+ parents");
        Self::new(tree, parents, author, message)
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
    pub fn get_hash(&self) -> Hash {
        self.commit_hash
    }

    /// Get short hash (first 8 hex characters)
    pub fn get_short_hash(&self) -> String {
        let hex = hash_to_hex(&self.commit_hash);
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
        let mut tree_hash = [0u8; 32];
        tree_hash.copy_from_slice(&bytes[offset..offset + 32]);
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
            commit_hash: [0u8; 32], // Temporary
            tree_hash,
            parents,
            author,
            author_time,
            commit_time,
            message,
        };

        // Compute hash from the content
        commit.commit_hash = hash_bytes(bytes);

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
        let hash_hex = hash_to_hex(hash);
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

/// Format Unix timestamp as a human-readable UTC string
pub fn format_timestamp(timestamp: u64) -> String {
    let datetime = DateTime::from_timestamp(timestamp as i64, 0).unwrap_or_default();

    datetime.format("%m/%d/%y %H:%M:%S").to_string()
}

pub struct CommitStore {
    repo_path: PathBuf,
    objects: FsObjectStore,
}

// A wrapper around the FsObjectStore with specific methods for reading and writing commits to the store
impl CommitStore {
    pub fn new(repo_path: &Path, objects: FsObjectStore) -> Result<Self> {
        let helix_dir = repo_path.join(".helix");
        if !helix_dir.exists() {
            anyhow::bail!("Not a Helix repository (no .helix directory found)");
        }

        Ok(Self {
            repo_path: repo_path.to_path_buf(),
            objects,
        })
    }

    /// Write a commit to storage
    pub fn write_commit(&self, commit: &Commit) -> Result<Hash> {
        let bytes = commit.to_bytes();
        self.objects.write_object(&ObjectType::Commit, &bytes)
    }

    /// Read a commit from storage
    pub fn read_commit(&self, hash: &Hash) -> Result<Commit> {
        let bytes = self.objects.read_object(&ObjectType::Commit, hash)?;
        Commit::from_bytes(&bytes)
    }

    /// Check if commit exists
    pub fn commit_exists(&self, hash: &Hash) -> bool {
        self.objects.has_object(&ObjectType::Commit, hash)
    }

    /// List all commit hashes
    pub fn list_commits(&self) -> Result<Vec<Hash>> {
        self.objects.list_object_hashes(&ObjectType::Commit)
    }

    /// Write multiple commits in parallel
    pub fn write_commits_batch(&self, commits: &[Commit]) -> Result<Vec<Hash>> {
        let bytes: Vec<Vec<u8>> = commits.iter().map(|c| c.to_bytes()).collect();
        self.objects
            .write_objects_batch(&ObjectType::Commit, &bytes)
    }

    /// Read multiple commits in parallel
    pub fn read_commits_batch(&self, hashes: &[Hash]) -> Result<Vec<Commit>> {
        let bytes = self
            .objects
            .read_objects_batch(&ObjectType::Commit, hashes)?;
        bytes.iter().map(|b| Commit::from_bytes(b)).collect()
    }

    /// Load commits starting from HEAD
    pub fn load_commits(&self, limit: usize) -> Result<Vec<Commit>> {
        let mut commits = Vec::new();

        let head_hash = match read_head(&self.repo_path) {
            Ok(hash) => hash,
            Err(_) => return Ok(commits),
        };

        let mut current_hash = head_hash;
        let mut visited = std::collections::HashSet::new();

        while commits.len() < limit {
            if !visited.insert(current_hash) {
                break;
            }

            let commit = match self.read_commit(&current_hash) {
                Ok(c) => c,
                Err(_) => break,
            };

            if commit.is_initial() {
                commits.push(commit);
                break;
            }

            let next_hash = commit.parents[0];
            commits.push(commit);
            current_hash = next_hash;
        }

        Ok(commits)
    }

    /// Get repository name from path
    pub fn get_repo_name(&self) -> String {
        self.repo_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("repository")
            .to_string()
    }

    pub fn load_commits_for_branch(&self, branch_name: &str, limit: usize) -> Result<Vec<Commit>> {
        self.load_commits_for_branch_until(branch_name, limit, None)
    }

    pub fn load_commits_for_branch_until(
        &self,
        branch_name: &str,
        limit: usize,
        stop_at: Option<&Hash>,
    ) -> Result<Vec<Commit>> {
        // Determine ref path based on branch type
        let ref_path = if branch_name.starts_with("sandboxes/") {
            let sandbox_name = branch_name.strip_prefix("sandboxes/").unwrap();
            self.repo_path
                .join(".helix")
                .join("refs/sandboxes")
                .join(sandbox_name)
        } else {
            self.repo_path
                .join(".helix")
                .join("refs/heads")
                .join(branch_name)
        };

        if !ref_path.exists() {
            return Ok(Vec::new());
        }

        let tip_hex = fs::read_to_string(&ref_path)
            .with_context(|| format!("Failed to read branch ref {:?}", ref_path))?;
        let mut current_hash = hex_to_hash(tip_hex.trim()).context("Invalid hash in branch ref")?;

        let mut commits = Vec::new();
        let mut visited = std::collections::HashSet::new();

        while commits.len() < limit {
            // Stop if we've reached the base commit
            if let Some(stop_hash) = stop_at {
                if &current_hash == stop_hash {
                    break;
                }
            }

            if !visited.insert(current_hash) {
                break;
            }

            let commit = match self.read_commit(&current_hash) {
                Ok(c) => c,
                Err(_) => break,
            };

            let is_initial = commit.is_initial();
            commits.push(commit);

            if is_initial {
                break;
            }

            current_hash = commits.last().unwrap().parents[0];
        }

        Ok(commits)
    }

    /// Get remote tracking information (placeholder)
    pub fn remote_tracking_info(&self) -> Option<(String, usize, usize)> {
        // TODO: Implement remote tracking
        // For now, return None (no remote)
        None
    }

    /// TODO:Checkout a commit
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

/// Read current HEAD commit hash
pub fn read_head(repo_path: &Path) -> Result<Hash> {
    let head_path = repo_path.join(".helix").join("HEAD");

    if !head_path.exists() {
        anyhow::bail!("HEAD not found");
    }

    let content = fs::read_to_string(&head_path).context("Failed to read HEAD")?;

    let content = content.trim();

    if content.starts_with("ref:") {
        // Symbolic reference
        let ref_path = content.strip_prefix("ref:").unwrap().trim();
        let full_ref_path = repo_path.join(".helix").join(ref_path);

        if !full_ref_path.exists() {
            anyhow::bail!("Reference {} not found", ref_path);
        }

        let ref_content = fs::read_to_string(&full_ref_path).context("Failed to read reference")?;

        hex_to_hash(ref_content.trim()).context("Invalid hash in reference")
    } else {
        // Direct hash
        hex_to_hash(content).context("Invalid hash in HEAD")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn setup_test_repo(temp_dir: &TempDir) -> Result<(FsObjectStore, CommitStore)> {
        // Create .helix directory so CommitStore doesn't fail
        let helix_dir = temp_dir.path().join(".helix");
        fs::create_dir_all(&helix_dir)?;

        let objects = FsObjectStore::new(temp_dir.path());
        let loader = CommitStore::new(temp_dir.path(), objects.clone())?;
        Ok((objects, loader))
    }

    #[test]
    fn test_commit_serialization() {
        let commit = Commit::initial(
            [1u8; 32],
            "John Doe <john@example.com>".to_string(),
            "Initial commit".to_string(),
        );

        let bytes = commit.to_bytes();
        let parsed = Commit::from_bytes(&bytes).unwrap();

        assert_eq!(parsed.tree_hash, commit.tree_hash);
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
            commit_hash: [2u8; 32],
            tree_hash: [1u8; 32],
            parents: vec![[2u8; 32]],
            author: "John Doe <john@example.com>".to_string(),
            author_time: 1234567890,
            commit_time: 1234567890,
            message: "Test commit".to_string(),
        };

        let commit2 = commit1.clone();

        assert_eq!(commit1.get_hash(), commit2.get_hash());
    }

    #[test]
    fn test_commit_storage_write_read() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let (_, loader) = setup_test_repo(&temp_dir)?;

        let commit = Commit::initial(
            [1u8; 32],
            "John Doe <john@example.com>".to_string(),
            "Initial commit".to_string(),
        );

        let hash = loader.write_commit(&commit)?;
        let read_commit = loader.read_commit(&hash)?;

        assert_eq!(read_commit.tree_hash, commit.tree_hash);
        assert_eq!(read_commit.message, commit.message);

        Ok(())
    }

    #[test]
    fn test_commit_storage_deduplication() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let (_, loader) = setup_test_repo(&temp_dir)?;

        let commit = Commit::initial(
            [1u8; 32],
            "John Doe <john@example.com>".to_string(),
            "Initial commit".to_string(),
        );

        let hash1 = loader.write_commit(&commit)?;
        let hash2 = loader.write_commit(&commit)?;

        assert_eq!(hash1, hash2);

        let all_commits = loader.list_commits()?;
        assert_eq!(all_commits.len(), 1);

        Ok(())
    }

    #[test]
    fn test_commit_storage_exists() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let (_, loader) = setup_test_repo(&temp_dir)?;

        let commit = Commit::initial(
            [1u8; 32],
            "John Doe <john@example.com>".to_string(),
            "Initial commit".to_string(),
        );

        let hash = loader.write_commit(&commit)?;

        assert!(loader.commit_exists(&hash));
        assert!(!loader.commit_exists(&[255u8; 32]));

        Ok(())
    }

    #[test]
    fn test_commit_batch_operations() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let (_, loader) = setup_test_repo(&temp_dir)?;

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
        let hashes = loader.write_commits_batch(&commits)?;
        assert_eq!(hashes.len(), 10);

        // Read all
        let read_commits = loader.read_commits_batch(&hashes)?;
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
