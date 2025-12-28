use anyhow::{bail, Context, Result};
use helix_protocol::hash::{hash_to_hex, Hash};
use helix_protocol::message::ObjectType;
use helix_protocol::storage::FsObjectStore;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

pub struct CheckoutOptions {
    pub verbose: bool,
    pub force: bool, // Overwrite existing files
}

impl Default for CheckoutOptions {
    fn default() -> Self {
        Self {
            verbose: false,
            force: false,
        }
    }
}

/// Checkout a commit's tree to the working directory
pub fn checkout_tree(
    repo_path: &Path,
    commit_hash: &Hash,
    options: &CheckoutOptions,
) -> Result<u64> {
    let store = FsObjectStore::new(repo_path);

    // Read commit to get tree hash
    let commit_bytes = store
        .read_object(&ObjectType::Commit, commit_hash)
        .with_context(|| format!("Failed to read commit {}", hash_to_hex(commit_hash)))?;

    let tree_hash = parse_tree_hash_from_commit(&commit_bytes)?;

    if options.verbose {
        println!("Checking out tree {}", &hash_to_hex(&tree_hash)[..8]);
    }

    // Recursively checkout the tree
    let files_written =
        checkout_tree_recursive(&store, repo_path, &tree_hash, Path::new(""), options)?;

    Ok(files_written)
}

/// Recursively checkout a tree to a directory
fn checkout_tree_recursive(
    store: &FsObjectStore,
    repo_path: &Path,
    tree_hash: &Hash,
    relative_path: &Path,
    options: &CheckoutOptions,
) -> Result<u64> {
    let tree_bytes = store
        .read_object(&ObjectType::Tree, tree_hash)
        .with_context(|| format!("Failed to read tree {}", hash_to_hex(tree_hash)))?;

    let entries = parse_tree_entries(&tree_bytes)?;
    let mut files_written = 0;

    for entry in entries {
        let entry_path = relative_path.join(&entry.name);
        let full_path = repo_path.join(&entry_path);

        match entry.entry_type {
            EntryType::Tree => {
                // Create directory and recurse
                fs::create_dir_all(&full_path).with_context(|| {
                    format!("Failed to create directory {}", full_path.display())
                })?;

                files_written +=
                    checkout_tree_recursive(store, repo_path, &entry.hash, &entry_path, options)?;
            }
            EntryType::File | EntryType::FileExecutable => {
                // Read blob and write to file
                let blob_bytes = store
                    .read_object(&ObjectType::Blob, &entry.hash)
                    .with_context(|| format!("Failed to read blob {}", hash_to_hex(&entry.hash)))?;

                // Create parent directory if needed
                if let Some(parent) = full_path.parent() {
                    fs::create_dir_all(parent)?;
                }

                // Check if file exists
                if full_path.exists() && !options.force {
                    if options.verbose {
                        println!("  Skipping {} (already exists)", entry_path.display());
                    }
                    continue;
                }

                fs::write(&full_path, &blob_bytes)
                    .with_context(|| format!("Failed to write file {}", full_path.display()))?;

                // Set executable bit if needed
                if entry.entry_type == EntryType::FileExecutable {
                    let mut perms = fs::metadata(&full_path)?.permissions();
                    perms.set_mode(0o755);
                    fs::set_permissions(&full_path, perms)?;
                }

                if options.verbose {
                    println!(
                        "  {} ({})",
                        entry_path.display(),
                        &hash_to_hex(&entry.hash)[..8]
                    );
                }

                files_written += 1;
            }
            EntryType::Symlink => {
                // Read blob as symlink target
                let target_bytes = store
                    .read_object(&ObjectType::Blob, &entry.hash)
                    .with_context(|| {
                        format!("Failed to read symlink blob {}", hash_to_hex(&entry.hash))
                    })?;

                let target =
                    String::from_utf8(target_bytes).context("Symlink target is not valid UTF-8")?;

                // Create parent directory if needed
                if let Some(parent) = full_path.parent() {
                    fs::create_dir_all(parent)?;
                }

                // Remove existing if force
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EntryType {
    File,
    FileExecutable,
    Tree,
    Symlink,
}

struct TreeEntry {
    name: String,
    entry_type: EntryType,
    hash: Hash,
}

/// Parse tree entries from tree bytes
/// Format per entry: type(1) + mode(4) + size(8) + name_len(2) + name(var) + oid(32)
fn parse_tree_entries(bytes: &[u8]) -> Result<Vec<TreeEntry>> {
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
        let entry_type = match bytes[offset] {
            0 => EntryType::File,
            1 => EntryType::FileExecutable,
            2 => EntryType::Tree,
            3 => EntryType::Symlink,
            t => bail!("Unknown entry type: {}", t),
        };
        offset += 1;

        // Mode (4 bytes) - skip
        offset += 4;

        // Size (8 bytes) - skip
        offset += 8;

        // Name length (2 bytes)
        let name_len = u16::from_le_bytes(bytes[offset..offset + 2].try_into()?) as usize;
        offset += 2;

        // Name
        if offset + name_len > bytes.len() {
            bail!("Tree entry name truncated");
        }
        let name = String::from_utf8(bytes[offset..offset + name_len].to_vec())
            .context("Invalid UTF-8 in entry name")?;
        offset += name_len;

        // OID (32 bytes)
        if offset + 32 > bytes.len() {
            bail!("Tree entry OID truncated");
        }
        let mut hash = [0u8; 32];
        hash.copy_from_slice(&bytes[offset..offset + 32]);
        offset += 32;

        entries.push(TreeEntry {
            name,
            entry_type,
            hash,
        });
    }

    Ok(entries)
}
