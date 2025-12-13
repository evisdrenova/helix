// Helix → Git converter
// Converts Helix commits (BLAKE3) to Git commits (SHA1) using push cache

use super::git_objects::{
    compute_git_sha, create_git_object, GitCommitBuilder, GitObjectType, GitTreeBuilder,
    GitTreeEntry,
};
use super::push_cache::PushCache;
use crate::helix_index::blob_storage::BlobStorage;
use crate::helix_index::commit::{Commit, CommitStorage};
use crate::helix_index::hash::Hash;
use crate::helix_index::tree::{Tree, TreeEntry, TreeStorage};
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub struct HelixToGitConverter {
    repo_path: PathBuf,
    cache: PushCache,
    commit_storage: CommitStorage,
    tree_storage: TreeStorage,
    blob_storage: BlobStorage,
}

impl HelixToGitConverter {
    pub fn new(repo_path: &Path) -> Result<Self> {
        let cache = PushCache::load(repo_path)?;
        let commit_storage = CommitStorage::for_repo(repo_path);
        let tree_storage = TreeStorage::for_repo(repo_path);
        let blob_storage = BlobStorage::for_repo(repo_path);

        Ok(Self {
            repo_path: repo_path.to_path_buf(),
            cache,
            commit_storage,
            tree_storage,
            blob_storage,
        })
    }

    /// Convert Helix commit to Git SHA, using cache
    pub fn convert_commit(&mut self, helix_hash: &Hash) -> Result<String> {
        // Check cache first
        if let Some(git_sha) = self.cache.get(helix_hash) {
            return Ok(git_sha.to_string());
        }

        // Load Helix commit
        let helix_commit = self
            .commit_storage
            .read(helix_hash)
            .with_context(|| format!("Failed to load Helix commit {:?}", helix_hash))?;

        // Convert tree
        let git_tree_sha = self.convert_tree(&helix_commit.tree_hash)?;

        // Convert parent commits (recursive)
        let git_parent_shas: Vec<String> = helix_commit
            .parents
            .iter()
            .map(|parent_hash| self.convert_commit(parent_hash))
            .collect::<Result<Vec<_>>>()?;

        // Build Git commit object
        let git_commit_content = GitCommitBuilder::new()
            .tree(git_tree_sha)
            .parents(git_parent_shas)
            .author(helix_commit.author.clone(), helix_commit.author_time)
            .committer(helix_commit.author.clone(), helix_commit.commit_time)
            .message(helix_commit.message.clone())
            .build()?;

        // Compute Git SHA
        let git_sha = compute_git_sha(GitObjectType::Commit, &git_commit_content);

        // Cache the mapping
        self.cache.insert(*helix_hash, git_sha.clone());

        Ok(git_sha)
    }

    /// Convert Helix tree to Git SHA
    fn convert_tree(&mut self, helix_tree_hash: &Hash) -> Result<String> {
        // Check cache first
        if let Some(git_sha) = self.cache.get(helix_tree_hash) {
            return Ok(git_sha.to_string());
        }

        // Load Helix tree
        let helix_tree = self
            .tree_storage
            .read(helix_tree_hash)
            .with_context(|| format!("Failed to load Helix tree {:?}", helix_tree_hash))?;

        // Convert entries
        let mut git_tree_builder = GitTreeBuilder::new();

        for entry in helix_tree.entries() {
            match entry {
                TreeEntry::File {
                    name,
                    blob_hash,
                    mode,
                } => {
                    // Convert blob
                    let git_blob_sha = self.convert_blob(blob_hash)?;

                    // Determine if executable
                    let entry = if *mode == 0o100755 {
                        GitTreeEntry::executable(name.clone(), git_blob_sha)
                    } else {
                        GitTreeEntry::file(name.clone(), git_blob_sha)
                    };

                    git_tree_builder = git_tree_builder.add_entry(entry);
                }
                TreeEntry::Directory { name, tree_hash } => {
                    // Convert subtree recursively
                    let git_subtree_sha = self.convert_tree(tree_hash)?;
                    git_tree_builder =
                        git_tree_builder.add_directory(name.clone(), git_subtree_sha);
                }
            }
        }

        // Build Git tree
        let git_tree_content = git_tree_builder.build()?;

        // Compute Git SHA
        let git_sha = compute_git_sha(GitObjectType::Tree, &git_tree_content);

        // Cache the mapping
        self.cache.insert(*helix_tree_hash, git_sha.clone());

        Ok(git_sha)
    }

    /// Convert Helix blob to Git SHA
    fn convert_blob(&mut self, helix_blob_hash: &Hash) -> Result<String> {
        // Check cache first
        if let Some(git_sha) = self.cache.get(helix_blob_hash) {
            return Ok(git_sha.to_string());
        }

        // Read blob content from Helix storage
        let content = self
            .blob_storage
            .read(helix_blob_hash)
            .with_context(|| format!("Failed to read Helix blob {:?}", helix_blob_hash))?;

        // Compute Git SHA (Git SHA is deterministic from content)
        let git_sha = compute_git_sha(GitObjectType::Blob, &content);

        // Cache the mapping
        self.cache.insert(*helix_blob_hash, git_sha.clone());

        Ok(git_sha)
    }

    /// Get all Git objects that need to be created
    /// Returns: (commits, trees, blobs) as Git object data
    pub fn get_git_objects(&mut self, helix_hash: &Hash) -> Result<GitObjects> {
        let mut git_objects = GitObjects::new();

        self.collect_git_objects(helix_hash, &mut git_objects)?;

        Ok(git_objects)
    }

    /// Recursively collect all Git objects needed
    fn collect_git_objects(
        &mut self,
        helix_hash: &Hash,
        git_objects: &mut GitObjects,
    ) -> Result<()> {
        // If already processed, skip
        if git_objects.processed.contains(helix_hash) {
            return Ok(());
        }

        git_objects.processed.insert(*helix_hash);

        // Load Helix commit
        let helix_commit = self.commit_storage.read(helix_hash)?;

        // Process parents first
        for parent_hash in &helix_commit.parents {
            self.collect_git_objects(parent_hash, git_objects)?;
        }

        // Process tree
        self.collect_tree_objects(&helix_commit.tree_hash, git_objects)?;

        // Convert and add this commit
        let git_commit_sha = self.convert_commit(helix_hash)?;
        let git_commit_content = self.build_git_commit_content(&helix_commit)?;

        git_objects.commits.insert(
            git_commit_sha,
            create_git_object(GitObjectType::Commit, &git_commit_content),
        );

        Ok(())
    }

    /// Collect all tree objects recursively
    fn collect_tree_objects(
        &mut self,
        helix_tree_hash: &Hash,
        git_objects: &mut GitObjects,
    ) -> Result<()> {
        if git_objects.processed.contains(helix_tree_hash) {
            return Ok(());
        }

        git_objects.processed.insert(*helix_tree_hash);

        let helix_tree = self.tree_storage.read(helix_tree_hash)?;

        // Process entries
        for entry in helix_tree.entries() {
            match entry {
                TreeEntry::File { blob_hash, .. } => {
                    // Add blob
                    if !git_objects.processed.contains(blob_hash) {
                        let content = self.blob_storage.read(blob_hash)?;
                        let git_blob_sha = compute_git_sha(GitObjectType::Blob, &content);

                        git_objects.blobs.insert(
                            git_blob_sha,
                            create_git_object(GitObjectType::Blob, &content),
                        );

                        git_objects.processed.insert(*blob_hash);
                    }
                }
                TreeEntry::Directory { tree_hash, .. } => {
                    // Process subtree recursively
                    self.collect_tree_objects(tree_hash, git_objects)?;
                }
            }
        }

        // Add this tree
        let git_tree_sha = self.convert_tree(helix_tree_hash)?;
        let git_tree_content = self.build_git_tree_content(&helix_tree)?;

        git_objects.trees.insert(
            git_tree_sha,
            create_git_object(GitObjectType::Tree, &git_tree_content),
        );

        Ok(())
    }

    /// Build Git commit content
    fn build_git_commit_content(&mut self, helix_commit: &Commit) -> Result<Vec<u8>> {
        let git_tree_sha = self.convert_tree(&helix_commit.tree_hash)?;

        let git_parent_shas: Vec<String> = helix_commit
            .parents
            .iter()
            .map(|p| self.convert_commit(p))
            .collect::<Result<Vec<_>>>()?;

        GitCommitBuilder::new()
            .tree(git_tree_sha)
            .parents(git_parent_shas)
            .author(helix_commit.author.clone(), helix_commit.author_time)
            .committer(helix_commit.author.clone(), helix_commit.commit_time)
            .message(helix_commit.message.clone())
            .build()
    }

    /// Build Git tree content
    fn build_git_tree_content(&mut self, helix_tree: &Tree) -> Result<Vec<u8>> {
        let mut git_tree_builder = GitTreeBuilder::new();

        for entry in helix_tree.entries() {
            match entry {
                TreeEntry::File {
                    name,
                    blob_hash,
                    mode,
                } => {
                    let git_blob_sha = self.convert_blob(blob_hash)?;

                    let entry = if *mode == 0o100755 {
                        GitTreeEntry::executable(name.clone(), git_blob_sha)
                    } else {
                        GitTreeEntry::file(name.clone(), git_blob_sha)
                    };

                    git_tree_builder = git_tree_builder.add_entry(entry);
                }
                TreeEntry::Directory { name, tree_hash } => {
                    let git_subtree_sha = self.convert_tree(tree_hash)?;
                    git_tree_builder =
                        git_tree_builder.add_directory(name.clone(), git_subtree_sha);
                }
            }
        }

        git_tree_builder.build()
    }

    /// Save cache to disk
    pub fn save_cache(&self) -> Result<()> {
        self.cache.save()
    }

    /// Get number of cached conversions
    pub fn cache_size(&self) -> usize {
        self.cache.len()
    }
}

/// Collection of Git objects to be pushed
#[derive(Debug)]
pub struct GitObjects {
    pub commits: HashMap<String, Vec<u8>>,
    pub trees: HashMap<String, Vec<u8>>,
    pub blobs: HashMap<String, Vec<u8>>,
    processed: std::collections::HashSet<Hash>,
}

impl GitObjects {
    fn new() -> Self {
        Self {
            commits: HashMap::new(),
            trees: HashMap::new(),
            blobs: HashMap::new(),
            processed: std::collections::HashSet::new(),
        }
    }

    pub fn total_count(&self) -> usize {
        self.commits.len() + self.trees.len() + self.blobs.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::helix_index::tree::TreeBuilder;
    use tempfile::TempDir;

    #[test]
    fn test_convert_simple_commit() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        // Create Helix structures
        let tree_builder = TreeBuilder::new(repo_path);
        let tree_hash = tree_builder.build_from_entries(&[])?;

        let helix_commit = Commit::initial(
            tree_hash,
            "Test Author <test@example.com>".to_string(),
            "Initial commit".to_string(),
        );

        let commit_storage = CommitStorage::for_repo(repo_path);
        let helix_hash = commit_storage.write(&helix_commit)?;

        // Convert to Git
        let mut converter = HelixToGitConverter::new(repo_path)?;
        let git_sha = converter.convert_commit(&helix_hash)?;

        // Git SHA should be 40 hex characters
        assert_eq!(git_sha.len(), 40);
        assert!(git_sha.chars().all(|c| c.is_ascii_hexdigit()));

        // Verify caching works
        let git_sha2 = converter.convert_commit(&helix_hash)?;
        assert_eq!(git_sha, git_sha2);

        Ok(())
    }
}
