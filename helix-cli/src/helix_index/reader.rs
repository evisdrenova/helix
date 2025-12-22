/// Defines functions and methods to read from the helix.index canonical file and the cached, memory-mapped representation of the helix.index file
use crate::helix_index::EntryFlags;

use super::format::{Entry, Footer, FormatError, Header, FOOTER_SIZE};
use anyhow::{Context, Result};
use memmap2::Mmap;
use rayon::prelude::*;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

pub struct Reader {
    repo_path: PathBuf,
}

/// Canonical helix index
#[derive(Debug, Clone)]
pub struct HelixIndex {
    pub header: Header,
    pub entries: Vec<Entry>,
}

/// Cached, optimized view of helix.idx (mmap + indices)
pub struct CachedHelixIndex {
    _mmap: Mmap, // Keep mmap alive
    data: HelixIndex,
    path_index: HashMap<PathBuf, usize>,
}

impl Reader {
    pub fn new(repo_path: &Path) -> Self {
        Self {
            repo_path: repo_path.to_path_buf(),
        }
    }

    /// Read and parse helix.idx into a memory mapped file for fast reads
    pub fn read(&self) -> Result<HelixIndex> {
        let index_path = self.repo_path.join(".helix/helix.idx");

        if !index_path.exists() {
            anyhow::bail!("helix.idx does not exist at {}", index_path.display());
        }

        let file = File::open(&index_path).context("Failed to open helix.idx")?;

        let mmap = unsafe { Mmap::map(&file) }.context("Failed to mmap helix.idx")?;

        self.parse(&mmap)
    }

    fn parse(&self, data: &[u8]) -> Result<HelixIndex> {
        if data.len() < Header::HEADER_SIZE + FOOTER_SIZE {
            anyhow::bail!("Index file too small");
        }

        // Parse header
        let header =
            Header::from_bytes(&data[0..Header::HEADER_SIZE]).context("Failed to parse header")?;

        // Parse entries (sequential - entries are variable length)
        let mut entries = Vec::with_capacity(header.entry_count as usize);
        let mut offset = Header::HEADER_SIZE;
        let entries_end = data.len() - FOOTER_SIZE;

        for i in 0..header.entry_count {
            if offset >= entries_end {
                anyhow::bail!("Unexpected end of entries at entry {}", i);
            }

            let entry = Entry::from_bytes(&data[offset..])
                .with_context(|| format!("Failed to parse entry {}", i))?;

            entries.push(entry);
            offset += Entry::ENTRY_MAX_SIZE;
        }

        // Parse footer
        let footer = Footer::from_bytes(&data[data.len() - FOOTER_SIZE..])
            .context("Failed to parse footer")?;

        // Verify checksum (parallel for large data)
        let computed_checksum = if data.len() > 1_000_000 {
            // Parallel checksum for large indexes (>1MB)
            self.parallel_checksum(&data[0..data.len() - FOOTER_SIZE])
        } else {
            // Sequential for small indexes
            let mut hasher = Sha256::new();
            hasher.update(&data[0..data.len() - FOOTER_SIZE]);
            hasher.finalize().into()
        };

        if computed_checksum != footer.checksum {
            return Err(FormatError::ChecksumMismatch.into());
        }

        Ok(HelixIndex { header, entries })
    }

    fn parallel_checksum(&self, data: &[u8]) -> [u8; 32] {
        // Split data into chunks for parallel hashing
        const CHUNK_SIZE: usize = 256 * 1024; // 256KB chunks

        let chunks: Vec<_> = data.chunks(CHUNK_SIZE).collect();

        // Hash each chunk in parallel
        let chunk_hashes: Vec<_> = chunks
            .par_iter()
            .map(|chunk| {
                let mut hasher = Sha256::new();
                hasher.update(chunk);
                hasher.finalize()
            })
            .collect();

        // Combine chunk hashes
        let mut final_hasher = Sha256::new();
        for hash in chunk_hashes {
            final_hasher.update(&hash);
        }

        final_hasher.finalize().into()
    }

    pub fn read_cached(&self) -> Result<CachedHelixIndex> {
        let index_path = self.repo_path.join(".helix/helix.idx");

        if !index_path.exists() {
            anyhow::bail!("helix.idx does not exist at {}", index_path.display());
        }

        let file = File::open(&index_path).context("Failed to open helix.idx")?;
        let mmap = unsafe { Mmap::map(&file) }.context("Failed to mmap helix.idx")?;

        let data = self.parse(&mmap)?;

        let path_index: HashMap<PathBuf, usize> = if data.entries.len() > 1000 {
            data.entries
                .par_iter()
                .enumerate()
                .map(|(i, entry)| (entry.path.clone(), i))
                .collect()
        } else {
            data.entries
                .iter()
                .enumerate()
                .map(|(i, entry)| (entry.path.clone(), i))
                .collect()
        };

        Ok(CachedHelixIndex {
            _mmap: mmap,
            data,
            path_index,
        })
    }

    pub fn exists(&self) -> bool {
        self.repo_path.join(".helix/helix.idx").exists()
    }

    pub fn read_header(&self) -> Result<Header> {
        let index_path = self.repo_path.join(".helix/helix.idx");
        let mut file = File::open(&index_path).context("Failed to open helix.idx")?;

        let mut header_bytes = [0u8; Header::HEADER_SIZE];
        file.read_exact(&mut header_bytes)
            .context("Failed to read header")?;

        Header::from_bytes(&header_bytes).context("Failed to parse header")
    }

    /// Get entry count without parsing all entries
    pub fn entry_count(&self) -> Result<u32> {
        Ok(self.read_header()?.entry_count)
    }

    /// Get generation without parsing all entries
    pub fn generation(&self) -> Result<u64> {
        Ok(self.read_header()?.generation)
    }
}

impl CachedHelixIndex {
    /// Get entry by path (O(1) lookup)
    pub fn get(&self, path: &Path) -> Option<&Entry> {
        self.path_index
            .get(path)
            .and_then(|&idx| self.data.entries.get(idx))
    }

    /// Check if path exists in index
    pub fn contains(&self, path: &Path) -> bool {
        self.path_index.contains_key(path)
    }

    /// Get all entries
    pub fn entries(&self) -> &[Entry] {
        &self.data.entries
    }

    /// Get header
    pub fn header(&self) -> &Header {
        &self.data.header
    }

    pub fn staged_files(&self) -> impl ParallelIterator<Item = &Entry> {
        self.data
            .entries
            .par_iter()
            .filter(|e| e.flags.contains(EntryFlags::STAGED))
    }

    /// Get all modified files
    pub fn modified_files(&self) -> impl ParallelIterator<Item = &Entry> {
        self.data
            .entries
            .par_iter()
            .filter(|e| e.flags.contains(EntryFlags::MODIFIED))
    }

    /// Get all untracked files
    pub fn untracked_files(&self) -> impl ParallelIterator<Item = &Entry> {
        self.data
            .entries
            .par_iter()
            .filter(|e| e.flags.contains(EntryFlags::UNTRACKED))
    }

    /// Get all tracked files
    pub fn tracked_files(&self) -> impl ParallelIterator<Item = &Entry> {
        self.data
            .entries
            .par_iter()
            .filter(|e| e.flags.contains(EntryFlags::TRACKED))
    }

    /// Get all files with conflicts
    pub fn conflict_files(&self) -> impl ParallelIterator<Item = &Entry> {
        self.data
            .entries
            .par_iter()
            .filter(|e| e.flags.contains(EntryFlags::CONFLICT))
    }

    /// Get entries in a specific directory
    pub fn entries_in_dir(&self, dir: &Path) -> impl ParallelIterator<Item = &Entry> {
        let dir = dir.to_path_buf();
        self.data
            .entries
            .par_iter()
            .filter(move |e| e.path.starts_with(&dir))
    }

    /// Collect staged paths
    pub fn collect_staged_paths(&self) -> Vec<PathBuf> {
        self.staged_files().map(|e| e.path.clone()).collect()
    }

    /// Collect modified paths
    pub fn collect_modified_paths(&self) -> Vec<PathBuf> {
        self.modified_files().map(|e| e.path.clone()).collect()
    }

    /// Count entries matching a predicate
    pub fn count_matching<F>(&self, predicate: F) -> usize
    where
        F: Fn(&Entry) -> bool + Sync + Send,
    {
        self.data
            .entries
            .par_iter()
            .filter(|e| predicate(e))
            .count()
    }

    /// Find first entry matching predicate
    pub fn find_entry<F>(&self, predicate: F) -> Option<&Entry>
    where
        F: Fn(&Entry) -> bool + Sync + Send,
    {
        self.data.entries.par_iter().find_any(|e| predicate(e))
    }

    /// Check if any entry matches predicate
    pub fn any_matching<F>(&self, predicate: F) -> bool
    where
        F: Fn(&Entry) -> bool + Sync + Send,
    {
        self.data.entries.par_iter().any(|e| predicate(e))
    }

    /// Convert to owned data (consumes the cache)
    pub fn into_data(self) -> HelixIndex {
        self.data
    }
}

impl HelixIndex {
    /// Get entry by path (O(n) lookup)
    pub fn get(&self, path: &Path) -> Option<&Entry> {
        self.entries.iter().find(|e| e.path == path)
    }

    /// Get mutable entry by path (O(n) lookup)
    pub fn get_mut(&mut self, path: &Path) -> Option<&mut Entry> {
        self.entries.iter_mut().find(|e| e.path == path)
    }

    /// Build a cached index from this data
    pub fn into_cached(self, mmap: Mmap) -> CachedHelixIndex {
        let path_index: HashMap<PathBuf, usize> = if self.entries.len() > 1000 {
            self.entries
                .par_iter()
                .enumerate()
                .map(|(i, entry)| (entry.path.clone(), i))
                .collect()
        } else {
            self.entries
                .iter()
                .enumerate()
                .map(|(i, entry)| (entry.path.clone(), i))
                .collect()
        };

        CachedHelixIndex {
            _mmap: mmap,
            data: self,
            path_index,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::helix_index::writer::Writer;
    use helix_protocol::hash;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_parallel_filtering() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();
        fs::create_dir_all(repo_path.join(".helix"))?;

        let writer = Writer::new_cached(repo_path);
        let header = Header::new(1, 1000);

        // Create 1000 entries with different flags
        let entries: Vec<_> = (0..1000)
            .map(|i| Entry {
                path: PathBuf::from(format!("file{}.txt", i)),
                size: 100,
                mtime_sec: 1000,
                mtime_nsec: 0,
                flags: if i % 3 == 0 {
                    EntryFlags::TRACKED | EntryFlags::STAGED
                } else if i % 3 == 1 {
                    EntryFlags::TRACKED | EntryFlags::MODIFIED
                } else {
                    EntryFlags::UNTRACKED
                },
                oid: hash::ZERO_HASH,
                merge_conflict_stage: 0,
                file_mode: 0o100644,
                reserved: [0; 33],
            })
            .collect();

        writer.write(&header, &entries)?;

        let reader = Reader::new(repo_path);
        let cached = reader.read_cached()?;

        // Test parallel collection
        let staged: Vec<_> = cached.staged_files().map(|e| &e.path).collect();
        assert_eq!(staged.len(), 334); // ~1000/3

        let modified: Vec<_> = cached.modified_files().map(|e| &e.path).collect();
        assert_eq!(modified.len(), 333);

        let untracked: Vec<_> = cached.untracked_files().map(|e| &e.path).collect();
        assert_eq!(untracked.len(), 333);

        Ok(())
    }

    #[test]
    fn test_parallel_count() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();
        fs::create_dir_all(repo_path.join(".helix"))?;

        let writer = Writer::new_cached(repo_path);
        let header = Header::new(1, 100);

        let entries: Vec<_> = (0..100)
            .map(|i| Entry {
                path: PathBuf::from(format!("file{}.txt", i)),
                size: i as u64,
                mtime_sec: 1000,
                mtime_nsec: 0,
                flags: EntryFlags::TRACKED,
                oid: hash::ZERO_HASH,
                merge_conflict_stage: 0,
                file_mode: 0o100644,
                reserved: [0; 33],
            })
            .collect();

        writer.write(&header, &entries)?;

        let reader = Reader::new(repo_path);
        let cached = reader.read_cached()?;

        // Count files > 50 bytes (parallel)
        let large_count = cached.count_matching(|e| e.size > 50);
        assert_eq!(large_count, 49);

        Ok(())
    }

    #[test]
    fn test_parallel_any() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();
        fs::create_dir_all(repo_path.join(".helix"))?;

        let writer = Writer::new_cached(repo_path);
        let header = Header::new(1, 10);

        let entries: Vec<_> = (0..10)
            .map(|i| {
                Entry::new(
                    PathBuf::from(format!("file{}.txt", i)),
                    1024,
                    1000,
                    hash::ZERO_HASH,
                    0,
                )
            })
            .collect();

        writer.write(&header, &entries)?;

        let reader = Reader::new(repo_path);
        let cached = reader.read_cached()?;

        // Check if any file has size > 50
        let has_large = cached.any_matching(|e| e.size > 50);
        assert!(has_large);

        Ok(())
    }
}
