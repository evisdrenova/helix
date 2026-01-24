// Unstage command - Remove files from staging area

use crate::helix_index::api::HelixIndexData;
use crate::sandbox_command::RepoContext;
use anyhow::Result;
use std::path::{Path, PathBuf};

pub struct UnstageOptions {
    pub verbose: bool,
    pub dry_run: bool,
}

impl Default for UnstageOptions {
    fn default() -> Self {
        Self {
            verbose: false,
            dry_run: false,
        }
    }
}

/// Unstage files from the staging area
pub fn unstage(repo_path: &Path, paths: &[PathBuf], options: UnstageOptions) -> Result<()> {
    let context = RepoContext::detect(repo_path)?;

    // Load index from context's index path
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

    // Get currently staged files
    let staged = index.get_staged();

    if staged.is_empty() {
        println!("No files are staged");
        return Ok(());
    }

    // Determine which files to unstage
    let files_to_unstage = resolve_files_to_unstage(&staged, paths, &options)?;

    if files_to_unstage.is_empty() {
        println!("No matching staged files to unstage");
        return Ok(());
    }

    if options.verbose {
        println!("Unstaging {} files...", files_to_unstage.len());
        for file in &files_to_unstage {
            println!("  unstage '{}'", file.display());
        }
    }

    if options.dry_run {
        for file in &files_to_unstage {
            println!("Would unstage: {}", file.display());
        }
        return Ok(());
    }

    // Unstage files
    let paths_refs: Vec<&Path> = files_to_unstage.iter().map(|p| p.as_path()).collect();
    index.unstage_files(&paths_refs)?;

    // Persist index to disk
    index.persist()?;

    let count = files_to_unstage.len();
    if count == 1 {
        println!("Unstaged '{}'", files_to_unstage[0].display());
    } else {
        println!("Unstaged {} files", count);
    }

    if options.verbose {
        println!("Index generation: {}", index.generation());
    }

    Ok(())
}

/// Unstage all staged files
pub fn unstage_all(repo_path: &Path, options: UnstageOptions) -> Result<()> {
    let context = RepoContext::detect(repo_path)?;

    // Load index from context's index path
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

    // Get currently staged files
    let staged = index.get_staged();

    if staged.is_empty() {
        println!("No files are staged");
        return Ok(());
    }

    let count = staged.len();

    if options.verbose {
        println!("Unstaging all {} files...", count);
        for file in &staged {
            println!("  unstage '{}'", file.display());
        }
    }

    if options.dry_run {
        for file in &staged {
            println!("Would unstage: {}", file.display());
        }
        return Ok(());
    }

    // Unstage all files
    index.unstage_all()?;

    // Persist index to disk
    index.persist()?;

    println!("Unstaged {} files", count);

    if options.verbose {
        println!("Index generation: {}", index.generation());
    }

    Ok(())
}

fn resolve_files_to_unstage(
    staged: &std::collections::HashSet<PathBuf>,
    paths: &[PathBuf],
    options: &UnstageOptions,
) -> Result<Vec<PathBuf>> {
    let mut files_to_unstage = Vec::new();

    for path in paths {
        // Handle "." to mean all staged files
        if path.as_os_str() == "." {
            files_to_unstage.extend(staged.iter().cloned());
            continue;
        }

        // Check if path is staged
        if staged.contains(path) {
            files_to_unstage.push(path.clone());
        } else {
            // Check if it's a directory prefix
            let prefix = path.to_string_lossy();
            let matches: Vec<_> = staged
                .iter()
                .filter(|p| p.to_string_lossy().starts_with(prefix.as_ref()))
                .cloned()
                .collect();

            if matches.is_empty() {
                if options.verbose {
                    eprintln!("Warning: '{}' is not staged, skipping", path.display());
                }
            } else {
                files_to_unstage.extend(matches);
            }
        }
    }

    // Remove duplicates
    files_to_unstage.sort();
    files_to_unstage.dedup();

    Ok(files_to_unstage)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::add_command::{add, AddOptions};
    use crate::helix_index::api::HelixIndexData;
    use std::fs;
    use tempfile::TempDir;

    fn init_test_repo(path: &Path) -> Result<()> {
        crate::init_command::init_helix_repo(path, None)?;

        let config_path = path.join("helix.toml");
        fs::write(
            &config_path,
            r#"
[user]
name = "Test User"
email = "test@test.com"
"#,
        )?;

        Ok(())
    }

    #[test]
    fn test_unstage_single_file() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        init_test_repo(repo_path)?;

        // Create and stage a file
        fs::write(repo_path.join("test.txt"), b"hello world")?;
        add(
            repo_path,
            &[PathBuf::from("test.txt")],
            AddOptions::default(),
        )?;

        // Verify it's staged
        let index = HelixIndexData::load_or_rebuild(repo_path)?;
        assert_eq!(index.get_staged().len(), 1);

        // Unstage it
        unstage(
            repo_path,
            &[PathBuf::from("test.txt")],
            UnstageOptions::default(),
        )?;

        // Verify unstaged but still tracked
        let index = HelixIndexData::load_or_rebuild(repo_path)?;
        assert_eq!(index.get_staged().len(), 0);
        assert_eq!(index.get_tracked().len(), 1);

        Ok(())
    }

    #[test]
    fn test_unstage_multiple_files() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        init_test_repo(repo_path)?;

        // Create and stage files
        fs::write(repo_path.join("file1.txt"), b"content 1")?;
        fs::write(repo_path.join("file2.txt"), b"content 2")?;
        fs::write(repo_path.join("file3.txt"), b"content 3")?;
        add(repo_path, &[PathBuf::from(".")], AddOptions::default())?;

        // Verify all staged (3 files + helix.toml = 4)
        let index = HelixIndexData::load_or_rebuild(repo_path)?;
        assert_eq!(index.get_staged().len(), 4);

        // Unstage specific files
        unstage(
            repo_path,
            &[PathBuf::from("file1.txt"), PathBuf::from("file2.txt")],
            UnstageOptions::default(),
        )?;

        // Verify only file3 and helix.toml remain staged
        let index = HelixIndexData::load_or_rebuild(repo_path)?;
        assert_eq!(index.get_staged().len(), 2);
        assert!(index.get_staged().contains(&PathBuf::from("file3.txt")));

        Ok(())
    }

    #[test]
    fn test_unstage_all() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        init_test_repo(repo_path)?;

        // Create and stage files
        fs::write(repo_path.join("file1.txt"), b"content 1")?;
        fs::write(repo_path.join("file2.txt"), b"content 2")?;
        add(repo_path, &[PathBuf::from(".")], AddOptions::default())?;

        // Verify staged (2 files + helix.toml = 3)
        let index = HelixIndexData::load_or_rebuild(repo_path)?;
        assert_eq!(index.get_staged().len(), 3);

        // Unstage all
        unstage_all(repo_path, UnstageOptions::default())?;

        // Verify all unstaged but still tracked
        let index = HelixIndexData::load_or_rebuild(repo_path)?;
        assert_eq!(index.get_staged().len(), 0);
        assert_eq!(index.get_tracked().len(), 3);

        Ok(())
    }

    #[test]
    fn test_unstage_dot_syntax() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        init_test_repo(repo_path)?;

        // Create and stage files
        fs::write(repo_path.join("file1.txt"), b"content 1")?;
        fs::write(repo_path.join("file2.txt"), b"content 2")?;
        add(repo_path, &[PathBuf::from(".")], AddOptions::default())?;

        // Unstage with "." (all)
        unstage(repo_path, &[PathBuf::from(".")], UnstageOptions::default())?;

        // Verify all unstaged
        let index = HelixIndexData::load_or_rebuild(repo_path)?;
        assert_eq!(index.get_staged().len(), 0);

        Ok(())
    }

    #[test]
    fn test_unstage_dry_run() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        init_test_repo(repo_path)?;

        // Create and stage a file
        fs::write(repo_path.join("test.txt"), b"content")?;
        add(
            repo_path,
            &[PathBuf::from("test.txt")],
            AddOptions::default(),
        )?;

        // Dry run unstage
        unstage(
            repo_path,
            &[PathBuf::from("test.txt")],
            UnstageOptions {
                dry_run: true,
                ..Default::default()
            },
        )?;

        // Verify still staged
        let index = HelixIndexData::load_or_rebuild(repo_path)?;
        assert_eq!(index.get_staged().len(), 1);

        Ok(())
    }

    #[test]
    fn test_unstage_no_staged_files() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        init_test_repo(repo_path)?;

        // No files staged - should not error
        let result = unstage(
            repo_path,
            &[PathBuf::from("nonexistent.txt")],
            UnstageOptions::default(),
        );

        assert!(result.is_ok());

        Ok(())
    }

    #[test]
    fn test_unstage_directory() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        init_test_repo(repo_path)?;

        // Create directory with files and stage them
        fs::create_dir_all(repo_path.join("src"))?;
        fs::write(repo_path.join("src/main.rs"), b"fn main() {}")?;
        fs::write(repo_path.join("src/lib.rs"), b"pub fn test() {}")?;
        fs::write(repo_path.join("README.md"), b"# Test")?;
        add(repo_path, &[PathBuf::from(".")], AddOptions::default())?;

        // Verify all staged (3 files + helix.toml = 4)
        let index = HelixIndexData::load_or_rebuild(repo_path)?;
        assert_eq!(index.get_staged().len(), 4);

        // Unstage src directory
        unstage(
            repo_path,
            &[PathBuf::from("src")],
            UnstageOptions::default(),
        )?;

        // Verify only README and helix.toml remain staged
        let index = HelixIndexData::load_or_rebuild(repo_path)?;
        assert_eq!(index.get_staged().len(), 2);
        assert!(index.get_staged().contains(&PathBuf::from("README.md")));

        Ok(())
    }

    #[test]
    fn test_unstage_idempotent() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        init_test_repo(repo_path)?;

        // Create and stage a file
        fs::write(repo_path.join("test.txt"), b"content")?;
        add(
            repo_path,
            &[PathBuf::from("test.txt")],
            AddOptions::default(),
        )?;

        // Unstage twice
        unstage(
            repo_path,
            &[PathBuf::from("test.txt")],
            UnstageOptions::default(),
        )?;

        // Second unstage should not error (file already unstaged)
        let result = unstage(
            repo_path,
            &[PathBuf::from("test.txt")],
            UnstageOptions::default(),
        );
        assert!(result.is_ok());

        Ok(())
    }
}
