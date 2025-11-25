use anyhow::Result;
use std::fs;
use std::path::Path;
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
fn test_helix_index_created_on_status() -> Result<()> {
    let temp_dir = TempDir::new()?;
    init_test_repo(temp_dir.path())?;

    // Create and stage files
    fs::write(temp_dir.path().join("file1.txt"), "content1")?;
    fs::write(temp_dir.path().join("file2.txt"), "content2")?;

    Command::new("git")
        .args(&["add", "."])
        .current_dir(temp_dir.path())
        .output()?;

    // Use the public API instead of accessing internal modules
    use helix::helix_index::api::HelixIndex;

    // Load or build the index (this is what status does internally)
    let index = HelixIndex::load_or_rebuild(temp_dir.path())?;

    // Verify helix index was created
    assert!(temp_dir.path().join(".helix/helix.idx").exists());

    // Verify staging info is correct
    let staged = index.get_staged();
    assert_eq!(staged.len(), 2);

    Ok(())
}

#[test]
fn test_external_git_add_detected() -> Result<()> {
    let temp_dir = TempDir::new()?;
    init_test_repo(temp_dir.path())?;

    fs::write(temp_dir.path().join("file1.txt"), "content")?;
    Command::new("git")
        .args(&["add", "file1.txt"])
        .current_dir(temp_dir.path())
        .output()?;

    use helix::helix_index::api::HelixIndex;

    let mut index = HelixIndex::load_or_rebuild(temp_dir.path())?;
    assert_eq!(index.get_staged().len(), 1);

    // External git add
    std::thread::sleep(std::time::Duration::from_millis(50));
    fs::write(temp_dir.path().join("file2.txt"), "content")?;
    Command::new("git")
        .args(&["add", "file2.txt"])
        .current_dir(temp_dir.path())
        .output()?;

    // Refresh should detect and resync
    index.refresh()?;

    assert_eq!(index.get_staged().len(), 2);

    Ok(())
}

#[test]
fn test_drift_detection_and_rebuild() -> Result<()> {
    let temp_dir = TempDir::new()?;
    init_test_repo(temp_dir.path())?;

    fs::write(temp_dir.path().join("file1.txt"), "content")?;
    Command::new("git")
        .args(&["add", "file1.txt"])
        .current_dir(temp_dir.path())
        .output()?;

    use helix::helix_index::{api::HelixIndex, verify::Verifier, verify::VerifyResult};

    // Build index
    let index1 = HelixIndex::load_or_rebuild(temp_dir.path())?;
    let gen1 = index1.generation();

    // Verify it's valid
    let verifier = Verifier::new(temp_dir.path());
    assert_eq!(verifier.verify()?, VerifyResult::Valid);

    // External git operation
    std::thread::sleep(std::time::Duration::from_millis(50));
    fs::write(temp_dir.path().join("file2.txt"), "content")?;
    Command::new("git")
        .args(&["add", "file2.txt"])
        .current_dir(temp_dir.path())
        .output()?;

    // Should detect drift
    assert_eq!(verifier.verify()?, VerifyResult::MtimeMismatch);

    // Load should rebuild
    let index2 = HelixIndex::load_or_rebuild(temp_dir.path())?;
    let gen2 = index2.generation();

    assert!(gen2 > gen1, "Generation should increment after rebuild");
    assert_eq!(index2.get_staged().len(), 2);

    Ok(())
}
