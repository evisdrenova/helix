/*
This file defines the logic that writes to the .helix/index file with atomic updates
 */

use std::{
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

use crate::helix_index::{format::Footer, Entry, Header};

pub struct Writer {
    repo_path: PathBuf,
}

impl Writer {
    pub fn new(repo_path: &Path) -> Self {
        Self {
            repo_path: repo_path.to_path_buf(),
        }
    }

    /// 1. Write helix.idx.new
    /// 2. flush
    /// 3. rename -> helix.idx
    pub fn write(&self, header: &Header, entries: &[Entry]) -> Result<()> {
        let helix_dir = self.repo_path.join(".git/helix");
        let index_path = helix_dir.join("helix.idx");
        let temp_path = helix_dir.join("helix.idx.new");

        // Ensure .git/helix directory exists
        fs::create_dir_all(&helix_dir).context("Failed to create .git/helix directory")?;

        // Write to temp file
        {
            let mut file = File::create(&temp_path).context("Failed to create temp index file")?;

            // Write header
            let header_bytes = header.to_bytes();
            file.write_all(&header_bytes)?;
            let mut hasher = Sha256::new();
            hasher.update(&header_bytes);

            // Write entries
            for entry in entries {
                let entry_bytes = entry.to_bytes();
                file.write_all(&entry_bytes)
                    .context("Failed to write entry")?;
                hasher.update(&entry_bytes);
            }

            // Compute footer
            let checksum: [u8; 32] = hasher.finalize().into();
            let footer = Footer::new(checksum);

            // Write footer
            file.write_all(&footer.to_bytes())
                .context("Failed to write footer")?;

            /*  we use flush() here instad of fsync because helix_index is derived and can alwyas be rebuilt since it relies on the git index in a way this is an optimistic sync that is very very fast, 10x+ faster than fsync. But there is
            the risk of a power outage in the window from when flush runs and the data is in the kernal page cache to
            when it is eventually written to disk. However, we have a checksum which verifies the validity of the helixindex and if it's off does a full re-write that is durable. In this way, we get speed almost all of the time and in teh rare cases when something crashes and it messes up our read-only index, we can always take the slower route to build. This is also the reason why we don't fsync the directory. we could do that below after we flush or fsync the file to make sure that it is definitely written to disk, but since we can always recreate the helix index, we're prioritizing speed over durability i could also make this configurable? we see the biggest penalities in large repos, so we could always check the number of entries in the git/index and if there's a lot then flush, otherwise fsync, since it won't impact us too much. */
            file.flush().context("Failed to sync temp file")?;
        }

        // Atomic rename
        fs::rename(&temp_path, &index_path).context("Failed to rename temp file to index")?;

        Ok(())
    }

    // Get the expected path for helix.idx
    pub fn index_path(&self) -> PathBuf {
        self.repo_path.join(".git/helix/helix.idx")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_write_creates_helix_directory() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();
        fs::create_dir_all(repo_path.join(".git"))?;

        let writer = Writer::new(repo_path);
        let header = Header::new(1, [0; 16], 0, 0, 0, [0; 20], 0);
        writer.write(&header, &[])?;

        assert!(repo_path.join(".git/helix").exists());
        assert!(repo_path.join(".git/helix/helix.idx").exists());

        Ok(())
    }

    // todo: need a test to see if those also works with parallel writes
    #[test]
    fn test_atomic_write() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();
        fs::create_dir_all(repo_path.join(".git"))?;

        let writer = Writer::new(repo_path);

        // Write version 1
        let header1 = Header::new(1, [0x11; 16], 1111, 0, 0, [0; 20], 0);
        writer.write(&header1, &[])?;

        // Write version 2
        let header2 = Header::new(2, [0x22; 16], 2222, 0, 0, [0; 20], 0);
        writer.write(&header2, &[])?;

        // Temp file should not exist
        let temp_path = repo_path.join(".git/helix/helix.idx.new");
        assert!(!temp_path.exists());

        Ok(())
    }
}
