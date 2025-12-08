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

pub struct SyncEngine {
    repo_path: PathBuf,
}

impl SyncEngine {
    pub fn new(repo_path: &Path) -> Self {
        Self {
            repo_path: repo_path.to_path_buf(),
        }
    }

    /// One-time import from Git to create initial Helix index
    /// ## Complete Workflow
    /// 1. **Import index entries** → Creates `.helix/helix.idx
    /// 2. **Import commits** → Gets all Git commits as `Helix_Commit` objects
    /// 3. **Store commits** → Writes them to `.helix/objects/commits/{hash}`
    /// 4. **Update HEAD** → Points to the latest commit
    /// 5. **Write index** → Finalizes the index file
    pub fn import_from_git(&self) -> Result<()> {
        wait_for_git_lock(&self.repo_path, Duration::from_secs(5))?;

        self.import_git_index()?;
        self.import_git_commits()?;

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

    fn import_git_commits(&self) -> Result<()> {
        let repo = gix::open(&self.repo_path)?;

        // Get HEAD commit (may not exist in empty repo)
        let head_commit = match repo.head()?.peel_to_commit() {
            Ok(commit) => commit,
            Err(_) => {
                // No commits yet
                return Ok(());
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

    #[test]
    fn test_import_git_commits_performance() -> Result<()> {
        let temp_dir = TempDir::new()?;
        init_test_repo(temp_dir.path())?;

        // Create 10 commits
        for i in 0..10 {
            fs::write(
                temp_dir.path().join(format!("file{}.txt", i)),
                format!("content {}", i),
            )?;
            Command::new("git")
                .args(&["add", "."])
                .current_dir(temp_dir.path())
                .output()?;
            Command::new("git")
                .args(&["commit", "-m", &format!("Commit {}", i)])
                .current_dir(temp_dir.path())
                .output()?;
        }

        // Time the import
        let syncer = SyncEngine::new(temp_dir.path());
        let start = Instant::now();
        let commits = syncer.import_git_commits()?;
        let elapsed = start.elapsed();

        assert_eq!(commits.len(), 10);
        println!("Imported 10 commits in {:?}", elapsed);

        // Should be reasonably fast (< 2 seconds for 10 commits)
        assert!(elapsed.as_secs() < 2, "Import took too long: {:?}", elapsed);

        Ok(())
    }
}
