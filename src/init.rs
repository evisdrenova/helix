/*
Creates a new helix index. if there is an existing .git/index in the repo then we read it and create the helix index from it. Otherwise, we create it from scratch.
*/

use anyhow::{Context, Result};
use std::{fs, path::Path, time::Instant};

use crate::helix_index::{self, sync::SyncEngine};

pub fn init_helix_repo(repo_path: &Path) -> Result<()> {
    println!(
        "Initializing Helix repository at {}...",
        repo_path.display()
    );
    println!();

    create_helix_directory(repo_path)?;
    import_from_git_if_needed(repo_path)?;
    create_repo_config(repo_path)?;
    print_success_message(repo_path)?;

    Ok(())
}

fn create_helix_directory(repo_path: &Path) -> Result<()> {
    let helix_dir = repo_path.join(".helix");

    if helix_dir.exists() {
        println!("✓ .helix/ directory exists");
        return Ok(());
    }

    fs::create_dir_all(&helix_dir).context("Failed to create .helix directory")?;

    println!("✓ Created .helix/ directory");
    Ok(())
}

/// Import from Git index if it exists, otherwise create empty Helix index
/// This is a ONE-TIME operation - after this, Helix operates independently
fn import_from_git_if_needed(repo_path: &Path) -> Result<()> {
    let helix_index_path = repo_path.join(".helix/helix.idx");
    let git_index_path = repo_path.join(".git/index");

    // Check if helix.idx already exists
    if helix_index_path.exists() {
        println!("○ helix.idx already exists, rebuilding from Git...");
    } else if git_index_path.exists() {
        println!("○ Importing from .git/index...");
    } else {
        println!("○ Creating empty helix.idx (no .git/index found)...");
    }

    let start = Instant::now();

    // Use SyncEngine to import from Git (one-time operation)
    // This reads .git/index if it exists, otherwise creates empty index
    let sync = SyncEngine::new(repo_path);
    sync.import_from_git()
        .context("Failed to import Git index")?;

    let elapsed = start.elapsed();

    // Read back to get stats
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
        println!("  → Helix is now independent of .git/index");
    } else {
        println!("✓ Created empty helix.idx in {:.0?}", elapsed);
        println!("  → Add files with 'helix add <path>'");
    }

    Ok(())
}

fn create_repo_config(repo_path: &Path) -> Result<()> {
    let helix_dir = repo_path.join(".helix");
    let config_path = helix_dir.join("config.toml");

    if config_path.exists() {
        println!("✓ .helix/config.toml exists");
        return Ok(());
    }

    fs::create_dir_all(&helix_dir).context("Failed to create .helix directory")?;

    let default_config = r#"# Helix repository configuration
#
# This file configures Helix behavior for this repository.
# Settings here override global settings in ~/.helix.toml

# Helix operates independently of Git by default
# The .git/index is only read once during 'helix init'
# After that, Helix maintains its own index at .helix/helix.idx

[core]
# Automatically stage modified files on commit
auto_stage = false

[ignore]
# Additional patterns to ignore (beyond .gitignore)
patterns = [
    "*.log",
    "*.tmp",
    "*.swp",
    ".DS_Store",
]

# Note: Helix does not sync with .git/index
# Use native Helix commands for all operations:
#   helix add     - Stage files
#   helix status  - View status
#   helix commit  - Commit changes
#   helix diff    - View changes
"#;

    fs::write(&config_path, default_config).context("Failed to write .helix/config.toml")?;

    println!("✓ Created .helix/config.toml");

    Ok(())
}

fn print_success_message(repo_path: &Path) -> Result<()> {
    println!();
    println!("🎉 Helix is ready!");
    println!();
    println!("Configuration:");
    println!("  Repo:   {}", repo_path.display());
    println!("  Config: {}/.helix/config.toml", repo_path.display());
    println!("  Index:  {}/.helix/helix.idx", repo_path.display());
    println!();
    println!("Quick start:");
    println!("  helix status           # View working directory status");
    println!("  helix add <files>      # Stage files for commit");
    println!("  helix commit           # Commit staged changes");
    println!("  helix diff             # View changes");
    println!();
    println!("Notes:");
    println!("  • Helix operates independently of Git after initialization");
    println!("  • Use Helix commands (not git commands) for version control");
    println!("  • Edit .helix/config.toml to customize behavior");
    println!("  • Run 'helix init' again to re-import from Git if needed");
    println!();

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::helix_index::format::EntryFlags;
    use std::fs;
    use std::process::Command;
    use tempfile::TempDir;

    fn init_git_repo(path: &Path) -> Result<()> {
        Command::new("git")
            .args(&["init"])
            .current_dir(path)
            .output()
            .context("Failed to run git init")?;

        Command::new("git")
            .args(&["config", "user.name", "Test User"])
            .current_dir(path)
            .output()?;

        Command::new("git")
            .args(&["config", "user.email", "test@test.com"])
            .current_dir(path)
            .output()?;

        Ok(())
    }

    #[test]
    fn test_init_fresh_directory() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        init_helix_repo(repo_path)?;

        // Verify helix directory created
        assert!(repo_path.join(".helix").exists());

        // Verify index created
        assert!(repo_path.join(".helix/helix.idx").exists());

        // Verify config created
        assert!(repo_path.join(".helix/config.toml").exists());

        // Verify empty index
        let reader = helix_index::Reader::new(repo_path);
        let data = reader.read()?;
        assert_eq!(
            data.entries.len(),
            0,
            "Fresh directory should have empty index"
        );
        assert_eq!(
            data.header.generation, 1,
            "First init should be generation 1"
        );

        Ok(())
    }

    #[test]
    fn test_init_existing_git_repo() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        // Pre-create git repo
        init_git_repo(repo_path)?;

        // Add some files
        fs::write(repo_path.join("test.txt"), "hello")?;
        Command::new("git")
            .args(&["add", "test.txt"])
            .current_dir(repo_path)
            .output()?;

        init_helix_repo(repo_path)?;

        // Verify helix index built with imported file
        let reader = helix_index::Reader::new(repo_path);
        let data = reader.read()?;
        assert_eq!(data.entries.len(), 1);
        assert_eq!(data.entries[0].path.to_str().unwrap(), "test.txt");

        Ok(())
    }

    #[test]
    fn test_init_with_multiple_files() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        init_git_repo(repo_path)?;

        // Add multiple files to Git
        fs::write(repo_path.join("file1.txt"), "content 1")?;
        fs::write(repo_path.join("file2.rs"), "fn main() {}")?;
        fs::create_dir_all(repo_path.join("src"))?;
        fs::write(repo_path.join("src/lib.rs"), "pub fn test() {}")?;

        Command::new("git")
            .args(&["add", "."])
            .current_dir(repo_path)
            .output()?;

        init_helix_repo(repo_path)?;

        // Verify all files imported
        let reader = helix_index::Reader::new(repo_path);
        let data = reader.read()?;

        assert_eq!(data.entries.len(), 3, "Should import all 3 files from Git");

        // Verify all files are tracked
        for entry in &data.entries {
            assert!(
                entry.flags.contains(EntryFlags::TRACKED),
                "File {:?} should be tracked",
                entry.path
            );
        }

        // Verify specific files exist
        let paths: Vec<_> = data.entries.iter().map(|e| e.path.as_path()).collect();
        assert!(paths.iter().any(|p| p.ends_with("file1.txt")));
        assert!(paths.iter().any(|p| p.ends_with("file2.rs")));
        assert!(paths.iter().any(|p| p.ends_with("src/lib.rs")));

        Ok(())
    }

    #[test]
    fn test_init_no_git_index() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        // Initialize git but don't add any files
        init_git_repo(repo_path)?;

        init_helix_repo(repo_path)?;

        // Verify empty helix index created
        let reader = helix_index::Reader::new(repo_path);
        let data = reader.read()?;
        assert_eq!(data.entries.len(), 0);

        Ok(())
    }

    #[test]
    fn test_init_detects_staged_files() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        init_git_repo(repo_path)?;

        // Create and commit a file
        fs::write(repo_path.join("committed.txt"), "v1")?;
        Command::new("git")
            .args(&["add", "committed.txt"])
            .current_dir(repo_path)
            .output()?;
        Command::new("git")
            .args(&["commit", "-m", "initial"])
            .current_dir(repo_path)
            .output()?;

        // Modify and stage the file
        fs::write(repo_path.join("committed.txt"), "v2")?;
        Command::new("git")
            .args(&["add", "committed.txt"])
            .current_dir(repo_path)
            .output()?;

        // Add a new file (not committed)
        fs::write(repo_path.join("new.txt"), "new content")?;
        Command::new("git")
            .args(&["add", "new.txt"])
            .current_dir(repo_path)
            .output()?;

        // Initialize Helix
        init_helix_repo(repo_path)?;

        let reader = helix_index::Reader::new(repo_path);
        let data = reader.read()?;

        assert_eq!(data.entries.len(), 2);

        // Modified file should be TRACKED and STAGED
        let committed_entry = data
            .entries
            .iter()
            .find(|e| e.path.ends_with("committed.txt"))
            .expect("committed.txt should be in index");

        assert!(committed_entry.flags.contains(EntryFlags::TRACKED));
        assert!(
            committed_entry.flags.contains(EntryFlags::STAGED),
            "Modified file should be staged"
        );

        // New file should be TRACKED and STAGED
        let new_entry = data
            .entries
            .iter()
            .find(|e| e.path.ends_with("new.txt"))
            .expect("new.txt should be in index");

        assert!(new_entry.flags.contains(EntryFlags::TRACKED));
        assert!(
            new_entry.flags.contains(EntryFlags::STAGED),
            "New file should be staged"
        );

        Ok(())
    }

    #[test]
    fn test_init_with_committed_unstaged_files() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        init_git_repo(repo_path)?;

        // Create, stage, and commit a file
        fs::write(repo_path.join("stable.txt"), "content")?;
        Command::new("git")
            .args(&["add", "stable.txt"])
            .current_dir(repo_path)
            .output()?;
        Command::new("git")
            .args(&["commit", "-m", "add stable file"])
            .current_dir(repo_path)
            .output()?;

        // Initialize Helix
        init_helix_repo(repo_path)?;

        let reader = helix_index::Reader::new(repo_path);
        let data = reader.read()?;

        assert_eq!(data.entries.len(), 1);

        let entry = &data.entries[0];
        assert!(entry.flags.contains(EntryFlags::TRACKED));
        assert!(
            !entry.flags.contains(EntryFlags::STAGED),
            "Committed file that matches HEAD should not be staged"
        );

        Ok(())
    }

    #[test]
    fn test_init_idempotent() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        // Init twice
        init_helix_repo(repo_path)?;

        // Read generation after first init
        let reader = helix_index::Reader::new(repo_path);
        let data1 = reader.read()?;
        let gen1 = data1.header.generation;

        init_helix_repo(repo_path)?;

        // Generation should increment on re-init
        let data2 = reader.read()?;
        let gen2 = data2.header.generation;

        assert_eq!(gen2, gen1 + 1, "Generation should increment on re-init");

        // Should still work
        assert!(repo_path.join(".helix/helix.idx").exists());

        Ok(())
    }

    #[test]
    fn test_init_preserves_git_repo() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        // Create git repo with commits
        init_git_repo(repo_path)?;

        fs::write(repo_path.join("file.txt"), "content")?;
        Command::new("git")
            .args(&["add", "file.txt"])
            .current_dir(repo_path)
            .output()?;

        Command::new("git")
            .args(&["commit", "-m", "initial"])
            .current_dir(repo_path)
            .output()?;

        // Init Helix
        init_helix_repo(repo_path)?;

        // Verify git history preserved
        let log_output = Command::new("git")
            .args(&["log", "--oneline"])
            .current_dir(repo_path)
            .output()?;

        assert!(log_output.status.success());
        assert!(String::from_utf8_lossy(&log_output.stdout).contains("initial"));

        Ok(())
    }

    #[test]
    fn test_init_config_created() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        init_helix_repo(repo_path)?;

        let config_path = repo_path.join(".helix/config.toml");
        assert!(config_path.exists(), "Config file should be created");

        let config_content = fs::read_to_string(config_path)?;
        assert!(config_content.contains("# Helix repository configuration"));
        assert!(config_content.contains("[core]"));
        assert!(config_content.contains("[ignore]"));

        Ok(())
    }

    #[test]
    fn test_init_multiple_times_reimports() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        init_git_repo(repo_path)?;

        // Add a file
        fs::write(repo_path.join("test.txt"), "content")?;
        Command::new("git")
            .args(&["add", "test.txt"])
            .current_dir(repo_path)
            .output()?;

        // First init
        init_helix_repo(repo_path)?;

        let reader = helix_index::Reader::new(repo_path);
        let data1 = reader.read()?;
        assert_eq!(data1.entries.len(), 1);

        // Add another file to Git
        fs::write(repo_path.join("new.txt"), "new")?;
        Command::new("git")
            .args(&["add", "new.txt"])
            .current_dir(repo_path)
            .output()?;

        // Second init should pick up new file
        init_helix_repo(repo_path)?;

        let data2 = reader.read()?;
        assert_eq!(
            data2.entries.len(),
            2,
            "Re-init should pick up new Git files"
        );
        assert!(data2.header.generation > data1.header.generation);

        Ok(())
    }
}
