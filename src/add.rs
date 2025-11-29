/// will be updated to not use git at all
use crate::helix_index::api::HelixIndexData;
use crate::helix_index::sync::SyncEngine;
use crate::helix_index::Reader;
use anyhow::{Context, Result};
use std::collections::HashSet;
use std::fs;
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

/// Add files to staging area
pub fn add(repo_path: &Path, paths: &[PathBuf], options: AddOptions) -> Result<()> {
    let start = Instant::now();

    if paths.is_empty() {
        anyhow::bail!("No paths specified. Use 'helix add <files>' or 'helix add .'");
    }

    // Load helix index
    let helix_index = HelixIndexData::load_or_rebuild(repo_path)?;

    // Determine which files need adding
    let files_to_add = resolve_files_to_add(repo_path, &helix_index, paths, &options)?;

    if files_to_add.is_empty() {
        if options.verbose {
            println!("No files to add (everything up-to-date)");
        } else {
            println!("No files to add");
        }
        return Ok(());
    }

    if options.verbose {
        println!("Adding {} files...", files_to_add.len());
        for f in &files_to_add {
            println!("  add '{}'", f.display());
        }
    }

    if options.dry_run {
        for f in &files_to_add {
            println!("Would add: {}", f.display());
        }
        return Ok(());
    }

    // Add files via git (updates .git/index and Git objects)
    add_files_via_git(repo_path, &files_to_add, &options)?;

    // Update helix.idx to reflect the staging
    update_helix_index(repo_path, &files_to_add)?;

    let elapsed = start.elapsed();
    if options.verbose {
        println!("Added {} files in {:?}", files_to_add.len(), elapsed);
    }

    Ok(())
}

fn resolve_files_to_add(
    repo_path: &Path,
    helix_index: &HelixIndexData,
    paths: &[PathBuf],
    options: &AddOptions,
) -> Result<Vec<PathBuf>> {
    let tracked_files = helix_index.get_tracked();
    let staged_files = helix_index.get_staged();

    if options.verbose {
        println!("Tracked files: {}", tracked_files.len());
        println!("Staged files: {}", staged_files.len());
    }

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
    helix_index: &HelixIndexData,
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
            .iter()
            .find(|e| e.path == relative_path)
            .ok_or_else(|| anyhow::anyhow!("Entry not found"))?;

        let metadata = fs::metadata(full_path)?;
        let disk_mtime = metadata
            .modified()?
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs();

        if disk_mtime == entry.mtime_sec {
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
        .iter()
        .find(|e| e.path == relative_path)
        .ok_or_else(|| anyhow::anyhow!("Entry not found"))?;

    let metadata = fs::metadata(full_path)?;
    let disk_mtime = metadata
        .modified()?
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();

    // If mtime different, file is modified
    if disk_mtime != entry.mtime_sec {
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
            // Skip .git and .helix directories
            !e.path()
                .components()
                .any(|c| c.as_os_str() == ".git" || c.as_os_str() == ".helix")
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

/// Add files via git add (updates .git/index and Git objects)
fn add_files_via_git(repo_path: &Path, paths: &[PathBuf], options: &AddOptions) -> Result<()> {
    if options.dry_run {
        for path in paths {
            println!("Would add: {}", path.display());
        }
        return Ok(());
    }

    if options.verbose {
        println!("Running git add for {} files...", paths.len());
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

/// Update helix.idx after git add
/// Since git add updated .git/index, we re-import to get the staged state
fn update_helix_index(repo_path: &Path, changed_paths: &[PathBuf]) -> Result<()> {
    let sync = SyncEngine::new(repo_path);
    sync.import_from_git()
        .context("Failed to update helix index after git add")?;

    if let Some(first_path) = changed_paths.first() {
        // Verify the file was actually staged
        let reader = Reader::new(repo_path);
        let data = reader.read()?;

        let staged = data.entries.iter().any(|e| {
            &e.path == first_path && e.flags.contains(crate::helix_index::EntryFlags::STAGED)
        });

        if !staged {
            eprintln!("Warning: File may not have been staged correctly");
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::helix_index::{self, EntryFlags, Reader};
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
        let repo_path = temp_dir.path();

        init_test_repo(repo_path)?;

        // Initialize helix
        crate::init::init_helix_repo(repo_path)?;

        // Create a file
        fs::write(repo_path.join("test.txt"), "hello")?;

        // Add it via helix
        add(
            repo_path,
            &[PathBuf::from("test.txt")],
            AddOptions {
                verbose: true,
                ..AddOptions::default()
            },
        )?;

        // Verify it's staged in Git
        let output = Command::new("git")
            .args(&["status", "--porcelain"])
            .current_dir(repo_path)
            .output()?;

        let status = String::from_utf8_lossy(&output.stdout);
        assert!(
            status.contains("A  test.txt") || status.contains("A test.txt"),
            "Expected file to be staged in Git, got: {}",
            status
        );

        // Verify it's staged in helix.idx
        let reader = Reader::new(repo_path);
        let data = reader.read()?;

        let staged_entry = data
            .entries
            .iter()
            .find(|e| e.path == PathBuf::from("test.txt"));

        assert!(staged_entry.is_some(), "File should be in helix.idx");
        assert!(
            staged_entry.unwrap().flags.contains(EntryFlags::STAGED),
            "File should be marked as STAGED in helix.idx"
        );

        Ok(())
    }

    #[test]
    fn test_add_directory() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        init_test_repo(repo_path)?;
        crate::init::init_helix_repo(repo_path)?;

        // Create files in a directory
        fs::create_dir_all(repo_path.join("src"))?;
        fs::write(repo_path.join("src/main.rs"), "fn main() {}")?;
        fs::write(repo_path.join("src/lib.rs"), "pub fn test() {}")?;

        // Add directory
        add(repo_path, &[PathBuf::from("src")], AddOptions::default())?;

        // Verify both files staged in Git
        let output = Command::new("git")
            .args(&["status", "--porcelain"])
            .current_dir(repo_path)
            .output()?;

        let status = String::from_utf8_lossy(&output.stdout);
        assert!(status.contains("main.rs"));
        assert!(status.contains("lib.rs"));

        // Verify in helix.idx
        let reader = Reader::new(repo_path);
        let data = reader.read()?;
        assert_eq!(data.entries.len(), 2);

        Ok(())
    }

    #[test]
    fn test_add_all() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        init_test_repo(repo_path)?;
        crate::init::init_helix_repo(repo_path)?;

        // Create multiple files
        fs::write(repo_path.join("file1.txt"), "one")?;
        fs::write(repo_path.join("file2.txt"), "two")?;
        fs::create_dir_all(repo_path.join("dir"))?;
        fs::write(repo_path.join("dir/file3.txt"), "three")?;

        // Add all
        add(repo_path, &[PathBuf::from(".")], AddOptions::default())?;

        // Verify all staged in Git
        let output = Command::new("git")
            .args(&["status", "--porcelain"])
            .current_dir(repo_path)
            .output()?;

        let status = String::from_utf8_lossy(&output.stdout);
        assert!(status.contains("file1.txt"));
        assert!(status.contains("file2.txt"));
        assert!(status.contains("file3.txt"));

        // Verify in helix.idx
        let reader = Reader::new(repo_path);
        let data = reader.read()?;
        assert_eq!(data.entries.len(), 3);

        Ok(())
    }

    #[test]
    fn test_helix_index_updated() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        init_test_repo(repo_path)?;
        crate::init::init_helix_repo(repo_path)?;

        // Create and add a file
        fs::write(repo_path.join("test.txt"), "hello")?;
        add(
            repo_path,
            &[PathBuf::from("test.txt")],
            AddOptions::default(),
        )?;

        let reader = Reader::new(repo_path);
        let data = reader.read()?;

        // Should have the file staged
        let staged = data
            .entries
            .iter()
            .filter(|e| e.flags.contains(EntryFlags::STAGED))
            .count();

        assert_eq!(staged, 1, "Should have exactly 1 staged file");

        Ok(())
    }

    #[test]
    fn test_add_modified_file() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        init_test_repo(repo_path)?;
        crate::init::init_helix_repo(repo_path)?;

        // Create, add, and commit a file
        fs::write(repo_path.join("test.txt"), "v1")?;
        add(
            repo_path,
            &[PathBuf::from("test.txt")],
            AddOptions::default(),
        )?;

        Command::new("git")
            .args(&["commit", "-m", "initial"])
            .current_dir(repo_path)
            .output()?;

        // Modify the file
        std::thread::sleep(std::time::Duration::from_millis(10));
        fs::write(repo_path.join("test.txt"), "v2")?;

        // Add again
        add(
            repo_path,
            &[PathBuf::from("test.txt")],
            AddOptions::default(),
        )?;

        // Should be staged
        let reader = Reader::new(repo_path);
        let data = reader.read()?;

        let entry = data
            .entries
            .iter()
            .find(|e| e.path == PathBuf::from("test.txt"));
        assert!(entry.is_some());
        assert!(entry.unwrap().flags.contains(EntryFlags::STAGED));

        Ok(())
    }
}
