/*
Sync engine for bootstrapping a Helix repo from an existing Git repo.

This module is only used during `helix init`. It performs a one-shot import of
Git state into Helix’s own storage. After that, Helix runs independently of
`.git`.

High-level responsibilities
---------------------------
SyncEngine::import_from_git wires together the full import pipeline:

1. Waits for `.git/index.lock` to disappear (wait_for_git_lock).
2. Imports the Git index into `.helix/helix.idx`:
   - Reads `.git/index` via GitIndex.
   - Applies ignore rules (IgnoreRules).
   - Compares index entries against the current HEAD tree to set flags.
3. Imports the entire commit graph:
   - Walks commits with gix (oldest to newest).
   - Builds Helix commits (Helix_Commit) and trees (TreeBuilder).
   - Writes them to `.helix/objects/commits` and `.helix/objects/trees`.
   - Builds a git_hash_to_helix_hash map (Git SHA -> Helix commit hash).
   - Updates `.helix/HEAD` to point at the latest commit.
4. Imports refs:
   - Branches: copies `.git/refs/heads/...` to `.helix/refs/heads/...`
     using the Git->Helix commit map.
   - Tags: copies `.git/refs/tags/...` to `.helix/refs/tags/...`
     (nested paths preserved), also via the Git->Helix map.
   - HEAD:
     - If symbolic (for example `ref: refs/heads/main`), mirror it into `.helix/HEAD`.
     - If detached, rewrite the Git SHA to the corresponding Helix commit hash.
5. Imports remotes:
   - Reads Git remotes via gix.
   - Writes them into `helix.toml` under a `[remotes]` table as
     `[remotes.<name>]` entries with URLs and refspecs.

After this import
-----------------
- `.helix/helix.idx` is the canonical index; Helix never writes `.git/index`.
- `.helix/objects/...` and `.helix/refs/...` form Helix’s own commit / tree /
  ref namespace, detached from Git.
- `helix.toml` becomes the persisted configuration source for remotes.

EntryFlags state model
----------------------
We model three "worlds" for each path:

- HEAD         : last committed state from Git.
- helix.idx    : Helix’s canonical index (replaces `.git/index`).
- working tree : files on disk.

Flags are derived during import_git_index as follows:

- TRACKED   -> this path exists in helix.idx
               (was present in `.git/index` during import).
- STAGED    -> index vs HEAD: the blob in `.git/index` differs from the blob
               in the HEAD tree (or the path is new and only in index).
- MODIFIED  -> working tree vs index: on-disk contents differ from the snapshot
               in `.git/index`.
- DELETED   -> file existed in HEAD but is missing from the working tree.
- UNTRACKED -> not handled here; only set later by FSMonitor / working-tree
               discovery for paths not in helix.idx.

This file is intentionally read-only with respect to Git: it never mutates
`.git/`. It only reads Git state and materializes the corresponding Helix view
under `.helix/` and `helix.toml`.
*/

use super::commit::Commit as Helix_Commit;
use super::format::{Entry, EntryFlags, Header};
use super::reader::Reader;
use super::state::set_branch_upstream;
use super::tree::TreeBuilder;
use super::writer::Writer;
use crate::ignore::IgnoreRules;
use crate::index::GitIndex;
use crate::init_command::{HelixConfig, IgnoreSection, RemotesTable};
use anyhow::{Context, Result};
use console::style;
use gix::revision::walk::Sorting;
use gix::{ObjectId, Repository};
use hash::compute_blob_oid;
use helix_protocol::hash::{self, hash_to_hex, Hash, ZERO_HASH};
use helix_protocol::message::ObjectType;
use helix_protocol::storage::FsObjectStore;
use indicatif::{ProgressBar, ProgressStyle};
use rayon::prelude::*;
use regex::Regex;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use toml::{value::Table, Value};
use walkdir::WalkDir;

pub struct SyncEngine {
    repo_path: PathBuf,
}

pub struct ImportSummary {
    pub commits_count: usize,
    pub files_count: usize,
    pub remotes_count: usize,
    pub author: Option<String>,
}

impl SyncEngine {
    pub fn new(repo_path: &Path) -> Self {
        Self {
            repo_path: repo_path.to_path_buf(),
        }
    }

    pub fn import_from_git(&self) -> Result<()> {
        let _ = wait_for_git_lock(&self.repo_path, Duration::from_secs(1));
        let store = FsObjectStore::new(&self.repo_path);

        println!("\n  {}", style("Helix").bold());
        println!(
            "  {}",
            style("────────────────────────────────────────────").dim()
        );

        let main_pb = ProgressBar::new_spinner();
        main_pb.set_style(
            ProgressStyle::default_spinner()
                .template("{spinner:.cyan} {msg}")?
                .tick_strings(&["⣾", "⣽", "⣻", "⢿", "⡿", "⣟", "⣯", "⣷"]),
        );
        main_pb.set_message("Analyzing Git history...");
        main_pb.enable_steady_tick(Duration::from_millis(80));

        let file_count = self.import_git_index(&store)?;
        let mapping = self.import_git_commits(&store, &main_pb)?;
        let commit_count = mapping.len();

        self.import_git_branches(&mapping)?;
        self.import_git_tags(&mapping)?;
        self.import_git_head(&mapping)?;

        let remote_count = self.import_git_remotes()?;
        let author = self.import_git_config()?;

        let mapping_path = self.repo_path.join(".helix/git-commit-mapping");
        if mapping_path.exists() {
            fs::remove_file(&mapping_path).ok();
        }

        main_pb.finish_and_clear();

        println!(
            "  {} Imported {} commits to Helix storage",
            style("✓").green(),
            style(commit_count).bold()
        );
        println!(
            "  {} Imported {} tracked files to index",
            style("✓").green(),
            style(file_count).bold()
        );

        if remote_count > 0 {
            println!(
                "  {} Migrated {} remote(s) to helix.toml",
                style("✓").green(),
                style(remote_count).bold()
            );
        }

        if let Some(a) = author {
            println!(
                "  {} Migrated author: {}",
                style("✓").green(),
                style(a).dim()
            );
        }

        println!("\n  {}", style("Helix repository initialized!").bold());

        println!("\n  {}", style("Next steps:").underlined());
        println!(
            "    {}      - Check working tree state",
            style("helix status").cyan()
        );
        println!(
            "    {}         - View imported history",
            style("helix log").cyan()
        );
        println!(
            "    {}      - Record new changes",
            style("helix commit").cyan()
        );
        println!("");

        Ok(())
    }

    fn import_git_config(&self) -> Result<Option<String>> {
        let repo = gix::open(&self.repo_path)?;
        let config = repo.config_snapshot();

        // Extract author info
        let author_name = match config.string("user.name") {
            Some(name) => name.to_string(),
            None => return Ok(None),
        };

        let author_email = match config.string("user.email") {
            Some(email) => email.to_string(),
            None => return Ok(None),
        };

        if author_name.trim().is_empty() || author_email.trim().is_empty() {
            return Ok(None);
        }

        let helix_toml_path = self.repo_path.join("helix.toml");

        // If helix.toml doesn't exist yet, we can't write to it.
        // In a 'h init', you might want to create it if it's missing,
        // but here we follow your original logic of skipping.
        if !helix_toml_path.exists() {
            return Ok(None);
        }

        // Parse and update TOML
        let content = fs::read_to_string(&helix_toml_path)
            .with_context(|| format!("Failed to read {}", helix_toml_path.display()))?;

        let mut root: Table = content
            .parse::<Value>()
            .context("Failed to parse helix.toml as TOML")?
            .try_into()
            .context("Root value is not a table")?;

        let user_table = root
            .entry("user".to_string())
            .or_insert_with(|| Value::Table(Table::new()))
            .as_table_mut()
            .context("[user] is not a table in helix.toml")?;

        user_table.insert("name".to_string(), Value::String(author_name.clone()));
        user_table.insert("email".to_string(), Value::String(author_email.clone()));

        let new_content =
            toml::to_string_pretty(&root).context("Failed to serialize helix.toml")?;

        fs::write(&helix_toml_path, new_content)
            .with_context(|| format!("Failed to write {}", helix_toml_path.display()))?;

        // Return the formatted string for the final TUI summary
        Ok(Some(format!("{} <{}>", author_name, author_email)))
    }

    // TODO: parallelize this as we walk the directory, we get the branch, transform it to a helix branch, get it's upstream adn then add it to the helix state
    fn import_git_branches(
        &self,
        git_hash_to_helix_hash: &HashMap<Vec<u8>, [u8; 32]>,
    ) -> Result<()> {
        let git_refs_dir = self.repo_path.join(".git/refs/heads");
        if !git_refs_dir.exists() {
            return Ok(());
        }

        // Track all branches we import for state file population
        let mut imported_branches = Vec::new();

        for entry in WalkDir::new(&git_refs_dir) {
            let entry = entry?;
            if !entry.file_type().is_file() {
                continue;
            }

            let git_sha = fs::read_to_string(entry.path())?.trim().to_string();
            let git_sha_bytes: Vec<u8> = hex::decode(&git_sha)?;

            // Convert Git SHA to Helix hash
            let helix_hash = match git_hash_to_helix_hash.get(&git_sha_bytes) {
                Some(hash) => hash,
                None => {
                    for (i, key) in git_hash_to_helix_hash.keys().take(3).enumerate() {
                        eprintln!("  {}: {}", i, hex::encode(key));
                    }
                    continue;
                }
            };

            // Preserve branch path structure
            let rel_path = entry.path().strip_prefix(&git_refs_dir)?;
            let helix_ref_path = self.repo_path.join(".helix/refs/heads").join(rel_path);

            fs::create_dir_all(helix_ref_path.parent().unwrap())?;
            fs::write(&helix_ref_path, hash::hash_to_hex(helix_hash))?;

            // Track branch name for state import
            if let Some(branch_name) = rel_path.to_str() {
                imported_branches.push(branch_name.to_string());
            }
        }

        // Import upstream tracking information from Git config
        self.import_branch_upstream_tracking(&imported_branches)?;

        Ok(())
    }

    /// Import upstream tracking information from .git/config to .helix/state
    fn import_branch_upstream_tracking(&self, branch_names: &[String]) -> Result<()> {
        let git_config_path = self.repo_path.join(".git/config");

        // Read git config if it exists
        let git_config = if git_config_path.exists() {
            Some(fs::read_to_string(&git_config_path).context("Failed to read .git/config")?)
        } else {
            None
        };

        // Determine the default branch (main or master)
        let default_branch = self.find_default_branch(branch_names);

        // Parse Git config for each branch
        for branch_name in branch_names {
            // Skip the default branch itself
            if Some(branch_name.as_str()) == default_branch.as_deref() {
                continue;
            }

            // Try to get upstream from Git config
            let upstream = if let Some(ref config) = git_config {
                self.parse_git_branch_upstream(config, branch_name)
            } else {
                None
            };

            // If no upstream found in Git config, use default branch
            let upstream = upstream.or(default_branch.clone());

            if let Some(upstream_branch) = upstream {
                if let Err(e) = set_branch_upstream(&self.repo_path, branch_name, &upstream_branch)
                {
                    eprintln!(
                        "Warning: Failed to import upstream for branch '{}': {}",
                        branch_name, e
                    );
                }
            }
        }

        Ok(())
    }

    /// Find the default branch (main or master)
    fn find_default_branch(&self, branch_names: &[String]) -> Option<String> {
        // Prefer "main", then "master", then first branch
        if branch_names.iter().any(|b| b == "main") {
            Some("main".to_string())
        } else if branch_names.iter().any(|b| b == "master") {
            Some("master".to_string())
        } else {
            branch_names.first().cloned()
        }
    }

    /// Parse Git config to extract upstream tracking for a branch
    fn parse_git_branch_upstream(&self, git_config: &str, branch_name: &str) -> Option<String> {
        let section_header = format!("[branch \"{}\"]", branch_name);

        let mut in_section = false;

        for line in git_config.lines() {
            let trimmed = line.trim();

            // Check if we're entering the right branch section
            if trimmed == section_header {
                in_section = true;
                continue;
            }

            // Check if we're leaving the section (new section starts)
            if trimmed.starts_with('[') && in_section {
                break;
            }

            if in_section {
                // Parse merge = refs/heads/main
                if let Some(merge) = trimmed.strip_prefix("merge = ") {
                    // Extract branch name from refs/heads/branch_name
                    if let Some(branch) = merge.strip_prefix("refs/heads/") {
                        return Some(branch.trim().to_string());
                    }
                }
            }
        }

        None
    }

    fn import_git_head(&self, git_hash_to_helix_hash: &HashMap<Vec<u8>, [u8; 32]>) -> Result<()> {
        let git_head = self.repo_path.join(".git/HEAD");
        let helix_head = self.repo_path.join(".helix/HEAD");

        let head_content = fs::read_to_string(&git_head)?;

        // Ensure .helix directory exists
        if let Some(parent) = helix_head.parent() {
            fs::create_dir_all(parent)?;
        }

        if head_content.starts_with("ref:") {
            // Symbolic reference (e.g., "ref: refs/heads/main")
            // Preserve it exactly
            fs::write(&helix_head, head_content)?;
        } else {
            // Detached HEAD: file contains raw commit SHA from Git
            let git_sha = head_content.trim();
            let git_sha_bytes = hex::decode(git_sha)?;

            let helix_hash = git_hash_to_helix_hash
                .get(&git_sha_bytes)
                .context("HEAD points to unknown commit")?;

            fs::write(&helix_head, hash::hash_to_hex(helix_hash))?;
        }

        Ok(())
    }

    fn import_git_remotes(&self) -> Result<usize> {
        let repo = gix::open(&self.repo_path)?;
        let remote_names: Vec<_> = repo
            .remote_names()
            .into_iter()
            .filter_map(|n| Some(n))
            .collect();

        if remote_names.is_empty() {
            return Ok(0);
        }

        let helix_toml_path = self.repo_path.join("helix.toml");

        let mut config: HelixConfig = if helix_toml_path.exists() {
            let content = fs::read_to_string(&helix_toml_path)?;
            toml::from_str(&content)
                .map_err(|e| anyhow::anyhow!("Failed to parse helix.toml: {}", e))?
        } else {
            HelixConfig {
                user: None,
                remotes: None,
                ignore: IgnoreSection::default(),
            }
        };

        let remotes_table = config.remotes.get_or_insert_with(|| RemotesTable {
            map: HashMap::new(),
        });

        // Regex to capture the host and path from SSH-style URLs
        // Handles: git@github.com:user/repo.git AND ssh://git@github.com/user/repo.git
        let ssh_re = Regex::new(r"^(?:ssh://)?git@(?P<host>[^:/]+)[:/](?P<path>.+)$").unwrap();

        let format_url = |url: String| -> String {
            // 1. Convert SSH to HTTPS if applicable
            let processed_url = if let Some(caps) = ssh_re.captures(&url) {
                format!("https://{}/{}", &caps["host"], &caps["path"])
            } else {
                url
            };

            // 2. Ensure https:// prefix and remove http://
            if processed_url.starts_with("https://") {
                processed_url
            } else {
                format!("https://{}", processed_url.trim_start_matches("http://"))
            }
        };

        let mut imported_count = 0;
        for remote_name in &remote_names {
            let name_str = remote_name.as_ref();
            let remote = match repo.find_remote(name_str) {
                Ok(r) => r,
                _ => continue,
            };

            // Process Pull
            if let Some(pull_url) = remote.url(gix::remote::Direction::Fetch) {
                let url_str = pull_url.to_bstring().to_string();
                remotes_table
                    .map
                    .insert(format!("{}_pull", name_str), format_url(url_str));
            }

            // Process Push
            let push_url = remote
                .url(gix::remote::Direction::Push)
                .or_else(|| remote.url(gix::remote::Direction::Fetch));

            if let Some(url) = push_url {
                let url_str = url.to_bstring().to_string();
                remotes_table
                    .map
                    .insert(format!("{}_push", name_str), format_url(url_str));
                imported_count += 1;
            }
        }

        if imported_count > 0 {
            let updated_content = toml::to_string_pretty(&config)?;
            fs::write(&helix_toml_path, updated_content)?;
        }

        Ok(imported_count)
    }

    fn import_git_tags(&self, git_hash_to_helix_hash: &HashMap<Vec<u8>, [u8; 32]>) -> Result<()> {
        let git_tags_dir = self.repo_path.join(".git/refs/tags");
        if !git_tags_dir.exists() {
            return Ok(());
        }

        for entry in WalkDir::new(&git_tags_dir) {
            let entry = entry?;
            if !entry.file_type().is_file() {
                continue;
            }

            let git_sha = fs::read_to_string(entry.path())?.trim().to_string();
            let git_sha_bytes = hex::decode(&git_sha)?;

            let helix_hash = match git_hash_to_helix_hash.get(&git_sha_bytes) {
                Some(h) => h,
                None => {
                    eprintln!(
                        "⚠️ Tag {:?} references unknown commit {} — skipping",
                        entry.path(),
                        git_sha
                    );
                    continue;
                }
            };

            // ✅ Preserve relative path under .git/refs/tags
            let rel_path = entry.path().strip_prefix(&git_tags_dir)?;
            let helix_tag_path = self.repo_path.join(".helix/refs/tags").join(rel_path);

            fs::create_dir_all(helix_tag_path.parent().unwrap())?;
            fs::write(&helix_tag_path, hash::hash_to_hex(helix_hash))?;
        }

        Ok(())
    }

    fn import_git_index(&self, store: &FsObjectStore) -> Result<usize> {
        let git_index_path = self.repo_path.join(".git/index");

        // Handle brand-new repo with no .git/index yet, return new empty Helix index
        if !&git_index_path.exists() {
            let header = Header::new(1, 0);
            let writer = Writer::new_canonical(&self.repo_path);
            writer.write(&header, &[])?;

            return Ok(0);
        }

        let reader = Reader::new(&self.repo_path);
        let (current_generation, current_entry_count) = if reader.exists() {
            reader
                .read()
                .ok()
                .map(|data| (data.header.generation, data.entries.len())) // Get entry count too
                .unwrap_or((0, 0))
        } else {
            (0, 0)
        };

        let git_index = GitIndex::open(&self.repo_path)?;
        let index_entries: Vec<_> = git_index.entries().collect();
        let total = index_entries.len();

        if total == 0 {
            return Ok(0);
        }

        let ignore_rules = IgnoreRules::load(&self.repo_path);

        let head_tree = self.load_full_head_tree()?;

        // only show if more than 1000 entries otherwise it flickers and looks weird
        let pb = if total > 1000 {
            let p = ProgressBar::new(total as u64);
            p.set_style(
                ProgressStyle::with_template(
                    "  {spinner:.green} [{wide_bar:.cyan/blue}] {pos}/{len} files",
                )?
                .progress_chars("#>-"),
            );
            Some(p)
        } else {
            None
        };

        let thread_safe_repo = gix::open(&self.repo_path)
            .context("Failed to open repository")?
            .into_sync();

        // i really don't like this but it handles an edge case but we should update this
        let is_first_import = current_entry_count == 0;

        // Build entries in parallel, updating the progress bar as we go
        let entries: Vec<Entry> = index_entries
            .into_par_iter()
            .map_init(
                || {
                    // This closure runs ONCE per worker thread
                    let local_repo = thread_safe_repo.to_thread_local();
                    (pb.clone(), &ignore_rules, local_repo)
                },
                |(pb, ignore_rules, local_repo), e| {
                    if let Some(ref p) = pb {
                        p.inc(1);
                    }

                    let path = Path::new(&e.path);
                    if ignore_rules.should_ignore(path) {
                        return None;
                    }

                    self.build_helix_entry_from_git_entry(
                        &e,
                        &head_tree,
                        local_repo,
                        store,
                        is_first_import,
                    )
                    .ok()
                },
            )
            .flatten()
            .collect();

        if let Some(p) = pb {
            p.finish_and_clear();
        }

        let header = Header::new(&current_generation + 1, entries.len() as u32);
        let writer = Writer::new_canonical(&self.repo_path);
        writer.write(&header, &entries)?;

        Ok(entries.len())
    }

    fn build_helix_entry_from_git_entry(
        &self,
        git_index_entry: &crate::index::IndexEntry,
        head_tree: &HashMap<PathBuf, Vec<u8>>,
        repo: &Repository,
        store: &FsObjectStore,
        _is_first_import: bool,
    ) -> Result<Entry> {
        let entry_path = PathBuf::from(&git_index_entry.path);
        let full_entry_path = self.repo_path.join(&PathBuf::from(&git_index_entry.path));

        let mut flags = EntryFlags::TRACKED;

        let git_index_entry_oid: &[u8; 20] = git_index_entry.oid.as_bytes();
        let git_object_id: ObjectId = ObjectId::from(*git_index_entry_oid);

        let blob_content: Vec<u8> = match repo.find_object(git_object_id) {
            Ok(obj) => obj
                .try_into_blob()
                .map(|b| b.data.to_vec())
                .unwrap_or_else(|_| fs::read(&full_entry_path).unwrap_or_default()),
            Err(_) => {
                if full_entry_path.exists() && full_entry_path.is_file() {
                    fs::read(&full_entry_path)?
                } else {
                    Vec::new()
                }
            }
        };

        // STAGED logic: only stage if git index differs from HEAD
        // Files matching HEAD are already committed, so not staged
        let is_staged = head_tree
            .get(&entry_path)
            .map(|head_git_oid| head_git_oid.as_slice() != git_index_entry_oid)
            .unwrap_or(true); // New files (not in HEAD) are staged
        if is_staged {
            flags |= EntryFlags::STAGED;
        }

        let helix_oid: [u8; 32] = store.write_object(&ObjectType::Blob, &blob_content)?;

        let was_in_head = head_tree.contains_key(&entry_path);

        // MODIFIED check: working tree vs index
        if full_entry_path.exists() && full_entry_path.is_file() {
            let working_content = fs::read(&full_entry_path)?;
            let working_git_oid = compute_blob_oid(&working_content);
            if &working_git_oid != git_index_entry_oid {
                flags |= EntryFlags::MODIFIED;
            }
        } else if was_in_head {
            flags |= EntryFlags::DELETED;
        }

        let (mtime_sec, file_size) = if full_entry_path.exists() {
            let metadata = fs::metadata(&full_entry_path)?;
            let mtime = metadata
                .modified()?
                .duration_since(std::time::UNIX_EPOCH)?
                .as_secs();
            (mtime, metadata.len())
        } else {
            (git_index_entry.mtime as u64, git_index_entry.size as u64)
        };

        Ok(Entry {
            path: entry_path,
            size: file_size,
            mtime_sec,
            mtime_nsec: 0,
            flags,
            merge_conflict_stage: 0,
            file_mode: git_index_entry.file_mode,
            oid: helix_oid,
            reserved: [0; 33],
        })
    }

    /// Get the current repo's HEAD commit and return a hashmap of all paths in the tree
    /// TODO: optimize with git commit import process, right now too seperate processes sequentially but can be combined which would only load the repo once and traverse the tree once to get commmit paths and objects
    fn load_full_head_tree(&self) -> Result<HashMap<PathBuf, Vec<u8>>> {
        let repo = gix::open(&self.repo_path).context("Failed to open repository with gix")?;

        let commit = match repo.head()?.peel_to_commit() {
            Ok(commit) => commit,
            Err(_) => return Ok(HashMap::new()),
        };

        let tree = commit
            .tree()
            .context("Failed to get tree from commit")?
            .to_owned();

        let mut recorder = gix::traverse::tree::Recorder::default();
        tree.traverse()
            .breadthfirst(&mut recorder)
            .context("Failed to traverse tree")?;

        let map: HashMap<PathBuf, Vec<u8>> = recorder
            .records
            .into_par_iter()
            .filter_map(|record| {
                if record.mode.is_blob() {
                    let path = PathBuf::from(record.filepath.to_string());
                    let oid_bytes = record.oid.to_owned().as_bytes().to_vec();
                    Some((path, oid_bytes))
                } else {
                    None
                }
            })
            .collect();
        Ok(map)
    }

    // TODO: parallelize the two different passes here: 1. to compute all committ hashes inparallel, 2. build final commits with correct parents in parallel
    fn import_git_commits(
        &self,
        store: &FsObjectStore,
        pb: &ProgressBar,
    ) -> Result<HashMap<Vec<u8>, [u8; 32]>> {
        let repo = gix::open(&self.repo_path)?;

        let mut seen = HashSet::new();
        let mut git_hash_to_helix_hash: HashMap<Vec<u8>, [u8; 32]> = HashMap::new();
        let mut collected_git_commits: Vec<(Vec<u8>, gix::Commit)> = Vec::new();

        pb.set_message(format!("Importing commits and files..."));

        // Collect commits from all branches (including head)
        let refs = repo.references()?;
        let branch_refs: Vec<_> = refs
            .all()?
            .filter_map(Result::ok)
            .filter(|r| r.name().as_bstr().starts_with(b"refs/heads/"))
            .collect();

        if branch_refs.is_empty() {
            return Ok(HashMap::new());
        }

        // Walk from each branch tip
        for mut branch_ref in branch_refs {
            let commit = match branch_ref.peel_to_commit() {
                Ok(c) => c,
                Err(_) => continue, // Skip if can't resolve
            };

            let commit_iter = commit.ancestors().sorting(Sorting::ByCommitTime(
                gix::traverse::commit::simple::CommitTimeOrder::NewestFirst,
            ));

            for commit_result in commit_iter.all()? {
                let commit_info = commit_result?;
                let git_id_bytes = commit_info.id().as_bytes().to_vec();

                // Skip if already seen (handles merge commits and shared history)
                if !seen.insert(git_id_bytes.clone()) {
                    continue;
                }

                let git_commit = commit_info.object()?;

                collected_git_commits.push((git_id_bytes, git_commit));
            }
        }

        // Sort oldest → newest by commit time
        collected_git_commits.sort_by_key(|(_, c)| c.time().unwrap().seconds);

        // Build Helix commits in oldest to newest order
        let mut helix_commits: Vec<Helix_Commit> = Vec::with_capacity(collected_git_commits.len());

        for (_i, (git_id_bytes, git_commit)) in collected_git_commits.into_iter().enumerate() {
            let helix_commit = self.build_helix_commit_from_git_commit(
                &git_commit,
                &repo,
                &git_hash_to_helix_hash,
            )?;

            // Now we know this commit's helix hash, so map git → helix for children
            git_hash_to_helix_hash.insert(git_id_bytes, helix_commit.commit_hash);

            helix_commits.push(helix_commit);
        }

        self.store_imported_commits(&store, &helix_commits)?;

        // Update HEAD to point to the current branch's commit
        if let Ok(mut head_ref) = repo.head() {
            if let Ok(head_commit) = head_ref.peel_to_commit() {
                let head_git_sha = head_commit.id().as_bytes().to_vec();
                if let Some(&helix_hash) = git_hash_to_helix_hash.get(&head_git_sha) {
                    self.update_head_to_commit(helix_hash)?;
                }
            }
        }

        self.save_git_helix_mapping(&git_hash_to_helix_hash)?;

        Ok(git_hash_to_helix_hash)
    }

    fn save_git_helix_mapping(&self, mapping: &HashMap<Vec<u8>, [u8; 32]>) -> Result<()> {
        let mapping_path = self.repo_path.join(".helix/git-commit-mapping");

        let mut content = String::new();
        for (git_sha, helix_hash) in mapping {
            let git_hex = hex::encode(git_sha);
            let helix_hex = hash_to_hex(helix_hash);
            // Write: helix_hash (64 chars) THEN git_sha (40 chars)
            content.push_str(&format!("{} {}\n", helix_hex, git_hex));
        }

        fs::write(&mapping_path, content)?;
        Ok(())
    }

    fn build_helix_commit_from_git_commit(
        &self,
        git_commit: &gix::Commit,
        repo: &gix::Repository,
        git_to_helix: &HashMap<Vec<u8>, [u8; 32]>,
    ) -> Result<Helix_Commit> {
        let message = git_commit.message()?;
        let author_name = git_commit.author()?.name.to_string();
        let author_email = git_commit.author()?.email.to_string();
        let author_timestamp = git_commit.author()?.time()?.seconds;
        let commit_time = git_commit.time()?.seconds;

        let full_message = format!(
            "{}{}{}",
            message.title.to_string(),
            if message.body.is_some() { "\n\n" } else { "" },
            message.body.map(|b| b.to_string()).unwrap_or_default()
        );

        let parent_commits: Vec<[u8; 32]> = git_commit
            .parent_ids()
            .filter_map(|git_parent_id| {
                let git_parent_bytes = git_parent_id.as_bytes().to_vec();
                git_to_helix.get(&git_parent_bytes).copied()
            })
            .collect();

        let tree_id = git_commit.tree()?.id;
        let tree_object = repo.find_object(tree_id)?;
        let git_tree = tree_object.into_tree();

        let mut recorder = gix::traverse::tree::Recorder::default();
        git_tree
            .traverse()
            .breadthfirst(&mut recorder)
            .context("Failed to traverse tree")?;

        let tree = self.build_helix_tree_from_recorder(recorder, repo)?;

        let mut commit = Helix_Commit {
            commit_hash: ZERO_HASH,
            tree_hash: tree.into(),
            parents: parent_commits,
            author: format!("{} <{}>", &author_name, author_email),
            author_time: author_timestamp as u64,
            commit_time: commit_time as u64,
            message: full_message,
        };

        commit.commit_hash = commit.compute_hash();

        Ok(commit)
    }

    /// Build Helix tree from gix Recorder (same pattern as load_full_head_tree)
    fn build_helix_tree_from_recorder(
        &self,
        recorder: gix::traverse::tree::Recorder,
        repo: &gix::Repository,
    ) -> Result<Hash> {
        let blob_storage = FsObjectStore::new(&self.repo_path);

        // Convert gix records to Helix Entry format
        let entries: Vec<Entry> = recorder
            .records
            .iter()
            .filter_map(|record| {
                // Only process blobs (files), skip trees (directories)
                if !record.mode.is_blob() && !record.mode.is_link() {
                    return None;
                }

                let path = PathBuf::from(record.filepath.to_string());

                // Read actual blob content from Git and store it in Helix
                let blob_content = match repo.find_object(record.oid) {
                    Ok(obj) => match obj.try_into_blob() {
                        Ok(blob) => blob.data.to_vec(),
                        Err(_) => {
                            eprintln!("Warning: Failed to read blob for {}", record.filepath);
                            return None;
                        }
                    },
                    Err(_) => {
                        eprintln!("Warning: Failed to find object for {}", record.filepath);
                        return None;
                    }
                };

                // Write blob content to Helix storage and get the BLAKE3 hash
                let oid = match blob_storage.write_object(&ObjectType::Blob, &blob_content) {
                    Ok(hash) => hash,
                    Err(e) => {
                        eprintln!(
                            "Warning: Failed to write blob for {}: {}",
                            record.filepath, e
                        );
                        return None;
                    }
                };

                // Determine file mode
                let file_mode = if record.mode.is_link() {
                    0o120000 // Symlink
                } else if record.mode.is_executable() {
                    0o100755 // Executable
                } else {
                    0o100644 // Regular file
                };

                Some(Entry {
                    path,
                    oid,
                    flags: EntryFlags::TRACKED,
                    size: blob_content.len() as u64,
                    mtime_sec: 0,
                    mtime_nsec: 0,
                    file_mode,
                    merge_conflict_stage: 0,
                    reserved: [0u8; 33],
                })
            })
            .collect();

        // Use TreeBuilder to create the tree structure
        let tree_builder = TreeBuilder::new(&self.repo_path);
        let tree_hash = tree_builder.build_from_entries(&entries)?;

        Ok(tree_hash.into())
    }

    fn store_imported_commits(
        &self,
        store: &FsObjectStore,
        commits: &[Helix_Commit],
    ) -> Result<()> {
        let pb = ProgressBar::new(commits.len() as u64);
        pb.set_style(
            ProgressStyle::with_template(
                "{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] \
             {pos}/{len} commits ({eta})",
            )?
            .progress_chars(">-"),
        );

        for commit in commits {
            let raw = commit.to_bytes();
            store.write_object_with_hash(&ObjectType::Commit, &commit.commit_hash, &raw)?;
        }
        pb.finish_with_message("commits stored");

        Ok(())
    }

    /// Update HEAD to point to the latest imported commit
    fn update_head_to_commit(&self, commit_hash: [u8; 32]) -> Result<()> {
        let head_path = self.repo_path.join(".helix").join("HEAD");

        // Read current HEAD to check if it's a symbolic reference
        if head_path.exists() {
            let content = fs::read_to_string(&head_path)?;
            let content = content.trim();

            if content.starts_with("ref:") {
                // Update the branch it points to
                let ref_path = content.strip_prefix("ref:").unwrap().trim();
                let full_ref_path = self.repo_path.join(".helix").join(ref_path);

                // Ensure parent directory exists
                if let Some(parent) = full_ref_path.parent() {
                    fs::create_dir_all(parent)?;
                }

                let hash_hex = hash::hash_to_hex(&commit_hash);
                fs::write(&full_ref_path, hash_hex)?;
                return Ok(());
            }
        }

        // Direct HEAD update (detached HEAD)
        let hash_hex = hash::hash_to_hex(&commit_hash);
        fs::write(&head_path, hash_hex)?;

        Ok(())
    }
}

fn wait_for_git_lock(repo_path: &Path, timeout: Duration) -> Result<()> {
    let lock_path = repo_path.join(".git/index.lock");
    let start = Instant::now();

    while lock_path.exists() {
        if start.elapsed() > timeout {
            anyhow::bail!("Timeout waiting for .git/index.lock");
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;
    use tempfile::TempDir;

    fn init_test_repo(path: &Path) -> Result<()> {
        fs::create_dir_all(path.join(".git"))?;
        Command::new("git")
            .args(&["init", "-b", "main"])
            .current_dir(path)
            .output()?;

        Command::new("git")
            .args(&["config", "user.name", "Test"])
            .current_dir(path)
            .output()?;
        Command::new("git")
            .args(&["config", "user.email", "test@test.com"])
            .current_dir(path)
            .output()?;

        Ok(())
    }

    fn git(path: &Path, args: &[&str]) -> Result<()> {
        let status = Command::new("git").args(args).current_dir(path).status()?;
        assert!(status.success(), "git {:?} failed", args);
        Ok(())
    }

    #[test]
    fn test_import_from_git() -> Result<()> {
        let temp_dir = TempDir::new()?;
        init_test_repo(temp_dir.path())?;

        fs::write(temp_dir.path().join("test.txt"), "hello")?;
        Command::new("git")
            .args(&["add", "test.txt"])
            .current_dir(temp_dir.path())
            .output()?;

        let syncer = SyncEngine::new(temp_dir.path());
        syncer.import_from_git()?;

        let reader = Reader::new(temp_dir.path());
        assert!(reader.exists());

        let data = reader.read()?;
        assert_eq!(data.entries.len(), 1);
        assert_eq!(data.entries[0].path, PathBuf::from("test.txt"));
        assert!(data.entries[0].flags.contains(EntryFlags::TRACKED));
        assert!(data.entries[0].flags.contains(EntryFlags::STAGED));

        Ok(())
    }

    #[test]
    fn test_import_empty_repo() -> Result<()> {
        let temp_dir = TempDir::new()?;
        init_test_repo(temp_dir.path())?;

        // No files added to Git
        let syncer = SyncEngine::new(temp_dir.path());
        syncer.import_from_git()?;

        let reader = Reader::new(temp_dir.path());
        let data = reader.read()?;

        // Should create empty index
        assert_eq!(data.entries.len(), 0);
        assert_eq!(data.header.generation, 1);

        Ok(())
    }

    #[test]
    fn test_import_increments_generation() -> Result<()> {
        let temp_dir = TempDir::new()?;
        init_test_repo(temp_dir.path())?;

        fs::write(temp_dir.path().join("test.txt"), "hello")?;
        Command::new("git")
            .args(&["add", "test.txt"])
            .current_dir(temp_dir.path())
            .output()?;

        let syncer = SyncEngine::new(temp_dir.path());

        // First import
        syncer.import_from_git()?;
        let reader = Reader::new(temp_dir.path());
        let data1 = reader.read()?;
        assert_eq!(data1.header.generation, 1);
        assert_eq!(data1.entries.len(), 1);
        assert_eq!(data1.entries[0].path, PathBuf::from("test.txt"));

        // Second import (re-init)
        syncer.import_from_git()?;
        let data2 = reader.read()?;
        assert_eq!(data2.header.generation, 2);
        assert_eq!(data2.entries.len(), 1);
        assert_eq!(data2.entries[0].path, PathBuf::from("test.txt"));

        Ok(())
    }

    #[test]
    fn test_import_detects_staged() -> Result<()> {
        let temp_dir = TempDir::new()?;
        init_test_repo(temp_dir.path())?;

        // Create file and commit
        fs::write(temp_dir.path().join("test.txt"), "hello")?;
        Command::new("git")
            .args(&["add", "test.txt"])
            .current_dir(temp_dir.path())
            .output()?;
        Command::new("git")
            .args(&["commit", "-m", "initial"])
            .current_dir(temp_dir.path())
            .output()?;

        // Modify and stage
        fs::write(temp_dir.path().join("test.txt"), "world")?;
        Command::new("git")
            .args(&["add", "test.txt"])
            .current_dir(temp_dir.path())
            .output()?;

        // Import
        let syncer = SyncEngine::new(temp_dir.path());
        syncer.import_from_git()?;

        let reader = Reader::new(temp_dir.path());
        let data = reader.read()?;

        assert_eq!(data.entries.len(), 1);
        assert!(data.entries[0].flags.contains(EntryFlags::STAGED));

        Ok(())
    }

    #[test]
    fn test_import_detects_unstaged() -> Result<()> {
        let temp_dir = TempDir::new()?;
        init_test_repo(temp_dir.path())?;

        // Create, stage, and commit a file
        fs::write(temp_dir.path().join("stable.txt"), "content")?;
        Command::new("git")
            .args(&["add", "stable.txt"])
            .current_dir(temp_dir.path())
            .output()?;
        Command::new("git")
            .args(&["commit", "-m", "add stable file"])
            .current_dir(temp_dir.path())
            .output()?;

        // Import
        let syncer = SyncEngine::new(temp_dir.path());
        syncer.import_from_git()?;

        let reader = Reader::new(temp_dir.path());
        let data = reader.read()?;

        assert_eq!(data.entries.len(), 1);

        let entry = &data.entries[0];
        assert!(entry.flags.contains(EntryFlags::TRACKED));
        assert!(
            !entry.flags.contains(EntryFlags::STAGED),
            "Committed file that matches HEAD should not be staged"
        );

        Ok(())
    }

    #[test]
    fn test_wait_for_git_lock_timeout() -> Result<()> {
        let temp_dir = TempDir::new()?;
        init_test_repo(temp_dir.path())?;

        // Create a fake lock file
        let lock_path = temp_dir.path().join(".git/index.lock");
        fs::write(&lock_path, "")?;

        // Should timeout
        let result = wait_for_git_lock(temp_dir.path(), Duration::from_millis(100));
        assert!(result.is_err());

        // Clean up
        fs::remove_file(&lock_path)?;

        Ok(())
    }

    #[test]
    fn test_import_from_git_new_uncommitted_file_tracked_and_staged() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo = temp_dir.path();
        init_test_repo(repo)?;

        // Untracked, staged-only file (no commits)
        fs::write(repo.join("test.txt"), "hello")?;
        git(repo, &["add", "test.txt"])?;

        let syncer = SyncEngine::new(repo);
        syncer.import_from_git()?;

        let reader = Reader::new(repo);
        assert!(reader.exists(), "helix index should exist after import");

        let data = reader.read()?;
        assert_eq!(data.entries.len(), 1);
        let entry = &data.entries[0];

        assert_eq!(entry.path, PathBuf::from("test.txt"));
        assert!(entry.flags.contains(EntryFlags::TRACKED));
        assert!(entry.flags.contains(EntryFlags::STAGED));
        assert!(
            !entry.flags.contains(EntryFlags::MODIFIED),
            "freshly staged file should not be marked MODIFIED"
        );
        assert!(
            !entry.flags.contains(EntryFlags::DELETED),
            "existing file should not be marked DELETED"
        );

        Ok(())
    }

    #[test]
    fn test_import_detects_unstaged_modified_file() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo = temp_dir.path();
        init_test_repo(repo)?;

        // Commit initial content
        fs::write(repo.join("file.txt"), "v1")?;
        git(repo, &["add", "file.txt"])?;
        git(repo, &["commit", "-m", "v1"])?;

        // Modify working tree but DO NOT stage
        fs::write(repo.join("file.txt"), "v2")?;

        let syncer = SyncEngine::new(repo);
        syncer.import_from_git()?;

        let reader = Reader::new(repo);
        let data = reader.read()?;

        assert_eq!(data.entries.len(), 1);
        let entry = &data.entries[0];

        assert!(entry.flags.contains(EntryFlags::TRACKED));
        assert!(
            !entry.flags.contains(EntryFlags::STAGED),
            "un-staged changes should not flip STAGED"
        );
        assert!(
            entry.flags.contains(EntryFlags::MODIFIED),
            "working != index should set MODIFIED"
        );
        assert!(
            !entry.flags.contains(EntryFlags::DELETED),
            "file still exists on disk"
        );

        Ok(())
    }

    #[test]
    fn test_import_detects_clean_committed_file() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo = temp_dir.path();
        init_test_repo(repo)?;

        // Create and commit a file
        fs::write(repo.join("stable.txt"), "content")?;
        git(repo, &["add", "stable.txt"])?;
        git(repo, &["commit", "-m", "initial"])?;

        let syncer = SyncEngine::new(repo);
        syncer.import_from_git()?;

        let reader = Reader::new(repo);
        let data = reader.read()?;

        assert_eq!(data.entries.len(), 1);
        let entry = &data.entries[0];

        assert!(entry.flags.contains(EntryFlags::TRACKED));
        assert!(
            !entry.flags.contains(EntryFlags::STAGED),
            "committed file matching HEAD should not be STAGED"
        );
        assert!(
            !entry.flags.contains(EntryFlags::MODIFIED),
            "clean working tree should not be MODIFIED"
        );
        assert!(
            !entry.flags.contains(EntryFlags::DELETED),
            "existing committed file should not be DELETED"
        );

        Ok(())
    }

    #[test]
    fn test_import_detects_staged_but_not_modified() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo = temp_dir.path();
        init_test_repo(repo)?;

        // Commit v1
        fs::write(repo.join("file.txt"), "v1")?;
        git(repo, &["add", "file.txt"])?;
        git(repo, &["commit", "-m", "v1"])?;

        // Change content and stage it (index != HEAD, working == index)
        fs::write(repo.join("file.txt"), "v2")?;
        git(repo, &["add", "file.txt"])?;

        let syncer = SyncEngine::new(repo);
        syncer.import_from_git()?;

        let reader = Reader::new(repo);
        let data = reader.read()?;

        assert_eq!(data.entries.len(), 1);
        let entry = &data.entries[0];

        assert!(entry.flags.contains(EntryFlags::TRACKED));
        assert!(
            entry.flags.contains(EntryFlags::STAGED),
            "index != HEAD should set STAGED"
        );
        assert!(
            !entry.flags.contains(EntryFlags::MODIFIED),
            "working == index, so no MODIFIED"
        );
        assert!(
            !entry.flags.contains(EntryFlags::DELETED),
            "file exists on disk"
        );

        Ok(())
    }

    #[test]
    fn test_import_marks_deleted_if_missing_on_disk_but_in_head() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo = temp_dir.path();
        init_test_repo(repo)?;

        // Commit file into HEAD and index
        fs::write(repo.join("gone.txt"), "content")?;
        git(repo, &["add", "gone.txt"])?;
        git(repo, &["commit", "-m", "add gone"])?;

        // Manually delete from working tree (no git rm, so index + HEAD still think it exists)
        fs::remove_file(repo.join("gone.txt"))?;

        let syncer = SyncEngine::new(repo);
        syncer.import_from_git()?;

        let reader = Reader::new(repo);
        let data = reader.read()?;

        assert_eq!(data.entries.len(), 1);
        let entry = &data.entries[0];

        assert!(entry.flags.contains(EntryFlags::TRACKED));
        assert!(
            entry.flags.contains(EntryFlags::DELETED),
            "missing on disk but present in HEAD should be DELETED"
        );
        // Depending on how you want to interpret it, MODIFIED may or may not be set.
        // Currently, code only sets DELETED in this branch.
        assert!(
            !entry.flags.contains(EntryFlags::MODIFIED),
            "deleted files are handled via DELETED flag"
        );

        Ok(())
    }

    #[test]
    fn test_import_git_commits_empty_repo() -> Result<()> {
        let temp_dir = TempDir::new()?;
        init_test_repo(temp_dir.path())?;

        // No commits yet
        let syncer = SyncEngine::new(temp_dir.path());
        let main_pb = ProgressBar::new_spinner();
        let store = FsObjectStore::new(temp_dir.path());
        syncer.import_git_commits(&store, &main_pb)?;
        let commit_reader = CommitStore::new(temp_dir.path(), store)?;

        assert_eq!(
            commit_reader.list_commits()?.len(),
            0,
            "Empty repo should have no commits"
        );

        Ok(())
    }

    #[test]
    fn test_import_git_commits_single_commit() -> Result<()> {
        let temp_dir = TempDir::new()?;
        init_test_repo(temp_dir.path())?;

        // Create and commit a file
        fs::write(temp_dir.path().join("test.txt"), "hello")?;
        git(temp_dir.path(), &["add", "test.txt"])?;
        git(temp_dir.path(), &["commit", "-m", "Initial commit"])?;

        let main_pb = ProgressBar::new_spinner();

        // Import commits
        let syncer = SyncEngine::new(temp_dir.path());
        let store = FsObjectStore::new(temp_dir.path());
        syncer.import_git_commits(&store, &main_pb)?;

        let commit_reader = CommitStore::new(temp_dir.path(), store)?;

        let commits = &commit_reader.list_commits()?;
        let first_commit = commit_reader.read_commit(&commits[0])?;
        assert_eq!(first_commit.message, "Initial commit\n");
        assert!(
            first_commit.parents.is_empty(),
            "Initial commit should have no parents"
        );
        assert_ne!(
            first_commit.commit_hash, ZERO_HASH,
            "Commit hash should be computed"
        );
        assert_ne!(
            first_commit.tree_hash.as_ref(),
            &ZERO_HASH,
            "Tree hash should be computed"
        );

        Ok(())
    }

    fn import_commits(repo_dir: &Path) -> Result<Vec<Helix_Commit>> {
        let syncer = SyncEngine::new(repo_dir);
        let main_pb = ProgressBar::new_spinner();
        let store = FsObjectStore::new(repo_dir);
        syncer.import_git_commits(&store, &main_pb)?;
        let commit_reader = CommitStore::new(repo_dir, store)?;
        let commits = &commit_reader.list_commits()?;

        let mut commits: Vec<_> = commits
            .iter()
            .map(|hash| commit_reader.read_commit(hash).unwrap())
            .collect();

        commits.sort_by_key(|c| c.commit_time);

        Ok(commits)
    }

    #[test]
    fn test_import_git_commits_multiple_commits() -> Result<()> {
        let temp_dir = TempDir::new()?;
        init_test_repo(temp_dir.path())?;

        fs::write(temp_dir.path().join("file1.txt"), "content1")?;
        git(temp_dir.path(), &["add", "file1.txt"])?;
        git(temp_dir.path(), &["commit", "-m", "First commit"])?;
        std::thread::sleep(std::time::Duration::from_secs(1));

        fs::write(temp_dir.path().join("file2.txt"), "content2")?;
        git(temp_dir.path(), &["add", "file2.txt"])?;
        git(temp_dir.path(), &["commit", "-m", "Second commit"])?;
        std::thread::sleep(std::time::Duration::from_secs(1));

        fs::write(temp_dir.path().join("file3.txt"), "content3")?;
        git(temp_dir.path(), &["add", "file3.txt"])?;
        git(temp_dir.path(), &["commit", "-m", "Third commit"])?;

        let commits = import_commits(temp_dir.path())?;

        assert_eq!(commits[0].message, "First commit\n");
        assert_eq!(commits[1].message, "Second commit\n");
        assert_eq!(commits[2].message, "Third commit\n");

        assert!(commits[0].parents.is_empty(), "First commit has no parent");
        assert_eq!(commits[1].parents.len(), 1, "Second commit has 1 parent");
        assert_eq!(commits[2].parents.len(), 1, "Third commit has 1 parent");

        assert_eq!(
            commits[1].parents[0], commits[0].commit_hash,
            "Second commit's parent should be first commit"
        );
        assert_eq!(
            commits[2].parents[0], commits[1].commit_hash,
            "Third commit's parent should be second commit"
        );

        Ok(())
    }

    #[test]
    fn test_import_git_commits_with_multiline_message() -> Result<()> {
        let temp_dir = TempDir::new()?;
        init_test_repo(temp_dir.path())?;

        // Create commit with multiline message
        fs::write(temp_dir.path().join("test.txt"), "hello")?;
        git(temp_dir.path(), &["add", "test.txt"])?;

        let multiline_msg = "Short summary\n\nLonger description here.\nWith multiple lines.\n";
        git(temp_dir.path(), &["commit", "-m", multiline_msg])?;

        let commits = import_commits(temp_dir.path())?;

        assert_eq!(commits.len(), 1);
        assert_eq!(commits[0].message, multiline_msg);

        Ok(())
    }

    #[test]
    fn test_import_git_commits_preserves_tree_structure() -> Result<()> {
        let temp_dir = TempDir::new()?;
        init_test_repo(temp_dir.path())?;

        // Create nested directory structure
        fs::create_dir_all(temp_dir.path().join("src/lib"))?;
        fs::write(temp_dir.path().join("README.md"), "# Project")?;
        fs::write(temp_dir.path().join("src/main.rs"), "fn main() {}")?;
        fs::write(temp_dir.path().join("src/lib/mod.rs"), "pub mod lib;")?;

        git(temp_dir.path(), &["add", "."])?;
        git(temp_dir.path(), &["commit", "-m", "Initial structure"])?;

        // Whatever helper you have that triggers import + returns commits
        let commits = import_commits(temp_dir.path())?;
        assert_eq!(commits.len(), 1);

        // Verify tree was created and stored
        let tree_hash = commits[0].tree_hash;
        assert_ne!(
            tree_hash.as_ref(),
            &ZERO_HASH,
            "Tree hash should be computed"
        );

        let tree_storage = TreeStore::for_repo(temp_dir.path());
        let tree_hash_array: [u8; 32] = tree_hash.into();
        let root_tree = tree_storage.read(&tree_hash_array)?;

        println!("root tree entries {:?}", root_tree.entries);

        // Root should have README.md and src (directory)
        assert!(
            root_tree.entries.iter().any(|e| e.name == "README.md"),
            "Root tree should contain README.md"
        );
        let src_entry = root_tree
            .entries
            .iter()
            .find(|e| e.name == "src")
            .expect("Root tree should contain 'src' subtree");

        // Load src tree
        let src_tree_hash: [u8; 32] = src_entry.oid;
        let src_tree = tree_storage.read(&src_tree_hash)?;
        println!("src tree entries {:?}", src_tree.entries);

        assert!(
            src_tree.entries.iter().any(|e| e.name == "main.rs"),
            "src tree should contain main.rs"
        );
        let lib_entry = src_tree
            .entries
            .iter()
            .find(|e| e.name == "lib")
            .expect("src tree should contain 'lib' subtree");

        // Load src/lib tree
        let lib_tree_hash: [u8; 32] = lib_entry.oid;
        let lib_tree = tree_storage.read(&lib_tree_hash)?;
        println!("lib tree entries {:?}", lib_tree.entries);

        assert!(
            lib_tree.entries.iter().any(|e| e.name == "mod.rs"),
            "src/lib tree should contain mod.rs"
        );

        Ok(())
    }

    #[test]
    fn test_import_git_commits_deduplication() -> Result<()> {
        let temp_dir = TempDir::new()?;
        init_test_repo(temp_dir.path())?;

        // Create commit
        fs::write(temp_dir.path().join("test.txt"), "hello")?;
        git(temp_dir.path(), &["add", "test.txt"])?;
        git(temp_dir.path(), &["commit", "-m", "Test commit"])?;

        // Import twice
        let commits1 = import_commits(temp_dir.path())?;
        let commits2 = import_commits(temp_dir.path())?;

        // Should return same commits
        assert_eq!(commits1.len(), commits2.len());
        assert_eq!(commits1[0].commit_hash, commits2[0].commit_hash);

        let tree_hash1: [u8; 32] = commits1[0].tree_hash.into();
        let tree_hash2: [u8; 32] = commits2[0].tree_hash.into();
        assert_eq!(tree_hash1, tree_hash2);

        Ok(())
    }

    #[test]
    fn test_import_git_commits_with_author_info() -> Result<()> {
        let temp_dir = TempDir::new()?;
        init_test_repo(temp_dir.path())?;

        // Set specific author
        git(temp_dir.path(), &["config", "user.name", "John Doe"])?;
        git(
            temp_dir.path(),
            &["config", "user.email", "john@example.com"],
        )?;

        // Create commit
        fs::write(temp_dir.path().join("test.txt"), "hello")?;
        git(temp_dir.path(), &["add", "test.txt"])?;
        git(temp_dir.path(), &["commit", "-m", "Test commit"])?;

        let commits = import_commits(temp_dir.path())?;

        assert_eq!(commits.len(), 1);
        assert!(
            commits[0].author.contains("john@example.com"),
            "Author should contain email"
        );
        assert!(
            commits[0].author.contains("John Doe"),
            "Author should contain name"
        );

        println!("the commit {:?}", commits[0]);
        assert!(commits[0].author_time > 0, "Author time should be set");
        assert!(commits[0].commit_time > 0, "Commit time should be set");

        Ok(())
    }

    #[test]
    fn test_import_git_commits_hash_consistency() -> Result<()> {
        let temp_dir = TempDir::new()?;
        init_test_repo(temp_dir.path())?;

        // Create commit
        fs::write(temp_dir.path().join("test.txt"), "hello")?;
        git(temp_dir.path(), &["add", "test.txt"])?;
        git(temp_dir.path(), &["commit", "-m", "Test commit"])?;

        // Import commits
        let commits = import_commits(temp_dir.path())?;

        assert_eq!(commits.len(), 1);

        let commit = &commits[0];

        // Recompute hash and verify it matches
        let recomputed_hash = commit.compute_hash();
        assert_eq!(
            commit.commit_hash, recomputed_hash,
            "Stored hash should match recomputed hash"
        );

        Ok(())
    }

    #[test]
    fn test_import_git_commits_returns_git_to_helix_map() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo = temp_dir.path();
        init_test_repo(repo)?;

        // Create and commit a file
        fs::write(repo.join("test.txt"), "hello")?;
        git(repo, &["add", "test.txt"])?;
        git(repo, &["commit", "-m", "Test commit"])?;

        // Get the Git SHA for HEAD
        let output = Command::new("git")
            .args(&["rev-parse", "HEAD"])
            .current_dir(repo)
            .output()?;
        let git_sha = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let git_sha_bytes = hex::decode(&git_sha)?;

        let engine = SyncEngine::new(repo);
        let main_pb = ProgressBar::new_spinner();
        let store = FsObjectStore::new(repo);
        let git_to_helix = engine.import_git_commits(&store, &main_pb)?;

        // There should be an entry for HEAD
        let helix_hash_from_map = git_to_helix
            .get(&git_sha_bytes)
            .expect("git→helix map should contain HEAD commit");

        // And it should match the stored commit's hash
        let loader = CommitStore::new(repo, store)?;
        let hashes = loader.list_commits()?;
        assert_eq!(hashes.len(), 1);
        let stored_commit = loader.read_commit(&hashes[0])?;
        assert_eq!(
            &stored_commit.commit_hash, helix_hash_from_map,
            "map value should match stored commit hash"
        );

        Ok(())
    }

    #[test]
    fn test_import_git_commits_updates_helix_head() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo = temp_dir.path();
        init_test_repo(repo)?;

        // One commit
        fs::write(repo.join("test.txt"), "hello")?;
        git(repo, &["add", "test.txt"])?;
        git(repo, &["commit", "-m", "Test commit"])?;

        let engine = SyncEngine::new(repo);
        let main_pb = ProgressBar::new_spinner();
        let store = FsObjectStore::new(repo);
        let git_to_helix = engine.import_git_commits(&store, &main_pb)?;

        // Find HEAD Git SHA
        let output = Command::new("git")
            .args(&["rev-parse", "HEAD"])
            .current_dir(repo)
            .output()?;
        let git_sha = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let git_sha_bytes = hex::decode(&git_sha)?;

        let helix_hash = git_to_helix
            .get(&git_sha_bytes)
            .expect("git→helix map should contain HEAD commit");

        let helix_head_path = repo.join(".helix/HEAD");
        assert!(
            helix_head_path.exists(),
            ".helix/HEAD should be created by import_git_commits"
        );

        let contents = fs::read_to_string(&helix_head_path)?;
        assert_eq!(
            contents.trim(),
            hash::hash_to_hex(helix_hash),
            ".helix/HEAD should point at latest imported commit"
        );

        Ok(())
    }

    #[test]
    fn test_import_git_commits_with_merge_commit() -> Result<()> {
        let temp_dir = TempDir::new()?;
        init_test_repo(temp_dir.path())?;

        // Create initial commit on main
        fs::write(temp_dir.path().join("main.txt"), "main")?;
        git(temp_dir.path(), &["add", "main.txt"])?;
        git(temp_dir.path(), &["commit", "-m", "Initial commit"])?;

        // Add delay to ensure different timestamps
        std::thread::sleep(std::time::Duration::from_secs(1));

        // Create branch
        git(temp_dir.path(), &["checkout", "-b", "feature"])?;

        fs::write(temp_dir.path().join("feature.txt"), "feature")?;
        git(temp_dir.path(), &["add", "feature.txt"])?;
        git(temp_dir.path(), &["commit", "-m", "Feature commit"])?;

        // Add delay
        std::thread::sleep(std::time::Duration::from_secs(1));

        // Merge back to main
        git(temp_dir.path(), &["checkout", "main"])?;
        git(
            temp_dir.path(),
            &["merge", "feature", "--no-ff", "-m", "Merge feature"],
        )?;

        // Verify Git sees 3 commits
        let output = Command::new("git")
            .args(&["log", "--oneline", "--all"])
            .current_dir(temp_dir.path())
            .output()?;
        let log = String::from_utf8_lossy(&output.stdout);
        println!("Git log:\n{}", log);
        let git_commit_count = log.lines().count();
        assert_eq!(git_commit_count, 3, "Git should have 3 commits");

        // Import commits
        let commits = import_commits(temp_dir.path())?;

        // Should have 3 commits: initial, feature, merge
        assert_eq!(commits.len(), 3, "Should have 3 commits");

        Ok(())
    }

    /// branch tests
    fn build_git_to_helix_map_for_branch(
        repo: &Path,
        branch: &str,
        helix_hash: [u8; 32],
    ) -> Result<HashMap<Vec<u8>, [u8; 32]>> {
        let ref_path = repo.join(".git/refs/heads").join(branch);
        let git_sha = std::fs::read_to_string(&ref_path)?.trim().to_string();
        let git_sha_bytes = hex::decode(&git_sha)?;
        let mut map = HashMap::new();
        map.insert(git_sha_bytes, helix_hash);
        Ok(map)
    }

    use crate::helix_index::commit::CommitStore;
    use crate::helix_index::state::get_branch_upstream;
    use crate::helix_index::tree::TreeStore;

    #[test]
    fn test_import_git_branches_sets_default_upstream_for_feature() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo = temp_dir.path();
        init_test_repo(repo)?;

        // Commit on main
        std::fs::write(repo.join("main.txt"), "main")?;
        git(repo, &["add", "main.txt"])?;
        git(repo, &["commit", "-m", "main commit"])?;

        // Create feature branch with its own commit
        git(repo, &["checkout", "-b", "feature"])?;
        std::fs::write(repo.join("feature.txt"), "feature")?;
        git(repo, &["add", "feature.txt"])?;
        git(repo, &["commit", "-m", "feature commit"])?;

        // Build fake Git→Helix mapping for both heads
        let mut git_to_helix = HashMap::new();
        let main_hash = [1u8; 32];
        let feature_hash = [2u8; 32];

        git_to_helix.extend(build_git_to_helix_map_for_branch(repo, "main", main_hash)?);
        git_to_helix.extend(build_git_to_helix_map_for_branch(
            repo,
            "feature",
            feature_hash,
        )?);

        let engine = SyncEngine::new(repo);
        engine.import_git_branches(&git_to_helix)?;

        // Default branch detection should prefer "main"
        // feature's upstream should be set to "main"
        assert_eq!(
            get_branch_upstream(repo, "feature"),
            Some("main".to_string()),
            "feature branch should track default branch main when no explicit upstream exists"
        );

        // Default branch itself should not get an upstream entry
        assert_eq!(
            get_branch_upstream(repo, "main"),
            None,
            "default branch should not be given an upstream by import"
        );

        Ok(())
    }

    #[test]
    fn test_parse_git_branch_upstream_reads_merge_ref() -> Result<()> {
        let git_config = r#"
[core]
    repositoryformatversion = 0

[branch "feature"]
    remote = origin
    merge = refs/heads/main

[branch "other"]
    remote = origin
    merge = refs/heads/dev
"#;

        let engine = SyncEngine::new(Path::new("."));

        let upstream_feature = engine.parse_git_branch_upstream(git_config, "feature");
        let upstream_other = engine.parse_git_branch_upstream(git_config, "other");
        let upstream_missing = engine.parse_git_branch_upstream(git_config, "nonexistent");

        assert_eq!(
            upstream_feature.as_deref(),
            Some("main"),
            "should strip refs/heads/ prefix for feature"
        );
        assert_eq!(
            upstream_other.as_deref(),
            Some("dev"),
            "should parse different branch names as well"
        );
        assert_eq!(
            upstream_missing, None,
            "branch without section should return None"
        );

        Ok(())
    }

    #[test]
    fn test_find_default_branch_prefers_main_then_master_then_first() {
        let engine = SyncEngine::new(Path::new("."));

        let branches = vec![
            "feature".to_string(),
            "main".to_string(),
            "bugfix".to_string(),
        ];
        assert_eq!(
            engine.find_default_branch(&branches),
            Some("main".to_string())
        );

        let branches = vec![
            "feature".to_string(),
            "master".to_string(),
            "bugfix".to_string(),
        ];
        assert_eq!(
            engine.find_default_branch(&branches),
            Some("master".to_string())
        );

        let branches = vec!["foo".to_string(), "bar".to_string()];
        assert_eq!(
            engine.find_default_branch(&branches),
            Some("foo".to_string()),
            "when neither main nor master exist, first branch should be used"
        );

        let branches: Vec<String> = Vec::new();
        assert_eq!(engine.find_default_branch(&branches), None);
    }

    #[test]
    fn test_import_git_branches_single_branch() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo = temp_dir.path();
        init_test_repo(repo)?;

        // Create a single commit on main
        std::fs::write(repo.join("file.txt"), "content")?;
        git(repo, &["add", "file.txt"])?;
        git(repo, &["commit", "-m", "initial"])?;

        // Map Git commit → fake Helix hash
        let fake_helix_hash = [1u8; 32];
        let git_to_helix = build_git_to_helix_map_for_branch(repo, "main", fake_helix_hash)?;

        let engine = SyncEngine::new(repo);
        engine.import_git_branches(&git_to_helix)?;

        // Verify .helix ref created
        let helix_ref_path = repo.join(".helix/refs/heads/main");
        assert!(
            helix_ref_path.exists(),
            "Helix branch ref should be created for main"
        );

        let contents = std::fs::read_to_string(&helix_ref_path)?;
        assert_eq!(
            contents.trim(),
            hash::hash_to_hex(&fake_helix_hash),
            "Helix branch ref should contain mapped Helix hash"
        );

        Ok(())
    }

    #[test]
    fn test_import_git_branches_nested_branches_preserve_paths() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo = temp_dir.path();
        init_test_repo(repo)?;

        // Base commit on main
        std::fs::write(repo.join("base.txt"), "base")?;
        git(repo, &["add", "base.txt"])?;
        git(repo, &["commit", "-m", "base"])?;

        // Create nested branch: feature/foo
        git(repo, &["checkout", "-b", "feature/foo"])?;
        std::fs::write(repo.join("feature.txt"), "feature")?;
        git(repo, &["add", "feature.txt"])?;
        git(repo, &["commit", "-m", "feature"])?;

        // Another nested branch: bugfix/bar
        git(repo, &["checkout", "main"])?;
        git(repo, &["checkout", "-b", "bugfix/bar"])?;
        std::fs::write(repo.join("bugfix.txt"), "bug")?;
        git(repo, &["add", "bugfix.txt"])?;
        git(repo, &["commit", "-m", "bugfix"])?;

        // Build mapping for ALL branch heads
        let mut git_to_helix = HashMap::new();
        let main_hash = [1u8; 32];
        let feature_hash = [2u8; 32];
        let bugfix_hash = [3u8; 32];

        git_to_helix.extend(build_git_to_helix_map_for_branch(repo, "main", main_hash)?);
        git_to_helix.extend(build_git_to_helix_map_for_branch(
            repo,
            "feature/foo",
            feature_hash,
        )?);
        git_to_helix.extend(build_git_to_helix_map_for_branch(
            repo,
            "bugfix/bar",
            bugfix_hash,
        )?);

        let engine = SyncEngine::new(repo);
        engine.import_git_branches(&git_to_helix)?;

        // Verify paths and contents
        let main_ref = repo.join(".helix/refs/heads/main");
        let feature_ref = repo.join(".helix/refs/heads/feature/foo");
        let bugfix_ref = repo.join(".helix/refs/heads/bugfix/bar");

        assert!(main_ref.exists(), "main ref should exist");
        assert!(
            feature_ref.exists(),
            "feature/foo ref should exist with nested path"
        );
        assert!(
            bugfix_ref.exists(),
            "bugfix/bar ref should exist with nested path"
        );

        assert_eq!(
            std::fs::read_to_string(&main_ref)?.trim(),
            hash::hash_to_hex(&main_hash)
        );
        assert_eq!(
            std::fs::read_to_string(&feature_ref)?.trim(),
            hash::hash_to_hex(&feature_hash)
        );
        assert_eq!(
            std::fs::read_to_string(&bugfix_ref)?.trim(),
            hash::hash_to_hex(&bugfix_hash)
        );

        Ok(())
    }

    #[test]
    fn test_import_git_branches_no_refs_dir_is_noop() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo = temp_dir.path();
        init_test_repo(repo)?;

        // Ensure .git/refs/heads does NOT exist
        let git_refs_dir = repo.join(".git/refs/heads");
        if git_refs_dir.exists() {
            std::fs::remove_dir_all(&git_refs_dir)?;
        }

        let git_to_helix: HashMap<Vec<u8>, [u8; 32]> = HashMap::new();
        let engine = SyncEngine::new(repo);
        // Should succeed and not panic
        engine.import_git_branches(&git_to_helix)?;

        // No .helix/refs/heads should be created either
        let helix_refs_dir = repo.join(".helix/refs/heads");
        assert!(
            !helix_refs_dir.exists(),
            "No helix refs should be created when no git refs"
        );

        Ok(())
    }

    #[test]
    fn test_import_git_branches_unknown_commit_is_skipped() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo = temp_dir.path();
        init_test_repo(repo)?;

        // Create one commit so .git/refs/heads/main exists
        std::fs::write(repo.join("file.txt"), "content")?;
        git(repo, &["add", "file.txt"])?;
        git(repo, &["commit", "-m", "initial"])?;

        // Empty mapping: branch commit SHA won't be found
        let git_to_helix: HashMap<Vec<u8>, [u8; 32]> = HashMap::new();

        let engine = SyncEngine::new(repo);
        let result = engine.import_git_branches(&git_to_helix);

        // New behavior: no error, branch is just skipped
        assert!(
            result.is_ok(),
            "Branch pointing to unknown commit should be skipped, not error"
        );

        // No Helix ref should have been created for main
        let helix_ref_path = repo.join(".helix/refs/heads/main");
        assert!(
            !helix_ref_path.exists(),
            "Branch with unknown commit should not produce a Helix ref"
        );

        // And no upstream tracking should be written for it either
        use crate::helix_index::state::get_branch_upstream;
        assert_eq!(
            get_branch_upstream(repo, "main"),
            None,
            "No upstream should be imported for skipped branches"
        );

        Ok(())
    }

    #[test]
    fn test_import_git_branches_invalid_sha_fails() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo = temp_dir.path();
        init_test_repo(repo)?;

        // Create a commit to ensure refs/heads/main exists
        std::fs::write(repo.join("file.txt"), "content")?;
        git(repo, &["add", "file.txt"])?;
        git(repo, &["commit", "-m", "initial"])?;

        // Overwrite ref with invalid SHA
        let main_ref_path = repo.join(".git/refs/heads/main");
        std::fs::write(&main_ref_path, "not-a-hex-sha\n")?;

        let git_to_helix: HashMap<Vec<u8>, [u8; 32]> = HashMap::new();
        let engine = SyncEngine::new(repo);
        let result = engine.import_git_branches(&git_to_helix);

        assert!(
            result.is_err(),
            "Invalid SHA in branch ref should cause an error via hex::decode"
        );

        Ok(())
    }

    /// tag tests
    fn build_git_to_helix_map_for_tag(
        repo: &Path,
        tag: &str,
        helix_hash: [u8; 32],
    ) -> Result<HashMap<Vec<u8>, [u8; 32]>> {
        let ref_path = repo.join(".git/refs/tags").join(tag);
        let git_sha = std::fs::read_to_string(&ref_path)?.trim().to_string();
        let git_sha_bytes = hex::decode(&git_sha)?;
        let mut map = HashMap::new();
        map.insert(git_sha_bytes, helix_hash);
        Ok(map)
    }

    #[test]
    fn test_import_git_tags_no_tags_dir_is_noop() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo = temp_dir.path();
        init_test_repo(repo)?;

        // Ensure .git/refs/tags does NOT exist
        let git_tags_dir = repo.join(".git/refs/tags");
        if git_tags_dir.exists() {
            std::fs::remove_dir_all(&git_tags_dir)?;
        }

        let git_to_helix: HashMap<Vec<u8>, [u8; 32]> = HashMap::new();
        let engine = SyncEngine::new(repo);

        // Should just return Ok(()) and not create any helix tags
        engine.import_git_tags(&git_to_helix)?;

        let helix_tags_dir = repo.join(".helix/refs/tags");
        assert!(
            !helix_tags_dir.exists(),
            "No helix tags should be created when no git tags dir exists"
        );

        Ok(())
    }

    #[test]
    fn test_import_git_tags_single_tag() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo = temp_dir.path();
        init_test_repo(repo)?;

        // Create a commit
        std::fs::write(repo.join("file.txt"), "content")?;
        git(repo, &["add", "file.txt"])?;
        git(repo, &["commit", "-m", "initial"])?;

        // Lightweight tag pointing directly at commit
        git(repo, &["tag", "v1.0.0"])?;

        // Map the tag's commit SHA to a fake Helix hash
        let fake_helix_hash = [9u8; 32];
        let git_to_helix = build_git_to_helix_map_for_tag(repo, "v1.0.0", fake_helix_hash)?;

        let engine = SyncEngine::new(repo);
        engine.import_git_tags(&git_to_helix)?;

        let helix_tag = repo.join(".helix/refs/tags/v1.0.0");
        assert!(helix_tag.exists(), "Helix tag ref should be created");

        let contents = std::fs::read_to_string(&helix_tag)?;
        assert_eq!(
            contents.trim(),
            hash::hash_to_hex(&fake_helix_hash),
            "Helix tag should contain mapped Helix hash"
        );

        Ok(())
    }

    #[test]
    fn test_import_git_tags_multiple_and_nested_tags() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo = temp_dir.path();
        init_test_repo(repo)?;

        // Base commit
        std::fs::write(repo.join("file.txt"), "content")?;
        git(repo, &["add", "file.txt"])?;
        git(repo, &["commit", "-m", "initial"])?;

        // Create some tags (Git allows slashes in tag names)
        git(repo, &["tag", "v1"])?;
        git(repo, &["tag", "releases/v2"])?;
        git(repo, &["tag", "hotfix/v3"])?;

        // All tags point to the same commit, so they all map to the same Helix hash.
        let fake_helix_hash = [7u8; 32];

        // Build a mapping for that commit SHA → fake_helix_hash
        // (we can read SHA from any of the tag refs, they all point to same commit)
        let ref_path = repo.join(".git/refs/tags/v1");
        let git_sha = std::fs::read_to_string(&ref_path)?.trim().to_string();
        let git_sha_bytes = hex::decode(&git_sha)?;
        let mut git_to_helix = HashMap::new();
        git_to_helix.insert(git_sha_bytes, fake_helix_hash);

        let engine = SyncEngine::new(repo);
        engine.import_git_tags(&git_to_helix)?;

        let v1_ref = repo.join(".helix/refs/tags/v1");
        let v2_ref = repo.join(".helix/refs/tags/releases/v2");
        let v3_ref = repo.join(".helix/refs/tags/hotfix/v3");

        assert!(v1_ref.exists(), "v1 tag should exist");
        assert!(
            v2_ref.exists(),
            "releases/v2 tag should be preserved as nested path"
        );
        assert!(
            v3_ref.exists(),
            "hotfix/v3 tag should be preserved as nested path"
        );

        // All tags point to the same commit => same Helix hash
        let expected = hash::hash_to_hex(&fake_helix_hash);

        assert_eq!(std::fs::read_to_string(&v1_ref)?.trim(), expected);
        assert_eq!(std::fs::read_to_string(&v2_ref)?.trim(), expected);
        assert_eq!(std::fs::read_to_string(&v3_ref)?.trim(), expected);

        Ok(())
    }

    #[test]
    fn test_import_git_tags_unknown_commit_is_skipped() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo = temp_dir.path();
        init_test_repo(repo)?;

        // Create a commit and a tag
        std::fs::write(repo.join("file.txt"), "content")?;
        git(repo, &["add", "file.txt"])?;
        git(repo, &["commit", "-m", "initial"])?;
        git(repo, &["tag", "v1"])?;

        // Empty mapping: tag target SHA not present -> should be skipped
        let git_to_helix: HashMap<Vec<u8>, [u8; 32]> = HashMap::new();

        let engine = SyncEngine::new(repo);
        engine.import_git_tags(&git_to_helix)?;

        let helix_tag = repo.join(".helix/refs/tags/v1");
        assert!(
            !helix_tag.exists(),
            "Tag pointing to unknown commit should be skipped and not written"
        );

        Ok(())
    }

    #[test]
    fn test_import_git_tags_invalid_sha_errors() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo = temp_dir.path();
        init_test_repo(repo)?;

        // Create a commit and tag so .git/refs/tags/v1 exists
        std::fs::write(repo.join("file.txt"), "content")?;
        git(repo, &["add", "file.txt"])?;
        git(repo, &["commit", "-m", "initial"])?;
        git(repo, &["tag", "v1"])?;

        // Overwrite the tag ref with invalid hex contents
        let git_tag_ref = repo.join(".git/refs/tags/v1");
        std::fs::write(&git_tag_ref, "not-a-hex-sha\n")?;

        let git_to_helix: HashMap<Vec<u8>, [u8; 32]> = HashMap::new();
        let engine = SyncEngine::new(repo);
        let result = engine.import_git_tags(&git_to_helix);

        assert!(
            result.is_err(),
            "Invalid SHA in tag ref should cause an error via hex::decode"
        );

        Ok(())
    }

    /// git head  tests
    #[test]
    fn test_import_git_head_symbolic() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo = temp_dir.path();
        init_test_repo(repo)?;

        // Create at least one commit so HEAD points to a valid branch,
        // but we don't need to know the branch name ("main" vs "master").
        std::fs::write(repo.join("file.txt"), "content")?;
        git(repo, &["add", "file.txt"])?;
        git(repo, &["commit", "-m", "initial"])?;

        let git_head_path = repo.join(".git/HEAD");
        let git_head_content = std::fs::read_to_string(&git_head_path)?;

        // Use empty map: symbolic HEAD does not require mapping.
        let git_to_helix: HashMap<Vec<u8>, [u8; 32]> = HashMap::new();
        let engine = SyncEngine::new(repo);
        engine.import_git_head(&git_to_helix)?;

        let helix_head_path = repo.join(".helix/HEAD");
        assert!(helix_head_path.exists(), ".helix/HEAD should be created");

        let helix_head_content = std::fs::read_to_string(&helix_head_path)?;
        assert_eq!(
            git_head_content, helix_head_content,
            "Symbolic HEAD should be copied exactly"
        );

        Ok(())
    }

    #[test]
    fn test_import_git_head_detached() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo = temp_dir.path();
        init_test_repo(repo)?;

        // Create a commit
        std::fs::write(repo.join("file.txt"), "content")?;
        git(repo, &["add", "file.txt"])?;
        git(repo, &["commit", "-m", "initial"])?;

        // Get commit SHA
        let output = std::process::Command::new("git")
            .args(&["rev-parse", "HEAD"])
            .current_dir(repo)
            .output()?;
        let git_sha = String::from_utf8_lossy(&output.stdout).trim().to_string();

        // Checkout detached at that commit
        git(repo, &["checkout", &git_sha])?;

        // Build mapping: Git commit SHA bytes -> fake Helix hash
        let git_sha_bytes = hex::decode(&git_sha)?;
        let fake_helix_hash = [7u8; 32];

        let mut git_to_helix = HashMap::new();
        git_to_helix.insert(git_sha_bytes, fake_helix_hash);

        let engine = SyncEngine::new(repo);
        engine.import_git_head(&git_to_helix)?;

        let helix_head_path = repo.join(".helix/HEAD");
        assert!(helix_head_path.exists(), ".helix/HEAD should be created");

        let helix_head_content = std::fs::read_to_string(&helix_head_path)?;
        assert_eq!(
            helix_head_content.trim(),
            hash::hash_to_hex(&fake_helix_hash),
            "Detached HEAD should be converted to Helix hash in .helix/HEAD"
        );

        Ok(())
    }

    #[test]
    fn test_import_git_head_detached_unknown_commit_errors() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo = temp_dir.path();
        init_test_repo(repo)?;

        // Create a commit
        std::fs::write(repo.join("file.txt"), "content")?;
        git(repo, &["add", "file.txt"])?;
        git(repo, &["commit", "-m", "initial"])?;

        // Get commit SHA
        let output = std::process::Command::new("git")
            .args(&["rev-parse", "HEAD"])
            .current_dir(repo)
            .output()?;
        let git_sha = String::from_utf8_lossy(&output.stdout).trim().to_string();

        // Checkout detached at that commit
        git(repo, &["checkout", &git_sha])?;

        // Empty mapping: HEAD commit is not in git_hash_to_helix_hash
        let git_to_helix: HashMap<Vec<u8>, [u8; 32]> = HashMap::new();

        let engine = SyncEngine::new(repo);
        let result = engine.import_git_head(&git_to_helix);

        assert!(
            result.is_err(),
            "Detached HEAD with missing mapping should error"
        );
        let err = format!("{:?}", result.unwrap_err());
        assert!(
            err.contains("HEAD points to unknown commit"),
            "Error should contain context message"
        );

        // .helix/HEAD should not contain a bogus value
        let helix_head_path = repo.join(".helix/HEAD");
        if helix_head_path.exists() {
            let contents = std::fs::read_to_string(&helix_head_path)?;
            assert!(
                contents.trim().is_empty(),
                ".helix/HEAD should not contain a valid hash when mapping is missing"
            );
        }

        Ok(())
    }

    /// git remote tests
    #[test]
    fn test_import_git_remotes_no_remotes_creates_no_toml() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo = temp_dir.path();
        init_test_repo(repo)?;

        let engine = SyncEngine::new(repo);
        engine.import_git_remotes()?;

        let helix_toml_path = repo.join("helix.toml");
        assert!(
            !helix_toml_path.exists(),
            "helix.toml should not be created when there are no remotes"
        );

        Ok(())
    }

    #[test]
    fn test_import_git_remotes_single_origin() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo = temp_dir.path();
        init_test_repo(repo)?;

        // Add a single remote
        git(
            repo,
            &["remote", "add", "origin", "https://example.com/my-repo.git"],
        )?;

        let engine = SyncEngine::new(repo);
        engine.import_git_remotes()?;

        let helix_toml_path = repo.join("helix.toml");
        assert!(
            helix_toml_path.exists(),
            "helix.toml should be created when remotes exist"
        );

        let contents = fs::read_to_string(&helix_toml_path)?;
        // Has header
        assert!(
            contents.contains("# Helix Configuration"),
            "helix.toml should contain configuration header"
        );
        // Has [remotes] root table
        assert!(
            contents.contains("[remotes]"),
            "helix.toml should contain [remotes] section"
        );
        // Has [remotes] table
        assert!(
            contents.contains("[remotes]"),
            "helix.toml should contain [remotes] section"
        );
        // URL is correct
        assert!(
            contents.contains("pull=\"https://example.com/my-repo.git\""),
            "helix.toml should contain correct origin URL"
        );
        // Fetch refspecs should be present as a non-empty array
        assert!(
            contents.contains("pull=["),
            "helix.toml should define fetch refspecs"
        );
        // Pushurl should NOT be present because fetch == push by default
        assert!(
            !contents.contains("push="),
            "pushurl should not be written when push URL equals fetch URL"
        );

        Ok(())
    }

    #[test]
    fn test_import_git_remotes_origin_with_distinct_pushurl() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo = temp_dir.path();
        init_test_repo(repo)?;

        // Add origin
        git(
            repo,
            &["remote", "add", "origin", "https://example.com/my-repo.git"],
        )?;

        // Set a distinct push URL
        git(
            repo,
            &[
                "remote",
                "set-url",
                "--push",
                "origin",
                "ssh://git@example.com/my-repo.git",
            ],
        )?;

        let engine = SyncEngine::new(repo);
        engine.import_git_remotes()?;

        let helix_toml_path = repo.join("helix.toml");
        let contents = fs::read_to_string(&helix_toml_path)?;

        assert!(
            contents.contains("[remotes]"),
            "remotes section should exist"
        );
        assert!(
            contents.contains("pull=\"https://example.com/my-repo.git\""),
            "fetch URL should be recorded"
        );
        assert!(
            contents.contains("push=\"ssh://git@example.com/my-repo.git\""),
            "distinct push URL should be recorded as pushurl"
        );

        Ok(())
    }

    #[test]
    fn test_import_git_remotes_multiple_remotes() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo = temp_dir.path();
        init_test_repo(repo)?;

        // Add two remotes
        git(
            repo,
            &["remote", "add", "origin", "https://example.com/origin.git"],
        )?;
        git(
            repo,
            &[
                "remote",
                "add",
                "upstream",
                "https://example.com/upstream.git",
            ],
        )?;

        let engine = SyncEngine::new(repo);
        engine.import_git_remotes()?;

        let helix_toml_path = repo.join("helix.toml");
        let contents = fs::read_to_string(&helix_toml_path)?;

        assert!(
            contents.contains("[remotes]"),
            "root [remotes] section should exist"
        );
        assert!(
            contents.contains("[remotes]"),
            "origin remote section should exist"
        );
        assert!(
            contents.contains("[remotes.upstream]"),
            "upstream remote section should exist"
        );
        assert!(
            contents.contains("pull=\"https://example.com/origin.git\""),
            "origin URL should be recorded"
        );
        assert!(
            contents.contains("pull=\"https://example.com/upstream.git\""),
            "upstream URL should be recorded"
        );

        Ok(())
    }

    #[test]
    fn test_import_git_remotes_preserves_existing_helix_toml_and_remotes_section() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo = temp_dir.path();
        init_test_repo(repo)?;

        // Pre-populate helix.toml with some content and an existing [remotes] section
        let helix_toml_path = repo.join("helix.toml");
        fs::write(
            &helix_toml_path,
            r#"# Helix Configuration

[some_other_section]
key = "value"

[remotes]
"#,
        )?;

        // Add a remote in git
        git(
            repo,
            &["remote", "add", "origin", "https://example.com/origin.git"],
        )?;

        let engine = SyncEngine::new(repo);
        engine.import_git_remotes()?;

        let contents = fs::read_to_string(&helix_toml_path)?;

        // Ensure existing section is preserved
        assert!(
            contents.contains("[some_other_section]"),
            "existing sections should be preserved"
        );

        // [remotes] should appear only once
        let remotes_count = contents.match_indices("[remotes]").count();
        assert_eq!(
            remotes_count, 1,
            "[remotes] section should not be duplicated"
        );

        // New remote should be appended under remotes
        assert!(
            contents.contains("[remotes]"),
            "origin remote section should be added under [remotes]"
        );
        assert!(
            contents.contains("pull=\"https://example.com/origin.git\""),
            "origin URL should be recorded"
        );

        Ok(())
    }
}
