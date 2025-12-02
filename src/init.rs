/*
Creates a new helix repository with empty directory structure.

We detect if there is a git repo there and ask the user if they want to import their git data.
*/

use anyhow::{Context, Result};
use std::{
    fs,
    io::{stdin, BufRead},
    path::Path,
    time::Instant,
};

use crate::helix_index::{self, sync::SyncEngine, Header, Writer};

pub fn init_helix_repo(repo_path: &Path, auto: Option<String>) -> Result<()> {
    let git_path = repo_path.join(".git");

    if git_path.exists() {
        detect_git(repo_path, auto)?;
    } else {
        println!(
            "Initializing Helix repository at {}...",
            repo_path.display()
        );
    }

    create_directory_structure(repo_path)?;
    create_empty_index(repo_path)?;
    create_head_file(repo_path)?;
    create_repo_config(repo_path)?;
    print_success_message(repo_path)?;

    Ok(())
}

pub fn detect_git(repo_path: &Path, auto: Option<String>) -> Result<()> {
    let stdin = stdin();
    let handle = stdin.lock();
    detect_git_with_reader(repo_path, handle, auto)
}

pub fn detect_git_with_reader<R: BufRead>(
    repo_path: &Path,
    mut reader: R,
    auto: Option<String>,
) -> Result<()> {
    println!("Detected existing Git repo. Do you want to import your Git commits to Helix? (Y/N).");

    if let Some(auto) = auto {
        import_from_git(repo_path)?;
    } else {
        let mut input = String::new();
        reader.read_line(&mut input).expect("Failed to read line");

        let import_git = input.trim().to_lowercase();

        if import_git == "y" {
            import_from_git(repo_path)?;
        }
    }

    Ok(())
}

fn import_from_git(repo_path: &Path) -> Result<()> {
    let start = Instant::now();

    // Use SyncEngine to import from Git (one-time operation)
    // This reads .git/index if it exists, otherwise creates empty index
    let sync = SyncEngine::new(repo_path);
    sync.import_from_git()
        .context("Failed to import Git index")?;

    let elapsed = start.elapsed();

    let reader = helix_index::Reader::new(repo_path);
    let index_data = reader
        .read()
        .context("Failed to read newly created helix index")?;
    let file_count = index_data.entries.len();

    println!("{:#?}", index_data.entries);

    if file_count > 0 {
        println!(
            "✓ Imported {} tracked files from Git in {:.0?}",
            file_count, elapsed
        );
    }

    Ok(())
}

fn create_directory_structure(repo_path: &Path) -> Result<()> {
    let helix_dir = repo_path.join(".helix");

    if !helix_dir.exists() {
        fs::create_dir_all(&helix_dir).context("Failed to create .helix directory")?;
    }

    let objects_dirs = [
        helix_dir.join("objects"),
        helix_dir.join("objects/blobs"),
        helix_dir.join("objects/trees"),
        helix_dir.join("objects/commits"),
    ];

    for dir in &objects_dirs {
        if !dir.exists() {
            fs::create_dir_all(dir)
                .with_context(|| format!("Failed to create {}", dir.display()))?;
        }
    }

    let refs_dirs = [
        helix_dir.join("refs"),
        helix_dir.join("refs/heads"),
        helix_dir.join("refs/tags"),
    ];

    for dir in &refs_dirs {
        if !dir.exists() {
            fs::create_dir_all(dir)
                .with_context(|| format!("Failed to create {}", dir.display()))?;
        }
    }

    Ok(())
}

fn create_empty_index(repo_path: &Path) -> Result<()> {
    let index_path = repo_path.join(".helix/helix.idx");

    if index_path.exists() {
        return Ok(());
    }

    // Create empty index with generation 1
    let writer = Writer::new_canonical(repo_path);
    let empty_header = Header::new(1, 0);

    writer
        .write(&empty_header, &[])
        .context("Failed to create empty index")?;

    Ok(())
}

fn create_head_file(repo_path: &Path) -> Result<()> {
    let head_path = repo_path.join(".helix/HEAD");

    if head_path.exists() {
        println!("○ HEAD already exists, skipping");
        return Ok(());
    }

    // TODO: Create HEAD pointing to main branch
    fs::write(&head_path, "ref: refs/heads/main\n").context("Failed to create HEAD file")?;

    Ok(())
}

fn create_repo_config(repo_path: &Path) -> Result<()> {
    let config_path = repo_path.join(".helix/config.toml");

    if config_path.exists() {
        println!("○ config.toml already exists, skipping");
        return Ok(());
    }

    let default_config = r#"# Helix repository configuration
#
# This file configures Helix behavior for this repository.
# Settings here override global settings in ~/.helix.toml

[user]
# Author information for commits
# Uncomment and set these, or use environment variables:
#   HELIX_AUTHOR_NAME, HELIX_AUTHOR_EMAIL
# name = "Your Name"
# email = "you@example.com"


[ignore]
# Additional patterns to ignore (beyond .gitignore)
patterns = [
    "*.log",
    "*.tmp",
    "*.swp",
    ".DS_Store",
    ".helix/*"
]
"#;

    fs::write(&config_path, default_config).context("Failed to write .helix/config.toml")?;

    Ok(())
}

fn print_success_message(repo_path: &Path) -> Result<()> {
    println!();
    println!(
        "Initialized empty Helix repository at {}",
        repo_path.display()
    );
    println!();
    println!("Next steps:");
    println!("  1. Configure author (edit .helix/config.toml or set env vars)");
    println!("  2. Add files:    helix add <files>");
    println!("  3. Make commit:  helix commit -m \"Initial commit\"");
    println!();
    println!("View status:       helix status");
    println!("View history:      helix log");
    println!();

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::helix_index::{EntryFlags, Reader};
    use std::{
        collections::HashMap, os::unix::fs::PermissionsExt, path::PathBuf, process::Command,
    };
    use tempfile::TempDir;

    #[test]
    fn test_init_creates_directory_structure() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        init_helix_repo(repo_path, None)?;

        // Verify main directory
        assert!(repo_path.join(".helix").exists());

        // Verify objects directories
        assert!(repo_path.join(".helix/objects").exists());
        assert!(repo_path.join(".helix/objects/blobs").exists());
        assert!(repo_path.join(".helix/objects/trees").exists());
        assert!(repo_path.join(".helix/objects/commits").exists());

        // Verify refs directories
        assert!(repo_path.join(".helix/refs").exists());
        assert!(repo_path.join(".helix/refs/heads").exists());
        assert!(repo_path.join(".helix/refs/tags").exists());

        Ok(())
    }

    #[test]
    fn test_init_creates_empty_index() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        init_helix_repo(repo_path, None)?;

        // Verify index exists
        assert!(repo_path.join(".helix/helix.idx").exists());

        // Verify it's empty
        let reader = Reader::new(repo_path);
        let data = reader.read()?;

        assert_eq!(data.entries.len(), 0, "Index should be empty");
        assert_eq!(data.header.generation, 1, "Should be generation 1");

        Ok(())
    }

    #[test]
    fn test_init_creates_head_file() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        init_helix_repo(repo_path, None)?;

        let head_path = repo_path.join(".helix/HEAD");
        assert!(head_path.exists(), "HEAD file should exist");

        let content = fs::read_to_string(head_path)?;
        assert_eq!(
            content.trim(),
            "ref: refs/heads/main",
            "HEAD should point to main branch"
        );

        Ok(())
    }

    #[test]
    fn test_init_creates_config() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        init_helix_repo(repo_path, None)?;

        let config_path = repo_path.join(".helix/config.toml");
        assert!(config_path.exists(), "Config should exist");

        let content = fs::read_to_string(config_path)?;
        assert!(content.contains("# Helix repository configuration"));
        assert!(content.contains("[user]"));
        assert!(content.contains("[ignore]"));

        Ok(())
    }

    #[test]
    fn test_init_idempotent() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        // Init twice
        init_helix_repo(repo_path, None)?;
        init_helix_repo(repo_path, None)?;

        // Should still work
        assert!(repo_path.join(".helix/helix.idx").exists());
        assert!(repo_path.join(".helix/HEAD").exists());
        assert!(repo_path.join(".helix/config.toml").exists());

        // Index should still be valid
        let reader = Reader::new(repo_path);
        let data = reader.read()?;
        assert_eq!(data.entries.len(), 0);

        Ok(())
    }

    #[test]
    fn test_init_in_existing_git_repo_one_file() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        Command::new("git")
            .args(["init"])
            .current_dir(repo_path)
            .output()?;
        Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(repo_path)
            .output()?;
        Command::new("git")
            .args(["config", "user.name", "Test User"])
            .current_dir(repo_path)
            .output()?;
        fs::write(repo_path.join("test.txt"), "content")?;
        Command::new("git")
            .args(["add", "."])
            .current_dir(repo_path)
            .output()?;
        Command::new("git")
            .args(["commit", "-m", "initial"])
            .current_dir(repo_path)
            .output()?;

        let yes = "Y".to_string();
        init_helix_repo(repo_path, Some(yes))?;

        // Verify Helix index has one entry (test.txt)
        let reader = Reader::new(repo_path);
        let data = reader.read()?;

        assert_eq!(
            data.entries.len(),
            1,
            "Should have imported one entry from git"
        );
        assert_eq!(data.entries[0].path, std::path::PathBuf::from("test.txt"));

        Ok(())
    }

    #[test]
    fn test_detect_git_user_says_no() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        // Real git repo with a committed file
        Command::new("git")
            .args(["init"])
            .current_dir(repo_path)
            .output()?;
        Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(repo_path)
            .output()?;
        Command::new("git")
            .args(["config", "user.name", "Test User"])
            .current_dir(repo_path)
            .output()?;
        fs::write(repo_path.join("test.txt"), "content")?;
        Command::new("git")
            .args(["add", "."])
            .current_dir(repo_path)
            .output()?;
        Command::new("git")
            .args(["commit", "-m", "initial"])
            .current_dir(repo_path)
            .output()?;

        // Simulate user typing "n\n"
        let input = b"n\n";
        let reader = std::io::Cursor::new(input.as_slice());

        // Only call detect_git + init helpers, not init_helix_repo directly
        detect_git_with_reader(repo_path, reader, None)?;
        create_directory_structure(repo_path)?;
        create_empty_index(repo_path)?;
        create_head_file(repo_path)?;
        create_repo_config(repo_path)?;

        let reader = Reader::new(repo_path);
        let data = reader.read()?;
        assert_eq!(
            data.entries.len(),
            0,
            "Should not import from git when user says no"
        );

        Ok(())
    }

    #[test]
    fn test_init_in_existing_git_repo_migrate_multi_file() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        Command::new("git")
            .args(["init"])
            .current_dir(repo_path)
            .output()?;
        Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(repo_path)
            .output()?;
        Command::new("git")
            .args(["config", "user.name", "Test User"])
            .current_dir(repo_path)
            .output()?;
        fs::write(repo_path.join("main.rs"), "main content")?;
        // set permissions on main.rs file to be executable
        let mut perms = fs::metadata(repo_path.join("main.rs"))?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(repo_path.join("main.rs"), perms)?;
        fs::write(repo_path.join("lib.rs"), "lib content")?;
        Command::new("git")
            .args(["add", "."])
            .current_dir(repo_path)
            .output()?;
        Command::new("git")
            .args(["commit", "-m", "initial"])
            .current_dir(repo_path)
            .output()?;

        let yes = "Y".to_string();
        init_helix_repo(repo_path, Some(yes))?;

        // Verify Helix index has two entries (main.rs and lib.rs)
        let reader = Reader::new(repo_path);
        let data = reader.read()?;

        assert_eq!(
            data.entries.len(),
            2,
            "Should have imported two entries from git"
        );

        let main_entry = data
            .entries
            .iter()
            .find(|e| e.path == PathBuf::from("main.rs"))
            .expect("main.rs entry not found");
        let lib_entry = data
            .entries
            .iter()
            .find(|e| e.path == PathBuf::from("lib.rs"))
            .expect("lib.rs entry not found");

        assert_eq!(main_entry.file_mode, 0o100755);
        assert_eq!(lib_entry.file_mode, 0o100644);

        Ok(())
    }

    #[test]
    fn test_import_from_empty_git_repo_creates_empty_index() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        // git repo but no commits, no .git/index
        Command::new("git")
            .args(["init"])
            .current_dir(repo_path)
            .output()?;

        init_helix_repo(repo_path, Some("y".to_string()))?;

        let reader = Reader::new(repo_path);
        let data = reader.read()?;

        assert_eq!(
            data.entries.len(),
            0,
            "Empty git repo should produce empty index"
        );

        Ok(())
    }

    #[test]
    fn test_init_preserves_existing_files() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        // Create some files first
        fs::create_dir_all(repo_path.join("src"))?;
        fs::write(repo_path.join("README.md"), "# My Project")?;
        fs::write(repo_path.join("src/main.rs"), "fn main() {}")?;

        // Init Helix
        init_helix_repo(repo_path, None)?;

        // Verify files still exist
        assert!(repo_path.join("README.md").exists());
        assert!(repo_path.join("src/main.rs").exists());

        // Verify they're not tracked
        let reader = Reader::new(repo_path);
        let data = reader.read()?;
        assert_eq!(data.entries.len(), 0, "Files should not be auto-tracked");

        Ok(())
    }

    #[test]
    fn test_init_multiple_times_safe() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        // Init first time
        init_helix_repo(repo_path, None)?;

        // Manually modify index
        let reader = Reader::new(repo_path);
        let data1 = reader.read()?;
        let gen1 = data1.header.generation;

        // Init again - should skip existing files
        init_helix_repo(repo_path, None)?;

        // Generation should remain the same (index not overwritten)
        let data2 = reader.read()?;
        assert_eq!(
            data2.header.generation, gen1,
            "Re-init should not overwrite index"
        );

        Ok(())
    }

    #[test]
    fn test_all_directories_writable() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        init_helix_repo(repo_path, None)?;

        // Try writing to each objects directory
        fs::write(repo_path.join(".helix/objects/blobs/test"), "blob")?;
        fs::write(repo_path.join(".helix/objects/trees/test"), "tree")?;
        fs::write(repo_path.join(".helix/objects/commits/test"), "commit")?;

        // Try writing to refs directories
        fs::write(repo_path.join(".helix/refs/heads/test"), "abc123")?;
        fs::write(repo_path.join(".helix/refs/tags/v1.0"), "def456")?;

        // Verify all written
        assert!(repo_path.join(".helix/objects/blobs/test").exists());
        assert!(repo_path.join(".helix/objects/trees/test").exists());
        assert!(repo_path.join(".helix/objects/commits/test").exists());
        assert!(repo_path.join(".helix/refs/heads/test").exists());
        assert!(repo_path.join(".helix/refs/tags/v1.0").exists());

        Ok(())
    }

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
        crate::init::init_helix_repo(path, None)?;

        Ok(())
    }

    #[test]
    fn test_import_sets_flags_for_multiple_entries() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        init_test_repo(repo_path)?;

        // 1. committed.txt: committed and unchanged (index == HEAD)
        let committed = repo_path.join("committed.txt");
        fs::write(&committed, "v1")?;
        Command::new("git")
            .args(&["add", "committed.txt"])
            .current_dir(repo_path)
            .output()?;
        Command::new("git")
            .args(&["commit", "-m", "initial commit"])
            .current_dir(repo_path)
            .output()?;

        // 2. staged_new.txt: new file, staged but never committed (not in HEAD)
        let staged_new = repo_path.join("staged_new.txt");
        fs::write(&staged_new, "new staged content")?;
        Command::new("git")
            .args(&["add", "staged_new.txt"])
            .current_dir(repo_path)
            .output()?;

        // 3. staged_modified.txt: committed once, then modified and re-staged (index != HEAD)
        let staged_modified = repo_path.join("staged_modified.txt");
        fs::write(&staged_modified, "original")?;
        Command::new("git")
            .args(&["add", "staged_modified.txt"])
            .current_dir(repo_path)
            .output()?;
        Command::new("git")
            .args(&["commit", "-m", "add staged_modified"])
            .current_dir(repo_path)
            .output()?;

        // modify and stage again (HEAD still has "original")
        fs::write(&staged_modified, "modified")?;
        Command::new("git")
            .args(&["add", "staged_modified.txt"])
            .current_dir(repo_path)
            .output()?;

        // Import into Helix
        let syncer = SyncEngine::new(repo_path);
        syncer.import_from_git()?;

        let reader = Reader::new(repo_path);
        let data = reader.read()?;

        // Build a map: PathBuf -> flags for easy lookup
        let by_path: HashMap<PathBuf, EntryFlags> = data
            .entries
            .iter()
            .map(|e| (e.path.clone(), e.flags))
            .collect();

        // committed.txt: tracked, NOT staged (index == HEAD)
        let committed_flags = by_path
            .get(&PathBuf::from("committed.txt"))
            .expect("committed.txt not found in helix index");
        assert!(committed_flags.contains(EntryFlags::TRACKED));
        assert!(
            !committed_flags.contains(EntryFlags::STAGED),
            "committed.txt should not be marked STAGED"
        );

        // staged_new.txt: tracked + staged (not in HEAD)
        let staged_new_flags = by_path
            .get(&PathBuf::from("staged_new.txt"))
            .expect("staged_new.txt not found in helix index");
        assert!(staged_new_flags.contains(EntryFlags::TRACKED));
        assert!(
            staged_new_flags.contains(EntryFlags::STAGED),
            "staged_new.txt should be marked STAGED (new file not in HEAD)"
        );

        // staged_modified.txt: tracked + staged (index != HEAD)
        let staged_modified_flags = by_path
            .get(&PathBuf::from("staged_modified.txt"))
            .expect("staged_modified.txt not found in helix index");
        assert!(staged_modified_flags.contains(EntryFlags::TRACKED));
        assert!(
            staged_modified_flags.contains(EntryFlags::STAGED),
            "staged_modified.txt should be marked STAGED (differs from HEAD)"
        );

        Ok(())
    }

    #[test]
    fn test_import_comprehensive_flags() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();
        init_test_repo(repo_path)?;

        // ═══════════════════════════════════════════════════════════
        // SCENARIO 1: Committed file, unchanged
        // Expected: TRACKED only (clean state)
        // ═══════════════════════════════════════════════════════════
        fs::write(repo_path.join("clean.txt"), "v1")?;
        Command::new("git")
            .args(&["add", "clean.txt"])
            .current_dir(repo_path)
            .output()?;
        Command::new("git")
            .args(&["commit", "-m", "add clean.txt"])
            .current_dir(repo_path)
            .output()?;

        // ═══════════════════════════════════════════════════════════
        // SCENARIO 2: New file, staged
        // Expected: TRACKED | STAGED
        // ═══════════════════════════════════════════════════════════
        fs::write(repo_path.join("staged_new.txt"), "new content")?;
        Command::new("git")
            .args(&["add", "staged_new.txt"])
            .current_dir(repo_path)
            .output()?;

        // ═══════════════════════════════════════════════════════════
        // SCENARIO 3: Committed file, modified and staged
        // Expected: TRACKED | STAGED
        // ═══════════════════════════════════════════════════════════
        fs::write(repo_path.join("staged_modified.txt"), "original")?;
        Command::new("git")
            .args(&["add", "staged_modified.txt"])
            .current_dir(repo_path)
            .output()?;
        Command::new("git")
            .args(&["commit", "-m", "add staged_modified"])
            .current_dir(repo_path)
            .output()?;
        fs::write(repo_path.join("staged_modified.txt"), "changed")?;
        Command::new("git")
            .args(&["add", "staged_modified.txt"])
            .current_dir(repo_path)
            .output()?;

        // ═══════════════════════════════════════════════════════════
        // SCENARIO 4: Committed file, modified but NOT staged
        // Expected: TRACKED | MODIFIED
        // ═══════════════════════════════════════════════════════════
        fs::write(repo_path.join("unstaged_modified.txt"), "original")?;
        Command::new("git")
            .args(&["add", "unstaged_modified.txt"])
            .current_dir(repo_path)
            .output()?;
        Command::new("git")
            .args(&["commit", "-m", "add unstaged_modified"])
            .current_dir(repo_path)
            .output()?;
        // Modify WITHOUT staging
        fs::write(
            repo_path.join("unstaged_modified.txt"),
            "modified but not staged",
        )?;

        // ═══════════════════════════════════════════════════════════
        // SCENARIO 5: Partially staged (staged + modified)
        // Expected: TRACKED | STAGED | MODIFIED
        // ═══════════════════════════════════════════════════════════
        fs::write(repo_path.join("partially_staged.txt"), "v1")?;
        Command::new("git")
            .args(&["add", "partially_staged.txt"])
            .current_dir(repo_path)
            .output()?;
        Command::new("git")
            .args(&["commit", "-m", "add partially_staged"])
            .current_dir(repo_path)
            .output()?;
        // First change: stage it
        fs::write(repo_path.join("partially_staged.txt"), "v2")?;
        Command::new("git")
            .args(&["add", "partially_staged.txt"])
            .current_dir(repo_path)
            .output()?;
        // Second change: don't stage
        fs::write(repo_path.join("partially_staged.txt"), "v3")?;

        // ═══════════════════════════════════════════════════════════
        // Import into Helix
        // ═══════════════════════════════════════════════════════════
        let syncer = SyncEngine::new(repo_path);
        syncer.import_from_git()?;

        let reader = Reader::new(repo_path);
        let data = reader.read()?;

        let by_path: HashMap<PathBuf, EntryFlags> = data
            .entries
            .iter()
            .map(|e| (e.path.clone(), e.flags))
            .collect();

        // ═══════════════════════════════════════════════════════════
        // Assertions
        // ═══════════════════════════════════════════════════════════

        // 1. clean.txt: TRACKED only
        let clean = by_path.get(&PathBuf::from("clean.txt")).unwrap();
        assert!(
            clean.contains(EntryFlags::TRACKED),
            "clean.txt should be TRACKED"
        );
        assert!(
            !clean.contains(EntryFlags::STAGED),
            "clean.txt should NOT be STAGED"
        );
        assert!(
            !clean.contains(EntryFlags::MODIFIED),
            "clean.txt should NOT be MODIFIED"
        );

        // 2. staged_new.txt: TRACKED | STAGED
        let staged_new = by_path.get(&PathBuf::from("staged_new.txt")).unwrap();
        assert!(
            staged_new.contains(EntryFlags::TRACKED),
            "staged_new.txt should be TRACKED"
        );
        assert!(
            staged_new.contains(EntryFlags::STAGED),
            "staged_new.txt should be STAGED (new file)"
        );
        assert!(
            !staged_new.contains(EntryFlags::MODIFIED),
            "staged_new.txt should NOT be MODIFIED"
        );

        // 3. staged_modified.txt: TRACKED | STAGED
        let staged_mod = by_path.get(&PathBuf::from("staged_modified.txt")).unwrap();
        assert!(
            staged_mod.contains(EntryFlags::TRACKED),
            "staged_modified.txt should be TRACKED"
        );
        assert!(
            staged_mod.contains(EntryFlags::STAGED),
            "staged_modified.txt should be STAGED"
        );
        assert!(
            !staged_mod.contains(EntryFlags::MODIFIED),
            "staged_modified.txt should NOT be MODIFIED (changes are staged)"
        );

        // 4. unstaged_modified.txt: TRACKED | MODIFIED
        let unstaged_mod = by_path
            .get(&PathBuf::from("unstaged_modified.txt"))
            .unwrap();
        assert!(
            unstaged_mod.contains(EntryFlags::TRACKED),
            "unstaged_modified.txt should be TRACKED"
        );
        assert!(
            !unstaged_mod.contains(EntryFlags::STAGED),
            "unstaged_modified.txt should NOT be STAGED"
        );
        assert!(
            unstaged_mod.contains(EntryFlags::MODIFIED),
            "unstaged_modified.txt should be MODIFIED"
        );

        // 5. partially_staged.txt: TRACKED | STAGED | MODIFIED
        let partial = by_path.get(&PathBuf::from("partially_staged.txt")).unwrap();
        assert!(
            partial.contains(EntryFlags::TRACKED),
            "partially_staged.txt should be TRACKED"
        );
        assert!(
            partial.contains(EntryFlags::STAGED),
            "partially_staged.txt should be STAGED"
        );
        assert!(
            partial.contains(EntryFlags::MODIFIED),
            "partially_staged.txt should be MODIFIED"
        );

        Ok(())
    }

    #[test]
    fn test_import_deleted_file() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();
        init_test_repo(repo_path)?;

        // Commit a file
        fs::write(repo_path.join("will_delete.txt"), "content")?;
        Command::new("git")
            .args(&["add", "will_delete.txt"])
            .current_dir(repo_path)
            .output()?;
        Command::new("git")
            .args(&["commit", "-m", "add file"])
            .current_dir(repo_path)
            .output()?;

        // Delete it from working tree (but not staged)
        fs::remove_file(repo_path.join("will_delete.txt"))?;

        // Import
        let syncer = SyncEngine::new(repo_path);
        syncer.import_from_git()?;

        let reader = Reader::new(repo_path);
        let data = reader.read()?;

        let entry = data
            .entries
            .iter()
            .find(|e| e.path == PathBuf::from("will_delete.txt"))
            .expect("Deleted file should still be in index");

        assert!(entry.flags.contains(EntryFlags::TRACKED));
        assert!(
            entry.flags.contains(EntryFlags::DELETED),
            "Should be marked DELETED"
        );
        assert!(!entry.flags.contains(EntryFlags::STAGED));

        Ok(())
    }

    #[test]
    fn test_import_filters_helix_files() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();
        init_test_repo(repo_path)?;

        // Create and stage a normal file
        fs::write(repo_path.join("normal.txt"), "content")?;
        Command::new("git")
            .args(&["add", "normal.txt"])
            .current_dir(repo_path)
            .output()?;

        // Create .helix directory with files
        fs::create_dir_all(repo_path.join(".helix"))?;
        fs::write(repo_path.join(".helix/HEAD"), "ref: refs/heads/main")?;
        fs::write(repo_path.join(".helix/config.toml"), "# config")?;

        // Stage .helix files (shouldn't happen, but let's test it)
        Command::new("git")
            .args(&["add", ".helix/"])
            .current_dir(repo_path)
            .output()
            .ok(); // Might fail if gitignore present

        // Import
        let syncer = SyncEngine::new(repo_path);
        syncer.import_from_git()?;

        let reader = Reader::new(repo_path);
        let data = reader.read()?;

        // Should only have normal.txt, not .helix files
        assert_eq!(
            data.entries.len(),
            1,
            "Should only have 1 file (not .helix files)"
        );
        assert_eq!(data.entries[0].path, PathBuf::from("normal.txt"));

        // Verify .helix files are NOT in index
        for entry in &data.entries {
            assert!(
                !entry.path.starts_with(".helix/"),
                "Should not import .helix/ files: {:?}",
                entry.path
            );
        }

        Ok(())
    }

    #[test]
    fn test_import_correct_mtimes() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();
        init_test_repo(repo_path)?;

        // Create and stage a file
        fs::write(repo_path.join("test.txt"), "content")?;
        Command::new("git")
            .args(&["add", "test.txt"])
            .current_dir(repo_path)
            .output()?;

        // Get actual file mtime
        let metadata = fs::metadata(repo_path.join("test.txt"))?;
        let expected_mtime = metadata
            .modified()?
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs();

        // Import
        let syncer = SyncEngine::new(repo_path);
        syncer.import_from_git()?;

        let reader = Reader::new(repo_path);
        let data = reader.read()?;

        let entry = &data.entries[0];

        // Verify mtime is reasonable (Unix timestamp ~1.7 billion for 2024)
        assert!(
            entry.mtime_sec > 1_600_000_000,
            "mtime should be valid Unix timestamp, got: {}",
            entry.mtime_sec
        );
        assert!(
            entry.mtime_sec < 2_000_000_000,
            "mtime should be valid Unix timestamp, got: {}",
            entry.mtime_sec
        );

        // Should match actual file mtime (within a few seconds)
        let diff = (entry.mtime_sec as i64 - expected_mtime as i64).abs();
        assert!(
            diff < 5,
            "mtime should match file metadata, expected ~{}, got {}",
            expected_mtime,
            entry.mtime_sec
        );

        Ok(())
    }

    #[test]
    fn test_import_no_head_all_staged() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();
        init_test_repo(repo_path)?;

        // Stage files WITHOUT committing (no HEAD)
        fs::write(repo_path.join("file1.txt"), "content1")?;
        fs::write(repo_path.join("file2.txt"), "content2")?;
        Command::new("git")
            .args(&["add", "."])
            .current_dir(repo_path)
            .output()?;

        // Import
        let syncer = SyncEngine::new(repo_path);
        syncer.import_from_git()?;

        let reader = Reader::new(repo_path);
        let data = reader.read()?;

        // All files should be TRACKED | STAGED (no HEAD means everything is new)
        for entry in &data.entries {
            assert!(entry.flags.contains(EntryFlags::TRACKED));
            assert!(
                entry.flags.contains(EntryFlags::STAGED),
                "File {:?} should be STAGED (no HEAD)",
                entry.path
            );
        }

        Ok(())
    }
}
