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
    build_initial_index(repo_path)?;
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

fn build_initial_index(repo_path: &Path) -> Result<()> {
    let index_path = repo_path.join(".helix/helix.idx");

    if index_path.exists() {
        println!("○ helix.idx already exists, rebuilding...");
    } else {
        println!("○ Building helix.idx...");
    }

    let start = Instant::now();

    // Create sync engine and do full sync
    let sync = SyncEngine::new(repo_path);
    sync.full_sync().context("Failed to build helix index")?;

    let elapsed = start.elapsed();

    // Read back to get stats
    let reader = helix_index::Reader::new(repo_path);
    let index_data = reader.read()?;
    let file_count = index_data.entries.len();

    println!(
        "✓ Built helix.idx ({} tracked files in {:.0?})",
        file_count, elapsed
    );

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
    println!("Next steps:");
    println!("  helix status           # View working directory status");
    println!("  helix log              # View git history");
    println!("  helix commit <files>   # Add, generate message, and commit");
    println!();
    println!("Tips:");
    println!("  • Edit .helix/config.toml to customize settings");
    println!("  • Run 'helix init' again to rebuild the index");
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

        // Verify helix index built
        let reader = helix_index::Reader::new(repo_path);
        let data = reader.read()?;
        assert_eq!(data.entries.len(), 1);

        Ok(())
    }

    #[test]
    fn test_init_idempotent() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        // Init twice
        init_helix_repo(repo_path)?;
        init_helix_repo(repo_path)?;

        // Should still work
        assert!(repo_path.join(".helix/helix.idx").exists());

        Ok(())
    }
}
