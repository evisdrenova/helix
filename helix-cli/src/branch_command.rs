// Branch management for Helix
//
// Commands:
//   helix branch                  - List all branches
//   helix branch <name>           - Create new branch
//   helix branch -d <name>        - Delete branch
//   helix branch -m <old> <new>   - Rename branch

use anyhow::{anyhow, Context, Result};
use std::fs;
use std::path::Path;

use crate::branch_tui;
use crate::helix_index::state::{get_branch_upstream, remove_branch_state, set_branch_upstream};
use crate::sandbox_command::RepoContext;
use helix_protocol::hash::{hash_to_hex, hex_to_hash, Hash};

pub struct BranchOptions {
    pub delete: bool,
    pub rename: bool,
    pub force: bool,
    pub verbose: bool,
}

impl Default for BranchOptions {
    fn default() -> Self {
        Self {
            delete: false,
            rename: false,
            force: false,
            verbose: false,
        }
    }
}

pub fn run_branch_tui(repo_path: Option<&Path>) -> Result<()> {
    let repo_path = repo_path
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| std::env::current_dir().expect("Failed to get current directory"));

    let mut app = branch_tui::app::App::new(&repo_path)?;
    app.run()?;

    Ok(())
}

/// Create a new branch
pub fn create_branch(repo_path: &Path, name: &str, options: BranchOptions) -> Result<()> {
    // Validate branch name
    validate_branch_name(name)?;

    let branch_path = repo_path.join(format!(".helix/refs/heads/{}", name));

    if branch_path.exists() && !options.force {
        return Err(anyhow!(
            "Branch '{}' already exists. Use --force to overwrite.",
            name
        ));
    }

    let head_hash = read_head(repo_path)?;

    // Create branch pointing to current commit
    fs::create_dir_all(branch_path.parent().unwrap())?;
    fs::write(&branch_path, format!("{}\n", hash_to_hex(&head_hash)))?;

    let current_branch = get_current_branch(repo_path)?;
    if current_branch != "(no branch)" && current_branch != "(detached HEAD)" {
        if let Err(e) = set_branch_upstream(repo_path, name, &current_branch) {
            eprintln!("Warning: Failed to set upstream: {}", e);
        }
    }

    if options.verbose {
        println!(
            "Created branch '{}' at commit {}",
            name,
            short_hash(&head_hash)
        );
    } else {
        println!("Created branch '{}'", name);
    }

    Ok(())
}

/// Delete a branch
pub fn delete_branch(repo_path: &Path, name: &str, options: BranchOptions) -> Result<()> {
    let branch_path = repo_path.join(format!(".helix/refs/heads/{}", name));

    // Check if branch exists
    if !branch_path.exists() {
        return Err(anyhow!("Branch '{}' does not exist", name));
    }

    // Check if trying to delete current branch
    let current_branch = get_current_branch(repo_path)?;
    if current_branch == name && !options.force {
        return Err(anyhow!(
            "Cannot delete the current branch '{}'. \
             Switch to another branch first, or use --force.",
            name
        ));
    }

    // Delete the branch file
    fs::remove_file(&branch_path).with_context(|| format!("Failed to delete branch '{}'", name))?;

    if let Err(e) = remove_branch_state(repo_path, name) {
        eprintln!("Warning: Failed to remove branch state: {}", e);
    }

    if options.verbose {
        println!("Deleted branch '{}'", name);
    }

    Ok(())
}

/// Rename a branch
pub fn rename_branch(
    repo_path: &Path,
    old_name: &str,
    new_name: &str,
    options: BranchOptions,
) -> Result<()> {
    // Validate new branch name
    validate_branch_name(new_name)?;

    let old_path = repo_path.join(format!(".helix/refs/heads/{}", old_name));
    let new_path = repo_path.join(format!(".helix/refs/heads/{}", new_name));

    // Check if old branch exists
    if !old_path.exists() {
        return Err(anyhow!("Branch '{}' does not exist", old_name));
    }

    // Check if new branch already exists
    if new_path.exists() && !options.force {
        return Err(anyhow!(
            "Branch '{}' already exists. Use --force to overwrite.",
            new_name
        ));
    }

    // Read the commit hash
    let commit_hash = fs::read_to_string(&old_path)?;

    // Write to new location
    fs::write(&new_path, &commit_hash)?;

    // Delete old branch
    fs::remove_file(&old_path)?;

    // Update HEAD if renaming current branch
    let current_branch = get_current_branch(repo_path)?;
    if current_branch == old_name {
        let head_path = repo_path.join(".helix/HEAD");
        fs::write(&head_path, format!("ref: refs/heads/{}\n", new_name))?;
    }

    if let Some(upstream) = get_branch_upstream(repo_path, old_name) {
        if let Err(e) = remove_branch_state(repo_path, old_name) {
            eprintln!("Warning: Failed to remove old branch state: {}", e);
        }
        if let Err(e) = set_branch_upstream(repo_path, new_name, &upstream) {
            eprintln!("Warning: Failed to set upstream for renamed branch: {}", e);
        }
    }

    if options.verbose {
        println!("Renamed branch '{}' to '{}'", old_name, new_name);
    }

    Ok(())
}

/// Switch to a different branch (checkout)
pub fn switch_branch(repo_path: &Path, name: &str) -> Result<()> {
    // Check if it's a sandbox branch
    if name.starts_with("sandboxes/") {
        let sandbox_name = name.strip_prefix("sandboxes/").unwrap();
        let sandbox_ref_path = repo_path.join(format!(".helix/refs/sandboxes/{}", sandbox_name));

        if !sandbox_ref_path.exists() {
            return Err(anyhow!(
                "Sandbox '{}' does not exist. Create it with 'helix sandbox create {}'",
                sandbox_name,
                sandbox_name
            ));
        }

        // Update HEAD to point to sandbox ref
        let head_path = repo_path.join(".helix/HEAD");
        fs::write(
            &head_path,
            format!("ref: refs/sandboxes/{}\n", sandbox_name),
        )?;

        println!("Switched to sandbox '{}'", sandbox_name);
        return Ok(());
    }

    // Regular branch
    let branch_path = repo_path.join(format!(".helix/refs/heads/{}", name));

    if !branch_path.exists() {
        return Err(anyhow!(
            "Branch '{}' does not exist. Create it with 'helix branch {}'",
            name,
            name
        ));
    }

    // Update HEAD to point to new branch
    let head_path = repo_path.join(".helix/HEAD");
    fs::write(&head_path, format!("ref: refs/heads/{}\n", name))?;

    println!("Switched to branch '{}'", name);

    Ok(())
}

/// Get the current branch name
pub fn get_current_branch(repo_path: &Path) -> Result<String> {
    let head_path = repo_path.join(".helix/HEAD");

    if !head_path.exists() {
        return Ok("(no branch)".to_string());
    }

    let content = fs::read_to_string(&head_path)?;
    let content = content.trim();

    if content.starts_with("ref:") {
        // Symbolic reference: "ref: refs/heads/main"
        let ref_path = content.strip_prefix("ref:").unwrap().trim();

        if let Some(branch_name) = ref_path.strip_prefix("refs/heads/") {
            Ok(branch_name.to_string())
        } else {
            Ok("(unknown)".to_string())
        }
    } else {
        // Detached HEAD
        Ok("(detached HEAD)".to_string())
    }
}
pub fn get_all_branches(start_path: &Path) -> Result<Vec<String>> {
    // Use RepoContext to find the actual repo root
    let context = RepoContext::detect(start_path)?;
    let repo_root = &context.repo_root;

    let mut branches = Vec::new();

    // Regular branches from refs/heads
    let heads_dir = repo_root.join(".helix/refs/heads");
    if heads_dir.exists() {
        for entry in fs::read_dir(&heads_dir)? {
            let entry = entry?;
            let path = entry.path();

            if path.is_file() {
                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    branches.push(name.to_string());
                }
            }
        }
    }

    // Sandbox branches from refs/sandboxes (prefixed with "sandboxes/")
    let sandboxes_dir = repo_root.join(".helix/refs/sandboxes");
    if sandboxes_dir.exists() {
        for entry in fs::read_dir(&sandboxes_dir)? {
            let entry = entry?;
            let path = entry.path();

            if path.is_file() {
                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    branches.push(format!("sandboxes/{}", name));
                }
            }
        }
    }

    Ok(branches)
}

/// Read HEAD and return the commit hash
fn read_head(repo_path: &Path) -> Result<Hash> {
    let head_path = repo_path.join(".helix/HEAD");

    if !head_path.exists() {
        return Err(anyhow!("No commits yet."));
    }

    let content = fs::read_to_string(&head_path)?;
    let content = content.trim();

    if content.starts_with("ref:") {
        // Symbolic reference: read the branch file
        let ref_path = content.strip_prefix("ref:").unwrap().trim();
        let branch_path = repo_path.join(".helix").join(ref_path);

        if !branch_path.exists() {
            return Err(anyhow!("No commits yet'"));
        }

        let hash_str = fs::read_to_string(&branch_path)?;
        hex_to_hash(hash_str.trim())
    } else {
        // Direct hash
        hex_to_hash(content)
    }
}

/// Validate branch name (no special characters, slashes, etc.)
fn validate_branch_name(name: &str) -> Result<()> {
    if name.is_empty() {
        return Err(anyhow!("Branch name cannot be empty"));
    }

    // Check for invalid characters
    if name.contains('/') {
        return Err(anyhow!(
            "Branch name cannot contain '/'. Use simple names like 'main', 'dev', 'feature-x'"
        ));
    }

    if name.starts_with('.') || name.starts_with('-') {
        return Err(anyhow!("Branch name cannot start with '.' or '-'"));
    }

    if name.contains("..") {
        return Err(anyhow!("Branch name cannot contain '..'"));
    }

    // Reserved names
    if name == "HEAD" {
        return Err(anyhow!("'HEAD' is a reserved name"));
    }

    Ok(())
}

/// Get short hash (first 8 chars)
fn short_hash(hash: &Hash) -> String {
    hash_to_hex(hash)[..8].to_string()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::commit_command::{commit, CommitOptions};
    use crate::init_command::init_helix_repo;
    use tempfile::TempDir;

    fn init_test_repo(path: &Path) -> Result<()> {
        init_helix_repo(path, None)?;

        // Set up config
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

    fn make_initial_commit(repo_path: &Path) -> Result<Hash> {
        use crate::add_command::{add, AddOptions};

        // Create and add a file
        fs::write(repo_path.join("test.txt"), "content")?;
        add(
            repo_path,
            &[PathBuf::from("test.txt")],
            AddOptions::default(),
        )?;

        // Make initial commit
        commit(
            repo_path,
            CommitOptions {
                message: "Initial commit".to_string(),
                author: Some("Test <test@test.com>".to_string()),
                allow_empty: false,
                amend: false,
                verbose: false,
            },
        )
    }

    #[test]
    fn test_get_current_branch_before_commit() -> Result<()> {
        let temp_dir = TempDir::new()?;
        init_test_repo(temp_dir.path())?;

        let branch = get_current_branch(temp_dir.path())?;
        assert_eq!(branch, "main");

        Ok(())
    }

    #[test]
    fn test_create_branch() -> Result<()> {
        let temp_dir = TempDir::new()?;
        init_test_repo(temp_dir.path())?;
        make_initial_commit(temp_dir.path())?;

        // Create new branch
        create_branch(temp_dir.path(), "feature", BranchOptions::default())?;

        // Verify branch file exists
        let branch_path = temp_dir.path().join(".helix/refs/heads/feature");
        assert!(branch_path.exists());

        // Verify it points to current commit
        let main_hash = fs::read_to_string(temp_dir.path().join(".helix/refs/heads/main"))?;
        let feature_hash = fs::read_to_string(&branch_path)?;
        assert_eq!(main_hash.trim(), feature_hash.trim());

        Ok(())
    }

    #[test]
    fn test_create_branch_already_exists() -> Result<()> {
        let temp_dir = TempDir::new()?;
        init_test_repo(temp_dir.path())?;
        make_initial_commit(temp_dir.path())?;

        create_branch(temp_dir.path(), "feature", BranchOptions::default())?;

        // Try to create again - should fail
        let result = create_branch(temp_dir.path(), "feature", BranchOptions::default());
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("already exists"));

        Ok(())
    }

    #[test]
    fn test_create_branch_with_force() -> Result<()> {
        let temp_dir = TempDir::new()?;
        init_test_repo(temp_dir.path())?;
        make_initial_commit(temp_dir.path())?;

        create_branch(temp_dir.path(), "feature", BranchOptions::default())?;

        // Create again with force - should succeed
        let result = create_branch(
            temp_dir.path(),
            "feature",
            BranchOptions {
                force: true,
                ..Default::default()
            },
        );
        assert!(result.is_ok());

        Ok(())
    }

    #[test]
    fn test_delete_branch() -> Result<()> {
        let temp_dir = TempDir::new()?;
        init_test_repo(temp_dir.path())?;
        make_initial_commit(temp_dir.path())?;

        create_branch(temp_dir.path(), "feature", BranchOptions::default())?;

        // Delete the branch
        delete_branch(temp_dir.path(), "feature", BranchOptions::default())?;

        // Verify it's gone
        let branch_path = temp_dir.path().join(".helix/refs/heads/feature");
        assert!(!branch_path.exists());

        Ok(())
    }

    #[test]
    fn test_delete_current_branch_fails() -> Result<()> {
        let temp_dir = TempDir::new()?;
        init_test_repo(temp_dir.path())?;
        make_initial_commit(temp_dir.path())?;

        // Try to delete current branch - should fail
        let result = delete_branch(temp_dir.path(), "main", BranchOptions::default());
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("current branch"));

        Ok(())
    }

    #[test]
    fn test_switch_branch() -> Result<()> {
        let temp_dir = TempDir::new()?;
        init_test_repo(temp_dir.path())?;
        make_initial_commit(temp_dir.path())?;

        create_branch(temp_dir.path(), "feature", BranchOptions::default())?;

        // Switch to feature branch
        switch_branch(temp_dir.path(), "feature")?;

        // Verify current branch
        let current = get_current_branch(temp_dir.path())?;
        assert_eq!(current, "feature");

        // Switch back to main
        switch_branch(temp_dir.path(), "main")?;
        let current = get_current_branch(temp_dir.path())?;
        assert_eq!(current, "main");

        Ok(())
    }

    #[test]
    fn test_rename_branch() -> Result<()> {
        let temp_dir = TempDir::new()?;
        init_test_repo(temp_dir.path())?;
        make_initial_commit(temp_dir.path())?;

        create_branch(temp_dir.path(), "old-name", BranchOptions::default())?;

        // Rename the branch
        rename_branch(
            temp_dir.path(),
            "old-name",
            "new-name",
            BranchOptions::default(),
        )?;

        // Verify old name gone
        let old_path = temp_dir.path().join(".helix/refs/heads/old-name");
        assert!(!old_path.exists());

        // Verify new name exists
        let new_path = temp_dir.path().join(".helix/refs/heads/new-name");
        assert!(new_path.exists());

        Ok(())
    }

    #[test]
    fn test_rename_current_branch_updates_head() -> Result<()> {
        let temp_dir = TempDir::new()?;
        init_test_repo(temp_dir.path())?;
        make_initial_commit(temp_dir.path())?;

        // Rename current branch (main)
        rename_branch(temp_dir.path(), "main", "master", BranchOptions::default())?;

        // Verify HEAD updated
        let current = get_current_branch(temp_dir.path())?;
        assert_eq!(current, "master");

        Ok(())
    }

    #[test]
    fn test_list_branches() -> Result<()> {
        let temp_dir = TempDir::new()?;
        init_test_repo(temp_dir.path())?;
        make_initial_commit(temp_dir.path())?;

        create_branch(temp_dir.path(), "feature1", BranchOptions::default())?;
        create_branch(temp_dir.path(), "feature2", BranchOptions::default())?;

        let branches = get_all_branches(temp_dir.path())?;

        assert_eq!(branches.len(), 3);
        assert!(branches.contains(&"main".to_string()));
        assert!(branches.contains(&"feature1".to_string()));
        assert!(branches.contains(&"feature2".to_string()));

        Ok(())
    }

    #[test]
    fn test_validate_branch_name() -> Result<()> {
        // Valid names
        assert!(validate_branch_name("main").is_ok());
        assert!(validate_branch_name("feature").is_ok());
        assert!(validate_branch_name("bug-fix").is_ok());
        assert!(validate_branch_name("dev_123").is_ok());

        // Invalid names
        assert!(validate_branch_name("").is_err());
        assert!(validate_branch_name("feature/test").is_err());
        assert!(validate_branch_name(".hidden").is_err());
        assert!(validate_branch_name("-bad").is_err());
        assert!(validate_branch_name("bad..name").is_err());
        assert!(validate_branch_name("HEAD").is_err());

        Ok(())
    }

    #[test]
    fn test_branch_without_commits() -> Result<()> {
        let temp_dir = TempDir::new()?;
        init_test_repo(temp_dir.path())?;

        // Try to create branch before any commits
        let result = create_branch(temp_dir.path(), "feature", BranchOptions::default());
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("No commits yet"));

        Ok(())
    }
}
