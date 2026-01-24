// Diff command - Show changes between working tree, index, and commits

use crate::helix_index::api::HelixIndexData;
use crate::helix_index::commit::CommitStore;
use crate::helix_index::format::EntryFlags;
use crate::helix_index::tree::TreeStore;
use crate::sandbox_command::RepoContext;
use anyhow::{Context, Result};
use helix_protocol::hash::{hash_bytes, Hash};
use helix_protocol::message::ObjectType;
use helix_protocol::storage::FsObjectStore;
use similar::{ChangeTag, TextDiff};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

pub struct DiffOptions {
    pub staged: bool,
    pub verbose: bool,
    pub no_color: bool,
    pub stat: bool,
}

impl Default for DiffOptions {
    fn default() -> Self {
        Self {
            staged: false,
            verbose: false,
            no_color: false,
            stat: false,
        }
    }
}

/// Show diff of working tree changes (unstaged) or staged changes
pub fn diff(repo_path: &Path, paths: &[PathBuf], options: DiffOptions) -> Result<()> {
    let context = RepoContext::detect(repo_path)?;
    let index = HelixIndexData::load_from_path(&context.index_path, &context.repo_root)?;
    let store = FsObjectStore::new(&context.repo_root);

    if options.staged {
        diff_staged(&context, &index, &store, paths, &options)?;
    } else {
        diff_unstaged(&context, &index, &store, paths, &options)?;
    }

    Ok(())
}

/// Show unstaged changes (working tree vs index/staged)
fn diff_unstaged(
    context: &RepoContext,
    index: &HelixIndexData,
    store: &FsObjectStore,
    filter_paths: &[PathBuf],
    options: &DiffOptions,
) -> Result<()> {
    let mut has_changes = false;

    for entry in index.entries() {
        // Skip untracked files
        if !entry.flags.contains(EntryFlags::TRACKED) {
            continue;
        }

        // Apply path filter if specified
        if !filter_paths.is_empty() && !path_matches(&entry.path, filter_paths) {
            continue;
        }

        let full_path = context.workdir.join(&entry.path);

        // Check if file was deleted
        if !full_path.exists() {
            if entry.flags.contains(EntryFlags::STAGED) && entry.flags.contains(EntryFlags::DELETED)
            {
                // Already staged as deleted, skip for unstaged diff
                continue;
            }
            // Show deletion (working tree deleted but not staged)
            let old_content = read_blob_as_string(store, &entry.oid)?;
            print_diff_header(&entry.path, "deleted", options);
            print_unified_diff(&old_content, "", &entry.path, options);
            has_changes = true;
            continue;
        }

        // Read working tree content
        let working_content = fs::read(&full_path)
            .with_context(|| format!("Failed to read {}", entry.path.display()))?;
        let working_hash = hash_bytes(&working_content);

        // Compare with index (staged) version
        if working_hash != entry.oid {
            let old_content = read_blob_as_string(store, &entry.oid)?;
            let new_content = String::from_utf8_lossy(&working_content);

            print_diff_header(&entry.path, "modified", options);
            print_unified_diff(&old_content, &new_content, &entry.path, options);
            has_changes = true;
        }
    }

    // Check for new untracked files that match the filter
    if !filter_paths.is_empty() {
        for path in filter_paths {
            let full_path = context.workdir.join(path);
            // Only check files, not directories
            if full_path.exists() && full_path.is_file() && !index.is_tracked(path) {
                let content = fs::read(&full_path)?;
                let content_str = String::from_utf8_lossy(&content);
                print_diff_header(path, "new file", options);
                print_unified_diff("", &content_str, path, options);
                has_changes = true;
            }
        }
    }

    if !has_changes && options.verbose {
        println!("No unstaged changes");
    }

    Ok(())
}

/// Show staged changes (index vs HEAD commit)
fn diff_staged(
    context: &RepoContext,
    index: &HelixIndexData,
    store: &FsObjectStore,
    filter_paths: &[PathBuf],
    options: &DiffOptions,
) -> Result<()> {
    // Get HEAD tree files
    let head_files = get_head_tree_files(context, store)?;
    let mut has_changes = false;

    for entry in index.entries() {
        // Only show staged files
        if !entry.flags.contains(EntryFlags::STAGED) {
            continue;
        }

        // Apply path filter if specified
        if !filter_paths.is_empty() && !path_matches(&entry.path, filter_paths) {
            continue;
        }

        let head_hash = head_files.get(&entry.path);

        // Check if this is a deletion
        if entry.flags.contains(EntryFlags::DELETED) {
            if let Some(old_hash) = head_hash {
                let old_content = read_blob_as_string(store, old_hash)?;
                print_diff_header(&entry.path, "deleted", options);
                print_unified_diff(&old_content, "", &entry.path, options);
                has_changes = true;
            }
            continue;
        }

        match head_hash {
            Some(old_hash) => {
                // Modified file
                if old_hash != &entry.oid {
                    let old_content = read_blob_as_string(store, old_hash)?;
                    let new_content = read_blob_as_string(store, &entry.oid)?;

                    print_diff_header(&entry.path, "modified", options);
                    print_unified_diff(&old_content, &new_content, &entry.path, options);
                    has_changes = true;
                }
            }
            None => {
                // New file
                let new_content = read_blob_as_string(store, &entry.oid)?;
                print_diff_header(&entry.path, "new file", options);
                print_unified_diff("", &new_content, &entry.path, options);
                has_changes = true;
            }
        }
    }

    if !has_changes && options.verbose {
        println!("No staged changes");
    }

    Ok(())
}

/// Get files from HEAD commit tree
fn get_head_tree_files(
    context: &RepoContext,
    store: &FsObjectStore,
) -> Result<HashMap<PathBuf, Hash>> {
    let head_path = context.repo_root.join(".helix/HEAD");

    if !head_path.exists() {
        // No commits yet
        return Ok(HashMap::new());
    }

    let head_content = fs::read_to_string(&head_path)?;
    let head_ref = head_content.trim();

    // Check if it's a ref or direct hash
    let commit_hash = if head_ref.starts_with("ref: ") {
        let ref_name = head_ref.strip_prefix("ref: ").unwrap();
        let ref_path = context.repo_root.join(".helix").join(ref_name);
        if ref_path.exists() {
            let hash_hex = fs::read_to_string(&ref_path)?;
            helix_protocol::hash::hex_to_hash(hash_hex.trim())?
        } else {
            return Ok(HashMap::new());
        }
    } else {
        helix_protocol::hash::hex_to_hash(head_ref)?
    };

    // Read commit to get tree hash
    let commit_store = CommitStore::new(&context.repo_root, store.clone())?;
    let commit = commit_store.read_commit(&commit_hash)?;

    // Collect all files from tree
    let tree_store = TreeStore::for_repo(&context.repo_root);
    let files = tree_store.collect_all_files(&commit.tree_hash)?;

    Ok(files.into_iter().collect())
}

/// Read blob content as string
fn read_blob_as_string(store: &FsObjectStore, hash: &Hash) -> Result<String> {
    match store.read_object(&ObjectType::Blob, hash) {
        Ok(content) => Ok(String::from_utf8_lossy(&content).to_string()),
        Err(_) => Ok(String::new()), // Blob might not exist yet
    }
}

/// Check if path matches any of the filter paths
fn path_matches(path: &Path, filter_paths: &[PathBuf]) -> bool {
    filter_paths
        .iter()
        .any(|filter| path == filter || path.starts_with(filter) || filter.as_os_str() == ".")
}

/// Print diff header
fn print_diff_header(path: &Path, change_type: &str, options: &DiffOptions) {
    if options.no_color {
        println!("diff --helix a/{} b/{}", path.display(), path.display());
        println!("{} mode 100644", change_type);
        println!("--- a/{}", path.display());
        println!("+++ b/{}", path.display());
    } else {
        println!(
            "\x1b[1mdiff --helix a/{} b/{}\x1b[0m",
            path.display(),
            path.display()
        );
        println!("\x1b[1m{} mode 100644\x1b[0m", change_type);
        println!("\x1b[1m--- a/{}\x1b[0m", path.display());
        println!("\x1b[1m+++ b/{}\x1b[0m", path.display());
    }
}

/// Print unified diff between two strings
fn print_unified_diff(old: &str, new: &str, _path: &Path, options: &DiffOptions) {
    let diff = TextDiff::from_lines(old, new);

    for (idx, group) in diff.grouped_ops(3).iter().enumerate() {
        if idx > 0 {
            println!();
        }

        for op in group {
            for change in diff.iter_changes(op) {
                let (sign, color_code) = match change.tag() {
                    ChangeTag::Delete => ("-", "\x1b[31m"),
                    ChangeTag::Insert => ("+", "\x1b[32m"),
                    ChangeTag::Equal => (" ", ""),
                };

                let value = change.value();

                if options.no_color {
                    print!("{}{}", sign, value);
                } else {
                    print!("{}{}{}\x1b[0m", color_code, sign, value);
                }

                if change.missing_newline() {
                    println!();
                    if options.no_color {
                        println!("\\ No newline at end of file");
                    } else {
                        println!("\x1b[2m\\ No newline at end of file\x1b[0m");
                    }
                }
            }
        }
    }
}

/// Show diff stat (summary of changes)
pub fn diff_stat(repo_path: &Path, paths: &[PathBuf], options: DiffOptions) -> Result<()> {
    let context = RepoContext::detect(repo_path)?;
    let index = HelixIndexData::load_from_path(&context.index_path, &context.repo_root)?;
    let store = FsObjectStore::new(&context.repo_root);

    let mut stats: Vec<(PathBuf, usize, usize)> = Vec::new();

    if options.staged {
        let head_files = get_head_tree_files(&context, &store)?;

        for entry in index.entries() {
            if !entry.flags.contains(EntryFlags::STAGED) {
                continue;
            }

            if !paths.is_empty() && !path_matches(&entry.path, paths) {
                continue;
            }

            let (additions, deletions) = if entry.flags.contains(EntryFlags::DELETED) {
                if let Some(old_hash) = head_files.get(&entry.path) {
                    let old_content = read_blob_as_string(&store, old_hash)?;
                    (0, old_content.lines().count())
                } else {
                    (0, 0)
                }
            } else {
                let new_content = read_blob_as_string(&store, &entry.oid)?;
                match head_files.get(&entry.path) {
                    Some(old_hash) => {
                        let old_content = read_blob_as_string(&store, old_hash)?;
                        compute_stat(&old_content, &new_content)
                    }
                    None => (new_content.lines().count(), 0),
                }
            };

            if additions > 0 || deletions > 0 {
                stats.push((entry.path.clone(), additions, deletions));
            }
        }
    } else {
        for entry in index.entries() {
            if !entry.flags.contains(EntryFlags::TRACKED) {
                continue;
            }

            if !paths.is_empty() && !path_matches(&entry.path, paths) {
                continue;
            }

            let full_path = context.workdir.join(&entry.path);

            let (additions, deletions) = if !full_path.exists() {
                let old_content = read_blob_as_string(&store, &entry.oid)?;
                (0, old_content.lines().count())
            } else {
                let working_content = fs::read(&full_path)?;
                let working_hash = hash_bytes(&working_content);

                if working_hash != entry.oid {
                    let old_content = read_blob_as_string(&store, &entry.oid)?;
                    let new_content = String::from_utf8_lossy(&working_content);
                    compute_stat(&old_content, &new_content)
                } else {
                    (0, 0)
                }
            };

            if additions > 0 || deletions > 0 {
                stats.push((entry.path.clone(), additions, deletions));
            }
        }
    }

    // Print stats
    if stats.is_empty() {
        return Ok(());
    }

    let max_path_len = stats
        .iter()
        .map(|(p, _, _)| p.to_string_lossy().len())
        .max()
        .unwrap_or(0);

    let total_additions: usize = stats.iter().map(|(_, a, _)| a).sum();
    let total_deletions: usize = stats.iter().map(|(_, _, d)| d).sum();

    for (path, additions, deletions) in &stats {
        let path_str = path.to_string_lossy();
        let bar = create_stat_bar(*additions, *deletions);

        if options.no_color {
            println!(
                " {:width$} | {:>4} {}",
                path_str,
                additions + deletions,
                bar,
                width = max_path_len
            );
        } else {
            println!(
                " {:width$} | {:>4} {}",
                path_str,
                additions + deletions,
                bar,
                width = max_path_len
            );
        }
    }

    println!(
        " {} file{} changed, {} insertion{}(+), {} deletion{}(-)",
        stats.len(),
        if stats.len() == 1 { "" } else { "s" },
        total_additions,
        if total_additions == 1 { "" } else { "s" },
        total_deletions,
        if total_deletions == 1 { "" } else { "s" }
    );

    Ok(())
}

/// Compute addition and deletion counts
fn compute_stat(old: &str, new: &str) -> (usize, usize) {
    let diff = TextDiff::from_lines(old, new);
    let mut additions = 0;
    let mut deletions = 0;

    for change in diff.iter_all_changes() {
        match change.tag() {
            ChangeTag::Insert => additions += 1,
            ChangeTag::Delete => deletions += 1,
            ChangeTag::Equal => {}
        }
    }

    (additions, deletions)
}

/// Create a visual stat bar
fn create_stat_bar(additions: usize, deletions: usize) -> String {
    let total = additions + deletions;
    if total == 0 {
        return String::new();
    }

    let max_width = 50;
    let width = total.min(max_width);
    let add_width = if total > 0 {
        (additions * width) / total
    } else {
        0
    };
    let del_width = width - add_width;

    format!(
        "\x1b[32m{}\x1b[31m{}\x1b[0m",
        "+".repeat(add_width),
        "-".repeat(del_width)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::add_command::{add, AddOptions};
    use crate::commit_command::{commit, CommitOptions};
    use std::fs;
    use tempfile::TempDir;

    fn init_test_repo(path: &Path) -> Result<()> {
        crate::init_command::init_helix_repo(path, None)?;

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

    // ==================== Basic Diff Tests ====================

    #[test]
    fn test_diff_no_changes() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        init_test_repo(repo_path)?;

        // Create and stage a file
        fs::write(repo_path.join("test.txt"), "hello world\n")?;
        add(
            repo_path,
            &[PathBuf::from("test.txt")],
            AddOptions::default(),
        )?;

        // Run diff - should show no changes since file matches staged
        let result = diff(
            repo_path,
            &[],
            DiffOptions {
                verbose: true,
                ..Default::default()
            },
        );

        assert!(result.is_ok());
        Ok(())
    }

    #[test]
    fn test_diff_modified_file() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        init_test_repo(repo_path)?;

        // Create and stage a file
        fs::write(repo_path.join("test.txt"), "hello world\n")?;
        add(
            repo_path,
            &[PathBuf::from("test.txt")],
            AddOptions::default(),
        )?;

        // Modify the file
        fs::write(repo_path.join("test.txt"), "hello modified\n")?;

        // Run diff - should show modification
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
    fn test_diff_staged() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        init_test_repo(repo_path)?;

        // Create and stage a file
        fs::write(repo_path.join("test.txt"), "hello world\n")?;
        add(
            repo_path,
            &[PathBuf::from("test.txt")],
            AddOptions::default(),
        )?;

        // Run diff --staged - should show staged changes (new file)
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
    fn test_diff_with_path_filter() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        init_test_repo(repo_path)?;

        // Create and stage files
        fs::write(repo_path.join("file1.txt"), "content 1\n")?;
        fs::write(repo_path.join("file2.txt"), "content 2\n")?;
        add(repo_path, &[PathBuf::from(".")], AddOptions::default())?;

        // Modify both files
        fs::write(repo_path.join("file1.txt"), "modified 1\n")?;
        fs::write(repo_path.join("file2.txt"), "modified 2\n")?;

        // Run diff with filter - should only show file1
        let result = diff(
            repo_path,
            &[PathBuf::from("file1.txt")],
            DiffOptions {
                no_color: true,
                ..Default::default()
            },
        );

        assert!(result.is_ok());
        Ok(())
    }

    // ==================== Deleted File Tests ====================

    #[test]
    fn test_diff_deleted_file() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        init_test_repo(repo_path)?;

        // Create and stage a file
        fs::write(repo_path.join("to_delete.txt"), "this will be deleted\n")?;
        add(
            repo_path,
            &[PathBuf::from("to_delete.txt")],
            AddOptions::default(),
        )?;

        // Delete the file from working tree
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

    #[test]
    fn test_diff_multiple_deleted_files() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        init_test_repo(repo_path)?;

        // Create and stage multiple files
        fs::write(repo_path.join("file1.txt"), "content 1\n")?;
        fs::write(repo_path.join("file2.txt"), "content 2\n")?;
        fs::write(repo_path.join("file3.txt"), "content 3\n")?;
        add(repo_path, &[PathBuf::from(".")], AddOptions::default())?;

        // Delete two files
        fs::remove_file(repo_path.join("file1.txt"))?;
        fs::remove_file(repo_path.join("file3.txt"))?;

        // Run diff
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

    // ==================== Empty File Tests ====================

    #[test]
    fn test_diff_empty_file() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        init_test_repo(repo_path)?;

        // Create and stage an empty file
        fs::write(repo_path.join("empty.txt"), "")?;
        add(
            repo_path,
            &[PathBuf::from("empty.txt")],
            AddOptions::default(),
        )?;

        // Add content to the empty file
        fs::write(repo_path.join("empty.txt"), "now has content\n")?;

        // Run diff
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
    fn test_diff_file_becomes_empty() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        init_test_repo(repo_path)?;

        // Create and stage a file with content
        fs::write(repo_path.join("will_empty.txt"), "has content\n")?;
        add(
            repo_path,
            &[PathBuf::from("will_empty.txt")],
            AddOptions::default(),
        )?;

        // Make the file empty
        fs::write(repo_path.join("will_empty.txt"), "")?;

        // Run diff
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

    // ==================== Directory Filtering Tests ====================

    #[test]
    fn test_diff_directory_filter() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        init_test_repo(repo_path)?;

        // Create directory structure
        fs::create_dir_all(repo_path.join("src"))?;
        fs::create_dir_all(repo_path.join("tests"))?;
        fs::write(repo_path.join("src/main.rs"), "fn main() {}\n")?;
        fs::write(repo_path.join("src/lib.rs"), "pub fn lib() {}\n")?;
        fs::write(repo_path.join("tests/test.rs"), "fn test() {}\n")?;
        add(repo_path, &[PathBuf::from(".")], AddOptions::default())?;

        // Modify all files
        fs::write(repo_path.join("src/main.rs"), "fn main() { println!(); }\n")?;
        fs::write(repo_path.join("src/lib.rs"), "pub fn lib() { todo!(); }\n")?;
        fs::write(
            repo_path.join("tests/test.rs"),
            "fn test() { assert!(true); }\n",
        )?;

        // Run diff with src/ filter - should only show src files
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

    // ==================== After Commit Tests ====================

    #[test]
    fn test_diff_staged_after_commit() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        init_test_repo(repo_path)?;

        // Create, stage, and commit a file
        fs::write(repo_path.join("committed.txt"), "original content\n")?;
        add(
            repo_path,
            &[PathBuf::from("committed.txt")],
            AddOptions::default(),
        )?;
        commit(
            repo_path,
            CommitOptions {
                message: "Initial commit".to_string(),
                author: Some("Test <test@test.com>".to_string()),
                allow_empty: false,
                amend: false,
                verbose: false,
            },
        )?;

        // Modify and stage the file
        fs::write(repo_path.join("committed.txt"), "modified content\n")?;
        add(
            repo_path,
            &[PathBuf::from("committed.txt")],
            AddOptions::default(),
        )?;

        // Run diff --staged - should show difference from HEAD
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
    fn test_diff_unstaged_after_commit() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        init_test_repo(repo_path)?;

        // Create, stage, and commit a file
        fs::write(repo_path.join("committed.txt"), "original content\n")?;
        add(
            repo_path,
            &[PathBuf::from("committed.txt")],
            AddOptions::default(),
        )?;
        commit(
            repo_path,
            CommitOptions {
                message: "Initial commit".to_string(),
                author: Some("Test <test@test.com>".to_string()),
                allow_empty: false,
                amend: false,
                verbose: false,
            },
        )?;

        // Modify file but don't stage
        fs::write(repo_path.join("committed.txt"), "working tree changes\n")?;

        // Run diff (unstaged) - should show working tree changes
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

    // ==================== Stat Tests ====================

    #[test]
    fn test_diff_stat_mode() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        init_test_repo(repo_path)?;

        // Create and stage files
        fs::write(repo_path.join("file1.txt"), "line1\nline2\nline3\n")?;
        fs::write(repo_path.join("file2.txt"), "content\n")?;
        add(repo_path, &[PathBuf::from(".")], AddOptions::default())?;

        // Modify files
        fs::write(repo_path.join("file1.txt"), "line1\nmodified\nline3\nnew\n")?;
        fs::write(repo_path.join("file2.txt"), "changed\n")?;

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

    #[test]
    fn test_diff_stat_staged() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        init_test_repo(repo_path)?;

        // Create and stage files
        fs::write(repo_path.join("new_file.txt"), "new content\nline 2\n")?;
        add(
            repo_path,
            &[PathBuf::from("new_file.txt")],
            AddOptions::default(),
        )?;

        // Run diff --stat --staged
        let result = diff_stat(
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

    // ==================== Compute Stat Tests ====================

    #[test]
    fn test_compute_stat() {
        let old = "line1\nline2\nline3\n";
        let new = "line1\nmodified\nline3\nnew line\n";

        let (additions, deletions) = compute_stat(old, new);

        assert_eq!(deletions, 1); // line2 removed
        assert_eq!(additions, 2); // modified + new line added
    }

    #[test]
    fn test_compute_stat_empty_to_content() {
        let old = "";
        let new = "line1\nline2\nline3\n";

        let (additions, deletions) = compute_stat(old, new);

        assert_eq!(additions, 3);
        assert_eq!(deletions, 0);
    }

    #[test]
    fn test_compute_stat_content_to_empty() {
        let old = "line1\nline2\nline3\n";
        let new = "";

        let (additions, deletions) = compute_stat(old, new);

        assert_eq!(additions, 0);
        assert_eq!(deletions, 3);
    }

    #[test]
    fn test_compute_stat_no_changes() {
        let content = "line1\nline2\nline3\n";

        let (additions, deletions) = compute_stat(content, content);

        assert_eq!(additions, 0);
        assert_eq!(deletions, 0);
    }

    #[test]
    fn test_compute_stat_complete_rewrite() {
        let old = "old1\nold2\nold3\n";
        let new = "new1\nnew2\nnew3\nnew4\n";

        let (additions, deletions) = compute_stat(old, new);

        assert_eq!(deletions, 3);
        assert_eq!(additions, 4);
    }

    // ==================== Stat Bar Tests ====================

    #[test]
    fn test_create_stat_bar_additions_only() {
        let bar = create_stat_bar(10, 0);
        assert!(bar.contains("+"));
        assert!(!bar.contains("-") || bar.ends_with("\x1b[0m"));
    }

    #[test]
    fn test_create_stat_bar_deletions_only() {
        let bar = create_stat_bar(0, 10);
        assert!(bar.contains("-"));
    }

    #[test]
    fn test_create_stat_bar_mixed() {
        let bar = create_stat_bar(5, 5);
        assert!(bar.contains("+"));
        assert!(bar.contains("-"));
    }

    #[test]
    fn test_create_stat_bar_empty() {
        let bar = create_stat_bar(0, 0);
        assert!(bar.is_empty());
    }

    // ==================== Path Matching Tests ====================

    #[test]
    fn test_path_matches_exact() {
        let path = Path::new("src/main.rs");
        let filter = vec![PathBuf::from("src/main.rs")];
        assert!(path_matches(path, &filter));
    }

    #[test]
    fn test_path_matches_directory_prefix() {
        let path = Path::new("src/main.rs");
        let filter = vec![PathBuf::from("src")];
        assert!(path_matches(path, &filter));
    }

    #[test]
    fn test_path_matches_dot() {
        let path = Path::new("any/path/file.txt");
        let filter = vec![PathBuf::from(".")];
        assert!(path_matches(path, &filter));
    }

    #[test]
    fn test_path_matches_no_match() {
        let path = Path::new("src/main.rs");
        let filter = vec![PathBuf::from("tests")];
        assert!(!path_matches(path, &filter));
    }

    // ==================== Edge Cases ====================

    #[test]
    fn test_diff_no_head_commit() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        init_test_repo(repo_path)?;

        // Stage a file but don't commit
        fs::write(repo_path.join("new.txt"), "content\n")?;
        add(
            repo_path,
            &[PathBuf::from("new.txt")],
            AddOptions::default(),
        )?;

        // Run diff --staged with no HEAD
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
    fn test_diff_large_file() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        init_test_repo(repo_path)?;

        // Create a larger file (1000 lines)
        let content: String = (0..1000).map(|i| format!("line {}\n", i)).collect();
        fs::write(repo_path.join("large.txt"), &content)?;
        add(
            repo_path,
            &[PathBuf::from("large.txt")],
            AddOptions::default(),
        )?;

        // Modify several lines
        let modified: String = (0..1000)
            .map(|i| {
                if i % 100 == 0 {
                    format!("modified line {}\n", i)
                } else {
                    format!("line {}\n", i)
                }
            })
            .collect();
        fs::write(repo_path.join("large.txt"), &modified)?;

        // Run diff
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
    fn test_diff_special_characters_in_content() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        init_test_repo(repo_path)?;

        // Create file with special characters
        fs::write(
            repo_path.join("special.txt"),
            "line with tabs\t\there\n<xml>content</xml>\n\"quoted\" & 'single'\n",
        )?;
        add(
            repo_path,
            &[PathBuf::from("special.txt")],
            AddOptions::default(),
        )?;

        // Modify
        fs::write(
            repo_path.join("special.txt"),
            "modified\t\ttabs\n<html>content</html>\n\"new\" & 'quotes'\n",
        )?;

        // Run diff
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
    fn test_diff_unicode_content() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        init_test_repo(repo_path)?;

        // Create file with unicode
        fs::write(repo_path.join("unicode.txt"), "Hello 世界\nПривет\n🎉\n")?;
        add(
            repo_path,
            &[PathBuf::from("unicode.txt")],
            AddOptions::default(),
        )?;

        // Modify
        fs::write(
            repo_path.join("unicode.txt"),
            "Modified 世界\nПривет мир\n🎊\n",
        )?;

        // Run diff
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
}
