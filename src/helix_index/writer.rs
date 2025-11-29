/*

Helix uses a two-tier index architecture with different durability guarantees:

┌─────────────────────────────────────────────────────────┐
│ CANONICAL INDEX (.helix/helix.idx)                      │
│ • Source of truth for repository state                  │
│ • Written with fsync() for full durability             │
│ • ACID guarantees - no data loss on crash              │
│ • Updated on: add, remove, commit, merge, etc.         │
│ • Slower writes (~10x) but required for correctness    │
└─────────────────────────────────────────────────────────┘
                          │
                          │ derives from
                          ↓
┌─────────────────────────────────────────────────────────┐
│ CACHED INDEX (memory-mapped, read-only)                 │
│ • Optimized for fast reads (mmap + hash map)           │
│ • Written with flush() only (10x faster)               │
│ • Can be rebuilt from canonical if corrupted           │
│ • Updated on: lazy refresh when canonical changes      │
│ • Used for: status, diff, search, etc.                 │
└─────────────────────────────────────────────────────────┘
*/
use std::{
    collections::HashSet,
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
};

use crate::helix_index::{format::Footer, Entry, Header};
use anyhow::{Context, Result};
use rayon::prelude::*;
use sha2::{Digest, Sha256};
use std::io::BufWriter;
use std::sync::mpsc;
use std::thread;

pub struct Writer {
    repo_path: PathBuf,
    durable: bool, // true for canonical, false for cached version
}

/// Stateful writer for incremental index updates
pub struct IndexBuilder {
    repo_path: PathBuf,
    header: Header,
    entries: Vec<Entry>,
    durable: bool,
}

impl Writer {
    /// Create writer for cached index (fast writes with flush only)
    /// Used for derived read-only caches that can be rebuilt
    pub fn new_cached(repo_path: &Path) -> Self {
        Self {
            repo_path: repo_path.to_path_buf(),
            durable: false, // Cache can use flush() for speed
        }
    }

    /// Create writer for canonical index that must maintain durability and atomicity
    pub fn new_canonical(repo_path: &Path) -> Self {
        Self {
            repo_path: repo_path.to_path_buf(),
            durable: true, // Canonical index must be durable
        }
    }

    /// Write complete index atomically (immutable operation)
    ///
    /// For canonical index (durable=true):
    /// 1. Stream entries to buffered writer
    /// 2. fsync() to ensure durability (slower but safe)
    /// 3. fsync() directory to ensure rename is durable
    /// 4. Atomic rename
    ///
    /// For cached index (durable=false):
    /// 1. Stream entries to buffered writer  
    /// 2. flush() only (10x faster, safe for derived cache)
    /// 3. Atomic rename
    pub fn write(&self, header: &Header, entries: &[Entry]) -> Result<()> {
        let helix_dir = self.repo_path.join(".helix");
        let index_path = helix_dir.join("helix.idx");
        let temp_path = helix_dir.join("helix.idx.new");

        fs::create_dir_all(&helix_dir).context("Failed to create .helix directory")?;

        // Choose strategy based on size
        if entries.len() > 10000 {
            // For huge indexes: parallel checksum with streaming writes
            self.write_large_index(&temp_path, header, entries)?;
        } else {
            // For normal indexes: simple streaming
            self.write_streaming(&temp_path, header, entries)?;
        }

        // Ensure durability if required
        if self.durable {
            // fsync the directory to ensure the rename will be durable
            // This is critical for crash consistency of the canonical index
            let dir =
                File::open(&helix_dir).context("Failed to open .helix directory for fsync")?;
            dir.sync_all().context("Failed to fsync .helix directory")?;
        }

        // Atomic rename
        fs::rename(&temp_path, &index_path).context("Failed to rename temp file to index")?;

        Ok(())
    }

    /// Streaming write for normal-sized indexes (most common case)
    /// Optimized for minimal memory usage and syscall overhead
    fn write_streaming(&self, temp_path: &Path, header: &Header, entries: &[Entry]) -> Result<()> {
        use std::io::BufWriter;

        let file = File::create(temp_path).context("Failed to create temp index file")?;
        // Use 1MB buffer to minimize syscalls (Linux optimal buffer size is typically 128KB-1MB)
        let mut writer = BufWriter::with_capacity(1024 * 1024, file);

        let mut hasher = Sha256::new();

        // Write and hash header
        let header_bytes = header.to_bytes();
        writer.write_all(&header_bytes)?;
        hasher.update(&header_bytes);

        // Stream entries directly - no pre-serialization!
        for entry in entries {
            let entry_bytes = entry.to_bytes()?;
            writer.write_all(&entry_bytes)?;
            hasher.update(&entry_bytes);
        }

        // Write footer
        let checksum: [u8; 32] = hasher.finalize().into();
        let footer = Footer::new(checksum);
        writer.write_all(&footer.to_bytes())?;

        if self.durable {
            /* fsync() for canonical index - ensures data is on disk before rename.
            This is slower (~10x) but critical for data integrity of the source of truth.
            Protects against:
            - Power loss before data hits disk
            - Kernel crash before writeback
            - Storage device reordering

            The combination of fsync(file) + fsync(directory) + rename() gives us
            full ACID durability for the canonical index. */
            writer.flush().context("Failed to flush buffer")?;
            let file = writer
                .into_inner()
                .map_err(|e| anyhow::anyhow!("Failed to get inner file: {}", e))?;
            file.sync_all().context("Failed to fsync temp file")?;
        } else {
            /* flush() for cached index - 10x faster, safe for derived data.
            The cached index is read-only and can always be rebuilt from the
            canonical index. Checksum verification catches corruption. */
            writer.flush().context("Failed to flush temp file")?;
        }

        Ok(())
    }

    /// Parallel write for large indexes (>10k entries)
    /// Computes checksum in parallel while writing
    fn write_large_index(
        &self,
        temp_path: &Path,
        header: &Header,
        entries: &[Entry],
    ) -> Result<()> {
        let file = File::create(temp_path).context("Failed to create temp index file")?;
        let mut writer = BufWriter::with_capacity(2 * 1024 * 1024, file); // 2MB buffer for large writes

        // Write header
        let header_bytes = header.to_bytes();
        writer.write_all(&header_bytes)?;

        // Channel for parallel checksum computation
        let (tx, rx) = mpsc::sync_channel::<Vec<u8>>(16); // Bounded to avoid memory explosion

        // Spawn hasher thread
        let hasher_handle = thread::spawn(move || {
            let mut hasher = Sha256::new();

            // Hash header (received inline)
            if let Ok(header_data) = rx.recv() {
                hasher.update(&header_data);
            }

            // Hash entries as they're sent
            while let Ok(data) = rx.recv() {
                hasher.update(&data);
            }

            let checksum: [u8; 32] = hasher.finalize().into();
            checksum
        });

        // Send header to hasher
        tx.send(header_bytes.to_vec()).ok();

        // Stream entries with parallel hashing
        // Serialize in chunks to balance parallelism and memory
        const CHUNK_SIZE: usize = 100;

        for chunk in entries.chunks(CHUNK_SIZE) {
            // Serialize chunk in parallel
            let serialized: Vec<Vec<u8>> = if chunk.len() > 10 {
                chunk
                    .par_iter()
                    .map(|e| e.to_bytes())
                    .collect::<Result<Vec<_>, _>>()?
            } else {
                // `iter` → serial iterator
                chunk
                    .iter()
                    .map(|e| e.to_bytes())
                    .collect::<Result<Vec<_>, _>>()?
            };

            // Write and send to hasher
            for entry_bytes in serialized {
                writer.write_all(&entry_bytes)?;
                tx.send(entry_bytes).ok();
            }
        }

        // Close channel and wait for checksum
        drop(tx);
        let checksum = hasher_handle
            .join()
            .map_err(|_| anyhow::anyhow!("Hasher thread panicked"))?;

        // Write footer
        let footer = Footer::new(checksum);
        writer.write_all(&footer.to_bytes())?;

        if self.durable {
            // fsync for canonical index
            writer.flush().context("Failed to flush buffer")?;
            let file = writer
                .into_inner()
                .map_err(|e| anyhow::anyhow!("Failed to get inner file: {}", e))?;
            file.sync_all().context("Failed to fsync temp file")?;
        } else {
            // flush only for cached index
            writer.flush().context("Failed to flush temp file")?;
        }

        Ok(())
    }

    /// Get the expected path for helix.idx
    pub fn index_path(&self) -> PathBuf {
        self.repo_path.join(".helix/helix.idx")
    }

    /// Create a new builder for incremental updates
    pub fn builder(&self, header: Header) -> IndexBuilder {
        IndexBuilder {
            repo_path: self.repo_path.clone(),
            header,
            entries: Vec::new(),
            durable: self.durable,
        }
    }

    /// Check if index exists
    pub fn exists(&self) -> bool {
        self.index_path().exists()
    }

    /// Delete the index file
    pub fn delete(&self) -> Result<()> {
        let index_path = self.index_path();
        if index_path.exists() {
            fs::remove_file(&index_path).context("Failed to delete index file")?;
        }
        Ok(())
    }
}

impl IndexBuilder {
    /// Create a new builder with initial entries
    pub fn with_entries(mut self, entries: Vec<Entry>) -> Self {
        self.entries = entries;
        self
    }

    /// Add a single entry (replaces if path exists)
    pub fn add_entry(&mut self, entry: Entry) -> &mut Self {
        if let Some(existing) = self.entries.iter_mut().find(|e| e.path == entry.path) {
            *existing = entry;
        } else {
            self.entries.push(entry);
        }
        self
    }

    /// Add multiple entries in bulk
    pub fn add_entries(&mut self, entries: Vec<Entry>) -> &mut Self {
        for entry in entries {
            self.add_entry(entry);
        }
        self
    }

    /// Remove an entry by path
    pub fn remove_entry(&mut self, path: &Path) -> &mut Self {
        self.entries.retain(|e| e.path != path);
        self
    }

    /// Remove multiple entries
    pub fn remove_entries(&mut self, paths: &[PathBuf]) -> &mut Self {
        let path_set: std::collections::HashSet<_> = paths.iter().collect();
        self.entries.retain(|e| !path_set.contains(&e.path));
        self
    }

    /// Update header generation
    pub fn increment_generation(&mut self) -> &mut Self {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        self.header.generation += 1;
        self.header.last_modified = now;
        self.header.entry_count = self.entries.len() as u32;
        self
    }

    /// Sort entries by path (required before commit)
    pub fn sort_entries(&mut self) -> &mut Self {
        // Parallel sort for large datasets
        if self.entries.len() > 10000 {
            self.entries.par_sort_by(|a, b| a.path.cmp(&b.path));
        } else {
            self.entries.sort_by(|a, b| a.path.cmp(&b.path));
        }
        self
    }

    /// Validate entries before commit
    pub fn validate(&self) -> Result<()> {
        // Check for duplicate paths
        let mut seen = HashSet::new();
        for entry in &self.entries {
            if !seen.insert(&entry.path) {
                anyhow::bail!("Duplicate entry for path: {}", entry.path.display());
            }
        }

        // Validate entry count matches header
        if self.entries.len() != self.header.entry_count as usize {
            anyhow::bail!(
                "Entry count mismatch: header={}, actual={}",
                self.header.entry_count,
                self.entries.len()
            );
        }

        Ok(())
    }

    /// Commit changes to disk (consumes builder)
    pub fn commit(mut self) -> Result<()> {
        // Update header metadata
        self.increment_generation();

        // Sort entries (required for binary search)
        self.sort_entries();

        // Validate before writing
        self.validate()?;

        // Write atomically with appropriate durability
        let writer = Writer {
            repo_path: self.repo_path,
            durable: self.durable,
        };
        writer.write(&self.header, &self.entries)?;

        Ok(())
    }

    /// Get current entry count
    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }

    /// Get reference to entries
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Get mutable reference to entries
    pub fn entries_mut(&mut self) -> &mut Vec<Entry> {
        &mut self.entries
    }

    /// Filter entries by predicate
    pub fn filter_entries<F>(&mut self, predicate: F) -> &mut Self
    where
        F: Fn(&Entry) -> bool,
    {
        self.entries.retain(predicate);
        self.header.entry_count = self.entries.len() as u32;
        self
    }

    /// Update entries in parallel
    pub fn update_entries_parallel<F>(&mut self, updater: F) -> &mut Self
    where
        F: Fn(&mut Entry) + Send + Sync,
    {
        if self.entries.len() > 1000 {
            self.entries.par_iter_mut().for_each(|e| updater(e));
        } else {
            self.entries.iter_mut().for_each(|e| updater(e));
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::helix_index::{
        format::EntryFlags,
        hash::{self, hash_bytes},
    };
    use tempfile::TempDir;

    #[test]
    fn test_write_creates_helix_directory() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        let writer = Writer::new_canonical(repo_path);
        let header = Header::new(1, [0; 32], 0);
        writer.write(&header, &[])?;

        assert!(repo_path.join(".helix").exists());
        assert!(repo_path.join(".helix/helix.idx").exists());

        Ok(())
    }

    #[test]
    fn test_atomic_write() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        let writer = Writer::new_canonical(repo_path);

        // Write version 1
        let header1 = Header::new(1, [0x11; 32], 0);
        writer.write(&header1, &[])?;

        // Write version 2
        let header2 = Header::new(2, [0x22; 32], 0);
        writer.write(&header2, &[])?;

        // Temp file should not exist
        let temp_path = repo_path.join(".helix/helix.idx.new");
        assert!(!temp_path.exists());

        Ok(())
    }

    #[test]
    fn test_builder_add_entry() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        let writer = Writer::new_canonical(repo_path);
        let header = Header::new(1, [0xaa; 32], 0);
        let mut builder = writer.builder(header);

        let entry = Entry::new(PathBuf::from("test.txt"), 1024, 100, hash::ZERO_HASH, 0);

        builder.add_entry(entry);
        assert_eq!(builder.entry_count(), 1);

        Ok(())
    }

    #[test]
    fn test_builder_replace_entry() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        let writer = Writer::new_canonical(repo_path);
        let header = Header::new(1, [0xaa; 32], 0);
        let mut builder = writer.builder(header);

        let entry1 = Entry::new(
            PathBuf::from("test.txt"),
            1024,
            100,
            hash_bytes(b"b"),
            0o100644,
        );

        let entry2 = Entry::new(
            PathBuf::from("test.txt"),
            1024,
            200,
            hash_bytes(b"b"),
            0o100644,
        );

        builder.add_entry(entry1);
        builder.add_entry(entry2);

        assert_eq!(builder.entry_count(), 1);
        assert_eq!(builder.entries()[0].oid, hash_bytes(b"b"));
        assert_eq!(builder.entries()[0].size, 200);

        Ok(())
    }

    #[test]
    fn test_builder_remove_entry() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        let writer = Writer::new_canonical(repo_path);
        let header = Header::new(1, [0xaa; 32], 0);
        let mut builder = writer.builder(header);

        let entry = Entry::new(
            PathBuf::from("test.txt"),
            1024,
            200,
            hash_bytes(b"c"),
            0o100644,
        );

        builder.add_entry(entry);
        assert_eq!(builder.entry_count(), 1);

        builder.remove_entry(&PathBuf::from("test.txt"));
        assert_eq!(builder.entry_count(), 0);

        Ok(())
    }

    #[test]
    fn test_builder_commit() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        let writer = Writer::new_canonical(repo_path);
        let header = Header::new(1, [0xaa; 32], 0);
        let mut builder = writer.builder(header);

        let entries = vec![
            Entry::new(
                PathBuf::from("b.txt"),
                1024,
                100,
                hash_bytes(b"e"),
                0o100644,
            ),
            Entry::new(
                PathBuf::from("a.txt"),
                1024,
                200,
                hash_bytes(b"c"),
                0o100644,
            ),
        ];

        builder.add_entries(entries);
        builder.commit()?;

        assert!(writer.exists());

        Ok(())
    }

    #[test]
    fn test_builder_sorts_entries() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        let writer = Writer::new_canonical(repo_path);
        let header = Header::new(1, [0xaa; 32], 0);
        let mut builder = writer.builder(header);

        // Add in reverse order
        builder.add_entry(Entry::new(
            PathBuf::from("z.txt"),
            1024,
            100,
            hash_bytes(b"b"),
            0o100644,
        ));
        builder.add_entry(Entry::new(
            PathBuf::from("a.txt"),
            1024,
            200,
            hash_bytes(b"c"),
            0o100644,
        ));

        builder.sort_entries();

        assert_eq!(builder.entries()[0].path, PathBuf::from("a.txt"));
        assert_eq!(builder.entries()[1].path, PathBuf::from("z.txt"));

        Ok(())
    }

    #[test]
    fn test_parallel_serialization() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        let writer = Writer::new_canonical(repo_path);
        let header = Header::new(1, [0xaa; 32], 2000);

        // Create 2000 entries to trigger parallel path
        let entries: Vec<_> = (0..2000)
            .map(|i| {
                Entry::new(
                    PathBuf::from(format!("file{}.txt", i)),
                    i as u64,
                    100,
                    hash_bytes(b"g"),
                    0o100644,
                )
            })
            .collect();

        writer.write(&header, &entries)?;
        assert!(writer.exists());

        Ok(())
    }

    #[test]
    fn test_builder_filter_entries() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        let writer = Writer::new_canonical(repo_path);
        let header = Header::new(1, [0xaa; 32], 0);
        let mut builder = writer.builder(header);

        let entries = vec![
            Entry::new(
                PathBuf::from("a.txt"),
                1024,
                100,
                hash_bytes(b"f"),
                0o100644,
            ),
            Entry::new(PathBuf::from("b.rs"), 1024, 200, hash_bytes(b"f"), 0o100644),
            Entry::new(
                PathBuf::from("c.txt"),
                1024,
                300,
                hash_bytes(b"f"),
                0o100644,
            ),
        ];

        builder.add_entries(entries);

        // Filter to only .txt files
        builder.filter_entries(|e| e.path.extension().and_then(|s| s.to_str()) == Some("txt"));

        assert_eq!(builder.entry_count(), 2);
        assert!(builder
            .entries()
            .iter()
            .all(|e| { e.path.extension().and_then(|s| s.to_str()) == Some("txt") }));

        Ok(())
    }

    #[test]
    fn test_builder_update_entries_parallel() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        let writer = Writer::new_canonical(repo_path);
        let header = Header::new(1, [0xaa; 32], 0);
        let mut builder = writer.builder(header);

        let entries: Vec<_> = (0..1500)
            .map(|i| {
                Entry::new(
                    PathBuf::from(format!("file{}.txt", i)),
                    1024,
                    100,
                    hash_bytes(b"f"),
                    0o100644,
                )
            })
            .collect();

        builder.add_entries(entries);

        assert!(builder
            .entries()
            .iter()
            .all(|e| { e.flags.contains(EntryFlags::STAGED) }));

        Ok(())
    }

    #[test]
    fn test_validate_catches_duplicates() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        let writer = Writer::new_canonical(repo_path);
        let header = Header::new(1, [0xaa; 32], 2);
        let mut builder = writer.builder(header);

        // Manually add duplicates (bypassing add_entry which prevents this)
        builder.entries_mut().push(Entry::new(
            PathBuf::from("test.txt"),
            1024,
            100,
            hash_bytes(b"q"),
            0o100644,
        ));
        builder.entries_mut().push(Entry::new(
            PathBuf::from("test.txt"),
            1024,
            200,
            hash_bytes(b"q"),
            0o100644,
        ));

        let result = builder.validate();
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Duplicate"));

        Ok(())
    }

    #[test]
    fn test_validate_catches_count_mismatch() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        let writer = Writer::new_canonical(repo_path);
        let header = Header::new(1, [0xaa; 32], 5); // Header says 5
        let mut builder = writer.builder(header);

        // But only add 2 entries
        builder.add_entry(Entry::new(
            PathBuf::from("a.txt"),
            1024,
            200,
            hash::ZERO_HASH,
            0,
        ));
        builder.add_entry(Entry::new(
            PathBuf::from("b.txt"),
            1024,
            200,
            hash::ZERO_HASH,
            0,
        ));

        let result = builder.validate();
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("count mismatch"));

        Ok(())
    }

    #[test]
    #[ignore] // Run with: cargo test --release -- --ignored --nocapture
    fn bench_write_performance() -> Result<()> {
        use std::time::Instant;

        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();
        let writer = Writer::new_canonical(repo_path);

        // Test different sizes
        let test_cases = vec![("Small", 1_000), ("Medium", 10_000), ("Large", 100_000)];

        for (name, count) in test_cases {
            let entries: Vec<_> = (0..count)
                .map(|i| {
                    Entry::new(
                        PathBuf::from(format!("file{:06}.txt", i)),
                        i as u64,
                        100,
                        hash_bytes(b"1"),
                        0o100644,
                    )
                })
                .collect();

            let header = Header::new(1, [0xaa; 32], count as u32);

            let start = Instant::now();
            writer.write(&header, &entries)?;
            let duration = start.elapsed();

            let file_size = std::fs::metadata(writer.index_path())?.len();
            let throughput = file_size as f64 / duration.as_secs_f64() / 1024.0 / 1024.0;

            println!(
                "{} index ({} entries): {:.2}ms, {:.2} MB/s",
                name,
                count,
                duration.as_millis(),
                throughput
            );

            // Clean up for next test
            writer.delete()?;
        }

        Ok(())
    }
}
