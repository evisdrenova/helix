// Sandbox management for Helix

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

use crate::checkout::{checkout_tree_to_path, CheckoutOptions};
use crate::helix_index::state::{get_branch_upstream, remove_branch_state, set_branch_upstream};
use crate::sandbox_tui;
use helix_protocol::hash::{hash_to_hex, hex_to_hash, Hash};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxManifest {
    pub id: String,
    pub name: String,
    pub base_commit: String,
    pub created_at: u64,
    pub description: Option<String>,
}

pub struct Sandbox {
    pub manifest: SandboxManifest,
    pub root: PathBuf,
    pub workdir: PathBuf,
}

pub struct CreateOptions {
    pub base_commit: Option<Hash>,
    pub verbose: bool,
}

impl Default for CreateOptions {
    fn default() -> Self {
        Self {
            base_commit: None,
            verbose: false,
        }
    }
}

impl SandboxManifest {
    pub fn new(name: &str, base_commit: Hash) -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        Self {
            id: Uuid::new_v4().to_string(),
            name: name.to_string(),
            base_commit: hash_to_hex(&base_commit),
            created_at: now,
            description: None,
        }
    }

    pub fn save(&self, sandbox_root: &Path) -> Result<()> {
        let manifest_path = sandbox_root.join("manifest.toml");
        let content = toml::to_string_pretty(self).context("Failed to serialize manifest")?;
        fs::write(&manifest_path, content).context("Failed to write manifest")?;
        Ok(())
    }

    pub fn load(sandbox_root: &Path) -> Result<Self> {
        let manifest_path = sandbox_root.join("manifest.toml");
        let content = fs::read_to_string(&manifest_path)
            .with_context(|| format!("Failed to read manifest at {}", manifest_path.display()))?;
        toml::from_str(&content).context("Failed to parse sandbox manifest")
    }
}

pub fn run_sandbox_tui(repo_path: Option<&Path>) -> Result<()> {
    let repo_path = repo_path
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| std::env::current_dir().expect("Failed to get current directory"));

    let mut app = sandbox_tui::app::App::new(&repo_path)?;
    app.run()?;

    Ok(())
}

/// Create a new sandbox
pub fn create_sandbox(repo_path: &Path, name: &str, options: CreateOptions) -> Result<Sandbox> {
    validate_sandbox_name(name)?;

    let sandbox_root = repo_path.join(".helix").join("sandboxes").join(name);
    let workdir = sandbox_root.join("workdir");

    // Check if sandbox already exists
    if sandbox_root.exists() {
        bail!("Sandbox '{}' already exists", name);
    }

    // Get base commit (default to HEAD)
    let base_commit = match options.base_commit {
        Some(hash) => hash,
        None => read_head(repo_path).context("No HEAD found. Create a commit first.")?,
    };

    if options.verbose {
        println!(
            "Creating sandbox '{}' from commit {}",
            name,
            &hash_to_hex(&base_commit)[..8]
        );
    }

    // Create sandbox directories
    fs::create_dir_all(&workdir)
        .with_context(|| format!("Failed to create sandbox directory {}", workdir.display()))?;

    let checkout_options = CheckoutOptions {
        verbose: options.verbose,
        force: true,
    };

    let files_count = checkout_tree_to_path(repo_path, &base_commit, &workdir, &checkout_options)?;

    // Create and save manifest
    let manifest = SandboxManifest::new(name, base_commit);
    manifest.save(&sandbox_root)?;

    if options.verbose {
        println!(
            "Created sandbox '{}' with {} files (base: {})",
            name,
            files_count,
            &manifest.base_commit[..8]
        );
    } else {
        println!("Created sandbox '{}' ({} files)", name, files_count);
    }

    Ok(Sandbox {
        manifest,
        root: sandbox_root,
        workdir,
    })
}

/// Delete a branch
pub fn delete_branch(repo_path: &Path, name: &str, options: CreateOptions) -> Result<()> {
    let branch_path = repo_path.join(format!(".helix/refs/heads/{}", name));

    // Check if branch exists
    if !branch_path.exists() {
        return Err(anyhow!("Branch '{}' does not exist", name));
    }

    // Check if trying to delete current branch
    let current_branch = get_current_branch(repo_path)?;
    if current_branch == name {
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
    options: CreateOptions,
) -> Result<()> {
    // Validate new branch name
    validate_sandbox_name(new_name)?;

    let old_path = repo_path.join(format!(".helix/refs/heads/{}", old_name));
    let new_path = repo_path.join(format!(".helix/refs/heads/{}", new_name));

    // Check if old branch exists
    if !old_path.exists() {
        return Err(anyhow!("Branch '{}' does not exist", old_name));
    }

    // Check if new branch already exists
    if new_path.exists() {
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
    let branch_path = repo_path.join(format!(".helix/refs/heads/{}", name));

    // Check if branch exists
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

/// Get all branch names
pub fn get_all_branches(repo_path: &Path) -> Result<Vec<String>> {
    let refs_dir = repo_path.join(".helix/refs/heads");

    if !refs_dir.exists() {
        return Ok(vec![]);
    }

    let mut branches = Vec::new();

    for entry in fs::read_dir(refs_dir)? {
        let entry = entry?;
        let path = entry.path();

        if path.is_file() {
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                branches.push(name.to_string());
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
/// Validate sandbox name (alphanumeric, hyphens, underscores only)
fn validate_sandbox_name(name: &str) -> Result<()> {
    if name.is_empty() {
        bail!("Sandbox name cannot be empty");
    }

    if name.len() > 64 {
        bail!("Sandbox name too long (max 64 characters)");
    }

    if !name
        .chars()
        .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
    {
        bail!(
            "Invalid sandbox name '{}'. Use only alphanumeric characters, hyphens, and underscores.",
            name
        );
    }

    Ok(())
}

/// Get short hash (first 8 chars)
fn short_hash(hash: &Hash) -> String {
    hash_to_hex(hash)[..8].to_string()
}

// #[cfg(test)]
// mod tests {
//     use std::path::PathBuf;

//     use super::*;
//     use crate::commit_command::{commit, CommitOptions};
//     use crate::init_command::init_helix_repo;
//     use tempfile::TempDir;

//     fn init_test_repo(path: &Path) -> Result<()> {
//         init_helix_repo(path, None)?;

//         // Set up config
//         let config_path = path.join("helix.toml");
//         fs::write(
//             &config_path,
//             r#"
// [user]
// name = "Test User"
// email = "test@test.com"
// "#,
//         )?;

//         Ok(())
//     }

//     fn make_initial_commit(repo_path: &Path) -> Result<Hash> {
//         use crate::add_command::{add, AddOptions};

//         // Create and add a file
//         fs::write(repo_path.join("test.txt"), "content")?;
//         add(
//             repo_path,
//             &[PathBuf::from("test.txt")],
//             AddOptions::default(),
//         )?;

//         // Make initial commit
//         commit(
//             repo_path,
//             CommitOptions {
//                 message: "Initial commit".to_string(),
//                 author: Some("Test <test@test.com>".to_string()),
//                 allow_empty: false,
//                 amend: false,
//                 verbose: false,
//             },
//         )
//     }

//     #[test]
//     fn test_get_current_branch_before_commit() -> Result<()> {
//         let temp_dir = TempDir::new()?;
//         init_test_repo(temp_dir.path())?;

//         let branch = get_current_branch(temp_dir.path())?;
//         assert_eq!(branch, "main");

//         Ok(())
//     }

//     #[test]
//     fn test_create_branch() -> Result<()> {
//         let temp_dir = TempDir::new()?;
//         init_test_repo(temp_dir.path())?;
//         make_initial_commit(temp_dir.path())?;

//         // Create new branch
//         create_branch(temp_dir.path(), "feature", CreateOptions::default())?;

//         // Verify branch file exists
//         let branch_path = temp_dir.path().join(".helix/refs/heads/feature");
//         assert!(branch_path.exists());

//         // Verify it points to current commit
//         let main_hash = fs::read_to_string(temp_dir.path().join(".helix/refs/heads/main"))?;
//         let feature_hash = fs::read_to_string(&branch_path)?;
//         assert_eq!(main_hash.trim(), feature_hash.trim());

//         Ok(())
//     }

//     #[test]
//     fn test_create_branch_already_exists() -> Result<()> {
//         let temp_dir = TempDir::new()?;
//         init_test_repo(temp_dir.path())?;
//         make_initial_commit(temp_dir.path())?;

//         create_branch(temp_dir.path(), "feature", CreateOptions::default())?;

//         // Try to create again - should fail
//         let result = create_branch(temp_dir.path(), "feature", CreateOptions::default());
//         assert!(result.is_err());
//         assert!(result.unwrap_err().to_string().contains("already exists"));

//         Ok(())
//     }

//     #[test]
//     fn test_create_branch_with_force() -> Result<()> {
//         let temp_dir = TempDir::new()?;
//         init_test_repo(temp_dir.path())?;
//         make_initial_commit(temp_dir.path())?;

//         create_branch(temp_dir.path(), "feature", CreateOptions::default())?;

//         // Create again with force - should succeed
//         let result = create_branch(
//             temp_dir.path(),
//             "feature",
//             CreateOptions {
//                 ..Default::default()
//             },
//         );
//         assert!(result.is_ok());

//         Ok(())
//     }

//     #[test]
//     fn test_delete_branch() -> Result<()> {
//         let temp_dir = TempDir::new()?;
//         init_test_repo(temp_dir.path())?;
//         make_initial_commit(temp_dir.path())?;

//         create_branch(temp_dir.path(), "feature", CreateOptions::default())?;

//         // Delete the branch
//         delete_branch(temp_dir.path(), "feature", CreateOptions::default())?;

//         // Verify it's gone
//         let branch_path = temp_dir.path().join(".helix/refs/heads/feature");
//         assert!(!branch_path.exists());

//         Ok(())
//     }

//     #[test]
//     fn test_delete_current_branch_fails() -> Result<()> {
//         let temp_dir = TempDir::new()?;
//         init_test_repo(temp_dir.path())?;
//         make_initial_commit(temp_dir.path())?;

//         // Try to delete current branch - should fail
//         let result = delete_branch(temp_dir.path(), "main", CreateOptions::default());
//         assert!(result.is_err());
//         assert!(result.unwrap_err().to_string().contains("current branch"));

//         Ok(())
//     }

//     #[test]
//     fn test_switch_branch() -> Result<()> {
//         let temp_dir = TempDir::new()?;
//         init_test_repo(temp_dir.path())?;
//         make_initial_commit(temp_dir.path())?;

//         create_branch(temp_dir.path(), "feature", CreateOptions::default())?;

//         // Switch to feature branch
//         switch_branch(temp_dir.path(), "feature")?;

//         // Verify current branch
//         let current = get_current_branch(temp_dir.path())?;
//         assert_eq!(current, "feature");

//         // Switch back to main
//         switch_branch(temp_dir.path(), "main")?;
//         let current = get_current_branch(temp_dir.path())?;
//         assert_eq!(current, "main");

//         Ok(())
//     }

//     #[test]
//     fn test_rename_branch() -> Result<()> {
//         let temp_dir = TempDir::new()?;
//         init_test_repo(temp_dir.path())?;
//         make_initial_commit(temp_dir.path())?;

//         create_branch(temp_dir.path(), "old-name", CreateOptions::default())?;

//         // Rename the branch
//         rename_branch(
//             temp_dir.path(),
//             "old-name",
//             "new-name",
//             CreateOptions::default(),
//         )?;

//         // Verify old name gone
//         let old_path = temp_dir.path().join(".helix/refs/heads/old-name");
//         assert!(!old_path.exists());

//         // Verify new name exists
//         let new_path = temp_dir.path().join(".helix/refs/heads/new-name");
//         assert!(new_path.exists());

//         Ok(())
//     }

//     #[test]
//     fn test_rename_current_branch_updates_head() -> Result<()> {
//         let temp_dir = TempDir::new()?;
//         init_test_repo(temp_dir.path())?;
//         make_initial_commit(temp_dir.path())?;

//         // Rename current branch (main)
//         rename_branch(temp_dir.path(), "main", "master", CreateOptions::default())?;

//         // Verify HEAD updated
//         let current = get_current_branch(temp_dir.path())?;
//         assert_eq!(current, "master");

//         Ok(())
//     }

//     #[test]
//     fn test_list_branches() -> Result<()> {
//         let temp_dir = TempDir::new()?;
//         init_test_repo(temp_dir.path())?;
//         make_initial_commit(temp_dir.path())?;

//         create_branch(temp_dir.path(), "feature1", CreateOptions::default())?;
//         create_branch(temp_dir.path(), "feature2", CreateOptions::default())?;

//         let branches = get_all_branches(temp_dir.path())?;

//         assert_eq!(branches.len(), 3);
//         assert!(branches.contains(&"main".to_string()));
//         assert!(branches.contains(&"feature1".to_string()));
//         assert!(branches.contains(&"feature2".to_string()));

//         Ok(())
//     }

//     #[test]
//     fn test_validate_sandbox_name() -> Result<()> {
//         // Valid names
//         assert!(validate_sandbox_name("main").is_ok());
//         assert!(validate_sandbox_name("feature").is_ok());
//         assert!(validate_sandbox_name("bug-fix").is_ok());
//         assert!(validate_sandbox_name("dev_123").is_ok());

//         // Invalid names
//         assert!(validate_sandbox_name("").is_err());
//         assert!(validate_sandbox_name("feature/test").is_err());
//         assert!(validate_sandbox_name(".hidden").is_err());
//         assert!(validate_sandbox_name("-bad").is_err());
//         assert!(validate_sandbox_name("bad..name").is_err());
//         assert!(validate_sandbox_name("HEAD").is_err());

//         Ok(())
//     }

//     #[test]
//     fn test_branch_without_commits() -> Result<()> {
//         let temp_dir = TempDir::new()?;
//         init_test_repo(temp_dir.path())?;

//         // Try to create branch before any commits
//         let result = create_branch(temp_dir.path(), "feature", CreateOptions::default());
//         assert!(result.is_err());
//         assert!(result.unwrap_err().to_string().contains("No commits yet"));

//         Ok(())
//     }
// }
