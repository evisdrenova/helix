use super::*;
use crate::helix_index::format::EntryFlags;
use crate::helix_index::sync::Syncer;
use crate::helix_index::verify::{Verifier, VerifyResult};
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use tempfile::TempDir;

fn init_test_repo(path: &std::path::Path) -> anyhow::Result<()> {
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
fn test_full_sync_verify_cycle() -> anyhow::Result<()> {
    let temp_dir = TempDir::new()?;
    init_test_repo(temp_dir.path())?;

    // Create files
    fs::write(temp_dir.path().join("file1.txt"), "content1")?;
    fs::write(temp_dir.path().join("file2.txt"), "content2")?;

    // Stage them
    Command::new("git")
        .args(&["add", "."])
        .current_dir(temp_dir.path())
        .output()?;

    // Sync
    let syncer = Syncer::new(temp_dir.path());
    syncer.sync()?;

    // Verify
    let verifier = Verifier::new(temp_dir.path());
    let result = verifier.verify()?;
    assert_eq!(result, VerifyResult::Valid);

    // Read and check
    let reader = Reader::new(temp_dir.path());
    let data = reader.read()?;

    assert_eq!(data.header.generation, 1);
    assert_eq!(data.entries.len(), 2);

    for entry in &data.entries {
        assert!(entry.flags.contains(EntryFlags::TRACKED));
        assert!(entry.flags.contains(EntryFlags::STAGED));
    }

    Ok(())
}

#[test]
fn test_rebuild_after_drift() -> anyhow::Result<()> {
    let temp_dir = TempDir::new()?;
    init_test_repo(temp_dir.path())?;

    fs::write(temp_dir.path().join("test.txt"), "hello")?;
    Command::new("git")
        .args(&["add", "test.txt"])
        .current_dir(temp_dir.path())
        .output()?;

    // Initial sync
    let syncer = Syncer::new(temp_dir.path());
    syncer.sync()?;

    let verifier = Verifier::new(temp_dir.path());
    assert_eq!(verifier.verify()?, VerifyResult::Valid);

    // Simulate external git operation
    std::thread::sleep(std::time::Duration::from_millis(10));
    fs::write(temp_dir.path().join("another.txt"), "world")?;
    Command::new("git")
        .args(&["add", "another.txt"])
        .current_dir(temp_dir.path())
        .output()?;

    // Verify should detect drift
    assert_eq!(verifier.verify()?, VerifyResult::MtimeMismatch);

    // Rebuild
    syncer.sync()?;

    // Should be valid again
    assert_eq!(verifier.verify()?, VerifyResult::Valid);

    // Check generation incremented
    let reader = Reader::new(temp_dir.path());
    let data = reader.read()?;
    assert_eq!(data.header.generation, 2);
    assert_eq!(data.entries.len(), 2);

    Ok(())
}
