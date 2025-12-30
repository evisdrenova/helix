use std::{
    collections::{HashSet, VecDeque},
    fs,
    path::Path,
};

use crate::{
    hash::{hash_to_hex, hex_to_hash, Hash},
    message::ObjectType,
    storage::FsObjectStore,
};
use anyhow::{bail, Context, Result};

pub struct CommitData {
    pub hash: Hash,
    pub tree_hash: Hash,
    pub raw_bytes: Vec<u8>,        // Raw bytes (for parsing)
    pub compressed_bytes: Vec<u8>, // Compressed bytes (for sending over wire)
}

/// Parse commit bytes to extract tree_hash and parents for traversal.
pub fn parse_commit_for_walk(bytes: &[u8]) -> Result<(Hash, Vec<Hash>)> {
    if bytes.len() < 36 {
        bail!("Commit too short: {} bytes", bytes.len());
    }

    let mut offset = 0;

    // Tree hash (32 bytes)
    let mut tree_hash = [0u8; 32];
    tree_hash.copy_from_slice(&bytes[offset..offset + 32]);
    offset += 32;

    // Parent count (4 bytes)
    let parent_count = u32::from_le_bytes(bytes[offset..offset + 4].try_into()?) as usize;
    offset += 4;

    // Parent hashes (32 bytes each)
    let mut parents = Vec::with_capacity(parent_count);
    for _ in 0..parent_count {
        if offset + 32 > bytes.len() {
            bail!("Commit truncated reading parents");
        }
        let mut parent = [0u8; 32];
        parent.copy_from_slice(&bytes[offset..offset + 32]);
        parents.push(parent);
        offset += 32;
    }

    Ok((tree_hash, parents))
}

/// Collect all objects needed: commits, trees, and blobs
pub fn collect_objects_from_commits(
    store: &FsObjectStore,
    commits: &[CommitData],
) -> anyhow::Result<Vec<(ObjectType, Hash, Vec<u8>)>> {
    let mut objects = Vec::new();
    let mut seen_trees = HashSet::new();
    let mut seen_blobs = HashSet::new();

    // Add commits first
    for commit in commits {
        objects.push((
            ObjectType::Commit,
            commit.hash,
            commit.compressed_bytes.clone(),
        ));
    }

    // Collect trees and blobs from each commit's tree
    for commit in commits {
        collect_tree_recursive(
            store,
            commit.tree_hash,
            &mut seen_trees,
            &mut seen_blobs,
            &mut objects,
        )?;
    }

    Ok(objects)
}

#[derive(Debug, Clone, Copy)]
enum EntryKind {
    File,
    Tree,
}

/// Recursively collect a tree and all its blobs/subtrees.
pub fn collect_tree_recursive(
    store: &FsObjectStore,
    tree_hash: Hash,
    seen_trees: &mut HashSet<Hash>,
    seen_blobs: &mut HashSet<Hash>,
    objects: &mut Vec<(ObjectType, Hash, Vec<u8>)>,
) -> Result<()> {
    if !seen_trees.insert(tree_hash) {
        return Ok(()); // Already processed
    }

    let compressed = store.read_object_compressed(&ObjectType::Tree, &tree_hash)?;
    let raw = zstd::decode_all(&compressed[..]).context("Failed to decompress tree for parsing")?;

    objects.push((ObjectType::Tree, tree_hash, compressed));

    // Parse tree entries from raw bytes
    let entries = parse_tree_entries(&raw)?;

    for (entry_kind, hash) in entries {
        match entry_kind {
            EntryKind::File => {
                if seen_blobs.insert(hash) {
                    let blob_compressed = store.read_object_compressed(&ObjectType::Blob, &hash)?;
                    objects.push((ObjectType::Blob, hash, blob_compressed));
                }
            }
            EntryKind::Tree => {
                collect_tree_recursive(store, hash, seen_trees, seen_blobs, objects)?;
            }
        }
    }

    Ok(())
}

/// Parse tree entries from tree bytes.
/// Format per entry: type(1) + mode(4) + size(8) + name_len(2) + name(var) + oid(32)
fn parse_tree_entries(bytes: &[u8]) -> Result<Vec<(EntryKind, Hash)>> {
    if bytes.len() < 4 {
        bail!("Tree too short");
    }

    let entry_count = u32::from_le_bytes(bytes[0..4].try_into()?) as usize;
    let mut offset = 4;
    let mut entries = Vec::with_capacity(entry_count);

    for _ in 0..entry_count {
        if offset + 15 > bytes.len() {
            bail!("Tree entry header truncated");
        }

        // Type (1 byte): 0=File, 1=FileExecutable, 2=Tree, 3=Symlink
        let entry_type_byte = bytes[offset];
        let entry_kind = if entry_type_byte == 2 {
            EntryKind::Tree
        } else {
            EntryKind::File // File, FileExecutable, Symlink all point to blobs
        };
        offset += 1;

        // Mode (4 bytes) - skip
        offset += 4;

        // Size (8 bytes) - skip
        offset += 8;

        // Name length (2 bytes)
        let name_len = u16::from_le_bytes(bytes[offset..offset + 2].try_into()?) as usize;
        offset += 2;

        // Name (variable) - skip
        offset += name_len;

        // OID (32 bytes)
        if offset + 32 > bytes.len() {
            bail!("Tree entry OID truncated");
        }
        let mut hash = [0u8; 32];
        hash.copy_from_slice(&bytes[offset..offset + 32]);
        offset += 32;

        entries.push((entry_kind, hash));
    }

    Ok(entries)
}

/// Walk from `from` (remote_head) backwards until we hit `to` (last_known) or run out of parents.
pub fn walk_commits_between(
    store: &FsObjectStore,
    from: Hash,
    to: Option<Hash>,
) -> anyhow::Result<Vec<CommitData>> {
    let mut result = Vec::new();
    let mut queue = VecDeque::new();
    let mut seen = HashSet::new();

    queue.push_back(from);

    while let Some(hash) = queue.pop_front() {
        // Stop if we've reached what client already has
        if to == Some(hash) {
            continue;
        }

        if !seen.insert(hash) {
            continue;
        }

        // Read both compressed (for sending) and raw (for parsing)
        let compressed_bytes = store.read_object_compressed(&ObjectType::Commit, &hash)?;
        let raw_bytes =
            zstd::decode_all(&compressed_bytes[..]).context("Failed to decompress commit")?;

        let (tree_hash, parents) = parse_commit_for_walk(&raw_bytes)?;

        // Queue parents
        for parent in &parents {
            queue.push_back(*parent);
        }

        result.push(CommitData {
            hash,
            tree_hash,
            raw_bytes,
            compressed_bytes,
        });
    }

    Ok(result)
}

/// Compute objects to push by walking from new_target back to server_head.
/// Only sends commits, trees, and blobs that the server doesn't have.
pub fn compute_objects_to_push(
    store: &FsObjectStore,
    new_target: Hash,
    server_head: Option<Hash>,
) -> Result<Vec<(ObjectType, Hash, Vec<u8>)>> {
    // Walk commits from new_target back to server_head
    let missing_commits = walk_commits_between(store, new_target, server_head)?;

    if missing_commits.is_empty() {
        return Ok(vec![]);
    }

    // Collect all objects from those commits (returns compressed bytes)
    collect_objects_from_commits(store, &missing_commits)
}

/// Read a local Helix ref from .helix/refs/<...>
pub fn read_local_ref(repo_path: &Path, ref_name: &str) -> Result<Hash> {
    let ref_path = repo_path.join(".helix").join(ref_name);

    let hex_contents = fs::read_to_string(&ref_path)
        .with_context(|| format!("Failed to read ref {} ({})", ref_name, ref_path.display()))?;

    hex_to_hash(hex_contents.trim())
}

/// Read remote-tracking ref: .helix/refs/remotes/<remote>/<branch>
pub fn read_remote_tracking(repo_path: &Path, remote: &str, branch: &str) -> Result<Hash> {
    let path = repo_path
        .join(".helix")
        .join("refs")
        .join("remotes")
        .join(remote)
        .join(branch);

    if !path.exists() {
        bail!("Remote-tracking ref does not exist: {}", path.display());
    }

    let hex_contents = fs::read_to_string(&path)
        .with_context(|| format!("Failed to read remote-tracking ref {}", path.display()))?;

    hex_to_hash(hex_contents.trim())
}

/// Write remote-tracking ref: .helix/refs/remotes/<remote>/<branch>
pub fn write_remote_tracking(
    repo_path: &Path,
    remote: &str,
    branch: &str,
    target: Hash,
) -> Result<()> {
    let path = repo_path
        .join(".helix")
        .join("refs")
        .join("remotes")
        .join(remote);

    fs::create_dir_all(&path).with_context(|| format!("Failed to create {}", path.display()))?;

    let full = path.join(branch);
    let hex = hash_to_hex(&target);
    fs::write(&full, hex + "\n").with_context(|| format!("Failed to write {}", full.display()))?;

    Ok(())
}
