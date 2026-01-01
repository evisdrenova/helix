// Sandbox management for Helix

use anyhow::{bail, Context, Result};
use console::style;
use helix_protocol::message::ObjectType;
use helix_protocol::storage::{FsObjectStore, FsRefStore};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

use crate::add_command::get_file_mode;
use crate::checkout::{checkout_tree_to_path, CheckoutOptions};
use crate::helix_index::commit::{read_head, Commit};
use crate::helix_index::tree::{TreeBuilder, TreeStore};
use crate::helix_index::{Entry, EntryFlags, Header, Writer};
use crate::sandbox_tui;
use helix_protocol::hash::{hash_bytes, hash_to_hex, hex_to_hash, Hash};

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
impl Default for CreateOptions {
    fn default() -> Self {
        Self {
            base_commit: None,
            verbose: false,
        }
    }
}

pub struct DestroyOptions {
    pub force: bool,
    pub verbose: bool,
}

impl Default for DestroyOptions {
    fn default() -> Self {
        Self {
            force: false,
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

pub struct ListOptions {
    pub verbose: bool,
}

impl Default for ListOptions {
    fn default() -> Self {
        Self { verbose: false }
    }
}

pub struct CommitOptions {
    pub message: String,
    pub author: Option<String>,
    pub verbose: bool,
}

// Manifest file for each sandbox
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

/// Represents the current working context - either main repo or a sandbox
#[derive(Clone, Debug)]
pub struct RepoContext {
    pub repo_root: PathBuf,
    pub sandbox_root: Option<PathBuf>,
    pub workdir: PathBuf,
    pub index_path: PathBuf,
}

impl RepoContext {
    pub fn detect(start_path: &Path) -> Result<Self> {
        let start_path = start_path.canonicalize()?;

        // check if we're inside a sandbox workdir
        if let Some((sandbox_root, repo_root)) = detect_sandbox_from_path(&start_path) {
            return Ok(Self {
                repo_root: repo_root.clone(),
                sandbox_root: Some(sandbox_root.clone()),
                workdir: sandbox_root.join("workdir"),
                index_path: sandbox_root.join("helix.idx"),
            });
        }

        // Otherwise, find the repo root by looking for .helix
        let repo_root = find_repo_root(&start_path)?;

        Ok(Self {
            repo_root: repo_root.clone(),
            sandbox_root: None,
            workdir: repo_root.clone(),
            index_path: repo_root.join(".helix").join("helix.idx"),
        })
    }

    /// Check if we're in a sandbox
    pub fn is_sandbox(&self) -> bool {
        self.sandbox_root.is_some()
    }

    /// Get sandbox name if in a sandbox
    pub fn sandbox_name(&self) -> Option<String> {
        self.sandbox_root.as_ref().and_then(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|s| s.to_string())
        })
    }
}

fn detect_sandbox_from_path(path: &Path) -> Option<(PathBuf, PathBuf)> {
    for ancestor in path.ancestors() {
        // Check if this looks like a sandbox workdir
        if ancestor.file_name().and_then(|n| n.to_str()) == Some("workdir") {
            let potential_sandbox_root = ancestor.parent()?;

            // Verify it's a sandbox by checking for manifest.toml AND .helix dir
            if potential_sandbox_root.join("manifest.toml").exists()
                && potential_sandbox_root.join(".helix").is_dir()
            {
                // Find repo root (go up from .helix/sandboxes/<name>)
                let sandboxes_dir = potential_sandbox_root.parent()?;
                let helix_dir = sandboxes_dir.parent()?;
                let repo_root = helix_dir.parent()?;

                return Some((
                    potential_sandbox_root.to_path_buf(),
                    repo_root.to_path_buf(),
                ));
            }
        }
    }
    None
}

/// Find repository root by looking for .helix directory
fn find_repo_root(start: &Path) -> Result<PathBuf> {
    for ancestor in start.ancestors() {
        if ancestor.join(".helix").is_dir() {
            return Ok(ancestor.to_path_buf());
        }
    }
    bail!(
        "Not a helix repository (or any parent up to root): {}",
        start.display()
    )
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

    // use helix index methods
    let entries = build_index_entries_from_commit(repo_path, &base_commit, &workdir)?;
    write_sandbox_index(&sandbox_root, &entries)?;

    if options.verbose {
        println!("Created sandbox index with {} entries", entries.len());
    }

    // create a branch for the sandbox with the name as the branch_name
    let branch_name = format!("sandboxes/{}", name);
    let ref_name = format!("refs/heads/{}", branch_name);
    let refs = FsRefStore::new(repo_path);

    refs.set_ref(&ref_name, base_commit)
        .with_context(|| format!("Failed to create branch '{}'", branch_name))?;

    let mut manifest = SandboxManifest::new(name, base_commit);
    manifest.branch = Some(branch_name.clone());
    manifest.save(&sandbox_root)?;

    activate_sandbox(
        repo_path,
        name,
        &manifest,
        &workdir,
        true,
        Some(files_count),
    )?;

    Ok(Sandbox {
        manifest,
        root: sandbox_root,
        workdir,
    })
}

fn activate_sandbox(
    repo_path: &Path,
    name: &str,
    manifest: &SandboxManifest,
    workdir: &Path,
    created: bool,
    files_count: Option<u64>,
) -> Result<()> {
    // Update HEAD to point to sandbox branch
    if let Some(ref branch_name) = manifest.branch {
        let head_path = repo_path.join(".helix").join("HEAD");
        fs::write(&head_path, format!("ref: refs/heads/{}\n", branch_name))?;
    }

    // Optional: record the currently active sandbox (handy for other commands)
    let active_path = repo_path.join(".helix").join("ACTIVE_SANDBOX");
    let _ = fs::write(
        &active_path,
        format!("name={}\nworkdir={}\n", name, workdir.display()),
    );

    println!();
    println!("  {}", style("Sandbox").bold());
    println!(
        "  {}",
        style("────────────────────────────────────────────").dim()
    );

    if created {
        let files_suffix = files_count
            .map(|n| format!(" ({} files)", n))
            .unwrap_or_default();

        println!(
            "  {} Created and switched to sandbox {}{}",
            style("✓").green(),
            style(name).yellow().bold(),
            style(files_suffix).dim()
        );
    } else {
        println!(
            "  {} Switched to sandbox {}",
            style("✓").green(),
            style(name).yellow().bold()
        );
    }

    println!(
        "  {} Branch:  {}",
        style("•").cyan(),
        style(manifest.branch.as_deref().unwrap_or("(none)")).dim()
    );
    println!(
        "  {} Workdir: {}",
        style("•").cyan(),
        style(workdir.display()).dim()
    );

    println!();
    println!("  {}", style("Next steps:").underlined());
    println!(
        "    {}      - open the sandbox workdir",
        style(format!("cd {}", workdir.display())).cyan()
    );
    println!(
        "    {}      - run helix commands inside the sandbox",
        style("helix status / helix commit").cyan()
    );
    println!(
        "    {}      - record changes from the sandbox",
        style("helix commit -m \"message\"").cyan()
    );
    println!();

    Ok(())
}

/// builds the index entries from commit
// in the sync.rs module, we have a build_index_entries_from_git, we might at some point want to ccombine and generalize the two?
fn build_index_entries_from_commit(
    repo_path: &Path,
    commit_hash: &Hash,
    workdir: &Path,
) -> Result<Vec<Entry>> {
    let tree_store = TreeStore::for_repo(repo_path);
    let store = FsObjectStore::new(repo_path);

    let commit_bytes = store.read_object(&ObjectType::Commit, commit_hash)?;
    if commit_bytes.len() < 32 {
        bail!("Commit too short");
    }

    let mut tree_hash = [0u8; 32];
    tree_hash.copy_from_slice(&commit_bytes[0..32]);

    let files = tree_store.collect_all_files(&tree_hash)?;

    let mut entries: Vec<Entry> = files
        .into_iter()
        .map(|(path, oid)| Entry::from_blob(path, oid, workdir))
        .collect();

    entries.sort_by(|a, b| a.path.cmp(&b.path));

    Ok(entries)
}

fn write_sandbox_index(sandbox_root: &Path, entries: &[Entry]) -> Result<()> {
    // Create .helix subdir in sandbox (mirrors repo structure)
    let helix_dir = sandbox_root.join(".helix");
    fs::create_dir_all(&helix_dir)?;

    let header = Header::new(1, entries.len() as u32);

    // Writer will write to {sandbox_root}/.helix/helix.idx
    let writer = Writer::new_canonical(sandbox_root);
    writer.write(&header, entries)?;

    Ok(())
}

/// Delete a sandbox
pub fn destroy_sandbox(repo_path: &Path, name: &str, options: DestroyOptions) -> Result<()> {
    let sandbox_root = repo_path.join(".helix").join("sandboxes").join(name);

    if !sandbox_root.exists() {
        bail!("Sandbox '{}' does not exist", name);
    }

    let manifest = SandboxManifest::load(&sandbox_root)?;

    if !options.force {
        let changes = get_sandbox_changes(repo_path, name)?;

        if !changes.is_empty() {
            bail!(
                "Sandbox '{}' has {} uncommitted change(s).\n\
                 Use --force to destroy anyway, or commit first with:\n\
                   helix sandbox commit {} -m <message>",
                name,
                changes.len(),
                name
            );
        }
    }

    fs::remove_dir_all(&sandbox_root).with_context(|| {
        format!(
            "Failed to remove sandbox directory {}",
            sandbox_root.display()
        )
    })?;

    if let Some(branch_name) = manifest.branch {
        // Branch is stored as "sandbox/name", need to handle nested path
        let ref_path = repo_path
            .join(".helix")
            .join("refs")
            .join("heads")
            .join(&branch_name);

        if ref_path.exists() {
            fs::remove_file(&ref_path).ok();
            // Also try to remove parent "sandbox" dir if empty
            if let Some(parent) = ref_path.parent() {
                fs::remove_dir(parent).ok();
            }
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
    let sandbox_root = repo_path.join(".helix").join("sandboxes").join(name);

    if !sandbox_root.exists() {
        bail!(
            "Sandbox '{}' does not exist. Create it with 'helix sandbox create {}'",
            name,
            name
        );
    }

    let manifest = SandboxManifest::load(&sandbox_root)?;
    let workdir = sandbox_root.join("workdir");

    // Just activate it (update HEAD + UI)
    activate_sandbox(repo_path, name, &manifest, &workdir, false, None)
}

/// Validate sandbox name (no special characters, slashes, etc.), (alphanumeric, hyphens, underscores only)
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

pub fn get_sandbox_changes(repo_path: &Path, name: &str) -> Result<Vec<SandboxChange>> {
    let sandbox_root = repo_path.join(".helix").join("sandboxes").join(name);
    let workdir = sandbox_root.join("workdir");
    let manifest = SandboxManifest::load(&sandbox_root)?;
    let base_commit = manifest.base_commit_hash()?;

    // Get files from base commit's tree in order to match against the workdir
    let base_files = collect_files_from_commit(&repo_path, &base_commit)?;

    // Get files from sandbox workdir to match against the base commit tree
    let workdir_files = collect_files_from_workdir(&workdir)?;

    compute_sandbox_diff(&base_files, &workdir_files, &workdir)
}

/// Collect all file paths and hashes from a commit's tree
fn collect_files_from_commit(
    repo_path: &Path,
    base_commit: &Hash,
) -> Result<HashMap<PathBuf, Hash>> {
    let object_store = FsObjectStore::new(repo_path);
    let tree_store = TreeStore::for_repo(repo_path);

    let commit_bytes = object_store.read_object(&ObjectType::Commit, base_commit)?;

    if commit_bytes.len() < 32 {
        bail!("Commit too short");
    }

    let tree_hash = Commit::from_bytes(&commit_bytes)?;
    tree_store.collect_all_files(&tree_hash.tree_hash)
}

/// Collect all file paths from sandbox workdir
fn collect_files_from_workdir(workdir: &Path) -> Result<HashSet<PathBuf>> {
    let mut files = HashSet::new();
    collect_workdir_files_recursive(workdir, workdir, &mut files)?;
    Ok(files)
}

fn collect_workdir_files_recursive(
    root: &Path,
    current: &Path,
    files: &mut HashSet<PathBuf>,
) -> Result<()> {
    if !current.exists() {
        return Ok(());
    }

    for entry in fs::read_dir(current)? {
        let entry = entry?;
        let path = entry.path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");

        // Skip hidden and common large directories
        if name.starts_with('.')
            || name == "node_modules"
            || name == "target"
            || name == "__pycache__"
            || name == ".venv"
            || name == "venv"
        {
            continue;
        }

        if path.is_dir() {
            collect_workdir_files_recursive(root, &path, files)?;
        } else if path.is_file() {
            let relative = path.strip_prefix(root)?;
            files.insert(relative.to_path_buf());
        }
    }

    Ok(())
}

/// Compute diff between base commit files and workdir files
fn compute_sandbox_diff(
    base_files: &HashMap<PathBuf, Hash>,
    workdir_files: &HashSet<PathBuf>,
    workdir: &Path,
) -> Result<Vec<SandboxChange>> {
    let mut changes = Vec::new();

    // Check for added and modified files
    for path in workdir_files {
        let full_path = workdir.join(path);
        let content = fs::read(&full_path)?;
        let current_hash = hash_bytes(&content);

        match base_files.get(path) {
            None => {
                changes.push(SandboxChange {
                    path: path.clone(),
                    kind: SandboxChangeKind::Added,
                });
            }
            Some(base_hash) => {
                if &current_hash != base_hash {
                    changes.push(SandboxChange {
                        path: path.clone(),
                        kind: SandboxChangeKind::Modified,
                    });
                }
            }
        }
    }

    // Check for deleted files
    for path in base_files.keys() {
        if !workdir_files.contains(path) {
            changes.push(SandboxChange {
                path: path.clone(),
                kind: SandboxChangeKind::Deleted,
            });
        }
    }

    changes.sort_by(|a, b| a.path.cmp(&b.path));

    Ok(changes)
}

/// Commit sandbox changes
pub fn commit_sandbox(repo_path: &Path, name: &str, options: CommitOptions) -> Result<Hash> {
    if options.message.trim().is_empty() {
        bail!("Commit message cannot be empty");
    }

    let sandbox_root = repo_path.join(".helix").join("sandboxes").join(name);

    if !sandbox_root.exists() {
        bail!("Sandbox '{}' does not exist", name);
    }

    let manifest = SandboxManifest::load(&sandbox_root)?;
    let base_commit = manifest.base_commit_hash()?;
    let workdir = sandbox_root.join("workdir");

    // Check for changes first
    let changes = get_sandbox_changes(repo_path, name)?;
    if changes.is_empty() {
        bail!("No changes to commit in sandbox '{}'", name);
    }

    if options.verbose {
        println!("Committing {} changes in sandbox '{}'", changes.len(), name);
    }

    // Build tree from sandbox workdir
    let store = FsObjectStore::new(repo_path);
    // let tree_builder = TreeBuilder::new(repo_path)
    let tree_hash = build_tree_from_workdir(&store, &workdir, repo_path)?;

    // Get author
    let author = match options.author {
        Some(a) => a,
        None => get_author(repo_path)?,
    };

    // Create commit with base as parent
    let commit_hash = Commit::new(tree_hash, vec![base_commit], author, options.message);

    // Update sandbox branch
    if let Some(ref branch_name) = manifest.branch {
        let ref_name = format!("refs/heads/{}", branch_name);
        let refs = FsRefStore::new(repo_path);
        refs.set_ref(&ref_name, commit_hash.commit_hash)?;
    }

    // Store commit hash in sandbox metadata (for merge)
    let commit_ref_path = sandbox_root.join("commit");
    fs::write(&commit_ref_path, hash_to_hex(&commit_hash.commit_hash))?;

    println!(
        "Created commit {} in sandbox '{}'",
        &hash_to_hex(&commit_hash.commit_hash)[..8],
        name
    );

    Ok(commit_hash.commit_hash)
}

/// Merge sandbox into a branch
pub fn merge_sandbox(repo_path: &Path, name: &str, options: MergeOptions) -> Result<Hash> {
    let sandbox_root = repo_path.join(".helix").join("sandboxes").join(name);

    if !sandbox_root.exists() {
        bail!("Sandbox '{}' does not exist", name);
    }

    let manifest = SandboxManifest::load(&sandbox_root)?;
    let base_commit = manifest.base_commit_hash()?;

    // Get the current sandbox branch HEAD
    let sandbox_branch = manifest
        .branch
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Sandbox has no branch"))?;

    let ref_name = format!("refs/heads/{}", sandbox_branch);
    let refs = FsRefStore::new(repo_path);

    let sandbox_head = refs
        .get_ref(&ref_name)?
        .ok_or_else(|| anyhow::anyhow!("Sandbox branch not found"))?;

    // Check if there are any commits beyond base
    if sandbox_head == base_commit {
        bail!(
            "Sandbox '{}' has no commits. Make changes and commit first:\n\
             cd {}\n\
             helix add .\n\
             helix commit -m \"message\"",
            name,
            sandbox_root.join("workdir").display()
        );
    }

    // Determine target branch
    let target_branch = options.into_branch.unwrap_or_else(|| "main".to_string());
    let target_ref_name = format!("refs/heads/{}", target_branch);

    if options.verbose {
        println!("Merging sandbox '{}' into {}", name, target_branch);
    }

    // Get current target HEAD
    let target_head = refs.get_ref(&target_ref_name)?;

    match target_head {
        None => {
            refs.set_ref(&target_ref_name, sandbox_head)?;
            println!(
                "Created branch '{}' at {}",
                target_branch,
                &hash_to_hex(&sandbox_head)[..8]
            );
        }
        Some(current_head) => {
            if current_head == base_commit {
                refs.set_ref(&target_ref_name, sandbox_head)?;
                println!("Fast-forward merged '{}' into '{}'", name, target_branch);
            } else {
                bail!(
                    "Cannot fast-forward: '{}' has diverged from sandbox base.\n\
                     Manual merge required (TODO:: not yet implemented).",
                    target_branch
                );
            }
        }
    }

    Ok(sandbox_head)
}

/// Build a tree from workdir files
fn build_tree_from_workdir(
    store: &FsObjectStore,
    workdir: &Path,
    repo_path: &Path,
) -> Result<Hash> {
    let files = collect_files_from_workdir(workdir)?;
    let mut entries = Vec::new();

    for path in files {
        let full_path = workdir.join(&path);
        let content = fs::read(&full_path)?;
        let oid = store.write_object(&ObjectType::Blob, &content)?;

        let metadata = fs::metadata(&full_path)?;
        let file_mode = get_file_mode(&metadata);

        entries.push(Entry {
            path,
            oid,
            flags: EntryFlags::TRACKED,
            size: metadata.len(),
            mtime_sec: 0,
            mtime_nsec: 0,
            file_mode,
            merge_conflict_stage: 0,
            reserved: [0u8; 33],
        });
    }

    let tree_builder = TreeBuilder::new(repo_path);
    tree_builder.build_from_entries(&entries)
}

fn get_author(repo_path: &Path) -> Result<String> {
    let config_path = repo_path.join("helix.toml");

    if config_path.exists() {
        let content = fs::read_to_string(&config_path)?;

        if let Ok(config) = content.parse::<toml::Value>() {
            if let Some(user) = config.get("user") {
                let name = user.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let email = user.get("email").and_then(|v| v.as_str()).unwrap_or("");

                if !name.is_empty() && !email.is_empty() {
                    return Ok(format!("{} <{}>", name, email));
                }
            }
        }
    }

    bail!("Author not configured. Add [user] section to helix.toml")
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
