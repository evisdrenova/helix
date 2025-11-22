use super::format::{Entry, Footer, FormatError, Header, FOOTER_SIZE, HEADER_SIZE};
use anyhow::{Context, Result};
use memmap2::Mmap;
use sha2::{Digest, Sha256};
use std::fs::File;
use std::path::{Path, PathBuf};

pub struct Reader {
    repo_path: PathBuf,
}

#[derive(Debug, Clone)]
pub struct HelixIndexData {
    pub header: Header,
    pub entries: Vec<Entry>,
}

impl Reader {
    pub fn new(repo_path: &Path) -> Self {
        Self {
            repo_path: repo_path.to_path_buf(),
        }
    }

    // Read and parse helix.idx
    pub fn read(&self) -> Result<HelixIndexData> {
        let index_path = self.repo_path.join(".git/helix/helix.idx");

        if !index_path.exists() {
            anyhow::bail!("helix.idx does not exist at {}", index_path.display());
        }

        let file = File::open(&index_path).context("Failed to open helix.idx")?;

        let mmap = unsafe { Mmap::map(&file) }.context("Failed to mmap helix.idx")?;

        self.parse(&mmap)
    }

    fn parse(&self, data: &[u8]) -> Result<HelixIndexData> {
        if data.len() < HEADER_SIZE + FOOTER_SIZE {
            anyhow::bail!("Index file too small");
        }

        // Parse header
        let header = Header::from_bytes(&data[0..HEADER_SIZE]).context("Failed to parse header")?;

        // Parse entries
        let mut entries = Vec::with_capacity(header.entry_count as usize);
        let mut offset = HEADER_SIZE;
        let entries_end = data.len() - FOOTER_SIZE;

        for _ in 0..header.entry_count {
            if offset >= entries_end {
                anyhow::bail!("Unexpected end of entries");
            }

            let (entry, consumed) =
                Entry::from_bytes(&data[offset..]).context("Failed to parse entry")?;

            entries.push(entry);
            offset += consumed;
        }

        // Parse footer
        let footer = Footer::from_bytes(&data[data.len() - FOOTER_SIZE..])
            .context("Failed to parse footer")?;

        // Verify checksum
        let mut hasher = Sha256::new();
        hasher.update(&data[0..data.len() - FOOTER_SIZE]);
        let computed_checksum: [u8; 32] = hasher.finalize().into();

        if computed_checksum != footer.checksum {
            return Err(FormatError::ChecksumMismatch.into());
        }

        Ok(HelixIndexData { header, entries })
    }

    pub fn exists(&self) -> bool {
        self.repo_path.join(".git/helix/helix.idx").exists()
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::helix_index::format::EntryFlags;
    use crate::helix_index::writer::Writer;
    use tempfile::TempDir;

    #[test]
    fn test_read_nonexistent() {
        let temp_dir = TempDir::new().unwrap();
        let reader = Reader::new(temp_dir.path());
        assert!(!reader.exists());
        assert!(reader.read().is_err());
    }

    #[test]
    fn test_generation_preserved() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();
        fs::create_dir_all(repo_path.join(".git"))?;

        let writer = Writer::new(repo_path);
        let header = Header::new(42, [0xaa; 16], 1234567890, 123, 4096, [0xbb; 20], 1);

        let entries = vec![Entry {
            path: PathBuf::from("test.txt"),
            size: 100,
            mtime_sec: 1000,
            mtime_nsec: 0,
            flags: EntryFlags::TRACKED,
            oid: [1; 20],
            reserved: [0; 64],
        }];

        writer.write(&header, &entries)?;

        let reader = Reader::new(repo_path);
        let data = reader.read()?;

        assert_eq!(data.header.generation, 42);
        assert_eq!(data.header.repo_fingerprint, [0xaa; 16]);

        Ok(())
    }
}
