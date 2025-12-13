// Push command - Push Helix commits to Git remote
// helix push <remote> <branch>
//
// Workflow:
// 1. Load current Helix HEAD
// 2. Convert Helix commits → Git commits (with caching)
// 3. Write Git objects to .git/objects/
// 4. Use gix to push to remote
// 5. Save push cache

use crate::helix_index::commit::CommitLoader;
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
