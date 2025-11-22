/*
Generate a unique fingerprint for this repository

Uses: MD5(repo_canonical_path + HEAD_oid)

This ensures helix.idx can't be accidentally reused across repos
*/
use anyhow::{Context, Result};
use md5::{Digest, Md5};
use std::path::Path;

pub fn generate_repo_fingerprint(repo_path: &Path) -> Result<[u8; 16]> {
    let canonical_path = repo_path
        .canonicalize()
        .context("Failed to canonicalize repo path")?;

    // Get HEAD commit OID
    let head_oid = get_head_oid(repo_path).unwrap_or_else(|_| vec![0u8; 20]);

    let mut hasher = Md5::new();
    hasher.update(canonical_path.to_string_lossy().as_bytes());
    hasher.update(&head_oid);

    let result: [u8; 16] = hasher.finalize().into();
    Ok(result)
}

fn get_head_oid(repo_path: &Path) -> Result<Vec<u8>> {
    use git2::Repository;

    let repo = Repository::open(repo_path).context("Failed to open repository")?;

    let head = repo.head().context("Failed to get HEAD")?;

    if let Some(target) = head.target() {
        Ok(target.as_bytes().to_vec())
    } else {
        // HEAD is unborn (no commits yet)
        Ok(vec![0u8; 20])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;
    use tempfile::TempDir;

    #[test]
    fn test_fingerprint_deterministic() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        // Initialize git repo
        fs::create_dir_all(repo_path.join(".git"))?;
        Command::new("git")
            .args(&["init"])
            .current_dir(repo_path)
            .output()?;

        let fp1 = generate_repo_fingerprint(repo_path)?;
        let fp2 = generate_repo_fingerprint(repo_path)?;

        assert_eq!(fp1, fp2);

        Ok(())
    }

    #[test]
    fn test_different_repos_different_fingerprints() -> Result<()> {
        let temp_dir1 = TempDir::new()?;
        let temp_dir2 = TempDir::new()?;

        for dir in [&temp_dir1, &temp_dir2] {
            let repo_path = dir.path();
            fs::create_dir_all(repo_path.join(".git"))?;
            Command::new("git")
                .args(&["init"])
                .current_dir(repo_path)
                .output()?;
        }

        let fp1 = generate_repo_fingerprint(temp_dir1.path())?;
        let fp2 = generate_repo_fingerprint(temp_dir2.path())?;

        assert_ne!(fp1, fp2);

        Ok(())
    }
}
