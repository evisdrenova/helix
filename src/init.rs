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
    println!("Detected existing git repo, import your git commits to Helix? (Y/N).");

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

    if file_count > 0 {
        println!(
            "✓ Imported {} tracked files from Git in {:.0?}",
            file_count, elapsed
        );
    } else {
        println!("✓ Created empty helix.idx in {:.0?}", elapsed);
        println!("  → Add files with 'helix add <path>'");
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
]
"#;

    fs::write(&config_path, default_config).context("Failed to write .helix/config.toml")?;

    Ok(())
}

fn print_success_message(repo_path: &Path) -> Result<()> {
    println!();
    println!(
        "Initialized empty Helix repository at {} !",
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
    use crate::helix_index::Reader;
    use std::{os::unix::fs::PermissionsExt, path::PathBuf, process::Command};
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
}
