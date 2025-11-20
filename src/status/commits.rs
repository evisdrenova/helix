use anyhow::{Context, Result};
use chrono::{DateTime, Local};
use git2::{Commit as GitCommit, Repository};
use std::{collections::HashMap, path::Path};

#[derive(Debug, Clone)]
pub struct Commit {
    pub hash: String,
    pub short_hash: String,
    pub author_name: String,
    pub author_email: String,
    pub timestamp: DateTime<Local>,
    pub message: String,
    pub summary: String,
    pub file_changes: Vec<FileChanges>,
    pub is_merge: bool,
    pub insertions: usize,
    pub deletions: usize,
}

#[derive(Debug, Clone)]
pub struct FileChanges {
    pub path: String,
    pub insertions: usize,
    pub deletions: usize,
}

impl Commit {
    /// Create a Commit from a git2::Commit
    pub fn create_commit(commit: &GitCommit, repo: &Repository) -> Result<Self> {
        let hash = commit.id().to_string();
        let short_hash = commit.id().to_string()[..7].to_string();

        let author = commit.author();
        let author_name = author.name().unwrap_or("Unknown").to_string();
        let author_email = author.email().unwrap_or("").to_string();

        let timestamp = DateTime::from_timestamp(commit.time().seconds(), 0)
            .context("Invalid timestamp")?
            .with_timezone(&Local);

        let message = commit.message().unwrap_or("").to_string();
        let summary = commit.summary().unwrap_or("").to_string();

        let is_merge = commit.parent_count() > 1;

        // Calculate diff stats
        let file_stats = calculate_diff_stats(commit, repo)?;

        // convert file stats hash map to vector
        let file_changes: Vec<FileChanges> = file_stats
            .into_iter()
            .map(|(p, (insertions, deletions))| FileChanges {
                path: p,
                insertions: insertions,
                deletions: deletions,
            })
            .collect();

        // cache these in the struct since UI re-renders constantly, no need to recalculate them
        let total_insertions: usize = file_changes.iter().map(|f| f.insertions).sum();
        let total_deletions: usize = file_changes.iter().map(|f| f.deletions).sum();

        Ok(Commit {
            hash,
            short_hash,
            author_name,
            author_email,
            timestamp,
            message,
            summary,
            file_changes,
            is_merge,
            insertions: total_insertions,
            deletions: total_deletions,
        })
    }

    /// Format timestamp relative to now (e.g., "2 hours ago", "yesterday")
    pub fn relative_time(&self) -> String {
        let now = Local::now();
        let duration = now.signed_duration_since(self.timestamp);

        if duration.num_seconds() < 60 {
            "just now".to_string()
        } else if duration.num_minutes() < 60 {
            let mins = duration.num_minutes();
            format!("{} minute{} ago", mins, if mins == 1 { "" } else { "s" })
        } else if duration.num_hours() < 24 {
            let hours = duration.num_hours();
            format!("{} hour{} ago", hours, if hours == 1 { "" } else { "s" })
        } else if duration.num_days() < 7 {
            let days = duration.num_days();
            if days == 1 {
                "yesterday".to_string()
            } else {
                format!("{} days ago", days)
            }
        } else if duration.num_weeks() < 4 {
            let weeks = duration.num_weeks();
            format!("{} week{} ago", weeks, if weeks == 1 { "" } else { "s" })
        } else {
            self.timestamp.format("%b %d, %Y").to_string()
        }
    }

    /// Format timestamp with time (e.g., "Nov 12, 2:34 PM")
    pub fn formatted_time(&self) -> String {
        let now = Local::now();
        let duration = now.signed_duration_since(self.timestamp);

        if duration.num_hours() < 24 {
            self.timestamp.format("Today, %l:%M %p").to_string()
        } else if duration.num_days() == 1 {
            self.timestamp.format("Yesterday, %l:%M %p").to_string()
        } else if duration.num_days() < 7 {
            self.timestamp.format("%A, %l:%M %p").to_string()
        } else {
            self.timestamp.format("%b %d, %l:%M %p").to_string()
        }
    }
}

/// Calculate diff statistics for a commit
fn calculate_diff_stats(
    commit: &GitCommit,
    repo: &Repository,
) -> Result<HashMap<String, (usize, usize)>> {
    let tree = commit.tree().context("Failed to get commit tree")?;

    let parent_tree = if commit.parent_count() > 0 {
        Some(commit.parent(0)?.tree()?)
    } else {
        None
    };

    let diff = repo.diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), None)?;

    let mut file_stats: HashMap<String, (usize, usize)> = HashMap::new();
    // use a map here since the deltas aren't ordered as they come in
    // if we have too many deltas, this might be slow generally, will need to watch
    diff.print(git2::DiffFormat::Patch, |delta, _hunk, line| {
        if let Some(path) = delta.new_file().path().and_then(|p| p.to_str()) {
            let entry = file_stats.entry(path.to_string()).or_insert((0, 0));

            match line.origin() {
                '+' => entry.0 += 1,
                '-' => entry.1 += 1,
                _ => {}
            }
        }
        true
    })?;

    Ok(file_stats)
}

pub struct CommitLoader {
    repo: Repository,
}

impl CommitLoader {
    pub fn open_repo_at_path(path: &Path) -> Result<Self> {
        let repo = Repository::discover(path).context("Failed to open git repository")?;
        Ok(Self { repo })
    }

    pub fn checkout_commit(&self, commit_hash: &str, create_branch: Option<&str>) -> Result<()> {
        // Find the commit
        let oid = git2::Oid::from_str(commit_hash).context("Invalid commit hash")?;
        let commit = self.repo.find_commit(oid).context("Commit not found")?;

        // If a branch name is provided, create it
        if let Some(branch_name) = create_branch {
            // Create a new branch at this commit
            self.repo
                .branch(branch_name, &commit, false)
                .context("Failed to create branch")?;

            // Checkout the new branch
            let reference = format!("refs/heads/{}", branch_name);
            self.repo
                .set_head(&reference)
                .context("Failed to set HEAD to new branch")?;
        } else {
            // Detached HEAD checkout
            self.repo
                .set_head_detached(oid)
                .context("Failed to detach HEAD")?;
        }

        // Checkout the tree
        let obj = self
            .repo
            .find_object(oid, Some(git2::ObjectType::Commit))
            .context("Failed to find commit object")?;

        self.repo
            .checkout_tree(
                &obj,
                Some(
                    git2::build::CheckoutBuilder::new().force(), // Use force to overwrite working directory
                ),
            )
            .context("Failed to checkout tree")?;

        Ok(())
    }

    /// Load the most recent N commits
    pub fn load_commits(&self, limit: usize) -> Result<Vec<Commit>> {
        let mut revwalk = self.repo.revwalk()?;
        revwalk.push_head()?;
        revwalk.set_sorting(git2::Sort::TIME)?;

        let mut commits = Vec::new();

        for (i, oid_result) in revwalk.enumerate() {
            if i >= limit {
                break;
            }

            let oid = oid_result?;
            let git_commit = self.repo.find_commit(oid)?;

            match Commit::create_commit(&git_commit, &self.repo) {
                Ok(commit) => commits.push(commit),
                Err(e) => {
                    eprintln!("Warning: Failed to process commit {}: {}", oid, e);
                    continue;
                }
            }
        }

        Ok(commits)
    }

    pub fn get_current_branch_name(&self) -> Result<String> {
        let head = self.repo.head()?;
        if let Some(name) = head.shorthand() {
            Ok(name.to_string())
        } else {
            Ok("HEAD".to_string())
        }
    }

    pub fn get_repo_name(&self) -> String {
        self.repo
            .workdir()
            .and_then(|path| path.file_name())
            .and_then(|name| name.to_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "repository".to_string())
    }

    /// Get remote tracking branch info (name, ahead, behind)
    pub fn remote_tracking_info(&self) -> Option<(String, usize, usize)> {
        let head = self.repo.head().ok()?;
        let local_branch = head.name()?;

        // Get the upstream branch
        let upstream_name = self.repo.branch_upstream_name(local_branch).ok()?;
        let upstream_name_str = upstream_name.as_str()?;

        // Get the local and upstream commits
        let local_oid = head.target()?;
        let upstream_ref = self.repo.find_reference(upstream_name_str).ok()?;
        let upstream_oid = upstream_ref.target()?;

        // Calculate ahead/behind
        let (ahead, behind) = self.repo.graph_ahead_behind(local_oid, upstream_oid).ok()?;

        // Extract just the branch name (e.g., "origin/main" from "refs/remotes/origin/main")
        let remote_branch = upstream_name_str
            .strip_prefix("refs/remotes/")
            .unwrap_or(upstream_name_str)
            .to_string();

        Some((remote_branch, ahead, behind))
    }
}
