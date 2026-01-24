use anyhow::Result;
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

fn init_test_repo(path: &Path) -> Result<()> {
    helix_cli::init_command::init_helix_repo(path, None)?;

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
fn test_unstage_workflow() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let repo_path = temp_dir.path();

    init_test_repo(repo_path)?;

    // Create and stage files
    fs::write(repo_path.join("file1.txt"), "content1")?;
    fs::write(repo_path.join("file2.txt"), "content2")?;

    use helix_cli::add_command::{add, AddOptions};
    use helix_cli::helix_index::api::HelixIndexData;
    use helix_cli::unstage_command::{unstage, UnstageOptions};

    add(repo_path, &[PathBuf::from(".")], AddOptions::default())?;

    // Verify staged (2 files + helix.toml = 3)
    let index = HelixIndexData::load_or_rebuild(repo_path)?;
    assert_eq!(index.get_staged().len(), 3);

    // Unstage one file
    unstage(
        repo_path,
        &[PathBuf::from("file1.txt")],
        UnstageOptions::default(),
    )?;

    // Verify file1 is unstaged, file2 and helix.toml remain staged
    let index = HelixIndexData::load_or_rebuild(repo_path)?;
    assert_eq!(index.get_staged().len(), 2);
    assert!(index.get_staged().contains(&PathBuf::from("file2.txt")));

    // All should still be tracked
    assert_eq!(index.get_tracked().len(), 3);

    Ok(())
}

#[test]
fn test_unstage_all_workflow() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let repo_path = temp_dir.path();

    init_test_repo(repo_path)?;

    // Create and stage files
    fs::write(repo_path.join("file1.txt"), "content1")?;
    fs::write(repo_path.join("file2.txt"), "content2")?;
    fs::write(repo_path.join("file3.txt"), "content3")?;

    use helix_cli::add_command::{add, AddOptions};
    use helix_cli::helix_index::api::HelixIndexData;
    use helix_cli::unstage_command::{unstage_all, UnstageOptions};

    add(repo_path, &[PathBuf::from(".")], AddOptions::default())?;

    // Verify all staged (3 files + helix.toml = 4)
    let index = HelixIndexData::load_or_rebuild(repo_path)?;
    assert_eq!(index.get_staged().len(), 4);

    // Unstage all
    unstage_all(repo_path, UnstageOptions::default())?;

    // Verify none staged
    let index = HelixIndexData::load_or_rebuild(repo_path)?;
    assert_eq!(index.get_staged().len(), 0);

    // All should still be tracked
    assert_eq!(index.get_tracked().len(), 4);

    Ok(())
}

#[test]
fn test_unstage_preserves_tracking() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let repo_path = temp_dir.path();

    init_test_repo(repo_path)?;

    use helix_cli::add_command::{add, AddOptions};
    use helix_cli::helix_index::api::HelixIndexData;
    use helix_cli::unstage_command::{unstage, UnstageOptions};

    // Create and stage a file
    fs::write(repo_path.join("test.txt"), "original content")?;
    add(
        repo_path,
        &[PathBuf::from("test.txt")],
        AddOptions::default(),
    )?;

    // Verify staged
    let index = HelixIndexData::load_or_rebuild(repo_path)?;
    assert_eq!(index.get_staged().len(), 1);
    assert_eq!(index.get_tracked().len(), 1);

    // Unstage
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
fn test_unstage_directory_prefix() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let repo_path = temp_dir.path();

    init_test_repo(repo_path)?;

    use helix_cli::add_command::{add, AddOptions};
    use helix_cli::helix_index::api::HelixIndexData;
    use helix_cli::unstage_command::{unstage, UnstageOptions};

    // Create directory structure
    fs::create_dir_all(repo_path.join("src"))?;
    fs::write(repo_path.join("src/main.rs"), "fn main() {}")?;
    fs::write(repo_path.join("src/lib.rs"), "pub fn lib() {}")?;
    fs::write(repo_path.join("README.md"), "# Test")?;

    // Stage all
    add(repo_path, &[PathBuf::from(".")], AddOptions::default())?;

    // Verify all staged (3 files + helix.toml = 4)
    let index = HelixIndexData::load_or_rebuild(repo_path)?;
    assert_eq!(index.get_staged().len(), 4);

    // Unstage src/ directory
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
