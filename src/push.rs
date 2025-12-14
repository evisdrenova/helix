// Push command - Push Helix commits to Git remote
// helix push <remote> <branch>
//
// Workflow:
// 1. Load current Helix HEAD
// 2. Convert Helix commits → Git commits (with caching)
// 3. Write Git objects to .git/objects/
// 4. Use gix to push to remote
// 5. Save push cache

use crate::helix_index::api::HelixIndexData;
use crate::helix_index::blob_storage::BlobStorage;
use crate::helix_index::commit::{CommitLoader, CommitStorage};
use crate::helix_index::hash::{self, Hash};
use crate::helix_index::tree::{EntryType, TreeStorage};
use crate::helix_to_git::{converter, git_objects};
use anyhow::{Context, Result};
use converter::HelixToGitConverter;
use std::fs;
use std::path::Path;

pub struct PushOptions {
    pub verbose: bool,
    pub dry_run: bool,
    pub force: bool,
}

impl Default for PushOptions {
    fn default() -> Self {
        Self {
            verbose: false,
            dry_run: false,
            force: false,
        }
    }
}

/// Push Helix commits to Git remote
///
/// Usage:
///   helix push origin main
///   helix push origin main --verbose
///   helix push origin feature-branch --force
pub fn push(repo_path: &Path, remote: &str, branch: &str, options: PushOptions) -> Result<()> {
    if options.verbose {
        println!("Pushing to {}/{}...", remote, branch);
    }

    let _ = debug_check_objects(repo_path);

    // 1. Load Helix HEAD
    let loader = CommitLoader::new(repo_path)?;
    let head_hash = loader
        .read_head()
        .context("Failed to read HEAD. No commits to push?")?;

    if options.verbose {
        println!(
            "HEAD commit: {}",
            crate::helix_index::hash::hash_to_hex(&head_hash)[..8].to_string()
        );
    }

    // 2. Convert Helix commits → Git
    if options.verbose {
        println!("Converting Helix commits to Git format...");
    }

    let mut converter = HelixToGitConverter::new(repo_path)?;

    // Convert HEAD commit (recursively converts entire history)
    let git_head_sha = converter
        .convert_commit(&head_hash)
        .context("Failed to convert Helix commit to Git")?;

    if options.verbose {
        println!("Git HEAD SHA: {}", &git_head_sha[..8]);
        println!("Cached {} conversions", converter.cache_size());
    }

    // 3. Get all Git objects to push
    let git_objects = converter
        .get_git_objects(&head_hash)
        .context("Failed to collect Git objects")?;

    if options.verbose {
        println!(
            "Objects to push: {} commits, {} trees, {} blobs",
            git_objects.commits.len(),
            git_objects.trees.len(),
            git_objects.blobs.len()
        );
    }

    if options.dry_run {
        println!("Dry run - would push:");
        println!("  HEAD: {}", git_head_sha);
        println!("  Total objects: {}", git_objects.total_count());
        return Ok(());
    }

    // 4. Write Git objects to .git/objects/
    if options.verbose {
        println!("Writing Git objects...");
    }

    write_git_objects(repo_path, &git_objects)?;

    // 5. Push using gix
    if options.verbose {
        println!("Pushing to remote...");
    }

    push_with_gix(repo_path, remote, branch, &git_head_sha, &options)?;

    // 6. Save push cache
    converter
        .save_cache()
        .context("Failed to save push cache")?;

    println!("✓ Pushed to {}/{}", remote, branch);

    Ok(())
}

pub fn debug_check_objects(repo_path: &Path) -> Result<()> {
    println!("=== Checking Helix Object Integrity ===\n");

    // 1. Check index entries
    let index = HelixIndexData::load_or_rebuild(repo_path)?;
    let blob_storage = BlobStorage::for_repo(repo_path);

    println!("Index entries: {}", index.entries().len());

    let mut missing_blobs = Vec::new();
    for entry in index.entries() {
        let hex = hash::hash_to_hex(&entry.oid);
        if !blob_storage.exists(&entry.oid) {
            println!(
                "  ✗ MISSING blob for {}: {}",
                entry.path.display(),
                &hex[..16]
            );
            missing_blobs.push((entry.path.display().to_string(), hex));
        } else {
            println!(
                "  ✓ Found blob for {}: {}",
                entry.path.display(),
                &hex[..16]
            );
        }
    }

    if !missing_blobs.is_empty() {
        println!("\n⚠️  Found {} missing blobs:", missing_blobs.len());
        for (path, hash) in &missing_blobs {
            println!("  - {}: {}", path, &hash[..16]);
        }
        println!("\nThese files need to be re-added with 'helix add'");
    }

    // 2. Check HEAD commit
    println!("\n=== Checking HEAD Commit ===\n");

    let loader = CommitLoader::new(repo_path)?;
    let head_hash = loader.read_head()?;
    let head_hex = hash::hash_to_hex(&head_hash);

    println!("HEAD: {}", &head_hex[..16]);

    // 3. Check commit tree
    let commit_storage = CommitStorage::for_repo(repo_path);
    let commit = commit_storage.read(&head_hash)?;

    // 4. Walk tree and check all blobs
    let tree_storage = TreeStorage::for_repo(repo_path);
    let mut trees_to_check = vec![commit.tree_hash];
    let mut checked_trees = std::collections::HashSet::new();
    let mut all_tree_blobs = Vec::new();

    while let Some(tree_hash) = trees_to_check.pop() {
        if checked_trees.contains(&tree_hash) {
            continue;
        }
        checked_trees.insert(tree_hash);

        let tree = tree_storage.read(&tree_hash)?;

        for entry in &tree.entries {
            match entry.entry_type {
                EntryType::Tree => {
                    trees_to_check.push(entry.oid);
                }
                _ => {
                    // It's a blob
                    all_tree_blobs.push((entry.name.clone(), entry.oid));
                }
            }
        }
    }

    println!("\n=== Blobs in Commit Tree ===\n");
    println!("Found {} blobs in tree:", all_tree_blobs.len());

    let mut missing_tree_blobs = Vec::new();
    for (name, oid) in &all_tree_blobs {
        let hex = hash::hash_to_hex(oid);
        if !blob_storage.exists(oid) {
            println!("  ✗ MISSING: {} -> {}", name, &hex[..16]);
            missing_tree_blobs.push((name.clone(), hex));
        } else {
            println!("  ✓ Found: {} -> {}", name, &hex[..16]);
        }
    }

    if !missing_tree_blobs.is_empty() {
        println!(
            "\n❌ PROBLEM: {} blobs referenced in tree but missing from storage:",
            missing_tree_blobs.len()
        );
        for (name, hash) in &missing_tree_blobs {
            println!("  - {}: {}", name, hash);
        }

        println!("\n🔧 FIX:");
        println!("This means the tree was built with blob hashes that don't exist.");
        println!("You need to:");
        println!("1. Re-add the files: helix add .");
        println!("2. Create a new commit: helix commit -m 'Fix missing blobs'");

        anyhow::bail!("Cannot push - missing blobs in tree");
    }

    println!("\n✅ All objects are consistent!");

    Ok(())
}

/// Write Git objects to .git/objects/
fn write_git_objects(repo_path: &Path, git_objects: &converter::GitObjects) -> Result<()> {
    let objects_dir = repo_path.join(".git").join("objects");

    // Write blobs
    for (sha, object_data) in &git_objects.blobs {
        write_git_object(&objects_dir, sha, object_data)?;
    }

    // Write trees
    for (sha, object_data) in &git_objects.trees {
        write_git_object(&objects_dir, sha, object_data)?;
    }

    // Write commits
    for (sha, object_data) in &git_objects.commits {
        write_git_object(&objects_dir, sha, object_data)?;
    }

    Ok(())
}

/// Write single Git object to .git/objects/xx/yyyyyy...
fn write_git_object(objects_dir: &Path, sha: &str, object_data: &[u8]) -> Result<()> {
    if sha.len() != 40 {
        anyhow::bail!("Invalid Git SHA length: {}", sha.len());
    }

    // Git stores objects as .git/objects/ab/cdef...
    let dir_name = &sha[..2];
    let file_name = &sha[2..];

    let object_dir = objects_dir.join(dir_name);
    fs::create_dir_all(&object_dir)
        .with_context(|| format!("Failed to create objects directory {:?}", object_dir))?;

    let object_path = object_dir.join(file_name);

    // Skip if already exists (deduplication)
    if object_path.exists() {
        return Ok(());
    }

    // Compress with zlib
    let compressed =
        git_objects::compress_git_object(object_data).context("Failed to compress Git object")?;

    // Write atomically
    let temp_path = object_path.with_extension("tmp");
    fs::write(&temp_path, &compressed)
        .with_context(|| format!("Failed to write Git object {:?}", temp_path))?;

    fs::rename(&temp_path, &object_path)
        .with_context(|| format!("Failed to rename Git object {:?}", object_path))?;

    Ok(())
}

/// Push to remote using gix
fn push_with_gix(
    repo_path: &Path,
    remote_name: &str,
    branch: &str,
    git_head_sha: &str,
    options: &PushOptions,
) -> Result<()> {
    // For now, use git command directly
    // TODO: Replace with pure gix implementation when API stabilizes
    use std::process::Command;

    if options.verbose {
        println!("Pushing to {}/{} using git...", remote_name, branch);
    }

    // Update the branch ref first
    let branch_ref_path = repo_path
        .join(".git")
        .join("refs")
        .join("heads")
        .join(branch);

    // Ensure refs/heads directory exists
    if let Some(parent) = branch_ref_path.parent() {
        fs::create_dir_all(parent)?;
    }

    // Write the Git SHA to the branch ref
    fs::write(&branch_ref_path, format!("{}\n", git_head_sha))?;

    if options.verbose {
        println!(
            "Updated .git/refs/heads/{} to {}",
            branch,
            &git_head_sha[..8]
        );
    }

    // Build git push command
    let mut cmd = Command::new("git");
    cmd.current_dir(repo_path);
    cmd.arg("push");

    if options.force {
        cmd.arg("--force");
    }

    if options.verbose {
        cmd.arg("--verbose");
    }

    cmd.arg(remote_name);
    cmd.arg(format!("refs/heads/{}:refs/heads/{}", branch, branch));

    // Execute push
    let output = cmd
        .output()
        .context("Failed to execute git push. Is git installed?")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("Git push failed: {}", stderr);
    }

    if options.verbose {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !stdout.is_empty() {
            println!("{}", stdout);
        }
        if !stderr.is_empty() {
            eprintln!("{}", stderr);
        }
    }

    Ok(())
}

/// Get current branch name from Helix HEAD
fn get_current_branch(repo_path: &Path) -> Result<String> {
    let loader = CommitLoader::new(repo_path)?;
    loader.get_current_branch_name()
}

/// Push current branch
pub fn push_current_branch(repo_path: &Path, remote: &str, options: PushOptions) -> Result<()> {
    let branch = get_current_branch(repo_path)?;

    if branch == "(no branch)" || branch == "(detached HEAD)" {
        anyhow::bail!(
            "Not on any branch. Specify branch explicitly: helix push {} <branch>",
            remote
        );
    }

    push(repo_path, remote, &branch, options)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::helix_index::commit::{Commit, CommitStorage};
    use crate::helix_index::tree::TreeBuilder;
    use tempfile::TempDir;

    #[test]
    fn test_push_dry_run() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        // Initialize .git directory (minimal)
        fs::create_dir_all(repo_path.join(".git").join("objects"))?;

        // Create Helix commit
        let tree_builder = TreeBuilder::new(repo_path);
        let tree_hash = tree_builder.build_from_entries(&[])?;

        let commit = Commit::initial(
            tree_hash,
            "Test <test@example.com>".to_string(),
            "Test commit".to_string(),
        );

        let commit_storage = CommitStorage::for_repo(repo_path);
        let commit_hash = commit_storage.write(&commit)?;

        // Update HEAD
        fs::create_dir_all(repo_path.join(".helix"))?;
        fs::write(
            repo_path.join(".helix").join("HEAD"),
            crate::helix_index::hash::hash_to_hex(&commit_hash),
        )?;

        // Dry run push
        let result = push(
            repo_path,
            "origin",
            "main",
            PushOptions {
                verbose: true,
                dry_run: true,
                ..Default::default()
            },
        );

        // Should succeed with dry run
        assert!(result.is_ok());

        Ok(())
    }
}
