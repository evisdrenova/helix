use anyhow::Result;
use std::fs;
use std::path::Path;
use tempfile::TempDir;

fn init_test_repo(path: &Path) -> Result<()> {
    // Initialize pure Helix repo (no Git needed)
    helix::init::init_helix_repo(path, None)?;

    // Set up author config
    let config_path = path.join(".helix/config.toml");
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
fn test_helix_index_created_on_add() -> Result<()> {
    let temp_dir = TempDir::new()?;
    init_test_repo(temp_dir.path())?;

    // Create files
    fs::write(temp_dir.path().join("file1.txt"), "content1")?;
    fs::write(temp_dir.path().join("file2.txt"), "content2")?;

    // Use helix add command
    use helix::add::{add, AddOptions};
    use std::path::PathBuf;

    add(
        temp_dir.path(),
        &[PathBuf::from(".")],
        AddOptions::default(),
    )?;

    // Verify helix index was created
    assert!(temp_dir.path().join(".helix/helix.idx").exists());

    // Load and verify staging info
    use helix::helix_index::api::HelixIndexData;
    let index = HelixIndexData::load_or_rebuild(temp_dir.path())?;

    // Verify both files are staged
    let staged = index.get_staged();
    assert_eq!(staged.len(), 2);
    assert!(staged.contains(&PathBuf::from("file1.txt")));
    assert!(staged.contains(&PathBuf::from("file2.txt")));

    Ok(())
}

#[test]
fn test_stage_unstage_workflow() -> Result<()> {
    let temp_dir = TempDir::new()?;
    init_test_repo(temp_dir.path())?;

    // Create and add file
    fs::write(temp_dir.path().join("file1.txt"), "content")?;

    use helix::add::{add, AddOptions};
    use helix::helix_index::api::HelixIndexData;
    use std::path::PathBuf;

    add(
        temp_dir.path(),
        &[PathBuf::from("file1.txt")],
        AddOptions::default(),
    )?;

    let mut index = HelixIndexData::load_or_rebuild(temp_dir.path())?;
    assert_eq!(index.get_staged().len(), 1);

    // Unstage the file
    index.unstage_file(Path::new("file1.txt"))?;
    index.persist()?;

    // Reload and verify unstaged
    let index = HelixIndexData::load_or_rebuild(temp_dir.path())?;
    assert_eq!(index.get_staged().len(), 0);

    // File should still be tracked
    let tracked = index.get_tracked();
    assert_eq!(tracked.len(), 1);
    assert!(tracked.contains(&PathBuf::from("file1.txt")));

    // Stage it again
    let mut index = HelixIndexData::load_or_rebuild(temp_dir.path())?;
    index.stage_file(Path::new("file1.txt"))?;
    index.persist()?;

    // Verify staged again
    let index = HelixIndexData::load_or_rebuild(temp_dir.path())?;
    assert_eq!(index.get_staged().len(), 1);

    Ok(())
}

#[test]
fn test_index_persistence_and_reload() -> Result<()> {
    let temp_dir = TempDir::new()?;
    init_test_repo(temp_dir.path())?;

    // Create and stage files
    fs::write(temp_dir.path().join("file1.txt"), "content")?;

    use helix::add::{add, AddOptions};
    use helix::helix_index::api::HelixIndexData;
    use std::path::PathBuf;

    add(
        temp_dir.path(),
        &[PathBuf::from("file1.txt")],
        AddOptions::default(),
    )?;

    // Get generation
    let index1 = HelixIndexData::load_or_rebuild(temp_dir.path())?;
    let gen1 = index1.generation();

    // Add another file (should increment generation)
    std::thread::sleep(std::time::Duration::from_millis(50));
    fs::write(temp_dir.path().join("file2.txt"), "content")?;

    add(
        temp_dir.path(),
        &[PathBuf::from("file2.txt")],
        AddOptions::default(),
    )?;

    // Reload and check generation increased
    let index2 = HelixIndexData::load_or_rebuild(temp_dir.path())?;
    let gen2 = index2.generation();

    assert!(gen2 > gen1, "Generation should increment after changes");
    assert_eq!(index2.get_staged().len(), 2);

    Ok(())
}

#[test]
fn test_commit_workflow() -> Result<()> {
    let temp_dir = TempDir::new()?;
    init_test_repo(temp_dir.path())?;

    // Create and stage file
    fs::write(temp_dir.path().join("file1.txt"), "content")?;

    use helix::add::{add, AddOptions};
    use helix::commit::{commit, CommitOptions};
    use helix::helix_index::api::HelixIndexData;
    use std::path::PathBuf;

    add(
        temp_dir.path(),
        &[PathBuf::from("file1.txt")],
        AddOptions::default(),
    )?;

    // Verify staged
    let index = HelixIndexData::load_or_rebuild(temp_dir.path())?;
    assert_eq!(index.get_staged().len(), 1);

    // Commit
    let commit_hash = commit(
        temp_dir.path(),
        CommitOptions {
            message: "Initial commit".to_string(),
            author: Some("Test User <test@test.com>".to_string()),
            allow_empty: false,
            amend: false,
            verbose: false,
        },
    )?;

    // Verify commit was created
    use helix::helix_index::commit::CommitStorage;
    let storage = CommitStorage::for_repo(temp_dir.path());
    let commit_obj = storage.read(&commit_hash)?;

    assert_eq!(commit_obj.message, "Initial commit");
    assert!(commit_obj.is_initial()); // No parents

    // Verify HEAD was updated
    assert!(temp_dir.path().join(".helix/HEAD").exists());

    // Verify staged flags were cleared
    let index = HelixIndexData::load_or_rebuild(temp_dir.path())?;
    assert_eq!(index.get_staged().len(), 0);

    // But file should still be tracked
    assert_eq!(index.get_tracked().len(), 1);

    Ok(())
}

#[test]
fn test_second_commit_workflow() -> Result<()> {
    let temp_dir = TempDir::new()?;
    init_test_repo(temp_dir.path())?;

    use helix::add::{add, AddOptions};
    use helix::commit::{commit, CommitOptions};
    use helix::helix_index::commit::CommitStorage;
    use std::path::PathBuf;

    // First commit
    fs::write(temp_dir.path().join("file1.txt"), "content1")?;
    add(
        temp_dir.path(),
        &[PathBuf::from("file1.txt")],
        AddOptions::default(),
    )?;

    let commit1_hash = commit(
        temp_dir.path(),
        CommitOptions {
            message: "First commit".to_string(),
            author: Some("Test User <test@test.com>".to_string()),
            allow_empty: false,
            amend: false,
            verbose: false,
        },
    )?;

    // Second commit
    fs::write(temp_dir.path().join("file2.txt"), "content2")?;
    add(
        temp_dir.path(),
        &[PathBuf::from("file2.txt")],
        AddOptions::default(),
    )?;

    let commit2_hash = commit(
        temp_dir.path(),
        CommitOptions {
            message: "Second commit".to_string(),
            author: Some("Test User <test@test.com>".to_string()),
            allow_empty: false,
            amend: false,
            verbose: false,
        },
    )?;

    // Verify commit chain
    let storage = CommitStorage::for_repo(temp_dir.path());

    let commit1 = storage.read(&commit1_hash)?;
    assert!(commit1.is_initial());
    assert_eq!(commit1.message, "First commit");

    let commit2 = storage.read(&commit2_hash)?;
    assert!(!commit2.is_initial());
    assert_eq!(commit2.parents.len(), 1);
    assert_eq!(commit2.parents[0], commit1_hash);
    assert_eq!(commit2.message, "Second commit");

    Ok(())
}

#[test]
fn test_blob_deduplication() -> Result<()> {
    let temp_dir = TempDir::new()?;
    init_test_repo(temp_dir.path())?;

    // Create two files with same content
    fs::write(temp_dir.path().join("file1.txt"), "same content")?;
    fs::write(temp_dir.path().join("file2.txt"), "same content")?;

    use helix::add::{add, AddOptions};
    use helix::helix_index::api::HelixIndexData;
    use helix::helix_index::blob_storage::BlobStorage;
    use std::path::PathBuf;

    add(
        temp_dir.path(),
        &[PathBuf::from("file1.txt"), PathBuf::from("file2.txt")],
        AddOptions::default(),
    )?;

    // Verify both files point to same blob
    let index = HelixIndexData::load_or_rebuild(temp_dir.path())?;
    let entries = index.entries();

    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].oid, entries[1].oid, "Should have same hash");

    // Verify only one blob stored
    let storage = BlobStorage::for_repo(temp_dir.path());
    let all_blobs = storage.list_all()?;
    assert_eq!(all_blobs.len(), 1, "Should only store one blob");

    Ok(())
}

#[test]
fn test_fsmonitor_integration() -> Result<()> {
    let temp_dir = TempDir::new()?;
    init_test_repo(temp_dir.path())?;

    use helix::fsmonitor::FSMonitor;

    let mut monitor = FSMonitor::new(temp_dir.path())?;
    monitor.start_watching_repo()?;

    // Give it time to start
    std::thread::sleep(std::time::Duration::from_millis(100));

    // Create a file
    fs::write(temp_dir.path().join("test.txt"), "content")?;

    // Give FSMonitor time to detect
    std::thread::sleep(std::time::Duration::from_millis(150));

    // Check if detected
    let dirty_files = monitor.get_dirty_files();
    assert!(!dirty_files.is_empty(), "FSMonitor should detect new file");

    Ok(())
}

#[test]
fn test_stage_all_unstage_all() -> Result<()> {
    let temp_dir = TempDir::new()?;
    init_test_repo(temp_dir.path())?;

    // Create multiple files
    fs::write(temp_dir.path().join("file1.txt"), "content1")?;
    fs::write(temp_dir.path().join("file2.txt"), "content2")?;
    fs::write(temp_dir.path().join("file3.txt"), "content3")?;

    use helix::add::{add, AddOptions};
    use helix::helix_index::api::HelixIndexData;
    use std::path::PathBuf;

    add(
        temp_dir.path(),
        &[PathBuf::from(".")],
        AddOptions::default(),
    )?;

    // Verify all staged
    let index = HelixIndexData::load_or_rebuild(temp_dir.path())?;
    assert_eq!(index.get_staged().len(), 3);

    // Unstage all
    let mut index = HelixIndexData::load_or_rebuild(temp_dir.path())?;
    index.unstage_all()?;
    index.persist()?;

    // Verify all unstaged
    let index = HelixIndexData::load_or_rebuild(temp_dir.path())?;
    assert_eq!(index.get_staged().len(), 0);
    assert_eq!(index.get_tracked().len(), 3); // Still tracked

    // Stage all again
    let mut index = HelixIndexData::load_or_rebuild(temp_dir.path())?;
    index.stage_all()?;
    index.persist()?;

    // Verify all staged
    let index = HelixIndexData::load_or_rebuild(temp_dir.path())?;
    assert_eq!(index.get_staged().len(), 3);

    Ok(())
}
