// Commit command - Create commits from staged files
// commit.rs
// PURE HELIX COMMIT WORKFLOW:
// 1. Load staged entries from .helix/helix.idx
// 2. Build tree from staged entries
// 3. Get current HEAD commit (if exists)
// 4. Create new commit with tree and parent
// 5. Store commit in .helix/objects/commits/
// 6. Update HEAD reference
//
// helix commit -m "Message"                    # Basic
// helix commit -m "Message" -v                 # Verbose
// helix commit -m "Message" --author "Name"    # Custom author
// helix commit -m "Message" --amend            # Amend previous
// helix commit -m "Message" --allow-empty      # Empty commit

use crate::helix_index::api::HelixIndexData;
use crate::helix_index::commit::{Commit, CommitStore};
use crate::helix_index::format::EntryFlags;
use crate::helix_index::tree::TreeBuilder;
use crate::init_command::HelixConfig;
use anyhow::{Context, Result};
use helix_protocol::hash::{hash_to_hex, hex_to_hash, Hash};
use helix_protocol::storage::FsObjectStore;
use std::fs;
use std::path::Path;

pub struct CommitOptions {
    pub message: String,
    pub author: Option<String>,
    pub allow_empty: bool,
    pub amend: bool,
    pub verbose: bool,
}

impl Default for CommitOptions {
    fn default() -> Self {
        Self {
            message: String::new(),
            author: None,
            allow_empty: false,
            amend: false,
            verbose: false,
        }
    }
}

/// Create a commit from staged files
pub fn commit(repo_path: &Path, options: CommitOptions) -> Result<Hash> {
    let object_store = FsObjectStore::new(repo_path);
    let commit_store = CommitStore::new(repo_path, object_store)?;

    if options.message.trim().is_empty() {
        anyhow::bail!("Commit message cannot be empty. Use -m <message>");
    }

    let index = HelixIndexData::load_or_rebuild(repo_path)?;

    if options.verbose {
        println!("Loaded index (generation {})", index.generation());
    }

    let staged_entries: Vec<_> = index
        .entries()
        .iter()
        .filter(|e| e.flags.contains(EntryFlags::STAGED))
        .cloned()
        .collect();

    if staged_entries.is_empty() && !options.allow_empty {
        anyhow::bail!("No changes staged for commit. Use 'helix add <files>' to stage changes.");
    }

    if options.verbose {
        println!("Staging area: {} files", staged_entries.len());
        for entry in &staged_entries {
            println!("  {}", entry.path.display());
        }
    }

    // Get current HEAD (if exists)
    let head_commit_hash = read_head(repo_path).ok();

    if options.amend && head_commit_hash.is_none() {
        anyhow::bail!("Cannot amend - no previous commit exists");
    }

    // Build tree from staged entries
    if options.verbose {
        println!("Building tree from {} entries...", staged_entries.len());
    }

    let tree_builder = TreeBuilder::new(repo_path);
    let tree_hash = tree_builder
        .build_from_entries(&staged_entries)
        .context("Failed to build tree")?;

    if options.verbose {
        println!("Created tree: {}", hash_to_hex(&tree_hash)[..8].to_string());
    }

    // Check if tree built from index entries would be same as HEAD commit (no changes)
    if !options.allow_empty && !options.amend {
        if let Some(head_hash) = head_commit_hash {
            let head_commit_obj = commit_store.read_commit(&head_hash)?;

            if tree_hash == head_commit_obj.tree_hash {
                // Allow if this is the first native commit after import
                if has_native_commits(repo_path) {
                    anyhow::bail!("No changes to commit. The tree is identical to HEAD.");
                }

                if options.verbose {
                    println!("Creating first native Helix commit after import.");
                }
            }
        }
    }

    let author = if let Some(author) = options.author {
        author
    } else {
        get_author(repo_path)?
    };

    let commit = if options.amend {
        let head_hash = head_commit_hash.unwrap();
        let prev_commit = commit_store.read_commit(&head_hash)?;

        Commit::new(tree_hash, prev_commit.parents, author, options.message)
    } else if let Some(parent_hash) = head_commit_hash {
        // Normal commit with parent
        Commit::with_parent(tree_hash, parent_hash, author, options.message)
    } else {
        // Initial commit
        Commit::initial(tree_hash, author, options.message)
    };

    // Store commit
    if options.verbose {
        println!("Writing commit object...");
    }

    let commit_hash = commit.get_hash();

    // Write to storage (returns the same hash)
    commit_store
        .write_commit(&commit)
        .context("Failed to write commit")?;

    if options.verbose {
        println!(
            "Created commit: {}",
            hash_to_hex(&commit_hash)[..8].to_string()
        );
    }

    // Update HEAD
    write_head(repo_path, commit_hash)?;

    if options.verbose {
        println!("Updated HEAD");
    }

    // Clear staged flags in index
    clear_staged_flags(repo_path)?;

    // Print commit summary
    let short_hash = hash_to_hex(&commit_hash);
    let short_hash = &short_hash[..8];

    if commit.is_initial() {
        println!("[{}] {}", short_hash, commit.summary());
        println!("{} files changed", staged_entries.len());
    } else {
        println!("[{}] {}", short_hash, commit.summary());
        println!("{} files changed", staged_entries.len());
    }

    if !has_native_commits(repo_path) {
        mark_native_commit_exists(repo_path)?;
    }

    Ok(commit_hash)
}

/// Read current HEAD commit hash
fn read_head(repo_path: &Path) -> Result<Hash> {
    let head_path = repo_path.join(".helix").join("HEAD");

    if !head_path.exists() {
        anyhow::bail!("HEAD not found");
    }

    let content = fs::read_to_string(&head_path).context("Failed to read HEAD")?;

    // HEAD can be:
    // 1. Direct hash: "a1b2c3d4..."
    // 2. Symbolic ref: "ref: refs/heads/main"

    let content = content.trim();

    if content.starts_with("ref:") {
        // Symbolic reference
        let ref_path = content.strip_prefix("ref:").unwrap().trim();
        let full_ref_path = repo_path.join(".helix").join(ref_path);

        if !full_ref_path.exists() {
            anyhow::bail!("Reference {} not found", ref_path);
        }

        let ref_content = fs::read_to_string(&full_ref_path).context("Failed to read reference")?;

        hex_to_hash(ref_content.trim()).context("Invalid hash in reference")
    } else {
        // Direct hash
        hex_to_hash(content).context("Invalid hash in HEAD")
    }
}

/// Write new HEAD commit hash
fn write_head(repo_path: &Path, commit_hash: Hash) -> Result<()> {
    let head_path = repo_path.join(".helix").join("HEAD");

    // Read current HEAD to check if it's a symbolic reference
    if head_path.exists() {
        let content = fs::read_to_string(&head_path)?;
        let content = content.trim();

        if content.starts_with("ref:") {
            // Update the branch it points to
            let ref_path = content.strip_prefix("ref:").unwrap().trim();
            let full_ref_path = repo_path.join(".helix").join(ref_path);

            // Ensure parent directory exists
            if let Some(parent) = full_ref_path.parent() {
                fs::create_dir_all(parent)?;
            }

            let hash_hex = hash_to_hex(&commit_hash);
            fs::write(&full_ref_path, hash_hex)?;
            return Ok(());
        }
    }

    // Direct HEAD update (detached HEAD)
    let hash_hex = hash_to_hex(&commit_hash);
    fs::write(&head_path, hash_hex)?;

    Ok(())
}

/// Get author from config or environment
fn get_author(repo_path: &Path) -> Result<String> {
    let config_path = repo_path.join("helix.toml");

    if config_path.exists() {
        let content = fs::read_to_string(&config_path)
            .with_context(|| format!("Failed to read {}", config_path.display()))?;

        // Parse TOML into our struct
        let config: HelixConfig = toml::from_str(&content)
            .with_context(|| format!("Failed to parse TOML in {}", config_path.display()))?;

        if let Some(user) = config.user {
            match (user.name, user.email) {
                // If both name and email exist, format as "Name <email>"
                (Some(n), Some(e)) => {
                    return Ok(format!("{} <{}>", n.trim(), e.trim()));
                }
                // If only name exists, use it as-is
                (Some(n), None) => {
                    let trimmed = n.trim();
                    if !trimmed.is_empty() {
                        return Ok(trimmed.to_string());
                    }
                }
                _ => {}
            }
        }
    }

    anyhow::bail!(
        "Author not configured. Set in helix.toml:\n\
         \n\
         [user]\n\
         name = \"Your Name\"\n\
         email = \"you@example.com\"\n\
         \n\
         Or use environment variables:\n\
         export HELIX_AUTHOR_NAME=\"Your Name\"\n\
         export HELIX_AUTHOR_EMAIL=\"your@email.com\""
    )
}

/// Clear staged flags from index after successful commit
fn clear_staged_flags(repo_path: &Path) -> Result<()> {
    let mut index = HelixIndexData::load_or_rebuild(repo_path)?;

    // Remove STAGED flag from all entries
    for entry in index.entries_mut() {
        entry.flags.remove(EntryFlags::STAGED);
    }

    // Persist updated index
    index.persist()?;

    Ok(())
}

/// Show what would be committed (dry run)
pub fn show_staged(repo_path: &Path) -> Result<()> {
    let index = HelixIndexData::load_or_rebuild(repo_path)?;

    let staged_entries: Vec<_> = index
        .entries()
        .iter()
        .filter(|e| e.flags.contains(EntryFlags::STAGED))
        .collect();

    if staged_entries.is_empty() {
        println!("No changes staged for commit.");
        println!("Use 'helix add <files>' to stage changes.");
        return Ok(());
    }

    println!("Changes to be committed:");
    println!();

    let num_entries = staged_entries.len();

    for entry in staged_entries {
        // Check if this is a new file or modified file
        if entry.flags.contains(EntryFlags::TRACKED) && !entry.flags.contains(EntryFlags::MODIFIED)
        {
            println!("  new file:   {}", entry.path.display());
        } else {
            println!("  modified:   {}", entry.path.display());
        }
    }

    println!();
    println!("{} files staged", num_entries);

    Ok(())
}

// hate this but im tired and want to move on TODO
/// Check if any native Helix commits have been created (vs only imported commits)
fn has_native_commits(repo_path: &Path) -> bool {
    repo_path
        .join(".helix")
        .join("native-commit-exists")
        .exists()
}

/// Mark that a native Helix commit has been created
fn mark_native_commit_exists(repo_path: &Path) -> Result<()> {
    let path = repo_path.join(".helix").join("native-commit-exists");
    fs::write(&path, "1")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::helix_index::format::Entry;
    use helix_protocol::storage::FsObjectStore;
    use std::path::PathBuf;
    use std::process::Command;
    use std::time::Instant;
    use tempfile::TempDir;

    fn init_test_repo(path: &Path) -> Result<()> {
        // Initialize git repo
        fs::create_dir_all(path.join(".git"))?;
        Command::new("git")
            .args(&["init"])
            .current_dir(path)
            .output()?;
        Command::new("git")
            .args(&["config", "user.name", "Test User"])
            .current_dir(path)
            .output()?;
        Command::new("git")
            .args(&["config", "user.email", "test@example.com"])
            .current_dir(path)
            .output()?;

        // Initialize helix
        crate::init_command::init_helix_repo(path, None)?;

        // Set author in config
        let config_path = path.join("helix.toml");
        fs::write(&config_path, "author = \"Test User <test@example.com>\"\n")?;

        Ok(())
    }

    fn stage_file(repo_path: &Path, filename: &str, content: &[u8]) -> Result<()> {
        // Write file
        fs::write(repo_path.join(filename), content)?;

        // Store blob
        let storage = FsObjectStore::new(repo_path);
        let hash = storage.write_object(&helix_protocol::message::ObjectType::Blob, content)?;

        // Add to index as staged
        let mut index = HelixIndexData::load_or_rebuild(repo_path)?;

        let entry = Entry {
            path: PathBuf::from(filename),
            oid: hash,
            flags: EntryFlags::TRACKED | EntryFlags::STAGED,
            size: content.len() as u64,
            mtime_sec: 0,
            mtime_nsec: 0,
            file_mode: 0o100644,
            merge_conflict_stage: 0,
            reserved: [0u8; 33],
        };

        index.entries_mut().push(entry);
        index.persist()?;

        Ok(())
    }

    #[test]
    fn test_initial_commit() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        init_test_repo(repo_path)?;

        // Stage a file
        stage_file(repo_path, "README.md", b"# Hello World")?;

        // Create commit
        let commit_hash = commit(
            repo_path,
            CommitOptions {
                message: "Initial commit".to_string(),
                ..Default::default()
            },
        )?;

        // Verify commit exists
        let store = FsObjectStore::new(temp_dir.path());
        let commit_reader = CommitStore::new(temp_dir.path(), store)?;
        let commit_obj = commit_reader.read_commit(&commit_hash)?;

        assert_eq!(commit_obj.message, "Initial commit");
        assert!(commit_obj.is_initial());
        assert_eq!(commit_obj.get_hash(), commit_hash);

        // Verify HEAD points to commit
        let head_hash = read_head(repo_path)?;
        assert_eq!(head_hash, commit_hash);

        Ok(())
    }

    #[test]
    fn test_second_commit() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        init_test_repo(repo_path)?;

        // First commit
        stage_file(repo_path, "file1.txt", b"content 1")?;
        let first_hash = commit(
            repo_path,
            CommitOptions {
                message: "First commit".to_string(),
                ..Default::default()
            },
        )?;

        // Second commit
        stage_file(repo_path, "file2.txt", b"content 2")?;
        let second_hash = commit(
            repo_path,
            CommitOptions {
                message: "Second commit".to_string(),
                ..Default::default()
            },
        )?;

        // Verify second commit has first as parent
        let store = FsObjectStore::new(temp_dir.path());
        let commit_reader = CommitStore::new(temp_dir.path(), store)?;
        let second_commit = commit_reader.read_commit(&second_hash)?;

        assert_eq!(second_commit.parents.len(), 1);
        assert_eq!(second_commit.parents[0], first_hash);

        Ok(())
    }

    #[test]
    fn test_commit_without_staged_files() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        init_test_repo(repo_path)?;

        // Try to commit without staging anything
        let result = commit(
            repo_path,
            CommitOptions {
                message: "Empty commit".to_string(),
                ..Default::default()
            },
        );

        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("No changes staged"));

        Ok(())
    }

    #[test]
    fn test_commit_with_empty_message() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        init_test_repo(repo_path)?;
        stage_file(repo_path, "test.txt", b"test")?;

        // Try to commit with empty message
        let result = commit(
            repo_path,
            CommitOptions {
                message: "".to_string(),
                ..Default::default()
            },
        );

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("cannot be empty"));

        Ok(())
    }

    #[test]
    fn test_commit_clears_staged_flags() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        init_test_repo(repo_path)?;

        // Stage files
        stage_file(repo_path, "file1.txt", b"content 1")?;
        stage_file(repo_path, "file2.txt", b"content 2")?;

        // Verify staged
        let index = HelixIndexData::load_or_rebuild(repo_path)?;
        let staged_count = index
            .entries()
            .iter()
            .filter(|e| e.flags.contains(EntryFlags::STAGED))
            .count();
        assert_eq!(staged_count, 2);

        // Commit
        commit(
            repo_path,
            CommitOptions {
                message: "Test commit".to_string(),
                ..Default::default()
            },
        )?;

        // Verify staged flags cleared
        let index = HelixIndexData::load_or_rebuild(repo_path)?;
        let staged_count = index
            .entries()
            .iter()
            .filter(|e| e.flags.contains(EntryFlags::STAGED))
            .count();
        assert_eq!(staged_count, 0);

        Ok(())
    }

    #[test]
    fn test_commit_allow_empty() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        init_test_repo(repo_path)?;

        // Commit with no staged files but allow_empty=true
        let commit_hash = commit(
            repo_path,
            CommitOptions {
                message: "Empty commit".to_string(),
                allow_empty: true,
                ..Default::default()
            },
        )?;

        let store = FsObjectStore::new(temp_dir.path());
        let commit_reader = CommitStore::new(temp_dir.path(), store)?;
        let commit_obj = commit_reader.read_commit(&commit_hash)?;

        assert_eq!(commit_obj.message, "Empty commit");

        Ok(())
    }

    #[test]
    fn test_show_staged() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        init_test_repo(repo_path)?;

        // Stage files
        stage_file(repo_path, "file1.txt", b"content 1")?;
        stage_file(repo_path, "file2.txt", b"content 2")?;

        // Show staged (should not error)
        show_staged(repo_path)?;

        Ok(())
    }

    #[test]
    fn test_commit_with_custom_author() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        init_test_repo(repo_path)?;
        stage_file(repo_path, "test.txt", b"test")?;

        let commit_hash = commit(
            repo_path,
            CommitOptions {
                message: "Test commit".to_string(),
                author: Some("Custom Author <custom@example.com>".to_string()),
                ..Default::default()
            },
        )?;

        let store = FsObjectStore::new(temp_dir.path());
        let commit_reader = CommitStore::new(temp_dir.path(), store)?;
        let commit_obj = commit_reader.read_commit(&commit_hash)?;

        assert_eq!(commit_obj.author, "Custom Author <custom@example.com>");

        Ok(())
    }

    #[test]
    fn test_commit_performance() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        init_test_repo(repo_path)?;

        // Stage 100 files
        for i in 0..100 {
            stage_file(
                repo_path,
                &format!("file{}.txt", i),
                format!("content {}", i).as_bytes(),
            )?;
        }

        // Time the commit
        let start = Instant::now();
        commit(
            repo_path,
            CommitOptions {
                message: "Commit 100 files".to_string(),
                ..Default::default()
            },
        )?;
        let elapsed = start.elapsed();

        println!("Committed 100 files in {:?}", elapsed);

        // Should be fast (expect <200ms)
        assert!(
            elapsed.as_millis() < 200,
            "Commit took {}ms, expected <200ms",
            elapsed.as_millis()
        );

        Ok(())
    }
}
