// this shoudl eventually go away as we build out helix a little more

use anyhow::{Context, Result};
use std::process::Command;

pub struct Git;

impl Git {
    /// Add files to staging area
    pub fn add_files(files: &[String]) -> Result<()> {
        let mut cmd = Command::new("git");
        cmd.arg("add");

        if files.is_empty() {
            // Add all changes if no specific files provided
            cmd.arg(".");
        } else {
            cmd.args(files);
        }

        let output = cmd.output().context("Failed to execute git add")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow::anyhow!("git add failed: {}", stderr));
        }

        Ok(())
    }

    /// Add all modified files (equivalent to git add .)
    pub fn add_all_changes() -> Result<()> {
        let output = Command::new("git")
            .args(["add", "."])
            .output()
            .context("Failed to execute git add .")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow::anyhow!("git add . failed: {}", stderr));
        }

        println!("📁 Staged all modified files");
        Ok(())
    }

    /// Check if there are any unstaged changes
    pub fn has_unstaged_changes() -> Result<bool> {
        let output = Command::new("git")
            .args(["diff", "--quiet"])
            .output()
            .context("Failed to check for unstaged changes")?;

        // git diff --quiet returns 0 if no changes, 1 if there are changes
        Ok(!output.status.success())
    }

    /// Get the staged diff for commit message generation
    pub fn get_staged_diff() -> Result<String> {
        let output = Command::new("git")
            .args(["diff", "--cached", "--no-color"])
            .output()
            .context("Failed to execute git diff")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow::anyhow!("git diff failed: {}", stderr));
        }

        let diff = String::from_utf8(output.stdout).context("Invalid UTF-8 in git diff output")?;

        if diff.trim().is_empty() {
            return Err(anyhow::anyhow!(
                "No staged changes found. Use 'git add' to stage files first."
            ));
        }

        Ok(Self::format_diff_for_llm(&diff))
    }

    /// Format the raw git diff into a more LLM-friendly format
    fn format_diff_for_llm(raw_diff: &str) -> String {
        let mut formatted = String::new();
        let mut current_file = String::new();
        let mut changes = Vec::new();

        for line in raw_diff.lines() {
            if line.starts_with("diff --git") {
                // Save previous file's changes
                if !current_file.is_empty() && !changes.is_empty() {
                    formatted.push_str(&format!("File: {}\n", current_file));
                    formatted.push_str(&changes.join("\n"));
                    formatted.push_str("\n\n");
                }

                // Extract file name from "diff --git a/file b/file"
                if let Some(file_part) = line.split_whitespace().nth(3) {
                    current_file = file_part.trim_start_matches("b/").to_string();
                }
                changes.clear();
            } else if line.starts_with("+++") || line.starts_with("---") {
                // Skip these lines
                continue;
            } else if line.starts_with("@@") {
                // Hunk header - extract line numbers for context
                changes.push(format!("Changes at {}", line));
            } else if line.starts_with("+") && !line.starts_with("+++") {
                changes.push(format!("  Added: {}", &line[1..]));
            } else if line.starts_with("-") && !line.starts_with("---") {
                changes.push(format!("  Removed: {}", &line[1..]));
            } else if line.starts_with(" ") {
                // Context line - only include if it's meaningful
                let content = &line[1..];
                if !content.trim().is_empty() && changes.len() < 20 {
                    changes.push(format!("  Context: {}", content));
                }
            }
        }

        // Add the last file
        if !current_file.is_empty() && !changes.is_empty() {
            formatted.push_str(&format!("File: {}\n", current_file));
            formatted.push_str(&changes.join("\n"));
        }

        // If the formatted diff is too long, truncate it
        if formatted.len() > 4000 {
            let truncated = &formatted[..4000];
            format!("{}\n\n[... diff truncated for brevity ...]", truncated)
        } else {
            formatted
        }
    }

    /// Create a commit with the generated message
    pub fn commit(subject: &str, body: Option<&str>) -> Result<()> {
        let mut cmd = Command::new("git");
        cmd.arg("commit").arg("-m").arg(subject);

        if let Some(body_text) = body {
            cmd.arg("-m").arg(body_text);
        }

        let output = cmd.output().context("Failed to execute git commit")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow::anyhow!("git commit failed: {}", stderr));
        }

        println!("✓ Commit created successfully");
        Ok(())
    }

    /// Push to remote repository
    pub fn push(branch: Option<&str>) -> Result<()> {
        let mut cmd = Command::new("git");
        cmd.arg("push");

        if let Some(branch_name) = branch {
            cmd.arg("origin").arg(branch_name);
        }

        let output = cmd.output().context("Failed to execute git push")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow::anyhow!("git push failed: {}", stderr));
        }

        println!("✓ Changes pushed to remote successfully");
        Ok(())
    }

    /// Get the current branch name
    pub fn get_get_current_branch_name() -> Result<String> {
        let output = Command::new("git")
            .args(["branch", "--show-current"])
            .output()
            .context("Failed to get current branch")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow::anyhow!("Failed to get current branch: {}", stderr));
        }

        let branch = String::from_utf8(output.stdout)
            .context("Invalid UTF-8 in branch name")?
            .trim()
            .to_string();

        Ok(branch)
    }

    /// Check if there are any staged changes
    pub fn has_staged_changes() -> Result<bool> {
        let output = Command::new("git")
            .args(["diff", "--cached", "--quiet"])
            .output()
            .context("Failed to check for staged changes")?;

        // git diff --quiet returns 0 if no changes, 1 if there are changes
        Ok(!output.status.success())
    }
}
