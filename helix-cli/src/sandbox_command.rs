// Sandbox management for Helix

use anyhow::{anyhow, bail, Context, Result};
use helix_protocol::storage::FsRefStore;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

use crate::checkout::{checkout_tree_to_path, CheckoutOptions};
use crate::sandbox_tui;
use helix_protocol::hash::{hash_to_hex, hex_to_hash, Hash};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxManifest {
    pub id: String,
    pub name: String,
    pub base_commit: String,
    pub created_at: u64,
    pub description: Option<String>,
    pub branch: Option<String>,
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

pub struct DestroyOptions {
    force: bool,
    verbose: bool,
}

impl Default for DestroyOptions {
    fn default() -> Self {
        Self {
            force: false,
            verbose: false,
        }
    }
}

impl Default for CreateOptions {
    fn default() -> Self {
        Self {
            base_commit: None,
            verbose: false,
        }
    }
}

pub struct MergeOptions {
    pub into_branch: Option<String>,
    pub verbose: bool,
}

impl Default for MergeOptions {
    fn default() -> Self {
        Self {
            into_branch: None,
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
            branch: None,
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

    pub fn base_commit_hash(&self) -> Result<Hash> {
        hex_to_hash(&self.base_commit)
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

// TODO: we should just combine this with the status_tui's FileStatus and make it generic
// we should be able to compare a dir against a commit regardless of where it is, but it's easier now to juts have it separate
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SandboxChangeKind {
    Added,
    Modified,
    Deleted,
}

#[derive(Debug, Clone)]
pub struct SandboxChange {
    pub path: PathBuf,
    pub kind: SandboxChangeKind,
}

impl SandboxChange {
    pub fn status_char(&self) -> char {
        match self.kind {
            SandboxChangeKind::Added => 'A',
            SandboxChangeKind::Modified => 'M',
            SandboxChangeKind::Deleted => 'D',
        }
    }
}

/// Create a new sandbox
pub fn create_sandbox(repo_path: &Path, name: &str, options: CreateOptions) -> Result<Sandbox> {
    validate_sandbox_name(name)?;

    let sandbox_root = repo_path.join(".helix").join("sandboxes").join(name);
    let workdir = sandbox_root.join("workdir");

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

    // create a branch for the sandbox with the name as the branch_name
    let branch_name = format!("sandbox/{}", name);
    let ref_name = format!("refs/heads/{}", branch_name);
    let refs = FsRefStore::new(repo_path);

    refs.set_ref(&ref_name, base_commit)
        .with_context(|| format!("Failed to create branch '{}'", branch_name))?;

    let mut manifest = SandboxManifest::new(name, base_commit);
    manifest.branch = Some(branch_name.clone());
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
    println!(
        "Created sandbox '{}' ({} files)\n  workdir: {}\n  branch:  {}",
        name,
        files_count,
        workdir.display(),
        branch_name
    );

    Ok(Sandbox {
        manifest,
        root: sandbox_root,
        workdir,
    })
}

/// Delete a sandbox
pub fn destroy_sandbox(repo_path: &Path, name: &str, options: DestroyOptions) -> Result<()> {
    let sandbox_root = repo_path.join(".helix").join("sandboxes").join(name);

    if !sandbox_root.exists() {
        bail!("Sandbox '{}' does not exist", name);
    }

    // Load manifest to get branch name
    let manifest = SandboxManifest::load(&sandbox_root)?;

    // Check for uncommitted changes unless force
    if !options.force {}

    // Delete the sandbox directory
    fs::remove_dir_all(&sandbox_root).with_context(|| {
        format!(
            "Failed to remove sandbox directory {}",
            sandbox_root.display()
        )
    })?;

    // Delete the branch
    if let Some(branch_name) = manifest.branch {
        let ref_path = repo_path
            .join(".helix")
            .join("refs")
            .join("heads")
            .join(&branch_name);
        if ref_path.exists() {
            fs::remove_file(&ref_path).ok(); // Ignore errors, branch might not exist
            if options.verbose {
                println!("Deleted branch '{}'", branch_name);
            }
        }
    }

    println!("Destroyed sandbox '{}'", name);

    Ok(())
}

/// Switch to a different sandbox (checkout)
pub fn switch_sandbox(repo_path: &Path, name: &str) -> Result<()> {
    let sandbox_path = repo_path.join(format!(".helix/refs/heads/{}", name));

    // Check if sandbox exists
    if !sandbox_path.exists() {
        return Err(anyhow!(
            "sandbox '{}' does not exist. Create it with 'helix sandbox {}'",
            name,
            name
        ));
    }

    // Update HEAD to point to new sandbox
    let head_path = repo_path.join(".helix/HEAD");
    fs::write(&head_path, format!("ref: refs/heads/{}\n", name))?;

    println!("Switched to sandbox '{}'", name);

    Ok(())
}

/// Get the current sandbox name
pub fn get_current_sandbox(repo_path: &Path) -> Result<String> {
    let head_path = repo_path.join(".helix/HEAD");

    if !head_path.exists() {
        return Ok("(no sandbox)".to_string());
    }

    let content = fs::read_to_string(&head_path)?;
    let content = content.trim();

    if content.starts_with("ref:") {
        // Symbolic reference: "ref: refs/heads/main"
        let ref_path = content.strip_prefix("ref:").unwrap().trim();

        if let Some(sandbox_name) = ref_path.strip_prefix("refs/heads/") {
            Ok(sandbox_name.to_string())
        } else {
            Ok("(unknown)".to_string())
        }
    } else {
        // Detached HEAD
        Ok("(detached HEAD)".to_string())
    }
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
        // Symbolic reference: read the sandbox file
        let ref_path = content.strip_prefix("ref:").unwrap().trim();
        let sandbox_path = repo_path.join(".helix").join(ref_path);

        if !sandbox_path.exists() {
            return Err(anyhow!("No commits yet'"));
        }

        let hash_str = fs::read_to_string(&sandbox_path)?;
        hex_to_hash(hash_str.trim())
    } else {
        // Direct hash
        hex_to_hash(content)
    }
}

/// Validate sandbox name (no special characters, slashes, etc.)
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
//     fn test_get_current_sandbox_before_commit() -> Result<()> {
//         let temp_dir = TempDir::new()?;
//         init_test_repo(temp_dir.path())?;

//         let sandbox = get_current_sandbox(temp_dir.path())?;
//         assert_eq!(sandbox, "main");

//         Ok(())
//     }

//     #[test]
//     fn test_create_sandbox() -> Result<()> {
//         let temp_dir = TempDir::new()?;
//         init_test_repo(temp_dir.path())?;
//         make_initial_commit(temp_dir.path())?;

//         // Create new sandbox
//         create_sandbox(temp_dir.path(), "feature", CreateOptions::default())?;

//         // Verify sandbox file exists
//         let sandbox_path = temp_dir.path().join(".helix/refs/heads/feature");
//         assert!(sandbox_path.exists());

//         // Verify it points to current commit
//         let main_hash = fs::read_to_string(temp_dir.path().join(".helix/refs/heads/main"))?;
//         let feature_hash = fs::read_to_string(&sandbox_path)?;
//         assert_eq!(main_hash.trim(), feature_hash.trim());

//         Ok(())
//     }

//     #[test]
//     fn test_create_sandbox_already_exists() -> Result<()> {
//         let temp_dir = TempDir::new()?;
//         init_test_repo(temp_dir.path())?;
//         make_initial_commit(temp_dir.path())?;

//         create_sandbox(temp_dir.path(), "feature", CreateOptions::default())?;

//         // Try to create again - should fail
//         let result = create_sandbox(temp_dir.path(), "feature", CreateOptions::default());
//         assert!(result.is_err());
//         assert!(result.unwrap_err().to_string().contains("already exists"));

//         Ok(())
//     }

//     #[test]
//     fn test_create_sandbox_with_force() -> Result<()> {
//         let temp_dir = TempDir::new()?;
//         init_test_repo(temp_dir.path())?;
//         make_initial_commit(temp_dir.path())?;

//         create_sandbox(temp_dir.path(), "feature", CreateOptions::default())?;

//         // Create again with force - should succeed
//         let result = create_sandbox(
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
//     fn test_delete_sandbox() -> Result<()> {
//         let temp_dir = TempDir::new()?;
//         init_test_repo(temp_dir.path())?;
//         make_initial_commit(temp_dir.path())?;

//         create_sandbox(temp_dir.path(), "feature", CreateOptions::default())?;

//         // Delete the sandbox
//         delete_sandbox(temp_dir.path(), "feature", CreateOptions::default())?;

//         // Verify it's gone
//         let sandbox_path = temp_dir.path().join(".helix/refs/heads/feature");
//         assert!(!sandbox_path.exists());

//         Ok(())
//     }

//     #[test]
//     fn test_delete_current_sandbox_fails() -> Result<()> {
//         let temp_dir = TempDir::new()?;
//         init_test_repo(temp_dir.path())?;
//         make_initial_commit(temp_dir.path())?;

//         // Try to delete current sandbox - should fail
//         let result = delete_sandbox(temp_dir.path(), "main", CreateOptions::default());
//         assert!(result.is_err());
//         assert!(result.unwrap_err().to_string().contains("current sandbox"));

//         Ok(())
//     }

//     #[test]
//     fn test_switch_sandbox() -> Result<()> {
//         let temp_dir = TempDir::new()?;
//         init_test_repo(temp_dir.path())?;
//         make_initial_commit(temp_dir.path())?;

//         create_sandbox(temp_dir.path(), "feature", CreateOptions::default())?;

//         // Switch to feature sandbox
//         switch_sandbox(temp_dir.path(), "feature")?;

//         // Verify current sandbox
//         let current = get_current_sandbox(temp_dir.path())?;
//         assert_eq!(current, "feature");

//         // Switch back to main
//         switch_sandbox(temp_dir.path(), "main")?;
//         let current = get_current_sandbox(temp_dir.path())?;
//         assert_eq!(current, "main");

//         Ok(())
//     }

//     #[test]
//     fn test_rename_sandbox() -> Result<()> {
//         let temp_dir = TempDir::new()?;
//         init_test_repo(temp_dir.path())?;
//         make_initial_commit(temp_dir.path())?;

//         create_sandbox(temp_dir.path(), "old-name", CreateOptions::default())?;

//         // Rename the sandbox
//         rename_sandbox(
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
//     fn test_rename_current_sandbox_updates_head() -> Result<()> {
//         let temp_dir = TempDir::new()?;
//         init_test_repo(temp_dir.path())?;
//         make_initial_commit(temp_dir.path())?;

//         // Rename current sandbox (main)
//         rename_sandbox(temp_dir.path(), "main", "master", CreateOptions::default())?;

//         // Verify HEAD updated
//         let current = get_current_sandbox(temp_dir.path())?;
//         assert_eq!(current, "master");

//         Ok(())
//     }

//     #[test]
//     fn test_list_sandboxes() -> Result<()> {
//         let temp_dir = TempDir::new()?;
//         init_test_repo(temp_dir.path())?;
//         make_initial_commit(temp_dir.path())?;

//         create_sandbox(temp_dir.path(), "feature1", CreateOptions::default())?;
//         create_sandbox(temp_dir.path(), "feature2", CreateOptions::default())?;

//         let sandboxes = get_all_sandboxes(temp_dir.path())?;

//         assert_eq!(sandboxes.len(), 3);
//         assert!(sandboxes.contains(&"main".to_string()));
//         assert!(sandboxes.contains(&"feature1".to_string()));
//         assert!(sandboxes.contains(&"feature2".to_string()));

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
//     fn test_sandbox_without_commits() -> Result<()> {
//         let temp_dir = TempDir::new()?;
//         init_test_repo(temp_dir.path())?;

//         // Try to create sandbox before any commits
//         let result = create_sandbox(temp_dir.path(), "feature", CreateOptions::default());
//         assert!(result.is_err());
//         assert!(result.unwrap_err().to_string().contains("No commits yet"));

//         Ok(())
//     }
// }
