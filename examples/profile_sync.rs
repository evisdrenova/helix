//! Profile sync operations to find bottlenecks

use anyhow::Result;
use helix::helix_index::sync::SyncEngine;
use std::env;
use std::fs;
use std::path::Path;
use std::process::Command;

fn main() -> Result<()> {
    let args: Vec<String> = env::args().collect();

    if args.len() < 2 {
        eprintln!("Usage: {} <repo_path>", args[0]);
        std::process::exit(1);
    }

    let repo_path = Path::new(&args[1]);

    println!("Profiling sync operation on: {}", repo_path.display());
    println!();

    // Remove existing index
    let helix_dir = repo_path.join(".git/helix");
    if helix_dir.exists() {
        fs::remove_dir_all(&helix_dir)?;
    }

    // Run sync with timing
    let syncer = SyncEngine::new(repo_path);

    println!("=== First Sync (cold) ===");
    let timing1 = syncer.sync_with_timing(std::time::Duration::from_secs(5))?;
    println!("{}", timing1);

    // Make a change
    println!("=== Making a change ===");
    let test_file = repo_path.join("_profile_test.txt");
    fs::write(&test_file, "test content")?;
    Command::new("git")
        .args(&["add", "_profile_test.txt"])
        .current_dir(repo_path)
        .output()?;

    std::thread::sleep(std::time::Duration::from_millis(50));

    println!("=== Second Sync (hot, after change) ===");
    let timing2 = syncer.sync_with_timing(std::time::Duration::from_secs(5))?;
    println!("{}", timing2);

    // Cleanup
    fs::remove_file(&test_file).ok();
    Command::new("git")
        .args(&["reset", "HEAD", "_profile_test.txt"])
        .current_dir(repo_path)
        .output()
        .ok();

    Ok(())
}
