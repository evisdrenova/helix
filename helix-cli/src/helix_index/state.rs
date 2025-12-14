// State Management for Helix Branch Metadata
//
// File format:
//   [branch "feature"]
//   upstream = main
//
// State stored here:
//   - Upstream branch relationships
//   - Future per-branch metadata

use anyhow::{Context, Result};
use ini::Ini;
use std::path::Path;

use crate::init::create_directory_structure;

const SECTION_PREFIX: &str = "branch";

fn state_path(repo_path: &Path) -> std::path::PathBuf {
    repo_path.join(".helix/state")
}

fn section_key(branch: &str) -> String {
    format!(r#"{} "{}""#, SECTION_PREFIX, branch)
}

/// Load state file (create empty if missing)
fn load_state(repo_path: &Path) -> Result<Ini> {
    let path = state_path(repo_path);

    if !path.exists() {
        return Ok(Ini::new());
    }

    let ini = Ini::load_from_file(&path).context("Failed to read .helix/state")?;
    Ok(ini)
}

/// Save state file, ensuring directory exists
fn save_state(repo_path: &Path, ini: &Ini) -> Result<()> {
    let helix_dir = repo_path.join(".helix");
    if !helix_dir.exists() {
        create_directory_structure(repo_path)?;
    }

    let path = state_path(repo_path);
    ini.write_to_file(&path)
        .context("Failed to write .helix/state")
}

/// Set the upstream branch for a given branch
pub fn set_branch_upstream(repo_path: &Path, branch_name: &str, upstream: &str) -> Result<()> {
    let mut ini = load_state(repo_path)?;
    let section = section_key(branch_name);

    // Insert or overwrite
    ini.with_section(Some(section)).set("upstream", upstream);

    save_state(repo_path, &ini)
}

/// Get "upstream" value for a branch
pub fn get_branch_upstream(repo_path: &Path, branch_name: &str) -> Option<String> {
    let ini = load_state(repo_path).ok()?;
    let section = section_key(branch_name);

    ini.section(Some(section))
        .and_then(|sec| sec.get("upstream"))
        .map(|s| s.to_string())
}

/// Remove an entire [branch "X"] section
pub fn remove_branch_state(repo_path: &Path, branch_name: &str) -> Result<()> {
    let mut ini = load_state(repo_path)?;
    let section = section_key(branch_name);

    ini.delete(Some(section));
    save_state(repo_path, &ini)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_set_and_get_upstream() -> Result<()> {
        let temp = TempDir::new()?;
        let repo = temp.path();

        set_branch_upstream(repo, "feature", "main")?;
        assert_eq!(
            get_branch_upstream(repo, "feature"),
            Some("main".to_string())
        );

        Ok(())
    }

    #[test]
    fn test_update_existing_upstream() -> Result<()> {
        let temp = TempDir::new()?;
        let repo = temp.path();

        set_branch_upstream(repo, "feature", "main")?;
        set_branch_upstream(repo, "feature", "develop")?;

        assert_eq!(
            get_branch_upstream(repo, "feature"),
            Some("develop".to_string())
        );

        Ok(())
    }

    #[test]
    fn test_multiple_branches() -> Result<()> {
        let temp = TempDir::new()?;
        let repo = temp.path();

        set_branch_upstream(repo, "feature1", "main")?;
        set_branch_upstream(repo, "feature2", "develop")?;
        set_branch_upstream(repo, "hotfix", "main")?;

        assert_eq!(
            get_branch_upstream(repo, "feature1"),
            Some("main".to_string())
        );
        assert_eq!(
            get_branch_upstream(repo, "feature2"),
            Some("develop".to_string())
        );
        assert_eq!(
            get_branch_upstream(repo, "hotfix"),
            Some("main".to_string())
        );

        Ok(())
    }

    #[test]
    fn test_remove_branch_state() -> Result<()> {
        let temp = TempDir::new()?;
        let repo = temp.path();

        set_branch_upstream(repo, "feature", "main")?;
        assert!(get_branch_upstream(repo, "feature").is_some());

        remove_branch_state(repo, "feature")?;
        assert!(get_branch_upstream(repo, "feature").is_none());

        Ok(())
    }

    #[test]
    fn test_get_nonexistent_branch() -> Result<()> {
        let temp = TempDir::new()?;
        let repo = temp.path();

        assert_eq!(get_branch_upstream(repo, "nope"), None);
        Ok(())
    }
}
