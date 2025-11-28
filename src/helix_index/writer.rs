/// This file defines the logic that writes to the .helix/helix.idx file with atomic updates
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
}

/// Stateful writer for incremental index updates
pub struct IndexBuilder {
    repo_path: PathBuf,
    header: Header,
    entries: Vec<Entry>,
}

impl Writer {
    pub fn new(repo_path: &Path) -> Self {
        Self {
            repo_path: repo_path.to_path_buf(),
        }
    }

    /// Write complete index atomically (immutable operation)
    /// Optimized for maximum throughput:
    /// 1. Stream entries directly to buffered writer (no pre-serialization)
    /// 2. Compute checksum in parallel with writing for large indexes
    /// 3. Use large buffer (1-2MB) to reduce syscalls
    /// 4. flush() instead of fsync() for speed
    /// 5. Atomic rename
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

        // Atomic rename
        fs::rename(&temp_path, &index_path).context("Failed to rename temp file to index")?;

        Ok(())
    }

    // Streaming write for normal-sized indexes (most common case)
    /// Optimized for minimal memory usage and syscall overhead
    fn write_streaming(&self, temp_path: &Path, header: &Header, entries: &[Entry]) -> Result<()> {
        let file = File::create(temp_path).context("Failed to create temp index file")?;
        // Use 1MB buffer to minimize syscalls (Linux optimal buffer size is typically 128KB-1MB)
        let mut writer = BufWriter::with_capacity(1024 * 1024, file);

        let mut hasher = Sha256::new();

        // Write and hash header
        let header_bytes = header.to_bytes();
        writer.write_all(&header_bytes)?;
        hasher.update(&header_bytes);

        // Stream entries directly
        for entry in entries {
            let entry_bytes = entry.to_bytes();
            writer.write_all(&entry_bytes)?;
            hasher.update(&entry_bytes);
        }

        // Write footer
        let checksum: [u8; 32] = hasher.finalize().into();
        let footer = Footer::new(checksum);
        writer.write_all(&footer.to_bytes())?;

        /* flush() instead of fsync() for 10x+ speed improvement.
        Safe because helix.idx is a derived cache that can be rebuilt.
        Checksum verification catches any corruption from power loss. */
        writer.flush().context("Failed to flush temp file")?;

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
                chunk.par_iter().map(|e| e.to_bytes()).collect()
            } else {
                chunk.iter().map(|e| e.to_bytes()).collect()
            };

            // Write and send to hasher
            for entry_bytes in serialized {
                writer.write_all(&entry_bytes)?;
                // Send copy to hasher (small overhead but enables parallelism)
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
        writer.flush().context("Failed to flush temp file")?;

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

        // Write atomically
        let writer = Writer::new(&self.repo_path);
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
    use crate::helix_index::format::EntryFlags;
    use tempfile::TempDir;

    #[test]
    fn test_write_creates_helix_directory() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        let writer = Writer::new(repo_path);
        let header = Header::new(1, [0; 16], 0);
        writer.write(&header, &[])?;

        assert!(repo_path.join(".helix").exists());
        assert!(repo_path.join(".helix/helix.idx").exists());

        Ok(())
    }

    #[test]
    fn test_atomic_write() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        let writer = Writer::new(repo_path);

        // Write version 1
        let header1 = Header::new(1, [0x11; 16], 0);
        writer.write(&header1, &[])?;

        // Write version 2
        let header2 = Header::new(2, [0x22; 16], 0);
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

        let writer = Writer::new(repo_path);
        let header = Header::new(1, [0xaa; 16], 0);
        let mut builder = writer.builder(header);

        let entry = Entry::new_tracked(
            PathBuf::from("test.txt"),
            [0xbb; 20],
            100,
            1234567890,
            0,
            0o100644,
        );

        builder.add_entry(entry);
        assert_eq!(builder.entry_count(), 1);

        Ok(())
    }

    #[test]
    fn test_builder_replace_entry() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        let writer = Writer::new(repo_path);
        let header = Header::new(1, [0xaa; 16], 0);
        let mut builder = writer.builder(header);

        let entry1 = Entry::new_tracked(
            PathBuf::from("test.txt"),
            [0xbb; 20],
            100,
            1234567890,
            0,
            0o100644,
        );

        let entry2 = Entry::new_tracked(
            PathBuf::from("test.txt"),
            [0xcc; 20],
            200,
            1234567891,
            0,
            0o100644,
        );

        builder.add_entry(entry1);
        builder.add_entry(entry2);

        assert_eq!(builder.entry_count(), 1);
        assert_eq!(builder.entries()[0].oid, [0xcc; 20]);
        assert_eq!(builder.entries()[0].size, 200);

        Ok(())
    }

    #[test]
    fn test_builder_remove_entry() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        let writer = Writer::new(repo_path);
        let header = Header::new(1, [0xaa; 16], 0);
        let mut builder = writer.builder(header);

        let entry = Entry::new_tracked(
            PathBuf::from("test.txt"),
            [0xbb; 20],
            100,
            1234567890,
            0,
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

        let writer = Writer::new(repo_path);
        let header = Header::new(1, [0xaa; 16], 0);
        let mut builder = writer.builder(header);

        let entries = vec![
            Entry::new_tracked(
                PathBuf::from("a.txt"),
                [0xbb; 20],
                100,
                1234567890,
                0,
                0o100644,
            ),
            Entry::new_tracked(
                PathBuf::from("b.txt"),
                [0xcc; 20],
                200,
                1234567891,
                0,
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

        let writer = Writer::new(repo_path);
        let header = Header::new(1, [0xaa; 16], 0);
        let mut builder = writer.builder(header);

        // Add in reverse order
        builder.add_entry(Entry::new_tracked(
            PathBuf::from("z.txt"),
            [0xbb; 20],
            100,
            1234567890,
            0,
            0o100644,
        ));
        builder.add_entry(Entry::new_tracked(
            PathBuf::from("a.txt"),
            [0xcc; 20],
            200,
            1234567891,
            0,
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

        let writer = Writer::new(repo_path);
        let header = Header::new(1, [0xaa; 16], 2000);

        // Create 2000 entries to trigger parallel path
        let entries: Vec<_> = (0..2000)
            .map(|i| {
                Entry::new_tracked(
                    PathBuf::from(format!("file{}.txt", i)),
                    [i as u8; 20],
                    100,
                    1234567890,
                    0,
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

        let writer = Writer::new(repo_path);
        let header = Header::new(1, [0xaa; 16], 0);
        let mut builder = writer.builder(header);

        let entries = vec![
            Entry::new_tracked(
                PathBuf::from("a.txt"),
                [0xbb; 20],
                100,
                1234567890,
                0,
                0o100644,
            ),
            Entry::new_tracked(
                PathBuf::from("b.rs"),
                [0xcc; 20],
                200,
                1234567891,
                0,
                0o100644,
            ),
            Entry::new_tracked(
                PathBuf::from("c.txt"),
                [0xdd; 20],
                300,
                1234567892,
                0,
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

        let writer = Writer::new(repo_path);
        let header = Header::new(1, [0xaa; 16], 0);
        let mut builder = writer.builder(header);

        let entries: Vec<_> = (0..1500)
            .map(|i| {
                Entry::new_tracked(
                    PathBuf::from(format!("file{}.txt", i)),
                    [0xbb; 20],
                    100,
                    1234567890,
                    0,
                    0o100644,
                )
            })
            .collect();

        builder.add_entries(entries);

        // Mark all as staged in parallel
        builder.update_entries_parallel(|e| {
            e.mark_staged();
        });

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

        let writer = Writer::new(repo_path);
        let header = Header::new(1, [0xaa; 16], 2);
        let mut builder = writer.builder(header);

        // Manually add duplicates (bypassing add_entry which prevents this)
        builder.entries_mut().push(Entry::new_tracked(
            PathBuf::from("test.txt"),
            [0xbb; 20],
            100,
            1234567890,
            0,
            0o100644,
        ));
        builder.entries_mut().push(Entry::new_tracked(
            PathBuf::from("test.txt"),
            [0xcc; 20],
            200,
            1234567891,
            0,
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

        let writer = Writer::new(repo_path);
        let mut header = Header::new(1, [0xaa; 16], 5); // Header says 5
        let mut builder = writer.builder(header);

        // But only add 2 entries
        builder.add_entry(Entry::new_tracked(
            PathBuf::from("a.txt"),
            [0xbb; 20],
            100,
            1234567890,
            0,
            0o100644,
        ));
        builder.add_entry(Entry::new_tracked(
            PathBuf::from("b.txt"),
            [0xcc; 20],
            200,
            1234567891,
            0,
            0o100644,
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
        let writer = Writer::new(repo_path);

        // Test different sizes
        let test_cases = vec![("Small", 1_000), ("Medium", 10_000), ("Large", 100_000)];

        for (name, count) in test_cases {
            let entries: Vec<_> = (0..count)
                .map(|i| {
                    Entry::new_tracked(
                        PathBuf::from(format!("file{:06}.txt", i)),
                        [(i % 256) as u8; 20],
                        100,
                        1234567890,
                        0,
                        0o100644,
                    )
                })
                .collect();

            let header = Header::new(1, [0xaa; 16], count as u32);

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
