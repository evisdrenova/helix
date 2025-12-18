use anyhow::{anyhow, Result};
use helix_protocol::ObjectType;
use std::collections::HashSet;
use std::io::Cursor;

use crate::storage::storage::FsObjectStore;

/// Collect all objects (commits + trees + blobs) reachable from `remote_head`.
/// Returns tuples of (ObjectType, hash, raw_bytes) suitable for sending over the wire.
pub fn collect_all_objects(
    objects: &FsObjectStore,
    remote_head: &[u8; 32],
) -> Result<Vec<(ObjectType, [u8; 32], Vec<u8>)>> {
    let mut result: Vec<(ObjectType, [u8; 32], Vec<u8>)> = Vec::new();

    let mut seen_commits: HashSet<[u8; 32]> = HashSet::new();
    let mut seen_trees: HashSet<[u8; 32]> = HashSet::new();
    let mut seen_blobs: HashSet<[u8; 32]> = HashSet::new();

    let mut stack: Vec<[u8; 32]> = vec![*remote_head];

    while let Some(commit_hash) = stack.pop() {
        if !seen_commits.insert(commit_hash) {
            continue;
        }

        // Read raw commit bytes (as stored on disk)
        let commit_bytes = objects.read_object(&ObjectType::Commit, &commit_hash)?;
        // We send exactly these bytes to clients
        result.push((ObjectType::Commit, commit_hash, commit_bytes.clone()));

        // Decode commit just enough to get tree + parents
        let decoded = maybe_decompress(&commit_bytes)?;
        let ParsedCommit { tree, parents } = parse_commit_header(&decoded)
            .map_err(|e| anyhow!("Failed to parse commit {:?}: {e}", hex::encode(commit_hash)))?;

        // Queue parents for traversal
        for parent in parents {
            if !seen_commits.contains(&parent) {
                stack.push(parent);
            }
        }

        // Walk tree graph for this commit
        walk_tree(
            &tree,
            objects,
            &mut seen_trees,
            &mut seen_blobs,
            &mut result,
        )?;
    }

    Ok(result)
}

/// Minimal view of a commit: only what we need for traversal.
struct ParsedCommit {
    tree: [u8; 32],
    parents: Vec<[u8; 32]>,
}

/// Try to zstd-decompress; if that fails, assume it's already plain bytes.
fn maybe_decompress(data: &[u8]) -> Result<Vec<u8>> {
    match zstd::stream::decode_all(Cursor::new(data)) {
        Ok(decoded) => Ok(decoded),
        Err(_) => Ok(data.to_vec()),
    }
}

fn parse_commit_header(bytes: &[u8]) -> Result<ParsedCommit> {
    let mut offset = 0usize;

    // tree hash (32 bytes)
    if bytes.len() < offset + 32 {
        return Err(anyhow!("commit too small to contain tree hash"));
    }
    let mut tree = [0u8; 32];
    tree.copy_from_slice(&bytes[offset..offset + 32]);
    offset += 32;

    // parent count (u32 LE)
    if bytes.len() < offset + 4 {
        return Err(anyhow!("commit too small to contain parent count"));
    }
    let parent_count = u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
    offset += 4;

    // parents: parent_count * 32 bytes
    let mut parents = Vec::with_capacity(parent_count as usize);
    for _ in 0..parent_count {
        if bytes.len() < offset + 32 {
            return Err(anyhow!("commit too small to contain parent hash"));
        }
        let mut parent = [0u8; 32];
        parent.copy_from_slice(&bytes[offset..offset + 32]);
        offset += 32;
        parents.push(parent);
    }

    Ok(ParsedCommit { tree, parents })
}

fn walk_tree(
    tree_hash: &[u8; 32],
    objects: &FsObjectStore,
    seen_trees: &mut HashSet<[u8; 32]>,
    seen_blobs: &mut HashSet<[u8; 32]>,
    result: &mut Vec<(ObjectType, [u8; 32], Vec<u8>)>,
) -> Result<()> {
    if !seen_trees.insert(*tree_hash) {
        return Ok(());
    }

    // Read raw tree bytes
    let tree_bytes = objects.read_object(&ObjectType::Tree, tree_hash)?;
    // Add to outgoing set
    result.push((ObjectType::Tree, *tree_hash, tree_bytes.clone()));

    // Decode for entries
    let decoded = maybe_decompress(&tree_bytes)?;
    parse_tree(&decoded, objects, seen_trees, seen_blobs, result)
}

fn parse_tree(
    bytes: &[u8],
    objects: &FsObjectStore,
    seen_trees: &mut HashSet<[u8; 32]>,
    seen_blobs: &mut HashSet<[u8; 32]>,
    result: &mut Vec<(ObjectType, [u8; 32], Vec<u8>)>,
) -> Result<()> {
    let mut offset = 0usize;

    if bytes.len() < 4 {
        return Err(anyhow!("tree too small to contain entry count"));
    }
    let entry_count = u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
    offset += 4;

    for _ in 0..entry_count {
        if bytes.len() < offset + 1 + 2 {
            return Err(anyhow!("tree entry header truncated"));
        }

        let kind = bytes[offset];
        offset += 1;

        let name_len = u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap()) as usize;
        offset += 2;

        if bytes.len() < offset + name_len + 32 {
            return Err(anyhow!("tree entry body truncated"));
        }

        // let name_bytes = &bytes[offset..offset + name_len]; // not used, but keep if you want
        offset += name_len;

        let mut hash = [0u8; 32];
        hash.copy_from_slice(&bytes[offset..offset + 32]);
        offset += 32;

        match kind {
            // 1 = blob
            1 => {
                if seen_blobs.insert(hash) {
                    let blob_bytes = objects.read_object(&ObjectType::Blob, &hash)?;
                    result.push((ObjectType::Blob, hash, blob_bytes));
                }
            }
            // 2 = tree
            2 => {
                walk_tree(&hash, objects, seen_trees, seen_blobs, result)?;
            }
            other => {
                return Err(anyhow!("unknown tree entry type: {}", other));
            }
        }
    }

    Ok(())
}
