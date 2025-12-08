/*
This file defines the sync engine that handles one-time import from .git/index during 'helix init'.

After initialization, Helix operates independently:
- .helix/helix.idx is the ONLY canonical source of truth
- No ongoing sync with .git/index
- All Helix operations update .helix/helix.idx only

State model for EntryFlags:

We model three worlds:
- HEAD         (last committed state from Git)
- helix.idx    (Helix's canonical index, replaces .git/index)
- working tree (files on disk)

Bits:

- TRACKED   -> this path exists in helix.idx (was in .git/index during import)
- STAGED    -> helix.idx differs from HEAD for this path (index != HEAD)
- MODIFIED  -> working tree differs from helix.idx (working != helix.idx)
- DELETED   -> tracked in helix.idx but missing from working tree
- UNTRACKED -> not in helix.idx, but discovered via FSMonitor

This file (sync.rs) only handles the one-time import during 'helix init'.
It compares **index vs HEAD** to set TRACKED and STAGED flags.
MODIFIED / DELETED / UNTRACKED are set by FSMonitor / working-tree operations.
*/

use super::format::{Entry, EntryFlags, Header};
use super::reader::Reader;
use super::writer::Writer;

use crate::helix_index::commit::Commit as Helix_Commit;
use crate::helix_index::commit::CommitStorage;
use crate::helix_index::hash::{self, ZERO_HASH};
use crate::helix_index::tree::TreeBuilder;
use crate::ignore::IgnoreRules;
use crate::index::GitIndex;

use anyhow::{Context, Result};
use blake3::Hash;
use gix::revision::walk::Sorting;
use hash::compute_blob_oid;
use indicatif::{ProgressBar, ProgressStyle};
use rayon::prelude::*;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use walkdir::WalkDir;

pub struct SyncEngine {
    repo_path: PathBuf,
}

impl SyncEngine {
    pub fn new(repo_path: &Path) -> Self {
        Self {
            repo_path: repo_path.to_path_buf(),
        }
    }

    pub fn import_from_git(&self) -> Result<()> {
        wait_for_git_lock(&self.repo_path, Duration::from_secs(5))?;

        self.import_git_index()?;
        let git_hash_to_helix_hash = self.import_git_commits()?;
        self.import_git_branches(&git_hash_to_helix_hash)?;
        self.import_git_tags(&git_hash_to_helix_hash)?;
        self.import_git_head(&git_hash_to_helix_hash)?;
        self.import_git_remotes()?;

        Ok(())
    }

    fn import_git_branches(
        &self,
        git_hash_to_helix_hash: &HashMap<Vec<u8>, [u8; 32]>,
    ) -> Result<()> {
        let git_refs_dir = self.repo_path.join(".git/refs/heads");
        if !git_refs_dir.exists() {
            return Ok(());
        }

        for entry in WalkDir::new(&git_refs_dir) {
            let entry = entry?;
            if !entry.file_type().is_file() {
                continue;
            }

            let git_sha = fs::read_to_string(entry.path())?.trim().to_string();
            let git_sha_bytes = hex::decode(&git_sha)?;

            // Convert Git SHA to Helix hash
            let helix_hash = git_hash_to_helix_hash
                .get(&git_sha_bytes)
                .context("Branch points to unknown commit")?;

            // Preserve branch path structure
            let rel_path = entry.path().strip_prefix(&git_refs_dir)?;
            let helix_ref_path = self.repo_path.join(".helix/refs/heads").join(rel_path);

            fs::create_dir_all(helix_ref_path.parent().unwrap())?;
            fs::write(&helix_ref_path, hash::hash_to_hex(helix_hash))?;
        }

        Ok(())
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

    fn import_git_remotes(&self) -> Result<()> {
        let repo = gix::open(&self.repo_path)?;

        // Get remote names
        let remote_names: Vec<_> = repo
            .remote_names()
            .into_iter()
            .filter_map(|name_result| Some(name_result))
            .collect();

        if remote_names.is_empty() {
            return Ok(());
        }

        // Read or create helix.toml
        let helix_toml_path = self.repo_path.join("helix.toml");

        let mut toml_content = if helix_toml_path.exists() {
            fs::read_to_string(&helix_toml_path)?
        } else {
            String::from("# Helix Configuration\n\n")
        };

        // Check if remotes section already exists
        if !toml_content.contains("[remotes]") {
            toml_content.push_str("\n# Imported remotes from Git\n");
            toml_content.push_str("[remotes]\n");
        }

        let remote_clone = remote_names.clone();

        for remote_name in remote_names {
            let remote_name_str = remote_name.as_ref();

            // Find the remote in the Git config
            let remote = match repo.find_remote(remote_name_str) {
                Ok(r) => r,
                Err(e) => {
                    eprintln!(
                        "Warning: Could not find remote '{}': {}",
                        remote_name_str, e
                    );
                    continue;
                }
            };

            // Add remote section to TOML
            toml_content.push_str(&format!("\n[remotes.{}]\n", remote_name_str));

            // Get fetch URL
            if let Some(url) = remote.url(gix::remote::Direction::Fetch) {
                toml_content.push_str(&format!("pull=\"{}\"\n", url));
            }

            // Get fetch refspecs
            let fetch_specs: Vec<_> = remote
                .refspecs(gix::remote::Direction::Fetch)
                .into_iter()
                .map(|spec| format!("\"{:?}\"", spec.to_ref()))
                .collect();

            if !fetch_specs.is_empty() {
                toml_content.push_str(&format!("pull=[{}]\n", fetch_specs.join(", ")));
            }

            // Get push URL (only if different from fetch)
            if let Some(push_url) = remote.url(gix::remote::Direction::Push) {
                let fetch_url = remote.url(gix::remote::Direction::Fetch);
                if fetch_url != Some(push_url) {
                    toml_content.push_str(&format!("push=\"{}\"\n", push_url));
                }
            }
        }

        fs::write(&helix_toml_path, toml_content)?;

        println!("✓ Imported {} remote(s) to helix.toml", remote_clone.len());

        Ok(())
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

    fn import_git_index(&self) -> Result<()> {
        let git_index_path = self.repo_path.join(".git/index");

        // Handle brand-new repo with no .git/index yet, return new empty Helix index
        if !&git_index_path.exists() {
            let header = Header::new(1, 0);
            let writer = Writer::new_canonical(&self.repo_path);
            writer.write(&header, &[])?;

            return Ok(());
        }

        let reader = Reader::new(&self.repo_path);
        let current_generation = if reader.exists() {
            reader
                .read()
                .ok()
                .map(|data| data.header.generation)
                .unwrap_or(0)
        } else {
            0
        };

        let git_index = GitIndex::open(&self.repo_path)?;
        let index_entries: Vec<_> = git_index.entries().collect();

        if index_entries.len() == 0 {
            return Ok(());
        }

        // filter out any files that we want to ignore
        let ignore_rules = IgnoreRules::load(&self.repo_path);

        let head_tree = self.load_full_head_tree()?;

        let pb = ProgressBar::new(index_entries.len() as u64);
        pb.set_style(
            ProgressStyle::with_template(
                "{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] \
             {pos}/{len} entries ({eta})",
            )?
            .progress_chars(">-"),
        );

        // Build entries in parallel, updating the progress bar as we go
        let entries: Vec<Entry> = index_entries
            .into_par_iter()
            .map_init(
                || (pb.clone(), ignore_rules.clone()),
                |(pb, ignore_rules), e| {
                    pb.inc(1);

                    // Check if ignored
                    let path = Path::new(&e.path);
                    if ignore_rules.should_ignore(path) {
                        return None;
                    }

                    self.build_helix_entry_from_git_entry(&e, &head_tree).ok()
                },
            )
            .flatten()
            .collect();

        pb.finish_with_message("Helix index built");

        let header = Header::new(&current_generation + 1, entries.len() as u32);
        let writer = Writer::new_canonical(&self.repo_path);
        writer.write(&header, &entries)?;

        Ok(())
    }

    fn build_helix_entry_from_git_entry(
        &self,
        git_index_entry: &crate::index::IndexEntry,
        head_tree: &HashMap<PathBuf, Vec<u8>>,
    ) -> Result<Entry> {
        let path = PathBuf::from(&git_index_entry.path);
        let full_path = self.repo_path.join(&path);

        let mut flags = EntryFlags::TRACKED;

        // Git's index snapshot blob-hash
        let index_git_oid = git_index_entry.oid.as_bytes();

        // Helix stores its own hash (of the Git oid bytes)
        let helix_oid = hash::hash_bytes(index_git_oid);

        // STAGED check: index vs HEAD
        // check if the hashed head oid from git head  is the same as the hashed helix oid from .git/index
        // if the same then the file is staged, if they're are different then the file is not staged
        // if it's a new file it will default be being staged
        let is_staged = head_tree
            .get(&path)
            .map(|head_git_oid| head_git_oid.as_slice() != index_git_oid)
            .unwrap_or(true);

        if is_staged {
            flags |= EntryFlags::STAGED;
        }

        let was_in_head = head_tree.contains_key(&path);

        // Always check working tree vs. index, this will catch repos with no commits yet
        if full_path.exists() && full_path.is_file() {
            let working_content = fs::read(&full_path)?;
            let working_git_oid = compute_blob_oid(&working_content);

            if &working_git_oid != index_git_oid {
                flags |= EntryFlags::MODIFIED;
            }
        } else if was_in_head {
            // Only mark DELETED if file was in HEAD
            // (Don't mark new staged files as deleted if they don't exist)
            flags |= EntryFlags::DELETED;
        }

        let (mtime_sec, file_size) = if full_path.exists() {
            let metadata = fs::metadata(&full_path)?;
            let mtime = metadata
                .modified()?
                .duration_since(std::time::UNIX_EPOCH)?
                .as_secs();
            (mtime, metadata.len())
        } else {
            (git_index_entry.mtime as u64, git_index_entry.size as u64)
        };

        Ok(Entry {
            path,
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
        println!("HEAD tree has {} files", map.len());
        Ok(map)
    }

    fn import_git_commits(&self) -> Result<HashMap<Vec<u8>, [u8; 32]>> {
        let repo = gix::open(&self.repo_path)?;

        // Get HEAD commit (may not exist in empty repo)
        let head_commit = match repo.head()?.peel_to_commit() {
            Ok(commit) => commit,
            Err(_) => {
                // No commits yet
                return Ok(HashMap::new());
            }
        };

        let mut seen = HashSet::new();
        let mut git_hash_to_helix_hash: HashMap<Vec<u8>, [u8; 32]> = HashMap::new();
        let mut collected_git_commits: Vec<(Vec<u8>, gix::Commit)> = Vec::new();

        let commit_iter = head_commit.ancestors().sorting(Sorting::ByCommitTime(
            gix::traverse::commit::simple::CommitTimeOrder::NewestFirst,
        ));

        for commit_result in commit_iter.all()? {
            let commit_info = commit_result?;
            let git_id_bytes = commit_info.id().as_bytes().to_vec();

            if !seen.insert(git_id_bytes.clone()) {
                continue;
            }

            let git_commit = commit_info.object()?;
            collected_git_commits.push((git_id_bytes, git_commit));
        }

        // Sort oldest → newest by commit time
        collected_git_commits.sort_by_key(|(_, c)| c.time().unwrap().seconds);

        println!(
            "Git reports {} commits in history",
            collected_git_commits.len()
        );

        let pb = ProgressBar::new_spinner();
        pb.set_message("Importing commits...");

        // Build Helix commits in oldest to newest order
        let mut helix_commits: Vec<Helix_Commit> = Vec::with_capacity(collected_git_commits.len());

        for (i, (git_id_bytes, git_commit)) in collected_git_commits.into_iter().enumerate() {
            pb.set_message(format!("Importing commit {}", i + 1));

            let helix_commit = self.build_helix_commit_from_git_commit(
                &git_commit,
                &repo,
                &git_hash_to_helix_hash,
            )?;

            // Now we know this commit's helix hash, so map git → helix for children
            git_hash_to_helix_hash.insert(git_id_bytes, helix_commit.commit_hash);

            helix_commits.push(helix_commit);
        }

        self.store_imported_commits(&helix_commits)?;

        if let Some(latest_commit) = helix_commits.last() {
            self.update_head_to_commit(latest_commit.commit_hash)?;
        }

        pb.finish_with_message(format!("Imported {} commits", helix_commits.len()));

        Ok(git_hash_to_helix_hash)
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

        let tree = self.build_helix_tree_from_recorder(recorder)?;

        let mut commit = Helix_Commit {
            commit_hash: ZERO_HASH,
            tree_hash: tree.into(),
            parents: parent_commits,
            author: author_email + &author_name,
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
    ) -> Result<Hash> {
        // Convert gix records to Helix Entry format
        let entries: Vec<Entry> = recorder
            .records
            .par_iter()
            .filter_map(|record| {
                // Only process blobs (files), skip trees (directories)
                if !record.mode.is_blob() && !record.mode.is_link() {
                    return None;
                }

                let path = PathBuf::from(record.filepath.to_string());

                // Convert Git OID to Helix hash
                let oid = hash::hash_bytes(record.oid.as_bytes());

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
                    size: 0, // Size not available from recorder
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

    fn store_imported_commits(&self, commits: &[Helix_Commit]) -> Result<()> {
        let commit_storage = CommitStorage::for_repo(&self.repo_path);

        println!("Storing {} commits to Helix...", commits.len());

        // Store all commits (with progress bar)
        let pb = ProgressBar::new(commits.len() as u64);
        pb.set_style(
            ProgressStyle::with_template(
                "{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] \
             {pos}/{len} commits ({eta})",
            )?
            .progress_chars(">-"),
        );

        for commit in commits {
            commit_storage.write(commit)?;
            pb.inc(1);
        }

        let commits_path = self
            .repo_path
            .join(".helix")
            .join("objects")
            .join("commits");

        println!("Listing commit files in {:?}", commits_path);

        if commits_path.exists() {
            for entry in fs::read_dir(&commits_path)? {
                let entry = entry?;
                println!(" - {:?}", entry.file_name());
            }
        } else {
            println!("Commit path does not exist yet: {:?}", commits_path);
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
            .args(&["init"])
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
    fn test_import_git_commit() -> Result<()> {
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
        syncer.import_git_commits()?;
        let commit_reader = CommitStorage::new(temp_dir.path());

        assert_eq!(
            commit_reader.list_all()?.len(),
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

        // Import commits
        let syncer = SyncEngine::new(temp_dir.path());
        syncer.import_git_commits()?;

        let commit_reader = CommitStorage::for_repo(temp_dir.path());

        let commits = &commit_reader.list_all()?;
        let first_commit = commit_reader.read(&commits[0])?;
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
        syncer.import_git_commits()?;
        let commit_reader = CommitStorage::for_repo(repo_dir);
        let commits = &commit_reader.list_all()?;

        let mut commits: Vec<_> = commits
            .iter()
            .map(|hash| commit_reader.read(hash).unwrap())
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

        use crate::helix_index::tree::TreeStorage;
        let tree_storage = TreeStorage::for_repo(temp_dir.path());
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
    fn test_import_git_branches_unknown_commit_errors() -> Result<()> {
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

        assert!(
            result.is_err(),
            "Branch pointing to unknown commit should return an error"
        );
        let err_string = format!("{:?}", result.unwrap_err());
        assert!(
            err_string.contains("Branch points to unknown commit"),
            "Error should contain context message"
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
        // Has [remotes.origin] table
        assert!(
            contents.contains("[remotes.origin]"),
            "helix.toml should contain [remotes.origin] section"
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
            contents.contains("[remotes.origin]"),
            "remotes.origin section should exist"
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
            contents.contains("[remotes.origin]"),
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
            contents.contains("[remotes.origin]"),
            "origin remote section should be added under [remotes]"
        );
        assert!(
            contents.contains("pull=\"https://example.com/origin.git\""),
            "origin URL should be recorded"
        );

        Ok(())
    }
}
