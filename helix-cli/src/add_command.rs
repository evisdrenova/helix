// Add command - Stage files using pure Helix storage

use crate::helix_index::api::HelixIndexData;
use crate::helix_index::format::{Entry, EntryFlags};
use crate::ignore::IgnoreRules;
use crate::sandbox_command::RepoContext;
use anyhow::{Context, Result};
use helix_protocol::message::ObjectType;
use helix_protocol::storage::FsObjectStore;
use rayon::prelude::*;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
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
    if paths.is_empty() {
        anyhow::bail!("No paths specified. Use 'helix add <files>' or 'helix add .'");
    }

    let context = RepoContext::detect(repo_path)?;

    // Use context's index path for loading
    let mut index = HelixIndexData::load_from_path(&context.index_path, &context.repo_root)?;

    if options.verbose {
        if context.is_sandbox() {
            println!(
                "Working in sandbox: {}",
                context.sandbox_name().unwrap_or_default()
            );
        }
        println!("Loaded index (generation {})", index.generation());
    }

    println!("DEBUG: Index has {} entries", index.entries().len());
    for entry in index.entries().iter() {
        let full_path = context.workdir.join(&entry.path);
        let disk_mtime = full_path
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);

        let modified = disk_mtime != entry.mtime_sec;
        println!(
            "  {} | index_mtime={} | disk_mtime={} | modified={}",
            entry.path.display(),
            entry.mtime_sec,
            disk_mtime,
            modified
        );
    }
    if options.verbose {
        println!("Loaded index (generation {})", index.generation());
    }

    // Resolve which files need adding (parallel)
    let files_to_add = resolve_files_to_add(&index, paths, &options, &context)?;

    println!("DEBUG: files_to_add = {:?}", files_to_add);

    if files_to_add.is_empty() {
        // Check if there are already staged files
        let staged_count = index.get_staged().len();

        println!("staged count {}", staged_count);

        if staged_count > 0 {
            if options.verbose {
                println!(
                    "No new files to add ({} files already staged)",
                    staged_count
                );
            } else {
                println!(
                    "No new files to add ({} already staged, ready to commit)",
                    staged_count
                );
            }
        } else {
            if options.verbose {
                println!("No files to add (everything up-to-date)");
            } else {
                println!("No files to add");
            }
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
    stage_files(&mut index, &files_to_add, &options, &context)?;

    // Persist index to disk
    index.persist()?;

    println!("DEBUG: persisted index to {}", context.index_path.display());

    if options.verbose {
        println!("Staged {} files", files_to_add.len());
        println!("Index generation: {}", index.generation());
    }

    Ok(())
}

fn resolve_files_to_add(
    index: &HelixIndexData,
    paths: &[PathBuf],
    options: &AddOptions,
    context: &RepoContext,
) -> Result<Vec<PathBuf>> {
    let tracked = index.get_tracked();
    let staged = index.get_staged();

    // Load ignore rules
    let ignore_rules = IgnoreRules::load(&context.workdir);

    if options.verbose {
        println!("Currently tracked: {}", tracked.len());
        println!("Currently staged: {}", staged.len());
    }

    // Expand paths (handle ".", directories, globs) - parallel
    let candidate_files = expand_paths_parallel(&context.workdir, paths)?;

    println!(
        "DEBUG: expand_paths found {} candidates:",
        candidate_files.len()
    );
    for f in &candidate_files {
        println!("  candidate: {}", f.display());
    }

    if options.verbose {
        println!("Found {} candidate files", candidate_files.len());
    }

    // Filter to files that actually need adding - parallel
    let files_to_add: Vec<PathBuf> = candidate_files
        .iter()
        .filter_map(|file_path| {
            let full_path = context.workdir.join(file_path);

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
            match should_add_file(
                file_path, &full_path, &tracked, &staged, index, options, context,
            ) {
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
    context: &RepoContext,
) -> Result<bool> {
    println!("should add?? {}", relative_path.display());

    // If untracked, always add
    if !tracked.contains(relative_path) {
        println!("  -> untracked, adding");
        return Ok(true);
    }

    if options.force {
        return Ok(true);
    }

    let entry = index
        .entries()
        .iter()
        .find(|e| e.path == relative_path)
        .ok_or_else(|| anyhow::anyhow!("Entry not found for {}", relative_path.display()))?;

    let metadata = fs::metadata(full_path)?;
    let current_mtime = metadata
        .modified()?
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();

    println!(
        "  entry.mtime={}, current_mtime={}",
        entry.mtime_sec, current_mtime
    );

    let store = FsObjectStore::new(&context.repo_root);

    println!("  staged.contains={}", staged.contains(relative_path));

    if staged.contains(relative_path) {
        if current_mtime != entry.mtime_sec {
            println!("  -> staged but modified, re-staging");
            return Ok(true);
        }

        if !store.has_object(&ObjectType::Blob, &entry.oid) {
            println!("  -> blob missing, re-staging");
            return Ok(true);
        }

        println!("  -> staged and unchanged, skipping");
        return Ok(false);
    }

    println!(
        "  checking mtime: {} != {} = {}",
        current_mtime,
        entry.mtime_sec,
        current_mtime != entry.mtime_sec
    );

    if current_mtime != entry.mtime_sec {
        println!("  -> modified (mtime differs), adding");
        return Ok(true);
    }

    if !store.has_object(&ObjectType::Blob, &entry.oid) {
        println!("  -> blob missing, adding");
        return Ok(true);
    }

    println!("  -> unchanged, skipping");
    Ok(false)
}

/// Stage files by writing to blob storage and updating index
fn stage_files(
    index: &mut HelixIndexData,
    files: &[PathBuf],
    options: &AddOptions,
    context: &RepoContext,
) -> Result<()> {
    if options.verbose {
        println!("Reading and hashing {} files...", files.len());
    }

    // Read all file contents
    let file_data: Vec<(PathBuf, Vec<u8>, fs::Metadata)> = files
        .iter()
        .map(|path| {
            let full_path = context.workdir.join(path);
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

    // Write blobs to MAIN REPO's object store (not sandbox)
    let store = FsObjectStore::new(&context.repo_root);

    let mut hashes = Vec::with_capacity(file_data.len());
    for (_, content, _) in &file_data {
        let h = store
            .write_object(&ObjectType::Blob, content)
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

        println!(
            "DEBUG: creating entry for {} with flags {:?}",
            path.display(),
            entry.flags
        );

        println!(
            "DEBUG stage_files: index now has {} entries",
            index.entries().len()
        );
        for entry in index.entries() {
            if entry.flags.contains(EntryFlags::STAGED) {
                println!("  STAGED: {}", entry.path.display());
            }
        }

        // Update or insert entry
        if let Some(existing) = index.entries_mut().iter_mut().find(|e| &e.path == path) {
            println!("DEBUG: updating existing entry for {}", path.display());
            *existing = entry;
        } else {
            println!("DEBUG: inserting new entry for {}", path.display());
            index.entries_mut().push(entry);
        }
    }

    println!(
        "DEBUG stage_files: index now has {} entries",
        index.entries().len()
    );
    for entry in index.entries() {
        println!(
            "  {} -> flags: {:?}, STAGED={}",
            entry.path.display(),
            entry.flags,
            entry.flags.contains(EntryFlags::STAGED)
        );
    }

    Ok(())
}

/// Get file mode (Unix permissions)
pub fn get_file_mode(metadata: &fs::Metadata) -> u32 {
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
            // Only skip .git and .helix if they're direct children of the directory we're walking
            // Not if they appear in the parent path
            let name = e.file_name().to_string_lossy();
            name != ".git" && name != ".helix"
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
    use std::fs;
    use std::process::Command;
    use std::time::Instant;
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
