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

// ==================== Basic Workflow Tests ====================

#[test]
fn test_diff_workflow_unstaged_changes() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let repo_path = temp_dir.path();

    init_test_repo(repo_path)?;

    use helix_cli::add_command::{add, AddOptions};
    use helix_cli::diff_command::{diff, DiffOptions};

    // Create and stage files
    fs::write(
        repo_path.join("file1.txt"),
        "original line 1\noriginal line 2\n",
    )?;
    fs::write(repo_path.join("file2.txt"), "file 2 content\n")?;
    add(repo_path, &[PathBuf::from(".")], AddOptions::default())?;

    // Modify file1 only
    fs::write(
        repo_path.join("file1.txt"),
        "modified line 1\noriginal line 2\nnew line 3\n",
    )?;

    // Run diff - should detect unstaged changes
    let result = diff(
        repo_path,
        &[],
        DiffOptions {
            no_color: true,
            ..Default::default()
        },
    );

    assert!(result.is_ok());
    Ok(())
}

#[test]
fn test_diff_workflow_staged_changes() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let repo_path = temp_dir.path();

    init_test_repo(repo_path)?;

    use helix_cli::add_command::{add, AddOptions};
    use helix_cli::commit_command::{commit, CommitOptions};
    use helix_cli::diff_command::{diff, DiffOptions};

    // Initial commit
    fs::write(repo_path.join("file.txt"), "v1\n")?;
    add(
        repo_path,
        &[PathBuf::from("file.txt")],
        AddOptions::default(),
    )?;
    commit(
        repo_path,
        CommitOptions {
            message: "Initial".to_string(),
            author: Some("Test <test@test.com>".to_string()),
            allow_empty: false,
            amend: false,
            verbose: false,
        },
    )?;

    // Modify and stage
    fs::write(repo_path.join("file.txt"), "v2\n")?;
    add(
        repo_path,
        &[PathBuf::from("file.txt")],
        AddOptions::default(),
    )?;

    // Run diff --staged
    let result = diff(
        repo_path,
        &[],
        DiffOptions {
            staged: true,
            no_color: true,
            ..Default::default()
        },
    );

    assert!(result.is_ok());
    Ok(())
}

#[test]
fn test_diff_workflow_mixed_staged_and_unstaged() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let repo_path = temp_dir.path();

    init_test_repo(repo_path)?;

    use helix_cli::add_command::{add, AddOptions};
    use helix_cli::commit_command::{commit, CommitOptions};
    use helix_cli::diff_command::{diff, DiffOptions};

    // Initial commit
    fs::write(repo_path.join("file.txt"), "line 1\nline 2\nline 3\n")?;
    add(
        repo_path,
        &[PathBuf::from("file.txt")],
        AddOptions::default(),
    )?;
    commit(
        repo_path,
        CommitOptions {
            message: "Initial".to_string(),
            author: Some("Test <test@test.com>".to_string()),
            allow_empty: false,
            amend: false,
            verbose: false,
        },
    )?;

    // Stage some changes
    fs::write(
        repo_path.join("file.txt"),
        "line 1\nstaged change\nline 3\n",
    )?;
    add(
        repo_path,
        &[PathBuf::from("file.txt")],
        AddOptions::default(),
    )?;

    // Make more unstaged changes
    fs::write(
        repo_path.join("file.txt"),
        "line 1\nstaged change\nunstaged change\n",
    )?;

    // Run diff (unstaged) - should show unstaged changes
    let result_unstaged = diff(
        repo_path,
        &[],
        DiffOptions {
            no_color: true,
            ..Default::default()
        },
    );
    assert!(result_unstaged.is_ok());

    // Run diff --staged - should show staged changes vs HEAD
    let result_staged = diff(
        repo_path,
        &[],
        DiffOptions {
            staged: true,
            no_color: true,
            ..Default::default()
        },
    );
    assert!(result_staged.is_ok());

    Ok(())
}

// ==================== Deletion Tests ====================

#[test]
fn test_diff_workflow_file_deletion() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let repo_path = temp_dir.path();

    init_test_repo(repo_path)?;

    use helix_cli::add_command::{add, AddOptions};
    use helix_cli::diff_command::{diff, DiffOptions};

    // Create and stage a file
    fs::write(
        repo_path.join("to_delete.txt"),
        "this file will be deleted\nline 2\n",
    )?;
    add(
        repo_path,
        &[PathBuf::from("to_delete.txt")],
        AddOptions::default(),
    )?;

    // Delete from working tree
    fs::remove_file(repo_path.join("to_delete.txt"))?;

    // Run diff - should show deletion
    let result = diff(
        repo_path,
        &[],
        DiffOptions {
            no_color: true,
            ..Default::default()
        },
    );

    assert!(result.is_ok());
    Ok(())
}

// ==================== Stat Mode Tests ====================

#[test]
fn test_diff_stat_workflow() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let repo_path = temp_dir.path();

    init_test_repo(repo_path)?;

    use helix_cli::add_command::{add, AddOptions};
    use helix_cli::diff_command::{diff_stat, DiffOptions};

    // Create and stage multiple files
    fs::write(repo_path.join("file1.txt"), "a\nb\nc\n")?;
    fs::write(repo_path.join("file2.txt"), "x\ny\nz\n")?;
    add(repo_path, &[PathBuf::from(".")], AddOptions::default())?;

    // Modify files
    fs::write(repo_path.join("file1.txt"), "a\nB\nc\nd\n")?;
    fs::write(repo_path.join("file2.txt"), "X\nY\nZ\n")?;

    // Run diff --stat
    let result = diff_stat(
        repo_path,
        &[],
        DiffOptions {
            no_color: true,
            ..Default::default()
        },
    );

    assert!(result.is_ok());
    Ok(())
}

// ==================== Path Filter Tests ====================

#[test]
fn test_diff_path_filter_workflow() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let repo_path = temp_dir.path();

    init_test_repo(repo_path)?;

    use helix_cli::add_command::{add, AddOptions};
    use helix_cli::diff_command::{diff, DiffOptions};

    // Create directory structure
    fs::create_dir_all(repo_path.join("src"))?;
    fs::create_dir_all(repo_path.join("tests"))?;
    fs::write(repo_path.join("src/main.rs"), "fn main() {}\n")?;
    fs::write(repo_path.join("tests/test.rs"), "fn test() {}\n")?;
    add(repo_path, &[PathBuf::from(".")], AddOptions::default())?;

    // Modify both
    fs::write(repo_path.join("src/main.rs"), "fn main() { todo!() }\n")?;
    fs::write(
        repo_path.join("tests/test.rs"),
        "fn test() { assert!(true) }\n",
    )?;

    // Filter to src only
    let result = diff(
        repo_path,
        &[PathBuf::from("src")],
        DiffOptions {
            no_color: true,
            ..Default::default()
        },
    );

    assert!(result.is_ok());
    Ok(())
}

// ==================== Edge Case Tests ====================

#[test]
fn test_diff_empty_repo() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let repo_path = temp_dir.path();

    init_test_repo(repo_path)?;

    use helix_cli::diff_command::{diff, DiffOptions};

    // Run diff on empty repo (no files staged)
    let result = diff(
        repo_path,
        &[],
        DiffOptions {
            no_color: true,
            ..Default::default()
        },
    );

    assert!(result.is_ok());
    Ok(())
}

#[test]
fn test_diff_no_modifications() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let repo_path = temp_dir.path();

    init_test_repo(repo_path)?;

    use helix_cli::add_command::{add, AddOptions};
    use helix_cli::diff_command::{diff, DiffOptions};

    // Create and stage a file
    fs::write(repo_path.join("unchanged.txt"), "content\n")?;
    add(
        repo_path,
        &[PathBuf::from("unchanged.txt")],
        AddOptions::default(),
    )?;

    // Don't modify - diff should show nothing
    let result = diff(
        repo_path,
        &[],
        DiffOptions {
            verbose: true,
            no_color: true,
            ..Default::default()
        },
    );

    assert!(result.is_ok());
    Ok(())
}

#[test]
fn test_diff_binary_like_content() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let repo_path = temp_dir.path();

    init_test_repo(repo_path)?;

    use helix_cli::add_command::{add, AddOptions};
    use helix_cli::diff_command::{diff, DiffOptions};

    // Create file with some binary-like content (null bytes, etc)
    let content: Vec<u8> = vec![
        0x48, 0x65, 0x6c, 0x6c, 0x6f, 0x00, 0x57, 0x6f, 0x72, 0x6c, 0x64,
    ];
    fs::write(repo_path.join("binary.bin"), &content)?;
    add(
        repo_path,
        &[PathBuf::from("binary.bin")],
        AddOptions::default(),
    )?;

    // Modify
    let modified: Vec<u8> = vec![0x48, 0x69, 0x00, 0x00, 0x57, 0x6f, 0x72, 0x6c, 0x64, 0x21];
    fs::write(repo_path.join("binary.bin"), &modified)?;

    // Run diff - should handle gracefully
    let result = diff(
        repo_path,
        &[],
        DiffOptions {
            no_color: true,
            ..Default::default()
        },
    );

    assert!(result.is_ok());
    Ok(())
}
