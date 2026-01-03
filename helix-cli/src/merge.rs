//! Three-way merge implementation for Helix
//!
//! This module handles merging two branches that have diverged from a common ancestor.
//! It supports:
//! - Tree diffing to find changes between commits
//! - Merge analysis to classify changes and detect conflicts
//! - Conflict marker generation for text files
//! - Merge commit creation with two parents

use anyhow::{Context, Result};
use helix_protocol::hash::{hash_to_hex, Hash};
use helix_protocol::message::ObjectType;
use helix_protocol::storage::FsObjectStore;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use crate::helix_index::commit::{Commit, CommitStore};
use crate::helix_index::format::{Entry, EntryFlags, Header};
use crate::helix_index::tree::TreeStore;
use crate::helix_index::writer::Writer;

/// A change detected between two trees
#[derive(Debug, Clone)]
pub enum TreeChange {
    Added {
        path: PathBuf,
        blob_hash: Hash,
        mode: u32,
    },
    Modified {
        path: PathBuf,
        old_hash: Hash,
        new_hash: Hash,
        mode: u32,
    },
    Deleted {
        path: PathBuf,
        old_hash: Hash,
    },
}

impl TreeChange {
    pub fn path(&self) -> &Path {
        match self {
            TreeChange::Added { path, .. } => path,
            TreeChange::Modified { path, .. } => path,
            TreeChange::Deleted { path, .. } => path,
        }
    }
}

/// Result of diffing two trees
#[derive(Debug, Default)]
pub struct TreeDiff {
    pub changes: Vec<TreeChange>,
}

impl TreeDiff {
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }

    pub fn len(&self) -> usize {
        self.changes.len()
    }

    /// Get all paths that were changed
    pub fn changed_paths(&self) -> HashSet<PathBuf> {
        self.changes
            .iter()
            .map(|c| c.path().to_path_buf())
            .collect()
    }
}

/// A conflict that needs resolution
#[derive(Debug, Clone)]
pub struct MergeConflict {
    pub path: PathBuf,
    pub base: Option<Hash>,    // None if file didn't exist in base
    pub target: Option<Hash>,  // None if deleted in target
    pub sandbox: Option<Hash>, // None if deleted in sandbox
    pub conflict_type: ConflictType,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ConflictType {
    /// Both sides modified the same file differently
    BothModified,
    /// One side modified, other side deleted
    ModifyDelete,
    /// Both sides added a file with different content
    BothAdded,
}

/// A resolved file entry for the merged tree
#[derive(Debug, Clone)]
pub struct ResolvedEntry {
    pub path: PathBuf,
    pub blob_hash: Option<Hash>, // None means file is deleted
    pub mode: u32,
}

/// Result of merge analysis
#[derive(Debug)]
pub struct MergeAnalysis {
    /// Changes that can be applied automatically (no conflict)
    pub auto_resolved: Vec<ResolvedEntry>,
    /// Conflicts that need resolution
    pub conflicts: Vec<MergeConflict>,
    /// Base tree entries (for reference)
    pub base_tree: HashMap<PathBuf, (Hash, u32)>,
}

impl MergeAnalysis {
    pub fn has_conflicts(&self) -> bool {
        !self.conflicts.is_empty()
    }

    pub fn is_fast_forward(&self) -> bool {
        // Fast-forward is possible when target has no changes from base
        false // This is determined earlier, not here
    }
}

/// How a conflict was resolved
#[derive(Debug, Clone)]
pub enum ConflictResolution {
    TakeTarget,
    TakeSandbox,
    TakeBase,
    Merged(Vec<u8>), // New content with merged/resolved content
    Delete,
}

/// Full merge result
#[derive(Debug)]
pub struct MergeResult {
    pub commit_hash: Hash,
    pub merged_tree_hash: Hash,
    pub conflicts_resolved: usize,
    pub files_changed: usize,
}
/// Compute the difference between two trees
pub fn diff_trees(
    repo_path: &Path,
    base_tree_hash: &Hash,
    other_tree_hash: &Hash,
) -> Result<TreeDiff> {
    let tree_store = TreeStore::for_repo(repo_path);

    // Collect all files from both trees
    let base_files = tree_store.collect_all_files(base_tree_hash)?;
    let other_files = tree_store.collect_all_files(other_tree_hash)?;

    // Convert to HashMaps for easy lookup
    let base_map: HashMap<PathBuf, Hash> = base_files.into_iter().collect();
    let other_map: HashMap<PathBuf, Hash> = other_files.into_iter().collect();

    let mut changes = Vec::new();

    // Find additions and modifications (files in other but not in base, or different)
    for (path, other_hash) in &other_map {
        match base_map.get(path) {
            None => {
                // File added in other
                changes.push(TreeChange::Added {
                    path: path.clone(),
                    blob_hash: *other_hash,
                    mode: 0o100644, // TODO: Get actual mode from tree
                });
            }
            Some(base_hash) => {
                if base_hash != other_hash {
                    // File modified in other
                    changes.push(TreeChange::Modified {
                        path: path.clone(),
                        old_hash: *base_hash,
                        new_hash: *other_hash,
                        mode: 0o100644,
                    });
                }
                // If hashes match, file is unchanged
            }
        }
    }

    // Find deletions (files in base but not in other)
    for (path, base_hash) in &base_map {
        if !other_map.contains_key(path) {
            changes.push(TreeChange::Deleted {
                path: path.clone(),
                old_hash: *base_hash,
            });
        }
    }

    Ok(TreeDiff { changes })
}

pub fn analyze_merge(
    repo_path: &Path,
    base_commit_hash: &Hash,
    target_commit_hash: &Hash,
    sandbox_commit_hash: &Hash,
) -> Result<MergeAnalysis> {
    let store = FsObjectStore::new(repo_path);
    let commit_store = CommitStore::new(repo_path, store.clone())?;

    // Read commits to get tree hashes
    let base_commit = commit_store.read_commit(base_commit_hash)?;
    let target_commit = commit_store.read_commit(target_commit_hash)?;
    let sandbox_commit = commit_store.read_commit(sandbox_commit_hash)?;

    // Get tree diffs
    let target_diff = diff_trees(repo_path, &base_commit.tree_hash, &target_commit.tree_hash)?;
    let sandbox_diff = diff_trees(repo_path, &base_commit.tree_hash, &sandbox_commit.tree_hash)?;

    // Build base tree map for reference
    let tree_store = TreeStore::for_repo(repo_path);
    let base_files = tree_store.collect_all_files(&base_commit.tree_hash)?;
    let base_tree: HashMap<PathBuf, (Hash, u32)> = base_files
        .into_iter()
        .map(|(path, hash)| (path, (hash, 0o100644)))
        .collect();

    // Build change maps for quick lookup
    let target_changes = build_change_map(&target_diff);
    let sandbox_changes = build_change_map(&sandbox_diff);

    // All paths that changed in either branch
    let all_changed_paths: HashSet<PathBuf> = target_diff
        .changed_paths()
        .union(&sandbox_diff.changed_paths())
        .cloned()
        .collect();

    let mut auto_resolved = Vec::new();
    let mut conflicts = Vec::new();

    for path in all_changed_paths {
        let target_change = target_changes.get(&path);
        let sandbox_change = sandbox_changes.get(&path);
        let base_entry = base_tree.get(&path);

        match (target_change, sandbox_change) {
            // Only target changed
            (Some(tc), None) => {
                auto_resolved.push(change_to_resolved(tc));
            }
            // Only sandbox changed
            (None, Some(sc)) => {
                auto_resolved.push(change_to_resolved(sc));
            }
            // Both changed - need to check if it's a conflict
            (Some(tc), Some(sc)) => {
                if let Some(conflict) = detect_conflict(&path, tc, sc, base_entry) {
                    conflicts.push(conflict);
                } else {
                    // Same change in both - take either one
                    auto_resolved.push(change_to_resolved(tc));
                }
            }
            // Neither changed (shouldn't happen since we're iterating changed paths)
            (None, None) => {}
        }
    }

    Ok(MergeAnalysis {
        auto_resolved,
        conflicts,
        base_tree,
    })
}

/// Build a map from path to change for quick lookup
fn build_change_map(diff: &TreeDiff) -> HashMap<PathBuf, TreeChange> {
    diff.changes
        .iter()
        .map(|c| (c.path().to_path_buf(), c.clone()))
        .collect()
}

/// Convert a TreeChange to a ResolvedEntry
fn change_to_resolved(change: &TreeChange) -> ResolvedEntry {
    match change {
        TreeChange::Added {
            path,
            blob_hash,
            mode,
        } => ResolvedEntry {
            path: path.clone(),
            blob_hash: Some(*blob_hash),
            mode: *mode,
        },
        TreeChange::Modified {
            path,
            new_hash,
            mode,
            ..
        } => ResolvedEntry {
            path: path.clone(),
            blob_hash: Some(*new_hash),
            mode: *mode,
        },
        TreeChange::Deleted { path, .. } => ResolvedEntry {
            path: path.clone(),
            blob_hash: None,
            mode: 0,
        },
    }
}

/// Detect if two changes to the same path constitute a conflict
fn detect_conflict(
    path: &Path,
    target_change: &TreeChange,
    sandbox_change: &TreeChange,
    base_entry: Option<&(Hash, u32)>,
) -> Option<MergeConflict> {
    match (target_change, sandbox_change) {
        // Both modified - conflict if different results
        (
            TreeChange::Modified {
                new_hash: t_hash, ..
            },
            TreeChange::Modified {
                new_hash: s_hash, ..
            },
        ) => {
            if t_hash == s_hash {
                None // Same modification, no conflict
            } else {
                Some(MergeConflict {
                    path: path.to_path_buf(),
                    base: base_entry.map(|(h, _)| *h),
                    target: Some(*t_hash),
                    sandbox: Some(*s_hash),
                    conflict_type: ConflictType::BothModified,
                })
            }
        }
        // Both added - conflict if different content
        (
            TreeChange::Added {
                blob_hash: t_hash, ..
            },
            TreeChange::Added {
                blob_hash: s_hash, ..
            },
        ) => {
            if t_hash == s_hash {
                None // Same content added, no conflict
            } else {
                Some(MergeConflict {
                    path: path.to_path_buf(),
                    base: None,
                    target: Some(*t_hash),
                    sandbox: Some(*s_hash),
                    conflict_type: ConflictType::BothAdded,
                })
            }
        }
        // Both deleted - no conflict
        (TreeChange::Deleted { .. }, TreeChange::Deleted { .. }) => None,
        // One modified, one deleted - conflict
        (TreeChange::Modified { new_hash, .. }, TreeChange::Deleted { old_hash, .. }) => {
            Some(MergeConflict {
                path: path.to_path_buf(),
                base: Some(*old_hash),
                target: Some(*new_hash),
                sandbox: None,
                conflict_type: ConflictType::ModifyDelete,
            })
        }
        (TreeChange::Deleted { old_hash, .. }, TreeChange::Modified { new_hash, .. }) => {
            Some(MergeConflict {
                path: path.to_path_buf(),
                base: Some(*old_hash),
                target: None,
                sandbox: Some(*new_hash),
                conflict_type: ConflictType::ModifyDelete,
            })
        }
        // Other combinations (add+modify, add+delete, etc.) - treat as conflicts
        _ => Some(MergeConflict {
            path: path.to_path_buf(),
            base: base_entry.map(|(h, _)| *h),
            target: get_new_hash(target_change),
            sandbox: get_new_hash(sandbox_change),
            conflict_type: ConflictType::BothModified,
        }),
    }
}

fn get_new_hash(change: &TreeChange) -> Option<Hash> {
    match change {
        TreeChange::Added { blob_hash, .. } => Some(*blob_hash),
        TreeChange::Modified { new_hash, .. } => Some(*new_hash),
        TreeChange::Deleted { .. } => None,
    }
}

pub fn generate_conflict_markers(
    repo_path: &Path,
    conflict: &MergeConflict,
    target_name: &str,
    sandbox_name: &str,
) -> Result<Vec<u8>> {
    let store = FsObjectStore::new(repo_path);

    let target_content = conflict
        .target
        .map(|h| store.read_object(&ObjectType::Blob, &h))
        .transpose()?
        .unwrap_or_default();

    let sandbox_content = conflict
        .sandbox
        .map(|h| store.read_object(&ObjectType::Blob, &h))
        .transpose()?
        .unwrap_or_default();

    let base_content = conflict
        .base
        .map(|h| store.read_object(&ObjectType::Blob, &h))
        .transpose()?;

    // Generate conflict markers
    let mut result = Vec::new();

    result.extend_from_slice(format!("<<<<<<< {} ({})\n", target_name, "target").as_bytes());
    result.extend_from_slice(&target_content);
    if !target_content.ends_with(b"\n") {
        result.push(b'\n');
    }

    if let Some(base) = base_content {
        result.extend_from_slice(b"||||||| base\n");
        result.extend_from_slice(&base);
        if !base.ends_with(b"\n") {
            result.push(b'\n');
        }
    }

    result.extend_from_slice(b"=======\n");
    result.extend_from_slice(&sandbox_content);
    if !sandbox_content.ends_with(b"\n") {
        result.push(b'\n');
    }
    result.extend_from_slice(format!(">>>>>>> {} ({})\n", sandbox_name, "sandbox").as_bytes());

    Ok(result)
}

pub fn execute_merge(
    repo_path: &Path,
    analysis: &MergeAnalysis,
    resolutions: &HashMap<PathBuf, ConflictResolution>,
    target_commit_hash: &Hash,
    sandbox_commit_hash: &Hash,
    author: &str,
    message: &str,
) -> Result<MergeResult> {
    let store = FsObjectStore::new(repo_path);
    let commit_store = CommitStore::new(repo_path, store.clone())?;

    // Verify all conflicts are resolved
    for conflict in &analysis.conflicts {
        if !resolutions.contains_key(&conflict.path) {
            anyhow::bail!("Unresolved conflict: {}", conflict.path.display());
        }
    }

    // Build the merged tree entries
    let mut merged_entries: HashMap<PathBuf, (Hash, u32)> = analysis.base_tree.clone();

    // Apply auto-resolved changes
    for entry in &analysis.auto_resolved {
        if let Some(hash) = entry.blob_hash {
            merged_entries.insert(entry.path.clone(), (hash, entry.mode));
        } else {
            merged_entries.remove(&entry.path);
        }
    }

    // Apply conflict resolutions
    for conflict in &analysis.conflicts {
        let resolution = resolutions.get(&conflict.path).unwrap();
        match resolution {
            ConflictResolution::TakeTarget => {
                if let Some(hash) = conflict.target {
                    merged_entries.insert(conflict.path.clone(), (hash, 0o100644));
                } else {
                    merged_entries.remove(&conflict.path);
                }
            }
            ConflictResolution::TakeSandbox => {
                if let Some(hash) = conflict.sandbox {
                    merged_entries.insert(conflict.path.clone(), (hash, 0o100644));
                } else {
                    merged_entries.remove(&conflict.path);
                }
            }
            ConflictResolution::TakeBase => {
                if let Some(hash) = conflict.base {
                    merged_entries.insert(conflict.path.clone(), (hash, 0o100644));
                } else {
                    merged_entries.remove(&conflict.path);
                }
            }
            ConflictResolution::Merged(content) => {
                // Write the merged content as a new blob
                let blob_hash = store.write_object(&ObjectType::Blob, content)?;
                merged_entries.insert(conflict.path.clone(), (blob_hash, 0o100644));
            }
            ConflictResolution::Delete => {
                merged_entries.remove(&conflict.path);
            }
        }
    }

    // Build index entries from merged tree
    let entries: Vec<Entry> = merged_entries
        .into_iter()
        .map(|(path, (oid, mode))| Entry {
            path,
            oid,
            flags: EntryFlags::TRACKED,
            size: 0,
            mtime_sec: 0,
            mtime_nsec: 0,
            file_mode: mode,
            merge_conflict_stage: 0,
            reserved: [0u8; 33],
        })
        .collect();

    // Build tree from entries
    let tree_builder = crate::helix_index::tree::TreeBuilder::new(repo_path);
    let merged_tree_hash = tree_builder.build_from_entries(&entries)?;

    let merge_commit = Commit::new(
        merged_tree_hash,
        vec![*target_commit_hash, *sandbox_commit_hash],
        author.to_string(),
        message.to_string(),
    );

    let commit_hash = commit_store.write_commit(&merge_commit)?;

    Ok(MergeResult {
        commit_hash,
        merged_tree_hash,
        conflicts_resolved: analysis.conflicts.len(),
        files_changed: analysis.auto_resolved.len() + analysis.conflicts.len(),
    })
}
