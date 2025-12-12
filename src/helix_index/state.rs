// Helper functions for managing .helix/state file
//
// This module provides utilities for reading and writing to the .helix/state
// file, which stores internal Helix runtime state like branch upstream tracking.
//
// Format is INI-style:
// [branch "feature"]
//     upstream = main

use anyhow::{Context, Result};
use std::fs;
use std::path::Path;

/// Set the upstream branch for a given branch in .helix/state
pub fn set_branch_upstream(repo_path: &Path, branch_name: &str, upstream: &str) -> Result<()> {
    let state_path = repo_path.join(".helix/state");

    // Read existing state or start with empty
    let mut content = if state_path.exists() {
        fs::read_to_string(&state_path).context("Failed to read .helix/state")?
    } else {
        String::new()
    };

    let section_header = format!("[branch \"{}\"]", branch_name);
    let upstream_line = format!("    upstream = {}", upstream);

    // Check if section already exists
    if let Some(section_start) = content.find(&section_header) {
        // Section exists, need to update or add upstream line
        let after_header = section_start + section_header.len();

        // Find the end of this section (next [ or end of file)
        let section_end = content[after_header..]
            .find("\n[")
            .map(|pos| after_header + pos)
            .unwrap_or(content.len());

        let section_content = &content[after_header..section_end];

        // Check if upstream already exists in this section
        if let Some(upstream_pos) = section_content.find("upstream = ") {
            // Replace existing upstream value
            let line_start =
                after_header + section_content[..upstream_pos].rfind('\n').unwrap_or(0);
            let line_end = after_header
                + upstream_pos
                + section_content[upstream_pos..]
                    .find('\n')
                    .unwrap_or(section_content[upstream_pos..].len());

            content.replace_range(line_start..line_end, &format!("\n{}", upstream_line));
        } else {
            // Add upstream line after section header
            content.insert_str(after_header, &format!("\n{}", upstream_line));
        }
    } else {
        // Section doesn't exist, append it
        if !content.is_empty() && !content.ends_with('\n') {
            content.push('\n');
        }
        content.push_str(&format!("{}\n{}\n", section_header, upstream_line));
    }

    // Ensure .helix directory exists
    let helix_dir = repo_path.join(".helix");
    if !helix_dir.exists() {
        fs::create_dir_all(&helix_dir)?;
    }

    // Write back to file
    fs::write(&state_path, content).context("Failed to write .helix/state")?;

    Ok(())
}

/// Get the upstream branch for a given branch from .helix/state
pub fn get_branch_upstream(repo_path: &Path, branch_name: &str) -> Option<String> {
    let state_path = repo_path.join(".helix/state");

    let state_content = fs::read_to_string(&state_path).ok()?;
    let section_header = format!("[branch \"{}\"]", branch_name);

    let mut in_section = false;

    for line in state_content.lines() {
        let trimmed = line.trim();

        if trimmed == section_header {
            in_section = true;
            continue;
        }

        // If we hit another section, we're done
        if trimmed.starts_with('[') && in_section {
            break;
        }

        if in_section {
            if let Some(value) = trimmed.strip_prefix("upstream = ") {
                return Some(value.trim().to_string());
            }
        }
    }

    None
}

/// Remove a branch section from .helix/state
pub fn remove_branch_state(repo_path: &Path, branch_name: &str) -> Result<()> {
    let state_path = repo_path.join(".helix/state");

    if !state_path.exists() {
        return Ok(());
    }

    let content = fs::read_to_string(&state_path).context("Failed to read .helix/state")?;

    let section_header = format!("[branch \"{}\"]", branch_name);

    if let Some(section_start) = content.find(&section_header) {
        // Find the start of the line containing the section header
        let line_start = content[..section_start]
            .rfind('\n')
            .map(|pos| pos + 1)
            .unwrap_or(0);

        // Find the end of this section (next [ or end of file)
        let after_header = section_start + section_header.len();
        let section_end = content[after_header..]
            .find("\n[")
            .map(|pos| after_header + pos)
            .unwrap_or(content.len());

        // Remove the entire section
        let mut new_content = String::new();
        new_content.push_str(&content[..line_start]);
        new_content.push_str(&content[section_end..]);

        fs::write(&state_path, new_content).context("Failed to write .helix/state")?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_set_and_get_upstream() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        // Set upstream for feature branch
        set_branch_upstream(repo_path, "feature", "main")?;

        // Verify it was written
        let upstream = get_branch_upstream(repo_path, "feature");
        assert_eq!(upstream, Some("main".to_string()));

        Ok(())
    }

    #[test]
    fn test_update_existing_upstream() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        // Set initial upstream
        set_branch_upstream(repo_path, "feature", "main")?;

        // Update it
        set_branch_upstream(repo_path, "feature", "develop")?;

        // Verify it was updated
        let upstream = get_branch_upstream(repo_path, "feature");
        assert_eq!(upstream, Some("develop".to_string()));

        Ok(())
    }

    #[test]
    fn test_multiple_branches() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        // Set upstream for multiple branches
        set_branch_upstream(repo_path, "feature1", "main")?;
        set_branch_upstream(repo_path, "feature2", "develop")?;
        set_branch_upstream(repo_path, "hotfix", "main")?;

        // Verify all are correct
        assert_eq!(
            get_branch_upstream(repo_path, "feature1"),
            Some("main".to_string())
        );
        assert_eq!(
            get_branch_upstream(repo_path, "feature2"),
            Some("develop".to_string())
        );
        assert_eq!(
            get_branch_upstream(repo_path, "hotfix"),
            Some("main".to_string())
        );

        Ok(())
    }

    #[test]
    fn test_remove_branch_state() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        // Set upstream
        set_branch_upstream(repo_path, "feature", "main")?;
        assert!(get_branch_upstream(repo_path, "feature").is_some());

        // Remove it
        remove_branch_state(repo_path, "feature")?;
        assert!(get_branch_upstream(repo_path, "feature").is_none());

        Ok(())
    }

    #[test]
    fn test_get_nonexistent_branch() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        // Try to get upstream for branch that doesn't exist
        let upstream = get_branch_upstream(repo_path, "nonexistent");
        assert_eq!(upstream, None);

        Ok(())
    }
}
