use anyhow::{bail, Context, Result};
use helix_protocol::hash::{hash_to_hex, Hash};
use helix_protocol::message::ObjectType;
use helix_protocol::storage::FsObjectStore;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use crate::helix_index::tree::{EntryType, Tree};

pub struct CheckoutOptions {
    pub verbose: bool,
    pub force: bool,
}

impl Default for CheckoutOptions {
    fn default() -> Self {
        Self {
            verbose: false,
            force: false,
        }
    }
}

/// Checkout a commit's tree to the repo's working directory
///
/// This is a convenience wrapper around `checkout_tree_to_path` that uses
/// repo_path as both the object store location and the destination.
pub fn checkout_tree(
    repo_path: &Path,
    commit_hash: &Hash,
    options: &CheckoutOptions,
) -> Result<u64> {
    checkout_tree_to_path(repo_path, commit_hash, repo_path, options)
}

/// Checkout a commit's tree to a specific destination path
///
/// - `repo_path`: Where the object store lives (.helix/objects/)
/// - `commit_hash`: The commit to checkout
/// - `dest_path`: Where to write the files (can be different from repo_path)
/// - `options`: Checkout options (verbose, force)
///
/// This is the core checkout function used by both normal checkout and sandboxes.
pub fn checkout_tree_to_path(
    repo_path: &Path,
    commit_hash: &Hash,
    dest_path: &Path,
    options: &CheckoutOptions,
) -> Result<u64> {
    let store = FsObjectStore::new(repo_path);

    // Read commit to get tree hash
    let commit_bytes = store
        .read_object(&ObjectType::Commit, commit_hash)
        .with_context(|| format!("Failed to read commit {}", hash_to_hex(commit_hash)))?;

    let tree_hash = parse_tree_hash_from_commit(&commit_bytes)?;

    if options.verbose {
        if repo_path == dest_path {
            println!("Checking out tree {}", &hash_to_hex(&tree_hash)[..8]);
        } else {
            println!(
                "Checking out tree {} to {}",
                &hash_to_hex(&tree_hash)[..8],
                dest_path.display()
            );
        }
    }

    // Create destination directory if different from repo
    if dest_path != repo_path {
        fs::create_dir_all(dest_path)
            .with_context(|| format!("Failed to create destination {}", dest_path.display()))?;
    }

    // Recursively checkout the tree
    checkout_tree_recursive(&store, dest_path, &tree_hash, Path::new(""), options)
}

/// Recursively checkout a tree to a directory
fn checkout_tree_recursive(
    store: &FsObjectStore,
    dest_root: &Path,
    tree_hash: &Hash,
    relative_path: &Path,
    options: &CheckoutOptions,
) -> Result<u64> {
    let tree_bytes = store
        .read_object(&ObjectType::Tree, tree_hash)
        .with_context(|| format!("Failed to read tree {}", hash_to_hex(tree_hash)))?;

    let tree = Tree::from_bytes(&tree_bytes)?;
    let mut files_written = 0;

    for entry in tree.entries {
        let entry_path = relative_path.join(&entry.name);
        let full_path = dest_root.join(&entry_path);

        match entry.entry_type {
            EntryType::Tree => {
                fs::create_dir_all(&full_path).with_context(|| {
                    format!("Failed to create directory {}", full_path.display())
                })?;

                files_written +=
                    checkout_tree_recursive(store, dest_root, &entry.oid, &entry_path, options)?;
            }
            EntryType::File | EntryType::FileExecutable => {
                let blob_bytes = store
                    .read_object(&ObjectType::Blob, &entry.oid)
                    .with_context(|| format!("Failed to read blob {}", hash_to_hex(&entry.oid)))?;

                if let Some(parent) = full_path.parent() {
                    fs::create_dir_all(parent)?;
                }

                if full_path.exists() && !options.force {
                    if options.verbose {
                        println!("  Skipping {} (already exists)", entry_path.display());
                    }
                    continue;
                }

                fs::write(&full_path, &blob_bytes)
                    .with_context(|| format!("Failed to write file {}", full_path.display()))?;

                if entry.entry_type == EntryType::FileExecutable {
                    let mut perms = fs::metadata(&full_path)?.permissions();
                    perms.set_mode(0o755);
                    fs::set_permissions(&full_path, perms)?;
                }

                if options.verbose {
                    println!(
                        "  {} ({})",
                        entry_path.display(),
                        &hash_to_hex(&entry.oid)[..8]
                    );
                }

                files_written += 1;
            }
            EntryType::Symlink => {
                let target_bytes = store
                    .read_object(&ObjectType::Blob, &entry.oid)
                    .with_context(|| {
                        format!("Failed to read symlink blob {}", hash_to_hex(&entry.oid))
                    })?;

                let target =
                    String::from_utf8(target_bytes).context("Symlink target is not valid UTF-8")?;

                if let Some(parent) = full_path.parent() {
                    fs::create_dir_all(parent)?;
                }

                if full_path.exists() || full_path.symlink_metadata().is_ok() {
                    if options.force {
                        fs::remove_file(&full_path).ok();
                    } else {
                        if options.verbose {
                            println!(
                                "  Skipping symlink {} (already exists)",
                                entry_path.display()
                            );
                        }
                        continue;
                    }
                }

                #[cfg(unix)]
                std::os::unix::fs::symlink(&target, &full_path)
                    .with_context(|| format!("Failed to create symlink {}", full_path.display()))?;

                if options.verbose {
                    println!("  {} -> {}", entry_path.display(), target);
                }

                files_written += 1;
            }
        }
    }

    Ok(files_written)
}

/// Parse tree hash from commit bytes (first 32 bytes)
fn parse_tree_hash_from_commit(bytes: &[u8]) -> Result<Hash> {
    if bytes.len() < 32 {
        bail!("Commit too short");
    }
    let mut hash = [0u8; 32];
    hash.copy_from_slice(&bytes[0..32]);
    Ok(hash)
}
