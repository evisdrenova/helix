// Add command - Stage files using pure Helix storage

use crate::helix_index::api::HelixIndexData;
use crate::helix_index::format::{Entry, EntryFlags};
use crate::ignore::IgnoreRules;
use anyhow::{Context, Result};
use helix_protocol::message::ObjectType;
use helix_protocol::storage::FsObjectStore;
use rayon::prelude::*;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
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

/// Add files to staging area using pure Helix storage
pub fn add(repo_path: &Path, paths: &[PathBuf], options: AddOptions) -> Result<()> {
    let start = Instant::now();

    if paths.is_empty() {
        anyhow::bail!("No paths specified. Use 'helix add <files>' or 'helix add .'");
    }

    // Load helix index
    let mut index = HelixIndexData::load_or_rebuild(repo_path)?;

    if options.verbose {
        println!("Loaded index (generation {})", index.generation());
    }

    // Resolve which files need adding (parallel)
    let files_to_add = resolve_files_to_add(repo_path, &index, paths, &options)?;

    if files_to_add.is_empty() {
        if options.verbose {
            println!("No files to add (everything up-to-date)");
        } else {
            println!("No files to add");
        }
        return Ok(());
    }

    if options.verbose {
        println!("Staging {} files...", files_to_add.len());
        for file in &files_to_add {
            println!("  add '{}'", file.display());
        }
    }

    if options.dry_run {
        for file in &files_to_add {
            println!("Would add: {}", file.display());
        }
        return Ok(());
    }

    // Stage files (parallel hashing + batch blob writes)
    stage_files(repo_path, &mut index, &files_to_add, &options)?;

    // Persist index to disk
    index.persist()?;

    let elapsed = start.elapsed();
    if options.verbose {
        println!("Staged {} files in {:?}", files_to_add.len(), elapsed);
        println!("Index generation: {}", index.generation());
    }

    Ok(())
}

fn resolve_files_to_add(
    repo_path: &Path,
    index: &HelixIndexData,
    paths: &[PathBuf],
    options: &AddOptions,
) -> Result<Vec<PathBuf>> {
    let tracked = index.get_tracked();
    let staged = index.get_staged();

    // Load ignore rules
    let ignore_rules = IgnoreRules::load(repo_path);

    if options.verbose {
        println!("Currently tracked: {}", tracked.len());
        println!("Currently staged: {}", staged.len());
    }

    // Expand paths (handle ".", directories, globs) - parallel
    let candidate_files = expand_paths_parallel(repo_path, paths)?;

    if options.verbose {
        println!("Found {} candidate files", candidate_files.len());
    }

    // Filter to files that actually need adding - parallel
    let files_to_add: Vec<PathBuf> = candidate_files
        .par_iter()
        .filter_map(|file_path| {
            let full_path = repo_path.join(file_path);

            // Must exist
            if !full_path.exists() {
                if options.verbose {
                    eprintln!("Warning: {} does not exist, skipping", file_path.display());
                }
                return None;
            }

            // Check if should be ignored (unless force)
            if !options.force && ignore_rules.should_ignore(file_path) {
                if options.verbose {
                    println!("  skipping '{}' (ignored)", file_path.display());
                }
                return None;
            }

            // Check if needs adding
            match should_add_file(file_path, &full_path, &tracked, &staged, index, options) {
                Ok(true) => Some(file_path.clone()),
                Ok(false) => None,
                Err(e) => {
                    if options.verbose {
                        eprintln!("Warning: Error checking {}: {}", file_path.display(), e);
                    }
                    None
                }
            }
        })
        .collect();

    Ok(files_to_add)
}
fn should_add_file(
    relative_path: &Path,
    full_path: &Path,
    tracked: &HashSet<PathBuf>,
    staged: &HashSet<PathBuf>,
    index: &HelixIndexData,
    options: &AddOptions,
) -> Result<bool> {
    // If untracked, always add
    if !tracked.contains(relative_path) {
        return Ok(true);
    }

    // If force flag, always re-add
    if options.force {
        return Ok(true);
    }

    // Get existing entry
    let entry = index
        .entries()
        .iter()
        .find(|e| e.path == relative_path)
        .ok_or_else(|| anyhow::anyhow!("Entry not found for {}", relative_path.display()))?;

    // Get current file metadata
    let metadata = fs::metadata(full_path)?;
    let current_mtime = metadata
        .modified()?
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();

    // Get repo_path by going up from full_path
    let repo_path = full_path
        .ancestors()
        .find(|p| p.join(".helix").exists())
        .ok_or_else(|| anyhow::anyhow!("Could not find repo root"))?;

    let store = FsObjectStore::new(repo_path);

    // Check if file is already staged
    if staged.contains(relative_path) {
        // File is staged - check if it's been modified since staging
        if current_mtime != entry.mtime_sec {
            // File modified after staging - need to re-stage
            return Ok(true);
        }

        // Mtime unchanged - verify blob exists

        if !store.has_object(&ObjectType::Blob, &entry.oid) {
            if options.verbose {
                eprintln!(
                    "⚠️  Blob missing for staged file {}, re-creating...",
                    relative_path.display()
                );
            }
            return Ok(true);
        }

        // Already staged, unchanged, and blob exists - skip
        return Ok(false);
    }

    // File is tracked but not staged
    // Check if file has been modified since last commit
    if current_mtime != entry.mtime_sec {
        // File modified - should be added/staged
        return Ok(true);
    }

    // File unchanged since last commit - verify blob exists anyway
    if !store.has_object(&ObjectType::Blob, &entry.oid) {
        if options.verbose {
            eprintln!(
                "⚠️  Blob missing for {}, re-creating...",
                relative_path.display()
            );
        }
        return Ok(true);
    }

    // File is tracked, unchanged, blob exists, but not staged
    // Don't auto-stage unchanged files - skip
    Ok(false)
}

/// Stage files by writing to blob storage and updating index
fn stage_files(
    repo_path: &Path,
    index: &mut HelixIndexData,
    files: &[PathBuf],
    options: &AddOptions,
) -> Result<()> {
    if options.verbose {
        println!("Reading and hashing {} files...", files.len());
    }

    // Read all file contents in parallel
    let file_data: Vec<(PathBuf, Vec<u8>, fs::Metadata)> = files
        .par_iter()
        .map(|path| {
            let full_path = repo_path.join(path);
            let content = fs::read(&full_path)
                .with_context(|| format!("Failed to read {}", path.display()))?;
            let metadata = fs::metadata(&full_path)
                .with_context(|| format!("Failed to get metadata for {}", path.display()))?;
            Ok::<_, anyhow::Error>((path.clone(), content, metadata))
        })
        .collect::<Result<Vec<_>>>()?;

    if options.verbose {
        println!("Writing blobs to storage...");
    }

    // Write all blobs

    let store = FsObjectStore::new(repo_path);

    let contents: Vec<Vec<u8>> = file_data.iter().map(|(_, c, _)| c.clone()).collect();

    let mut hashes = Vec::with_capacity(contents.len());
    for raw in &contents {
        let h = store
            .write_object(&ObjectType::Blob, raw)
            .context("Failed to write blob")?;
        hashes.push(h);
    }

    if options.verbose {
        println!("Updating index entries...");
    }

    // Update index entries
    for (i, (path, _, metadata)) in file_data.iter().enumerate() {
        let hash = hashes[i];

        let entry = Entry {
            path: path.clone(),
            oid: hash,
            flags: EntryFlags::TRACKED | EntryFlags::STAGED,
            size: metadata.len(),
            mtime_sec: metadata
                .modified()?
                .duration_since(std::time::UNIX_EPOCH)?
                .as_secs(),
            mtime_nsec: 0,
            file_mode: get_file_mode(metadata),
            merge_conflict_stage: 0,
            reserved: [0u8; 33],
        };

        // Update or insert entry
        if let Some(existing) = index.entries_mut().iter_mut().find(|e| &e.path == path) {
            *existing = entry;
        } else {
            index.entries_mut().push(entry);
        }
    }

    Ok(())
}

/// Get file mode (Unix permissions)
fn get_file_mode(metadata: &fs::Metadata) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = metadata.permissions().mode();
        if mode & 0o111 != 0 {
            0o100755 // Executable
        } else {
            0o100644 // Regular file
        }
    }

    #[cfg(not(unix))]
    {
        let _ = metadata; // Suppress unused warning
        0o100644 // Default to regular file on Windows
    }
}

/// Expand paths (handle ".", directories, globs) - parallel
fn expand_paths_parallel(repo_path: &Path, paths: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let expanded: Vec<PathBuf> = paths
        .par_iter()
        .flat_map(|path| {
            expand_single_path(repo_path, path).unwrap_or_else(|e| {
                eprintln!("Warning: Error expanding {}: {}", path.display(), e);
                vec![]
            })
        })
        .collect();

    // Remove duplicates
    let mut unique: Vec<PathBuf> = expanded;
    unique.sort();
    unique.dedup();

    Ok(unique)
}

/// Expand a single path
fn expand_single_path(repo_path: &Path, path: &Path) -> Result<Vec<PathBuf>> {
    let full_path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        repo_path.join(path)
    };

    if !full_path.exists() {
        return Ok(vec![]);
    }

    if full_path.is_file() {
        // Single file
        let relative = full_path
            .strip_prefix(repo_path)
            .context("Path is outside repository")?;
        return Ok(vec![relative.to_path_buf()]);
    }

    if full_path.is_dir() {
        // Directory - recursively collect all files (parallel)
        return collect_files_from_directory(&full_path, repo_path);
    }

    Ok(vec![])
}

/// Recursively collect files from a directory
fn collect_files_from_directory(dir: &Path, repo_root: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();

    for entry in WalkDir::new(dir)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            // Skip .git and .helix directories
            !e.path().components().any(|c| {
                let os_str = c.as_os_str();
                os_str == ".git" || os_str == ".helix"
            })
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::helix_index::{EntryFlags, Reader};
    use git2::Object;
    use std::fs;
    use std::process::Command;
    use tempfile::TempDir;

    fn init_test_repo(path: &Path) -> Result<()> {
        // Initialize git repo (for helix init to work)
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

        // Initialize helix
        crate::init_command::init_helix_repo(path, None)?;

        Ok(())
    }

    #[test]
    fn test_add_single_file() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        init_test_repo(repo_path)?;

        // Create a file
        fs::write(repo_path.join("test.txt"), b"hello world")?;

        // Add it
        add(
            repo_path,
            &[PathBuf::from("test.txt")],
            AddOptions {
                verbose: true,
                ..Default::default()
            },
        )?;

        // Verify in helix.idx
        let reader = Reader::new(repo_path);
        let data = reader.read()?;

        assert_eq!(data.entries.len(), 1);
        assert_eq!(data.entries[0].path, PathBuf::from("test.txt"));
        assert!(data.entries[0].flags.contains(EntryFlags::TRACKED));
        assert!(data.entries[0].flags.contains(EntryFlags::STAGED));

        // Verify blob exists
        let storage = FsObjectStore::new(repo_path);
        assert!(storage.has_object(&ObjectType::Blob, &data.entries[0].oid));

        Ok(())
    }

    #[test]
    fn test_add_multiple_files() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        init_test_repo(repo_path)?;

        // Create files
        fs::write(repo_path.join("file1.txt"), b"content 1")?;
        fs::write(repo_path.join("file2.txt"), b"content 2")?;
        fs::write(repo_path.join("file3.txt"), b"content 3")?;

        // Add all
        add(
            repo_path,
            &[
                PathBuf::from("file1.txt"),
                PathBuf::from("file2.txt"),
                PathBuf::from("file3.txt"),
            ],
            AddOptions::default(),
        )?;

        // Verify
        let reader = Reader::new(repo_path);
        let data = reader.read()?;

        assert_eq!(data.entries.len(), 3);

        // All should be staged
        for entry in &data.entries {
            assert!(entry.flags.contains(EntryFlags::STAGED));
        }

        Ok(())
    }

    #[test]
    fn test_add_directory() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        init_test_repo(repo_path)?;

        // Create directory with files
        fs::create_dir_all(repo_path.join("src"))?;
        fs::write(repo_path.join("src/main.rs"), b"fn main() {}")?;
        fs::write(repo_path.join("src/lib.rs"), b"pub fn test() {}")?;

        // Add directory
        add(repo_path, &[PathBuf::from("src")], AddOptions::default())?;

        // Verify both files added
        let reader = Reader::new(repo_path);
        let data = reader.read()?;

        assert_eq!(data.entries.len(), 2);
        assert!(data
            .entries
            .iter()
            .any(|e| e.path == PathBuf::from("src/main.rs")));
        assert!(data
            .entries
            .iter()
            .any(|e| e.path == PathBuf::from("src/lib.rs")));

        Ok(())
    }

    #[test]
    fn test_add_all() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        init_test_repo(repo_path)?;

        // Create multiple files
        fs::write(repo_path.join("file1.txt"), b"one")?;
        fs::write(repo_path.join("file2.txt"), b"two")?;
        fs::create_dir_all(repo_path.join("dir"))?;
        fs::write(repo_path.join("dir/file3.txt"), b"three")?;

        // Add all with "."
        add(repo_path, &[PathBuf::from(".")], AddOptions::default())?;

        // Verify all files added
        let reader = Reader::new(repo_path);
        let data = reader.read()?;

        assert_eq!(data.entries.len(), 3);

        Ok(())
    }

    #[test]
    fn test_add_blob_deduplication() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        init_test_repo(repo_path)?;

        // Create two files with same content
        fs::write(repo_path.join("file1.txt"), b"same content")?;
        fs::write(repo_path.join("file2.txt"), b"same content")?;

        // Add both
        add(
            repo_path,
            &[PathBuf::from("file1.txt"), PathBuf::from("file2.txt")],
            AddOptions::default(),
        )?;

        // Verify entries have same OID
        let reader = Reader::new(repo_path);
        let data = reader.read()?;

        assert_eq!(data.entries.len(), 2);
        assert_eq!(data.entries[0].oid, data.entries[1].oid);

        // Verify only one blob stored
        let storage = FsObjectStore::new(repo_path);

        let all_blobs = storage.list_object_hashes(&ObjectType::Blob)?;
        assert_eq!(all_blobs.len(), 1);

        Ok(())
    }

    #[test]
    fn test_add_idempotent() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        init_test_repo(repo_path)?;

        // Create and add file
        fs::write(repo_path.join("test.txt"), b"content")?;
        add(
            repo_path,
            &[PathBuf::from("test.txt")],
            AddOptions::default(),
        )?;

        let reader = Reader::new(repo_path);
        let data1 = reader.read()?;
        let gen1 = data1.header.generation;

        // Add again (should be no-op)
        add(
            repo_path,
            &[PathBuf::from("test.txt")],
            AddOptions::default(),
        )?;

        let data2 = reader.read()?;

        // Generation should not increase (no changes)
        assert_eq!(data2.header.generation, gen1);

        Ok(())
    }

    #[test]
    fn test_add_modified_file() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        init_test_repo(repo_path)?;

        // Create and add file
        fs::write(repo_path.join("test.txt"), b"version 1")?;
        add(
            repo_path,
            &[PathBuf::from("test.txt")],
            AddOptions::default(),
        )?;

        let reader = Reader::new(repo_path);
        let data1 = reader.read()?;
        let oid1 = data1.entries[0].oid;

        // Wait a bit to ensure different mtime
        std::thread::sleep(std::time::Duration::from_millis(10));

        // Modify file
        fs::write(repo_path.join("test.txt"), b"version 2")?;

        // Add again
        add(
            repo_path,
            &[PathBuf::from("test.txt")],
            AddOptions::default(),
        )?;

        let data2 = reader.read()?;
        let oid2 = data2.entries[0].oid;

        // OID should be different (content changed)
        assert_ne!(oid1, oid2);

        Ok(())
    }

    #[test]
    fn test_add_dry_run() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        init_test_repo(repo_path)?;

        // Create file
        fs::write(repo_path.join("test.txt"), b"content")?;

        // Dry run
        add(
            repo_path,
            &[PathBuf::from("test.txt")],
            AddOptions {
                dry_run: true,
                ..Default::default()
            },
        )?;

        // Verify nothing was actually added
        let reader = Reader::new(repo_path);
        let data = reader.read()?;

        assert_eq!(data.entries.len(), 0);

        Ok(())
    }

    #[test]
    fn test_add_respects_helix_directory() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        init_test_repo(repo_path)?;

        // Create file in .helix directory (should be ignored)
        fs::create_dir_all(repo_path.join(".helix/test"))?;
        fs::write(repo_path.join(".helix/test/file.txt"), b"internal")?;

        // Create normal file
        fs::write(repo_path.join("normal.txt"), b"normal")?;

        // Add all
        add(repo_path, &[PathBuf::from(".")], AddOptions::default())?;

        // Verify .helix file was not added
        let reader = Reader::new(repo_path);
        let data = reader.read()?;

        assert_eq!(data.entries.len(), 1);
        assert_eq!(data.entries[0].path, PathBuf::from("normal.txt"));

        Ok(())
    }

    #[test]
    fn test_add_performance() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        init_test_repo(repo_path)?;

        // Create 100 files
        for i in 0..100 {
            fs::write(
                repo_path.join(format!("file{}.txt", i)),
                format!("content {}", i),
            )?;
        }

        // Time the add operation
        let start = Instant::now();
        add(repo_path, &[PathBuf::from(".")], AddOptions::default())?;
        let elapsed = start.elapsed();

        println!("Added 100 files in {:?}", elapsed);

        // Should be fast (expect <200ms)
        assert!(
            elapsed.as_millis() < 500,
            "Add took {}ms, expected <500ms",
            elapsed.as_millis()
        );

        // Verify all added
        let reader = Reader::new(repo_path);
        let data = reader.read()?;
        assert_eq!(data.entries.len(), 100);

        Ok(())
    }
}
