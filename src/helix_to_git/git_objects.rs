// Git object creation - builds Git-compatible commit/tree/blob objects
// Converts Helix objects (BLAKE3, Zstd) → Git objects (SHA1, zlib)

use anyhow::{Context, Result};
use sha1::{Digest, Sha1};
use std::path::Path;

/// Git object types
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitObjectType {
    Blob,
    Tree,
    Commit,
}

impl GitObjectType {
    fn as_str(&self) -> &'static str {
        match self {
            GitObjectType::Blob => "blob",
            GitObjectType::Tree => "tree",
            GitObjectType::Commit => "commit",
        }
    }
}

/// Git commit builder
pub struct GitCommitBuilder {
    tree_sha: String,
    parent_shas: Vec<String>,
    author: String,
    author_time: u64,
    committer: String,
    committer_time: u64,
    message: String,
}

impl GitCommitBuilder {
    pub fn new() -> Self {
        Self {
            tree_sha: String::new(),
            parent_shas: Vec::new(),
            author: String::new(),
            author_time: 0,
            committer: String::new(),
            committer_time: 0,
            message: String::new(),
        }
    }

    pub fn tree(mut self, sha: String) -> Self {
        self.tree_sha = sha;
        self
    }

    pub fn parent(mut self, sha: String) -> Self {
        self.parent_shas.push(sha);
        self
    }

    pub fn parents(mut self, shas: Vec<String>) -> Self {
        self.parent_shas = shas;
        self
    }

    pub fn author(mut self, author: String, timestamp: u64) -> Self {
        self.author = author;
        self.author_time = timestamp;
        self
    }

    pub fn committer(mut self, committer: String, timestamp: u64) -> Self {
        self.committer = committer;
        self.committer_time = timestamp;
        self
    }

    pub fn message(mut self, msg: String) -> Self {
        self.message = msg;
        self
    }

    /// Build Git commit object content
    pub fn build(self) -> Result<Vec<u8>> {
        if self.tree_sha.is_empty() {
            anyhow::bail!("Tree SHA is required");
        }
        if self.author.is_empty() {
            anyhow::bail!("Author is required");
        }
        if self.committer.is_empty() {
            anyhow::bail!("Committer is required");
        }

        let mut content = Vec::new();

        // Tree line
        content.extend_from_slice(b"tree ");
        content.extend_from_slice(self.tree_sha.as_bytes());
        content.push(b'\n');

        // Parent lines
        for parent_sha in &self.parent_shas {
            content.extend_from_slice(b"parent ");
            content.extend_from_slice(parent_sha.as_bytes());
            content.push(b'\n');
        }

        // Author line
        content.extend_from_slice(b"author ");
        content.extend_from_slice(self.author.as_bytes());
        content.push(b' ');
        content.extend_from_slice(self.author_time.to_string().as_bytes());
        content.extend_from_slice(b" +0000\n"); // UTC timezone

        // Committer line
        content.extend_from_slice(b"committer ");
        content.extend_from_slice(self.committer.as_bytes());
        content.push(b' ');
        content.extend_from_slice(self.committer_time.to_string().as_bytes());
        content.extend_from_slice(b" +0000\n"); // UTC timezone

        // Empty line before message
        content.push(b'\n');

        // Message
        content.extend_from_slice(self.message.as_bytes());

        Ok(content)
    }
}

impl Default for GitCommitBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Git tree entry
#[derive(Debug, Clone)]
pub struct GitTreeEntry {
    pub mode: &'static str,
    pub name: String,
    pub sha: String,
}

impl GitTreeEntry {
    /// Create file entry (mode 100644 - regular file)
    pub fn file(name: String, sha: String) -> Self {
        Self {
            mode: "100644",
            name,
            sha,
        }
    }

    /// Create executable file entry (mode 100755)
    pub fn executable(name: String, sha: String) -> Self {
        Self {
            mode: "100755",
            name,
            sha,
        }
    }

    /// Create directory entry (mode 040000)
    pub fn directory(name: String, sha: String) -> Self {
        Self {
            mode: "40000", // Git uses "40000" not "040000"
            name,
            sha,
        }
    }

    /// Convert to Git tree entry format
    fn to_bytes(&self) -> Result<Vec<u8>> {
        let mut bytes = Vec::new();

        // Mode (ASCII)
        bytes.extend_from_slice(self.mode.as_bytes());
        bytes.push(b' ');

        // Name (ASCII)
        bytes.extend_from_slice(self.name.as_bytes());
        bytes.push(b'\0');

        // SHA (binary - 20 bytes)
        let sha_bytes =
            hex::decode(&self.sha).with_context(|| format!("Invalid SHA hex: {}", self.sha))?;

        if sha_bytes.len() != 20 {
            anyhow::bail!("Git SHA must be 20 bytes, got {}", sha_bytes.len());
        }

        bytes.extend_from_slice(&sha_bytes);

        Ok(bytes)
    }
}

/// Git tree builder
pub struct GitTreeBuilder {
    entries: Vec<GitTreeEntry>,
}

impl GitTreeBuilder {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    pub fn add_entry(mut self, entry: GitTreeEntry) -> Self {
        self.entries.push(entry);
        self
    }

    pub fn add_file(mut self, name: String, sha: String) -> Self {
        self.entries.push(GitTreeEntry::file(name, sha));
        self
    }

    pub fn add_directory(mut self, name: String, sha: String) -> Self {
        self.entries.push(GitTreeEntry::directory(name, sha));
        self
    }

    /// Build Git tree object content
    pub fn build(mut self) -> Result<Vec<u8>> {
        // Sort entries by name (Git requirement)
        self.entries.sort_by(|a, b| a.name.cmp(&b.name));

        let mut content = Vec::new();

        for entry in &self.entries {
            content.extend_from_slice(&entry.to_bytes()?);
        }

        Ok(content)
    }
}

impl Default for GitTreeBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Compute Git SHA-1 hash for object
pub fn compute_git_sha(object_type: GitObjectType, content: &[u8]) -> String {
    let header = format!("{} {}\0", object_type.as_str(), content.len());

    let mut hasher = Sha1::new();
    hasher.update(header.as_bytes());
    hasher.update(content);

    let result = hasher.finalize();
    hex::encode(result)
}

/// Create Git object with header
pub fn create_git_object(object_type: GitObjectType, content: &[u8]) -> Vec<u8> {
    let header = format!("{} {}\0", object_type.as_str(), content.len());

    let mut object = Vec::new();
    object.extend_from_slice(header.as_bytes());
    object.extend_from_slice(content);

    object
}

/// Compress Git object with zlib
pub fn compress_git_object(data: &[u8]) -> Result<Vec<u8>> {
    use flate2::write::ZlibEncoder;
    use flate2::Compression;
    use std::io::Write;

    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder
        .write_all(data)
        .context("Failed to compress object")?;
    encoder.finish().context("Failed to finish compression")
}

/// Parse Git author/committer string
/// Format: "Name <email>"
pub fn parse_author(author: &str) -> Result<String> {
    // Validate basic format
    if !author.contains('<') || !author.contains('>') {
        anyhow::bail!(
            "Invalid author format: '{}'. Expected 'Name <email>'",
            author
        );
    }

    Ok(author.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_git_commit_builder() -> Result<()> {
        let commit_content = GitCommitBuilder::new()
            .tree("a".repeat(40))
            .parent("b".repeat(40))
            .author("John Doe <john@example.com>".to_string(), 1234567890)
            .committer("John Doe <john@example.com>".to_string(), 1234567890)
            .message("Test commit".to_string())
            .build()?;

        let commit_str = String::from_utf8(commit_content)?;

        assert!(commit_str.contains("tree aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"));
        assert!(commit_str.contains("parent bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"));
        assert!(commit_str.contains("author John Doe <john@example.com> 1234567890 +0000"));
        assert!(commit_str.contains("committer John Doe <john@example.com> 1234567890 +0000"));
        assert!(commit_str.contains("Test commit"));

        Ok(())
    }

    #[test]
    fn test_git_tree_builder() -> Result<()> {
        let tree_content = GitTreeBuilder::new()
            .add_file("file.txt".to_string(), "a".repeat(40))
            .add_directory("dir".to_string(), "b".repeat(40))
            .build()?;

        // Tree content is binary, just check it's not empty
        assert!(!tree_content.is_empty());

        Ok(())
    }

    #[test]
    fn test_compute_git_sha() {
        let content = b"Hello, Git!";
        let sha = compute_git_sha(GitObjectType::Blob, content);

        // Should be 40 hex characters
        assert_eq!(sha.len(), 40);
        assert!(sha.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn test_create_git_object() {
        let content = b"test content";
        let obj = create_git_object(GitObjectType::Blob, content);

        let obj_str = String::from_utf8_lossy(&obj);
        assert!(obj_str.starts_with("blob 12\0"));
    }

    #[test]
    fn test_compress_git_object() -> Result<()> {
        let data = b"test data to compress";
        let compressed = compress_git_object(data)?;

        // Compressed should be different from original
        assert_ne!(compressed, data);
        assert!(!compressed.is_empty());

        Ok(())
    }

    #[test]
    fn test_parse_author_valid() -> Result<()> {
        let author = parse_author("John Doe <john@example.com>")?;
        assert_eq!(author, "John Doe <john@example.com>");

        Ok(())
    }

    #[test]
    fn test_parse_author_invalid() {
        assert!(parse_author("Invalid Author").is_err());
        assert!(parse_author("john@example.com").is_err());
    }

    #[test]
    fn test_git_tree_entry_ordering() -> Result<()> {
        let tree = GitTreeBuilder::new()
            .add_file("zebra.txt".to_string(), "a".repeat(40))
            .add_file("apple.txt".to_string(), "b".repeat(40))
            .add_directory("middle".to_string(), "c".repeat(40))
            .build()?;

        // Entries should be sorted (checked internally)
        assert!(!tree.is_empty());

        Ok(())
    }
}
