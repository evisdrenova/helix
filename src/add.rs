/*
Defines the helix add command for adding files to the .git/index and staging them
*/

use crate::helix_index::api::HelixIndex;
use crate::helix_index::sync::SyncEngine;
use anyhow::{Context, Result};
use flate2::write::ZlibEncoder;
use flate2::Compression;
use sha1::{Digest, Sha1};
use std::collections::HashSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;
use walkdir::WalkDir;

pub struct AddOptions {
    pub verbose: bool,
    pub dry_run: bool,
    pub force: bool,
}

impl Default for AddOptions {
    fn default() -> Self {
        Self {
            verbose: false,
            dry_run: false,
            force: false,
        }
    }
}

pub fn add(repo_path: &Path, paths: &[PathBuf], options: AddOptions) -> Result<()> {
    let start = Instant::now();

    if paths.is_empty() {
        anyhow::bail!("No paths specified. Use 'helix add <files>' or 'helix add .'");
    }

    let helix_index = HelixIndex::load_or_rebuild(repo_path)?;

    let files_to_add = resolve_files_to_add(repo_path, &helix_index, &paths, &options)?;

    if files_to_add.is_empty() {
        if options.verbose {
            println!("No files to add (everything up-to-date)");
        }
        return Ok(());
    }

    if options.verbose {
        println!("Adding {} files...", files_to_add.len());
        for file in &files_to_add {
            println!("  add '{}'", file.display());
        }
    }

    if options.dry_run {
        for path in &files_to_add {
            println!("Would add: {}", path.display());
        }
        return Ok(());
    }

    add_via_git(repo_path, &files_to_add, &options)?;

    update_helix_index(repo_path, &files_to_add)?;

    let elapsed = start.elapsed();

    if options.verbose {
        println!("Added {} files in {:?}", files_to_add.len(), elapsed);
    }

    Ok(())
}

fn resolve_files_to_add(
    repo_path: &Path,
    helix_index: &HelixIndex,
    paths: &[PathBuf],
    options: &AddOptions,
) -> Result<Vec<PathBuf>> {
    let tracked_files: HashSet<PathBuf> = helix_index
        .entries()
        .into_iter()
        .map(|entry| entry.path.clone())
        .collect();

    let staged_files = helix_index.get_staged();

    let unstaged_files = helix_index.get_untracked();

    println!("staged files: {:?}", staged_files);
    println!("unstaged files: {:?}", unstaged_files);

    let candidate_files = expand_paths(repo_path, paths)?;

    let mut files_to_add = Vec::new();

    for file_path in candidate_files {
        let full_path = repo_path.join(&file_path);

        if !full_path.exists() {
            if options.verbose {
                eprintln!("Warning: {} does not exist, skipping", file_path.display());
            }
            continue;
        }

        // Check if file needs to be added
        if should_add_file(
            &file_path,
            &full_path,
            &tracked_files,
            &staged_files,
            helix_index,
            options,
        )? {
            files_to_add.push(file_path);
        }
    }

    Ok(files_to_add)
}

fn should_add_file(
    relative_path: &Path,
    full_path: &Path,
    tracked_files: &HashSet<PathBuf>,
    staged_files: &HashSet<PathBuf>,
    helix_index: &HelixIndex,
    _options: &AddOptions,
) -> Result<bool> {
    // If file is untracked, always add it
    if !tracked_files.contains(relative_path) {
        return Ok(true);
    }

    // File is tracked - check if it's already staged and unchanged
    if staged_files.contains(relative_path) {
        // File is already staged
        // Check if it's been modified since staging
        let entry = helix_index
            .entries()
            .into_iter()
            .find(|e| e.path == relative_path)
            .ok_or_else(|| anyhow::anyhow!("Entry not found"))?;

        let metadata = fs::metadata(full_path)?;
        let disk_mtime = metadata
            .modified()?
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();

        if disk_mtime == entry.mtime_nsec as u128 {
            // File hasn't changed since staging - skip
            return Ok(false);
        }

        // File changed since staging - re-add it
        return Ok(true);
    }

    // File is tracked but not staged
    // Check if it's modified compared to index
    let entry = helix_index
        .entries()
        .into_iter()
        .find(|e| e.path == relative_path)
        .ok_or_else(|| anyhow::anyhow!("Entry not found"))?;

    let metadata = fs::metadata(full_path)?;
    let disk_mtime = metadata
        .modified()?
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();

    // If mtime different, file is modified
    if disk_mtime != entry.mtime_sec as u128 {
        return Ok(true);
    }

    // File unchanged - skip
    Ok(false)
}

/// Expand paths (handle ".", globs, directories)
fn expand_paths(repo_path: &Path, paths: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let mut expanded = Vec::new();

    for path in paths {
        let full_path = if path.is_absolute() {
            path.clone()
        } else {
            repo_path.join(path)
        };

        if !full_path.try_exists()? {
            eprintln!("Warning: '{}' does not exist, skipping", path.display());
            continue;
        }

        if full_path.is_file() {
            // Single file
            let relative = full_path
                .strip_prefix(repo_path)
                .context("Path is outside repository")?;
            expanded.push(relative.to_path_buf());
        } else if full_path.is_dir() {
            // Directory - recursively add all files
            let files = collect_files_recursive(&full_path, repo_path)?;
            expanded.extend(files);
        }
    }

    expanded.sort();
    expanded.dedup();

    Ok(expanded)
}

/// Recursively collect files from a directory
fn collect_files_recursive(dir: &Path, repo_root: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();

    for entry in WalkDir::new(dir)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            // Skip .git directory
            !e.path().components().any(|c| c.as_os_str() == ".git")
        })
    {
        let entry = entry?;
        if entry.file_type().is_file() {
            let relative = entry
                .path()
                .strip_prefix(repo_root)
                .context("Path outside repo")?;
            files.push(relative.to_path_buf());
        }
    }

    Ok(files)
}

/// Add files using git (ensures compatibility)
fn add_via_git(repo_path: &Path, paths: &[PathBuf], options: &AddOptions) -> Result<()> {
    // println!("the paths: {:?}", paths);
    if options.dry_run {
        for path in paths {
            println!("Would add: {}", path.display());
        }
        return Ok(());
    }

    // Build git add command
    let mut cmd = Command::new("git");
    cmd.current_dir(repo_path);
    cmd.arg("add");

    if options.force {
        cmd.arg("--force");
    }

    if options.verbose {
        cmd.arg("--verbose");
    }

    // Add paths
    for path in paths {
        cmd.arg(path);
    }

    // Execute
    let output = cmd.output().context("Failed to execute git add")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("git add failed: {}", stderr);
    }

    // Print git's output if verbose
    if options.verbose && !output.stdout.is_empty() {
        print!("{}", String::from_utf8_lossy(&output.stdout));
    }

    Ok(())
}

/// Update helix.idx incrementally after git add
fn update_helix_index(repo_path: &Path, changed_paths: &[PathBuf]) -> Result<()> {
    let sync = SyncEngine::new(repo_path);

    sync.incremental_sync(changed_paths)
        .context("Failed to update helix index")?;

    Ok(())
}

// /// Add with parallel optimization (future enhancement)
// #[allow(dead_code)]
// fn add_parallel(repo_path: &Path, paths: &[PathBuf]) -> Result<()> {
//     // Stage 1: Hash all files in parallel
//     let hashes: Vec<_> = paths
//         .par_iter()
//         .map(|path| {
//             let full_path = repo_path.join(path);
//             let content = fs::read(&full_path)?;
//             let hash = compute_git_hash(&content);
//             Ok::<_, anyhow::Error>((path.clone(), hash, content))
//         })
//         .collect::<Result<Vec<_>>>()?;

//     // Stage 2: Write objects in parallel
//     hashes.par_iter().try_for_each(|(path, hash, content)| {
//         write_git_object(repo_path, hash, content)?;
//         Ok::<_, anyhow::Error>(())
//     })?;

//     // Stage 3: Update index (sequential - must be atomic)
//     let mut index = GitIndex::open(repo_path)?;
//     for (path, hash, _content) in hashes {
//         index.add_entry(&path, &hash)?;
//     }
//     index.write()?;

//     Ok(())
// }

/// Compute git-style SHA-1 hash
fn compute_git_hash(content: &[u8]) -> [u8; 20] {
    let mut hasher = Sha1::new();

    // Git format: "blob <size>\0<content>"
    hasher.update(b"blob ");
    hasher.update(content.len().to_string().as_bytes());
    hasher.update(b"\0");
    hasher.update(content);

    let result = hasher.finalize();
    let mut hash = [0u8; 20];
    hash.copy_from_slice(&result);
    hash
}

/// Write object to .git/objects/ (git loose object format)
fn write_git_object(repo_path: &Path, hash: &[u8; 20], content: &[u8]) -> Result<()> {
    let hash_hex = hex::encode(hash);
    let dir = format!("{}", &hash_hex[0..2]);
    let file = format!("{}", &hash_hex[2..]);

    let objects_dir = repo_path.join(".git/objects").join(&dir);
    fs::create_dir_all(&objects_dir)?;

    let object_path = objects_dir.join(&file);

    // Don't overwrite if exists
    if object_path.exists() {
        return Ok(());
    }

    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());

    // Write header
    write!(encoder, "blob {}\0", content.len())?;
    encoder.write_all(content)?;

    let compressed = encoder.finish()?;

    // Write atomically (temp file + rename)
    let temp_path = object_path.with_extension("tmp");
    fs::write(&temp_path, compressed)?;
    fs::rename(temp_path, object_path)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::helix_index::{self, Reader};

    use super::*;
    use std::process::Command;
    use tempfile::TempDir;

    fn init_test_repo(path: &Path) -> Result<()> {
        fs::create_dir_all(path.join(".git"))?;
        Command::new("git")
            .args(&["init"])
            .current_dir(path)
            .output()?;
        Command::new("git")
            .args(&["config", "user.name", "Test"])
            .current_dir(path)
            .output()?;
        Command::new("git")
            .args(&["config", "user.email", "test@test.com"])
            .current_dir(path)
            .output()?;
        Ok(())
    }

    #[test]
    fn test_add_single_file() -> Result<()> {
        let temp_dir = TempDir::new()?;
        init_test_repo(temp_dir.path())?;

        // Create a file
        fs::write(temp_dir.path().join("test.txt"), "hello")?;

        // Add it
        add(
            temp_dir.path(),
            &[PathBuf::from("test.txt")],
            AddOptions::default(),
        )?;

        // Verify it's staged
        let output = Command::new("git")
            .args(&["status", "--porcelain"])
            .current_dir(temp_dir.path())
            .output()?;

        let status = String::from_utf8_lossy(&output.stdout);
        assert!(status.contains("A  test.txt") || status.contains("A test.txt"));

        Ok(())
    }

    #[test]
    fn test_add_directory() -> Result<()> {
        let temp_dir = TempDir::new()?;
        init_test_repo(temp_dir.path())?;

        // Create files in a directory
        fs::create_dir_all(temp_dir.path().join("src"))?;
        fs::write(temp_dir.path().join("src/main.rs"), "fn main() {}")?;
        fs::write(temp_dir.path().join("src/lib.rs"), "pub fn test() {}")?;

        // Add directory
        add(
            temp_dir.path(),
            &[PathBuf::from("src")],
            AddOptions::default(),
        )?;

        // Verify both files staged
        let output = Command::new("git")
            .args(&["status", "--porcelain"])
            .current_dir(temp_dir.path())
            .output()?;

        let status = String::from_utf8_lossy(&output.stdout);
        assert!(status.contains("main.rs"));
        assert!(status.contains("lib.rs"));

        Ok(())
    }

    #[test]
    fn test_add_all() -> Result<()> {
        let temp_dir = TempDir::new()?;
        init_test_repo(temp_dir.path())?;

        // Create multiple files
        fs::write(temp_dir.path().join("file1.txt"), "one")?;
        fs::write(temp_dir.path().join("file2.txt"), "two")?;
        fs::create_dir_all(temp_dir.path().join("dir"))?;
        fs::write(temp_dir.path().join("dir/file3.txt"), "three")?;

        // Add all
        add(
            temp_dir.path(),
            &[PathBuf::from(".")],
            AddOptions::default(),
        )?;

        // Verify all staged
        let output = Command::new("git")
            .args(&["status", "--porcelain"])
            .current_dir(temp_dir.path())
            .output()?;

        let status = String::from_utf8_lossy(&output.stdout);
        assert!(status.contains("file1.txt"));
        assert!(status.contains("file2.txt"));
        assert!(status.contains("file3.txt"));

        Ok(())
    }

    #[test]
    fn test_helix_index_updated() -> Result<()> {
        let temp_dir = TempDir::new()?;
        init_test_repo(temp_dir.path())?;

        // Initialize helix
        crate::init::init_helix_repo(temp_dir.path())?;

        // Create and add a file
        fs::write(temp_dir.path().join("test.txt"), "hello")?;
        add(
            temp_dir.path(),
            &[PathBuf::from("test.txt")],
            AddOptions::default(),
        )?;

        let reader = Reader::new(temp_dir.path());
        let data = reader.read()?;

        // Should have the file staged
        let staged = data
            .entries
            .iter()
            .filter(|e| e.flags.contains(helix_index::EntryFlags::STAGED))
            .count();

        assert_eq!(staged, 1);

        Ok(())
    }
}
