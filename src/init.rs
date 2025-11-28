use anyhow::{Context, Result};
use std::{fs, path::Path, process::Command, time::Instant};

use crate::helix_index::{self, sync::SyncEngine};

pub fn init_helix_repo(repo_path: &Path) -> Result<()> {
    println!(
        "Initializing Helix repository at {}...",
        repo_path.display()
    );
    println!();

    ensure_git_repo(repo_path)?;
    verify_git_config(repo_path)?;
    create_helix_directory(repo_path)?;
    import_from_git_if_needed(repo_path)?;
    create_repo_config(repo_path)?;
    print_success_message(repo_path)?;

    Ok(())
}

fn ensure_git_repo(repo_path: &Path) -> Result<()> {
    let git_dir = repo_path.join(".git");

    if git_dir.exists() && git_dir.is_dir() {
        println!("✓ Git repository found");
        return Ok(());
    }

    // No .git directory, initialize it
    println!("○ Initializing git repository...");

    let output = Command::new("git")
        .args(&["init"])
        .current_dir(repo_path)
        .output()
        .context("Failed to run 'git init'. Is git installed?")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("git init failed: {}", stderr);
    }

    println!("✓ Git repository initialized");
    Ok(())
}

fn verify_git_config(repo_path: &Path) -> Result<()> {
    // Check user.name
    let name_output = Command::new("git")
        .args(&["config", "user.name"])
        .current_dir(repo_path)
        .output()?;

    // Check user.email
    let email_output = Command::new("git")
        .args(&["config", "user.email"])
        .current_dir(repo_path)
        .output()?;

    let has_name = name_output.status.success() && !name_output.stdout.is_empty();
    let has_email = email_output.status.success() && !email_output.stdout.is_empty();

    if has_name && has_email {
        println!("✓ Git config verified");
        return Ok(());
    }

    // Missing config - try to set defaults
    println!("○ Setting up git config...");

    if !has_name {
        // Try to get system username
        let username = std::env::var("USER")
            .or_else(|_| std::env::var("USERNAME"))
            .unwrap_or_else(|_| "developer".to_string());

        Command::new("git")
            .args(&["config", "user.name", &username])
            .current_dir(repo_path)
            .output()?;

        println!("  ✓ Set user.name = {}", username);
    }

    if !has_email {
        // Use placeholder email
        let email = format!(
            "{}@localhost",
            std::env::var("USER")
                .or_else(|_| std::env::var("USERNAME"))
                .unwrap_or_else(|_| "developer".to_string())
        );

        Command::new("git")
            .args(&["config", "user.email", &email])
            .current_dir(repo_path)
            .output()?;

        println!("  ✓ Set user.email = {}", email);
    }

    println!("✓ Git config set (edit with 'git config --local user.name/email')");

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
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_init_fresh_directory() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        init_helix_repo(repo_path)?;

        // Verify git repo created
        assert!(repo_path.join(".git").exists());

        // Verify helix directory created
        assert!(repo_path.join(".helix").exists());

        // Verify index created
        assert!(repo_path.join(".helix/helix.idx").exists());

        // Verify config created
        assert!(repo_path.join(".helix/config.toml").exists());

        Ok(())
    }

    #[test]
    fn test_init_existing_git_repo() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        // Pre-create git repo
        Command::new("git")
            .args(&["init"])
            .current_dir(repo_path)
            .output()?;

        Command::new("git")
            .args(&["config", "user.name", "Test User"])
            .current_dir(repo_path)
            .output()?;

        Command::new("git")
            .args(&["config", "user.email", "test@test.com"])
            .current_dir(repo_path)
            .output()?;

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
    fn test_init_no_git_index() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        // Initialize git but don't add any files
        Command::new("git")
            .args(&["init"])
            .current_dir(repo_path)
            .output()?;

        Command::new("git")
            .args(&["config", "user.name", "Test User"])
            .current_dir(repo_path)
            .output()?;

        Command::new("git")
            .args(&["config", "user.email", "test@test.com"])
            .current_dir(repo_path)
            .output()?;

        init_helix_repo(repo_path)?;

        // Verify empty helix index created
        let reader = helix_index::Reader::new(repo_path);
        let data = reader.read()?;
        assert_eq!(data.entries.len(), 0);

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
        Command::new("git")
            .args(&["init"])
            .current_dir(repo_path)
            .output()?;

        Command::new("git")
            .args(&["config", "user.name", "Test User"])
            .current_dir(repo_path)
            .output()?;

        Command::new("git")
            .args(&["config", "user.email", "test@test.com"])
            .current_dir(repo_path)
            .output()?;

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
}
