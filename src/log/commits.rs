// src/log/commits.rs
//
// Git commit data structures and repository operations

use anyhow::{Context, Result};
use chrono::{DateTime, Local};
use git2::{Commit as GitCommit, Oid, Repository};
use std::path::Path;

/// A simplified commit structure optimized for display
#[derive(Debug, Clone)]
pub struct Commit {
    pub hash: String,
    pub short_hash: String,
    pub author_name: String,
    pub author_email: String,
    pub timestamp: DateTime<Local>,
    pub message: String,
    pub summary: String, // First line of message
    pub files_changed: usize,
    pub insertions: usize,
    pub deletions: usize,
    pub is_merge: bool,
}

impl Commit {
    /// Create a Commit from a git2::Commit
    pub fn from_git_commit(commit: &GitCommit, repo: &Repository) -> Result<Self> {
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
        let (files_changed, insertions, deletions) = calculate_diff_stats(commit, repo)?;

        Ok(Commit {
            hash,
            short_hash,
            author_name,
            author_email,
            timestamp,
            message,
            summary,
            files_changed,
            insertions,
            deletions,
            is_merge,
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

    /// Get a short stats summary (e.g., "3 files · +247 -18")
    pub fn stats_summary(&self) -> String {
        format!(
            "{} file{} · +{} -{}",
            self.files_changed,
            if self.files_changed == 1 { "" } else { "s" },
            self.insertions,
            self.deletions
        )
    }
}

/// Calculate diff statistics for a commit
fn calculate_diff_stats(commit: &GitCommit, repo: &Repository) -> Result<(usize, usize, usize)> {
    let tree = commit.tree().context("Failed to get commit tree")?;

    let parent_tree = if commit.parent_count() > 0 {
        Some(commit.parent(0)?.tree()?)
    } else {
        None
    };

    let diff = repo.diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), None)?;

    let stats = diff.stats()?;

    Ok((stats.files_changed(), stats.insertions(), stats.deletions()))
}

/// Repository wrapper for loading commits
pub struct CommitLoader {
    repo: Repository,
}

impl CommitLoader {
    /// Open a repository at the given path
    pub fn open(path: &Path) -> Result<Self> {
        let repo = Repository::discover(path).context("Failed to open git repository")?;
        Ok(Self { repo })
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

            match Commit::from_git_commit(&git_commit, &self.repo) {
                Ok(commit) => commits.push(commit),
                Err(e) => {
                    eprintln!("Warning: Failed to process commit {}: {}", oid, e);
                    continue;
                }
            }
        }

        Ok(commits)
    }

    /// Get current branch name
    pub fn current_branch(&self) -> Result<String> {
        let head = self.repo.head()?;
        if let Some(name) = head.shorthand() {
            Ok(name.to_string())
        } else {
            Ok("HEAD".to_string())
        }
    }
}
