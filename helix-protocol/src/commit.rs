use anyhow::{bail, Result};

use crate::hash::Hash;

pub struct CommitData {
    pub hash: Hash,
    pub tree_hash: Hash,
    pub bytes: Vec<u8>,
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
