use anyhow::Result;
use helix::{fsmonitor::FSMonitor, index::Index};
use std::fs;
use std::thread;
use std::time::Duration;

#[test]
fn test_fsmonitor_with_index() -> Result<()> {
    let temp_dir = tempfile::tempdir()?;
    let repo_path = temp_dir.path();

    // Initialize git repo
    std::process::Command::new("git")
        .args(&["init", repo_path.to_str().unwrap()])
        .output()?;

    std::process::Command::new("git")
        .current_dir(repo_path)
        .args(&["config", "user.name", "Test"])
        .output()?;

    std::process::Command::new("git")
        .current_dir(repo_path)
        .args(&["config", "user.email", "test@test.com"])
        .output()?;

    // Create initial file
    fs::write(repo_path.join("file1.txt"), "initial")?;

    std::process::Command::new("git")
        .current_dir(repo_path)
        .args(&["add", "."])
        .output()?;

    std::process::Command::new("git")
        .current_dir(repo_path)
        .args(&["commit", "-m", "initial"])
        .output()?;

    // Read initial index
    let index = Index::open(repo_path)?;
    let initial_count = index.entries().count();
    println!("Initial index entries: {}", initial_count);

    // Start monitoring
    let mut monitor = FSMonitor::new(repo_path)?;
    monitor.start_watching_repo()?;

    thread::sleep(Duration::from_millis(100));

    // Modify existing file
    fs::write(repo_path.join("file1.txt"), "modified")?;

    // Create new file
    fs::write(repo_path.join("file2.txt"), "new")?;

    // Wait for events
    thread::sleep(Duration::from_millis(100));

    // Check dirty files
    let dirty_files = monitor.get_dirty_files();
    println!("Dirty files: {:?}", dirty_files);

    assert!(dirty_files.iter().any(|p| p.ends_with("file1.txt")));
    assert!(dirty_files.iter().any(|p| p.ends_with("file2.txt")));

    // In a real implementation, we'd only check dirty files against index
    // instead of scanning all files
    println!("✓ FSMonitor successfully tracked changes!");

    Ok(())
}
