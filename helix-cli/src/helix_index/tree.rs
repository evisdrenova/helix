// Tree building - Create directory snapshots for commits

// Root Tree
// ├── file1.txt → File (hash)
// ├── file2.txt → File (hash)
// └── src/ → tree (hash)
//     ├── main.rs → File (hash)
//     └── lib.rs → File (hash)
//
// Tree format:
// - Each tree represents a directory
// - Contains entries for files (Files) and subdirectories (trees)
// - Entries are sorted by name for deterministic hashing
// - Trees are immutable once created

use crate::helix_index::hash::{hash_bytes, Hash};
use anyhow::{Context, Result};
use rayon::prelude::*;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// Tree entry type
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryType {
    /// Regular file (File)
    File = 0,
    /// Executable file (File with exec bit)
    FileExecutable = 1,
    /// Subdirectory (tree)
    Tree = 2,
    /// Symbolic link
    Symlink = 3,
}

impl EntryType {
    pub fn from_mode(mode: u32) -> Self {
        if mode & 0o120000 == 0o120000 {
            Self::Symlink
        } else if mode & 0o100000 == 0o100000 {
            if mode & 0o111 != 0 {
                Self::FileExecutable
            } else {
                Self::File
            }
        } else {
            // Default to File for unknown types
            Self::File
        }
    }

    pub fn to_mode(&self) -> u32 {
        match self {
            Self::File => 0o100644,
            Self::FileExecutable => 0o100755,
            Self::Tree => 0o040000,
            Self::Symlink => 0o120000,
        }
    }
}

/// Tree entry - represents a file or subdirectory
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeEntry {
    /// Entry name (just the filename, not full path)
    pub name: String,
    /// Entry type (File, tree, etc.)
    pub entry_type: EntryType,
    /// Object hash (BLAKE3)
    pub oid: Hash,
    /// File mode (Unix permissions)
    pub mode: u32,
    /// File size (0 for trees)
    pub size: u64,
}

impl TreeEntry {
    pub fn new_file(name: String, oid: Hash, mode: u32, size: u64) -> Self {
        Self {
            name,
            entry_type: EntryType::from_mode(mode),
            oid,
            mode,
            size,
        }
    }

    pub fn new_tree(name: String, oid: Hash) -> Self {
        Self {
            name,
            entry_type: EntryType::Tree,
            oid,
            mode: 0o040000,
            size: 0,
        }
    }

    /// Serialize entry to bytes for hashing
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();

        // Type (1 byte)
        bytes.push(self.entry_type as u8);

        // Mode (4 bytes)
        bytes.extend_from_slice(&self.mode.to_le_bytes());

        // Size (8 bytes)
        bytes.extend_from_slice(&self.size.to_le_bytes());

        // Name length (2 bytes)
        bytes.extend_from_slice(&(self.name.len() as u16).to_le_bytes());

        // Name (variable)
        bytes.extend_from_slice(self.name.as_bytes());

        // OID (32 bytes)
        bytes.extend_from_slice(&self.oid);

        bytes
    }

    /// Deserialize entry from bytes
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < 47 {
            anyhow::bail!("TreeEntry too short: {} bytes", bytes.len());
        }

        let mut offset = 0;

        // Type (1 byte)
        let entry_type = match bytes[offset] {
            0 => EntryType::File,
            1 => EntryType::FileExecutable,
            2 => EntryType::Tree,
            3 => EntryType::Symlink,
            t => anyhow::bail!("Invalid entry type: {}", t),
        };
        offset += 1;

        // Mode (4 bytes)
        let mode = u32::from_le_bytes(bytes[offset..offset + 4].try_into()?);
        offset += 4;

        // Size (8 bytes)
        let size = u64::from_le_bytes(bytes[offset..offset + 8].try_into()?);
        offset += 8;

        // Name length (2 bytes)
        let name_len = u16::from_le_bytes(bytes[offset..offset + 2].try_into()?) as usize;
        offset += 2;

        // Name (variable)
        if bytes.len() < offset + name_len + 32 {
            anyhow::bail!("TreeEntry name extends past end");
        }
        let name = String::from_utf8(bytes[offset..offset + name_len].to_vec())?;
        offset += name_len;

        // OID (32 bytes)
        let mut oid = [0u8; 32];
        oid.copy_from_slice(&bytes[offset..offset + 32]);

        Ok(Self {
            name,
            entry_type,
            oid,
            mode,
            size,
        })
    }
}

// Ordering: sorted by name for deterministic trees
impl Ord for TreeEntry {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.name.cmp(&other.name)
    }
}

impl PartialOrd for TreeEntry {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// Tree - represents a directory snapshot
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tree {
    /// Entries in this tree (sorted by name)
    pub entries: Vec<TreeEntry>,
}

impl Tree {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    pub fn add_entry(&mut self, entry: TreeEntry) {
        self.entries.push(entry);
    }

    /// Sort entries by name (required for deterministic hashing)
    pub fn sort(&mut self) {
        self.entries.sort();
    }

    /// Compute tree hash (BLAKE3)
    pub fn hash(&self) -> Hash {
        let bytes = self.to_bytes();
        hash_bytes(&bytes)
    }

    /// Serialize tree to bytes
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();

        // Entry count (4 bytes)
        bytes.extend_from_slice(&(self.entries.len() as u32).to_le_bytes());

        // Entries (variable)
        for entry in &self.entries {
            let entry_bytes = entry.to_bytes();
            bytes.extend_from_slice(&entry_bytes);
        }

        bytes
    }

    /// Deserialize tree from bytes
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < 4 {
            anyhow::bail!("Tree too short: {} bytes", bytes.len());
        }

        let entry_count = u32::from_le_bytes(bytes[0..4].try_into()?) as usize;
        let mut offset = 4;

        let mut entries = Vec::with_capacity(entry_count);

        for _ in 0..entry_count {
            // Parse entry (variable size)
            if offset >= bytes.len() {
                anyhow::bail!("Tree ended unexpectedly");
            }

            // Parse name length to know how many bytes to read
            if offset + 15 > bytes.len() {
                anyhow::bail!("Not enough bytes for entry header");
            }

            let name_len = u16::from_le_bytes(bytes[offset + 13..offset + 15].try_into()?) as usize;
            let entry_size = 15 + name_len + 32; // header + name + oid

            if offset + entry_size > bytes.len() {
                anyhow::bail!("Entry extends past tree end");
            }

            let entry = TreeEntry::from_bytes(&bytes[offset..offset + entry_size])?;
            entries.push(entry);
            offset += entry_size;
        }

        Ok(Self { entries })
    }
}

impl Default for Tree {
    fn default() -> Self {
        Self::new()
    }
}

/// Tree storage - stores trees in .helix/objects/trees/
pub struct TreeStorage {
    trees_dir: PathBuf,
}

impl TreeStorage {
    pub fn new(helix_dir: &Path) -> Self {
        Self {
            trees_dir: helix_dir.join("objects").join("trees"),
        }
    }

    pub fn for_repo(repo_path: &Path) -> Self {
        Self::new(&repo_path.join(".helix"))
    }

    /// Write tree to storage (with compression)
    pub fn write(&self, tree: &Tree) -> Result<Hash> {
        // Ensure directory exists
        fs::create_dir_all(&self.trees_dir).context("Failed to create trees directory")?;

        // Serialize tree
        let tree_bytes = tree.to_bytes();

        // Compress with Zstd
        let compressed = zstd::encode_all(&tree_bytes[..], 3).context("Failed to compress tree")?;

        // Compute hash
        let hash = hash_bytes(&tree_bytes);

        // Write to storage (atomic)
        let tree_path = self.tree_path(&hash);
        if tree_path.exists() {
            // Already stored (deduplication)
            return Ok(hash);
        }

        let temp_path = tree_path.with_extension("tmp");
        fs::write(&temp_path, &compressed)
            .with_context(|| format!("Failed to write tree to {:?}", temp_path))?;
        fs::rename(temp_path, &tree_path).context("Failed to rename tree file")?;

        Ok(hash)
    }

    /// Read tree from storage
    pub fn read(&self, hash: &Hash) -> Result<Tree> {
        let tree_path = self.tree_path(hash);

        if !tree_path.exists() {
            anyhow::bail!("Tree not found: {:?}", hash);
        }

        // Read compressed data
        let compressed = fs::read(&tree_path)
            .with_context(|| format!("Failed to read tree from {:?}", tree_path))?;

        // Decompress
        let tree_bytes = zstd::decode_all(&compressed[..]).context("Failed to decompress tree")?;

        // Deserialize
        Tree::from_bytes(&tree_bytes)
    }

    /// Check if tree exists
    pub fn exists(&self, hash: &Hash) -> bool {
        self.tree_path(hash).exists()
    }

    /// Delete tree
    pub fn delete(&self, hash: &Hash) -> Result<()> {
        let tree_path = self.tree_path(hash);
        if tree_path.exists() {
            fs::remove_file(&tree_path)
                .with_context(|| format!("Failed to delete tree {:?}", tree_path))?;
        }
        Ok(())
    }

    /// List all trees
    /// TODO: can prob parallelize this
    pub fn list_all(&self) -> Result<Vec<Hash>> {
        if !self.trees_dir.exists() {
            return Ok(Vec::new());
        }

        let mut hashes = Vec::new();

        for entry in fs::read_dir(&self.trees_dir)? {
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

    fn tree_path(&self, hash: &Hash) -> PathBuf {
        let hex = crate::helix_index::hash::hash_to_hex(hash);
        self.trees_dir.join(hex)
    }

    /// Write multiple trees in parallel (batch operation)
    pub fn write_batch(&self, trees: &[Tree]) -> Result<Vec<Hash>> {
        // Ensure directory exists
        fs::create_dir_all(&self.trees_dir).context("Failed to create trees directory")?;

        // Write all trees in parallel
        trees.par_iter().map(|tree| self.write(tree)).collect()
    }

    /// Read multiple trees in parallel (batch operation)
    pub fn read_batch(&self, hashes: &[Hash]) -> Result<Vec<Tree>> {
        hashes.par_iter().map(|hash| self.read(hash)).collect()
    }

    /// Check if multiple trees exist in parallel
    pub fn exists_batch(&self, hashes: &[Hash]) -> Vec<bool> {
        hashes.par_iter().map(|hash| self.exists(hash)).collect()
    }

    /// Delete multiple trees in parallel
    pub fn delete_batch(&self, hashes: &[Hash]) -> Result<()> {
        let results: Vec<Result<()>> = hashes.par_iter().map(|hash| self.delete(hash)).collect();

        // Check if any deletes failed
        for result in results {
            result?;
        }

        Ok(())
    }
}

/// Batch tree operations helper
pub struct TreeBatch<'a> {
    storage: &'a TreeStorage,
}

impl<'a> TreeBatch<'a> {
    pub fn new(storage: &'a TreeStorage) -> Self {
        Self { storage }
    }

    /// Write all trees (parallel)
    pub fn write_all(&self, trees: &[Tree]) -> Result<Vec<Hash>> {
        self.storage.write_batch(trees)
    }

    /// Read all trees (parallel)
    pub fn read_all(&self, hashes: &[Hash]) -> Result<Vec<Tree>> {
        self.storage.read_batch(hashes)
    }

    /// Check existence of all trees (parallel)
    pub fn exists_all(&self, hashes: &[Hash]) -> Vec<bool> {
        self.storage.exists_batch(hashes)
    }

    /// Delete all trees (parallel)
    pub fn delete_all(&self, hashes: &[Hash]) -> Result<()> {
        self.storage.delete_batch(hashes)
    }
}

/// Tree builder - constructs trees from index entries
pub struct TreeBuilder {
    storage: TreeStorage,
}

impl TreeBuilder {
    pub fn new(repo_path: &Path) -> Self {
        Self {
            storage: TreeStorage::for_repo(repo_path),
        }
    }

    // /// Build tree from entries (parallel)
    // pub fn build_from_entries(
    //     &self,
    //     entries: &[crate::helix_index::format::Entry],
    // ) -> Result<Hash> {
    //     // Group entries by directory (parallel)
    //     use std::sync::{Arc, Mutex};

    //     let dir_entries: Arc<Mutex<BTreeMap<PathBuf, Vec<&crate::helix_index::format::Entry>>>> =
    //         Arc::new(Mutex::new(BTreeMap::new()));

    //     entries.par_iter().for_each(|entry| {
    //         let parent = entry.path.parent().unwrap_or(Path::new(""));
    //         let mut map = dir_entries.lock().unwrap();
    //         map.entry(parent.to_path_buf())
    //             .or_insert_with(Vec::new)
    //             .push(entry);
    //     });

    //     let dir_entries = Arc::try_unwrap(dir_entries).unwrap().into_inner().unwrap();

    //     // Build trees bottom-up with parallel writes per level
    //     let mut tree_hashes: BTreeMap<PathBuf, Hash> = BTreeMap::new();

    //     // Sort directories by depth (deepest first)
    //     let mut dirs: Vec<_> = dir_entries.keys().cloned().collect();
    //     dirs.sort_by_key(|d| std::cmp::Reverse(d.components().count()));

    //     // Group directories by depth for parallel processing
    //     let mut depth_groups: Vec<Vec<PathBuf>> = Vec::new();
    //     let mut current_depth = None;
    //     let mut current_group = Vec::new();

    //     for dir in dirs {
    //         let depth = dir.components().count();
    //         if current_depth != Some(depth) {
    //             if !current_group.is_empty() {
    //                 depth_groups.push(std::mem::take(&mut current_group));
    //             }
    //             current_depth = Some(depth);
    //         }
    //         current_group.push(dir);
    //     }
    //     if !current_group.is_empty() {
    //         depth_groups.push(current_group);
    //     }

    //     // Process each depth level in parallel
    //     for depth_dirs in depth_groups {
    //         let tree_hashes_ref = &tree_hashes;

    //         // Build all trees at this depth in parallel
    //         let results: Vec<(PathBuf, Hash)> = depth_dirs
    //             .par_iter()
    //             .map(|dir| {
    //                 let dir_entries = dir_entries.get(dir).unwrap();
    //                 let mut tree = Tree::new();

    //                 // Add file entries
    //                 for entry in dir_entries {
    //                     let name = entry
    //                         .path
    //                         .file_name()
    //                         .unwrap()
    //                         .to_string_lossy()
    //                         .to_string();

    //                     tree.add_entry(TreeEntry::new_file(
    //                         name,
    //                         entry.oid,
    //                         entry.file_mode,
    //                         entry.size,
    //                     ));
    //                 }

    //                 // Add subdirectory entries (trees from previous depth levels)
    //                 for (subdir, subdir_hash) in tree_hashes_ref {
    //                     if subdir.parent() == Some(dir) {
    //                         let name = subdir.file_name().unwrap().to_string_lossy().to_string();
    //                         tree.add_entry(TreeEntry::new_tree(name, *subdir_hash));
    //                     }
    //                 }

    //                 // Sort and store tree
    //                 tree.sort();
    //                 let tree_hash = self.storage.write(&tree).unwrap();
    //                 (dir.clone(), tree_hash)
    //             })
    //             .collect();

    //         // Add results to tree_hashes
    //         for (dir, hash) in results {
    //             tree_hashes.insert(dir, hash);
    //         }
    //     }

    //     // Return root tree hash
    //     tree_hashes
    //         .get(Path::new(""))
    //         .copied()
    //         .ok_or_else(|| anyhow::anyhow!("No root tree created"))
    // }

    /// Build tree from entries (parallel)
    pub fn build_from_entries(
        &self,
        entries: &[crate::helix_index::format::Entry],
    ) -> Result<Hash> {
        // Handle empty entries case
        if entries.is_empty() {
            let empty_tree = Tree::new();
            return self.storage.write(&empty_tree);
        }

        // Group entries by directory (parallel)
        use std::sync::{Arc, Mutex};

        let dir_entries: Arc<Mutex<BTreeMap<PathBuf, Vec<&crate::helix_index::format::Entry>>>> =
            Arc::new(Mutex::new(BTreeMap::new()));

        entries.par_iter().for_each(|entry| {
            let parent = entry.path.parent().unwrap_or(Path::new(""));
            let mut map = dir_entries.lock().unwrap();
            map.entry(parent.to_path_buf())
                .or_insert_with(Vec::new)
                .push(entry);
        });

        let dir_entries = Arc::try_unwrap(dir_entries).unwrap().into_inner().unwrap();

        // Collect all directories that need trees (including ancestors)
        let mut all_dirs: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();

        for dir in dir_entries.keys() {
            // Add this directory
            all_dirs.insert(dir.clone());

            // Add all ancestor directories up to root
            let mut current = dir.clone();
            while let Some(parent) = current.parent() {
                all_dirs.insert(parent.to_path_buf());
                current = parent.to_path_buf();
            }
        }

        // Build trees bottom-up with parallel writes per level
        let mut tree_hashes: BTreeMap<PathBuf, Hash> = BTreeMap::new();

        // Sort directories by depth (deepest first)
        let mut dirs: Vec<_> = all_dirs.into_iter().collect();
        dirs.sort_by_key(|d| std::cmp::Reverse(d.components().count()));

        // Group directories by depth for parallel processing
        let mut depth_groups: Vec<Vec<PathBuf>> = Vec::new();
        let mut current_depth = None;
        let mut current_group = Vec::new();

        for dir in dirs {
            let depth = dir.components().count();
            if current_depth != Some(depth) {
                if !current_group.is_empty() {
                    depth_groups.push(std::mem::take(&mut current_group));
                }
                current_depth = Some(depth);
            }
            current_group.push(dir);
        }
        if !current_group.is_empty() {
            depth_groups.push(current_group);
        }

        // Process each depth level in parallel
        for depth_dirs in depth_groups {
            let tree_hashes_ref = &tree_hashes;

            // Build all trees at this depth in parallel
            let results: Vec<(PathBuf, Hash)> = depth_dirs
                .par_iter()
                .map(|dir| {
                    let mut tree = Tree::new();

                    // Add file entries (if any exist in this directory)
                    if let Some(entries_in_dir) = dir_entries.get(dir) {
                        for entry in entries_in_dir {
                            let name = entry
                                .path
                                .file_name()
                                .unwrap()
                                .to_string_lossy()
                                .to_string();

                            tree.add_entry(TreeEntry::new_file(
                                name,
                                entry.oid,
                                entry.file_mode,
                                entry.size,
                            ));
                        }
                    }

                    // Add subdirectory entries (trees from previous depth levels)
                    for (subdir, subdir_hash) in tree_hashes_ref {
                        if subdir.parent() == Some(dir.as_path()) {
                            let name = subdir.file_name().unwrap().to_string_lossy().to_string();
                            tree.add_entry(TreeEntry::new_tree(name, *subdir_hash));
                        }
                    }

                    // Sort and store tree
                    tree.sort();
                    let tree_hash = self.storage.write(&tree).unwrap();
                    (dir.clone(), tree_hash)
                })
                .collect();

            // Add results to tree_hashes
            for (dir, hash) in results {
                tree_hashes.insert(dir, hash);
            }
        }

        // Return root tree hash
        tree_hashes
            .get(Path::new(""))
            .copied()
            .ok_or_else(|| anyhow::anyhow!("No root tree created"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::helix_index::format::Entry;
    use tempfile::TempDir;

    #[test]
    fn test_entry_type_from_mode() {
        assert_eq!(EntryType::from_mode(0o100644), EntryType::File);
        assert_eq!(EntryType::from_mode(0o100755), EntryType::FileExecutable);
        assert_eq!(EntryType::from_mode(0o120000), EntryType::Symlink);
    }

    #[test]
    fn test_tree_entry_serialization() {
        let entry =
            TreeEntry::new_file("test.txt".to_string(), hash_bytes(b"test"), 0o100644, 1024);

        let bytes = entry.to_bytes();
        let parsed = TreeEntry::from_bytes(&bytes).unwrap();

        assert_eq!(parsed, entry);
    }

    #[test]
    fn test_tree_entry_ordering() {
        let e1 = TreeEntry::new_file("a.txt".to_string(), [0u8; 32], 0o100644, 0);
        let e2 = TreeEntry::new_file("b.txt".to_string(), [0u8; 32], 0o100644, 0);
        let e3 = TreeEntry::new_file("c.txt".to_string(), [0u8; 32], 0o100644, 0);

        assert!(e1 < e2);
        assert!(e2 < e3);
        assert!(e1 < e3);
    }

    #[test]
    fn test_tree_serialization() {
        let mut tree = Tree::new();
        tree.add_entry(TreeEntry::new_file(
            "file1.txt".to_string(),
            hash_bytes(b"content1"),
            0o100644,
            100,
        ));
        tree.add_entry(TreeEntry::new_file(
            "file2.txt".to_string(),
            hash_bytes(b"content2"),
            0o100755,
            200,
        ));
        tree.sort();

        let bytes = tree.to_bytes();
        let parsed = Tree::from_bytes(&bytes).unwrap();

        assert_eq!(parsed, tree);
    }

    #[test]
    fn test_tree_hash_deterministic() {
        let mut tree1 = Tree::new();
        tree1.add_entry(TreeEntry::new_file(
            "a.txt".to_string(),
            [1u8; 32],
            0o100644,
            10,
        ));
        tree1.add_entry(TreeEntry::new_file(
            "b.txt".to_string(),
            [2u8; 32],
            0o100644,
            20,
        ));
        tree1.sort();

        let mut tree2 = Tree::new();
        tree2.add_entry(TreeEntry::new_file(
            "b.txt".to_string(),
            [2u8; 32],
            0o100644,
            20,
        ));
        tree2.add_entry(TreeEntry::new_file(
            "a.txt".to_string(),
            [1u8; 32],
            0o100644,
            10,
        ));
        tree2.sort();

        assert_eq!(tree1.hash(), tree2.hash());
    }

    #[test]
    fn test_tree_storage_write_read() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let storage = TreeStorage::new(temp_dir.path());

        let mut tree = Tree::new();
        tree.add_entry(TreeEntry::new_file(
            "test.txt".to_string(),
            hash_bytes(b"test"),
            0o100644,
            100,
        ));
        tree.sort();

        let hash = storage.write(&tree)?;
        let read_tree = storage.read(&hash)?;

        assert_eq!(read_tree, tree);

        Ok(())
    }

    #[test]
    fn test_tree_storage_deduplication() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let storage = TreeStorage::new(temp_dir.path());

        let mut tree = Tree::new();
        tree.add_entry(TreeEntry::new_file(
            "test.txt".to_string(),
            hash_bytes(b"test"),
            0o100644,
            100,
        ));

        let hash1 = storage.write(&tree)?;
        let hash2 = storage.write(&tree)?;

        assert_eq!(hash1, hash2);

        let all_trees = storage.list_all()?;
        assert_eq!(all_trees.len(), 1);

        Ok(())
    }

    #[test]
    fn test_tree_storage_exists() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let storage = TreeStorage::new(temp_dir.path());

        let tree = Tree::new();
        let hash = storage.write(&tree)?;

        assert!(storage.exists(&hash));
        assert!(!storage.exists(&[255u8; 32]));

        Ok(())
    }

    #[test]
    fn test_tree_storage_delete() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let storage = TreeStorage::new(temp_dir.path());

        let tree = Tree::new();
        let hash = storage.write(&tree)?;

        assert!(storage.exists(&hash));

        storage.delete(&hash)?;

        assert!(!storage.exists(&hash));

        Ok(())
    }

    #[test]
    fn test_tree_builder_simple() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let builder = TreeBuilder::new(temp_dir.path());

        let entries = vec![
            Entry {
                path: PathBuf::from("file1.txt"),
                oid: hash_bytes(b"content1"),
                flags: crate::helix_index::format::EntryFlags::TRACKED,
                size: 100,
                mtime_sec: 0,
                mtime_nsec: 0,
                file_mode: 0o100644,
                merge_conflict_stage: 0,
                reserved: [0u8; 33],
            },
            Entry {
                path: PathBuf::from("file2.txt"),
                oid: hash_bytes(b"content2"),
                flags: crate::helix_index::format::EntryFlags::TRACKED,
                size: 200,
                mtime_sec: 0,
                mtime_nsec: 0,
                file_mode: 0o100644,
                merge_conflict_stage: 0,
                reserved: [0u8; 33],
            },
        ];

        let root_hash = builder.build_from_entries(&entries)?;

        // Tree should exist
        assert!(builder.storage.exists(&root_hash));

        // Should contain both files
        let tree = builder.storage.read(&root_hash)?;
        assert_eq!(tree.entries.len(), 2);

        Ok(())
    }

    #[test]
    fn test_tree_builder_nested() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let builder = TreeBuilder::new(temp_dir.path());

        let entries = vec![
            Entry {
                path: PathBuf::from("file.txt"),
                oid: hash_bytes(b"root"),
                flags: crate::helix_index::format::EntryFlags::TRACKED,
                size: 100,
                mtime_sec: 0,
                mtime_nsec: 0,
                file_mode: 0o100644,
                merge_conflict_stage: 0,
                reserved: [0u8; 33],
            },
            Entry {
                path: PathBuf::from("dir/nested.txt"),
                oid: hash_bytes(b"nested"),
                flags: crate::helix_index::format::EntryFlags::TRACKED,
                size: 200,
                mtime_sec: 0,
                mtime_nsec: 0,
                file_mode: 0o100644,
                merge_conflict_stage: 0,
                reserved: [0u8; 33],
            },
        ];

        let root_hash = builder.build_from_entries(&entries)?;

        // Root tree should exist
        let root_tree = builder.storage.read(&root_hash)?;

        // Should contain file + dir
        assert_eq!(root_tree.entries.len(), 2);

        // One should be a tree
        let dir_entry = root_tree
            .entries
            .iter()
            .find(|e| e.entry_type == EntryType::Tree)
            .unwrap();

        // Dir tree should exist and contain the nested file
        let dir_tree = builder.storage.read(&dir_entry.oid)?;
        assert_eq!(dir_tree.entries.len(), 1);
        assert_eq!(dir_tree.entries[0].name, "nested.txt");

        Ok(())
    }
}
