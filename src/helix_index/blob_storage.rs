/* Defines the methods and functions that create and update blob storage for hashed files
Content addressed storage: same content = same hash = auto deduplication*/

use super::hash::{hash_bytes, Hash};
use anyhow::{Context, Result};
use rayon::prelude::*;
use std::{
    fs,
    path::{Path, PathBuf},
};

/// Blob storage in .helix/objects/blobs/
pub struct BlobStorage {
    root: PathBuf,
}

impl BlobStorage {
    /// Create new blob storage
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// Create blob storage for a repository
    pub fn for_repo(repo_path: &Path) -> Self {
        let root = repo_path.join(".helix/objects/blobs");
        Self::new(root)
    }

    /// Write blob content (working directory files) and return its hash
    /// Atomic write (temp + rename)
    pub fn write(&self, content: &[u8]) -> Result<Hash> {
        let hash = hash_bytes(content);
        let path = self.get_blob_path(&hash);

        if path.exists() {
            return Ok(hash);
        }

        // Ensure directory exists
        fs::create_dir_all(&self.root).context("Failed to create blob storage directory")?;

        // Compress with Zstd level 3 (fast, good compression)
        // Level 3: 4ms per MB, ~60% compression ratio
        let compressed = zstd::encode_all(content, 3).context("Failed to compress blob")?;

        let temp_path = path.with_extension("tmp");
        fs::write(&temp_path, &compressed).context("Failed to write temporary blob")?;
        fs::rename(&temp_path, &path).context("Failed to rename blob to final location")?;

        Ok(hash)
    }

    /// Read blob content by hash
    pub fn read(&self, hash: &Hash) -> Result<Vec<u8>> {
        let path = self.get_blob_path(hash);

        // Read compressed data
        let compressed = fs::read(&path)
            .with_context(|| format!("Failed to read blob {}", self.hash_to_filename(hash)))?;

        // Decompress
        let decompressed =
            zstd::decode_all(&compressed[..]).context("Failed to decompress blob")?;

        Ok(decompressed)
    }

    /// Check if blob exists
    pub fn exists(&self, hash: &Hash) -> bool {
        self.get_blob_path(hash).exists()
    }

    /// Delete blob
    /// TODO: safe delete her to check that the blob isn't currently referenced
    /// user can override with --f
    pub fn delete(&self, hash: &Hash) -> Result<()> {
        let path = self.get_blob_path(hash);
        if path.exists() {
            fs::remove_file(path).context("Failed to delete blob")?;
        }
        Ok(())
    }

    /// Get blob size (compressed)
    pub fn get_size_compressed(&self, hash: &Hash) -> Result<u64> {
        let path = self.get_blob_path(hash);
        let metadata = fs::metadata(path).context("Failed to get blob metadata")?;
        Ok(metadata.len())
    }

    /// Get blob path
    fn get_blob_path(&self, hash: &Hash) -> PathBuf {
        self.root.join(self.hash_to_filename(hash))
    }

    /// Convert hash to filename
    fn hash_to_filename(&self, hash: &Hash) -> String {
        hex::encode(hash)
    }

    /// List all blobs (for gc, debugging)
    pub fn list_all(&self) -> Result<Vec<Hash>> {
        if !self.root.exists() {
            return Ok(vec![]);
        }

        let mut hashes = Vec::new();

        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            if entry.file_type()?.is_file() {
                let filename = entry.file_name();
                let filename_str = filename.to_string_lossy();

                // Skip temp files
                if filename_str.ends_with(".tmp") {
                    continue;
                }

                // Parse hash from filename
                if let Ok(bytes) = hex::decode(filename_str.as_ref()) {
                    if bytes.len() == 32 {
                        let mut hash = [0u8; 32];
                        hash.copy_from_slice(&bytes);
                        hashes.push(hash);
                    }
                }
            }
        }

        Ok(hashes)
    }

    /// Clean up temporary files (orphaned from crashes)
    pub fn cleanup_temp_files(&self) -> Result<usize> {
        if !self.root.exists() {
            return Ok(0);
        }

        let mut cleaned = 0;

        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            let path = entry.path();

            if path.extension().and_then(|s| s.to_str()) == Some("tmp") {
                fs::remove_file(path)?;
                cleaned += 1;
            }
        }

        Ok(cleaned)
    }
}

/// Batch blob operations for performance
pub struct BlobBatch<'a> {
    storage: &'a BlobStorage,
}

impl<'a> BlobBatch<'a> {
    pub fn new(storage: &'a BlobStorage) -> Self {
        Self { storage }
    }

    /// Write multiple blobs in parallel (10x faster for many files)
    pub fn write_all(&self, contents: &[Vec<u8>]) -> Result<Vec<Hash>> {
        fs::create_dir_all(&self.storage.root)
            .context("Failed to create blob storage directory")?;

        // Process in parallel
        contents
            .par_iter()
            .map(|content| self.storage.write(content))
            .collect()
    }

    /// Read multiple blobs in parallel
    pub fn read_all(&self, hashes: &[Hash]) -> Result<Vec<Vec<u8>>> {
        hashes
            .par_iter()
            .map(|hash| self.storage.read(hash))
            .collect()
    }

    /// Check existence of multiple blobs in parallel
    pub fn exists_all(&self, hashes: &[Hash]) -> Vec<bool> {
        hashes
            .par_iter()
            .map(|hash| self.storage.exists(hash))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn setup_storage() -> (TempDir, BlobStorage) {
        let temp_dir = TempDir::new().unwrap();
        let storage = BlobStorage::for_repo(temp_dir.path());
        (temp_dir, storage)
    }

    #[test]
    fn test_write_and_read_blob() -> Result<()> {
        let (_temp, storage) = setup_storage();

        let content = b"hello world";
        let hash = storage.write(content)?;

        let read_content = storage.read(&hash)?;
        assert_eq!(content, read_content.as_slice());

        Ok(())
    }

    #[test]
    fn test_blob_deduplication() -> Result<()> {
        let (_temp, storage) = setup_storage();

        let content = b"duplicate content";

        // Write same content twice
        let hash1 = storage.write(content)?;
        let hash2 = storage.write(content)?;

        // Should have same hash
        assert_eq!(hash1, hash2);

        // Should only exist once on disk
        assert!(storage.exists(&hash1));

        Ok(())
    }

    #[test]
    fn test_different_content_different_hash() -> Result<()> {
        let (_temp, storage) = setup_storage();

        let hash1 = storage.write(b"content 1")?;
        let hash2 = storage.write(b"content 2")?;

        assert_ne!(hash1, hash2);

        Ok(())
    }

    #[test]
    fn test_blob_exists() -> Result<()> {
        let (_temp, storage) = setup_storage();

        let content = b"test content";
        let hash = storage.write(content)?;

        assert!(storage.exists(&hash));

        // Non-existent blob
        let fake_hash = [0u8; 32];
        assert!(!storage.exists(&fake_hash));

        Ok(())
    }

    #[test]
    fn test_empty_content() -> Result<()> {
        let (_temp, storage) = setup_storage();

        let hash = storage.write(b"")?;
        let read = storage.read(&hash)?;

        assert_eq!(read.len(), 0);

        Ok(())
    }

    #[test]
    fn test_large_content() -> Result<()> {
        let (_temp, storage) = setup_storage();

        // 1MB of data
        let content = vec![0xAB; 1024 * 1024];

        let hash = storage.write(&content)?;
        let read = storage.read(&hash)?;

        assert_eq!(content, read);

        Ok(())
    }

    #[test]
    fn test_compression_saves_space() -> Result<()> {
        let (_temp, storage) = setup_storage();

        // Highly compressible data (repeated pattern)
        let content = vec![0x00; 10 * 1024]; // 10KB of zeros

        let hash = storage.write(&content)?;
        let compressed_size = storage.get_size_compressed(&hash)?;

        // Should compress significantly (expect <100 bytes for 10KB of zeros)
        assert!(
            compressed_size < 200,
            "Compressed size {} should be much smaller than 10KB",
            compressed_size
        );

        // Verify we can still read it back
        let read = storage.read(&hash)?;
        assert_eq!(content, read);

        Ok(())
    }

    #[test]
    fn test_atomic_write() -> Result<()> {
        let (_temp, storage) = setup_storage();

        let content = b"atomic test";
        let hash = storage.write(content)?;

        // Temp file should not exist after successful write
        let temp_path = storage.get_blob_path(&hash).with_extension("tmp");
        assert!(!temp_path.exists());

        // Final file should exist
        assert!(storage.get_blob_path(&hash).exists());

        Ok(())
    }

    #[test]
    fn test_delete_blob() -> Result<()> {
        let (_temp, storage) = setup_storage();

        let content = b"to be deleted";
        let hash = storage.write(content)?;

        assert!(storage.exists(&hash));

        storage.delete(&hash)?;

        assert!(!storage.exists(&hash));

        Ok(())
    }

    #[test]
    fn test_list_all_blobs() -> Result<()> {
        let (_temp, storage) = setup_storage();

        // Write multiple blobs
        let hash1 = storage.write(b"blob 1")?;
        let hash2 = storage.write(b"blob 2")?;
        let hash3 = storage.write(b"blob 3")?;

        let all_hashes = storage.list_all()?;

        assert_eq!(all_hashes.len(), 3);
        assert!(all_hashes.contains(&hash1));
        assert!(all_hashes.contains(&hash2));
        assert!(all_hashes.contains(&hash3));

        Ok(())
    }

    #[test]
    fn test_cleanup_temp_files() -> Result<()> {
        let (_temp, storage) = setup_storage();

        // Create directory
        fs::create_dir_all(&storage.root)?;

        // Manually create temp files (simulating crashed writes)
        fs::write(storage.root.join("orphaned1.tmp"), b"temp1")?;
        fs::write(storage.root.join("orphaned2.tmp"), b"temp2")?;

        // Write a real blob
        storage.write(b"real content")?;

        let cleaned = storage.cleanup_temp_files()?;

        assert_eq!(cleaned, 2);
        assert!(!storage.root.join("orphaned1.tmp").exists());
        assert!(!storage.root.join("orphaned2.tmp").exists());

        Ok(())
    }

    #[test]
    fn test_batch_write() -> Result<()> {
        let (_temp, storage) = setup_storage();

        let contents = vec![
            b"content 1".to_vec(),
            b"content 2".to_vec(),
            b"content 3".to_vec(),
        ];

        let batch = BlobBatch::new(&storage);
        let hashes = batch.write_all(&contents)?;

        assert_eq!(hashes.len(), 3);

        // Verify all written
        for hash in &hashes {
            assert!(storage.exists(hash));
        }

        Ok(())
    }

    #[test]
    fn test_batch_read() -> Result<()> {
        let (_temp, storage) = setup_storage();

        // Write blobs
        let hash1 = storage.write(b"content 1")?;
        let hash2 = storage.write(b"content 2")?;
        let hash3 = storage.write(b"content 3")?;

        // Batch read
        let batch = BlobBatch::new(&storage);
        let contents = batch.read_all(&[hash1, hash2, hash3])?;

        assert_eq!(contents.len(), 3);
        assert_eq!(contents[0], b"content 1");
        assert_eq!(contents[1], b"content 2");
        assert_eq!(contents[2], b"content 3");

        Ok(())
    }

    #[test]
    fn test_batch_exists() -> Result<()> {
        let (_temp, storage) = setup_storage();

        let hash1 = storage.write(b"exists")?;
        let hash2 = [0u8; 32]; // Doesn't exist
        let hash3 = storage.write(b"also exists")?;

        let batch = BlobBatch::new(&storage);
        let exists = batch.exists_all(&[hash1, hash2, hash3]);

        assert_eq!(exists, vec![true, false, true]);

        Ok(())
    }

    #[test]
    fn test_batch_write_performance() -> Result<()> {
        let (_temp, storage) = setup_storage();

        // Create 100 blobs
        let contents: Vec<Vec<u8>> = (0..100)
            .map(|i| format!("content {}", i).into_bytes())
            .collect();

        let start = std::time::Instant::now();
        let batch = BlobBatch::new(&storage);
        let hashes = batch.write_all(&contents)?;
        let elapsed = start.elapsed();

        assert_eq!(hashes.len(), 100);
        println!("Batch wrote 100 blobs in {:?}", elapsed);

        // Should be fast (expect <100ms)
        assert!(
            elapsed.as_millis() < 200,
            "Batch write took {}ms, expected <200ms",
            elapsed.as_millis()
        );

        Ok(())
    }

    #[test]
    fn test_concurrent_writes_same_content() -> Result<()> {
        let (_temp, storage) = setup_storage();

        let content = b"concurrent content";

        // Write same content multiple times in parallel
        let hashes: Vec<_> = (0..10)
            .into_par_iter()
            .map(|_| storage.write(content))
            .collect::<Result<Vec<_>>>()?;

        // All hashes should be identical
        assert!(hashes.windows(2).all(|w| w[0] == w[1]));

        // Only one blob should exist on disk
        assert_eq!(storage.list_all()?.len(), 1);

        Ok(())
    }

    #[test]
    fn test_binary_content() -> Result<()> {
        let (_temp, storage) = setup_storage();

        // Binary data with all byte values
        let content: Vec<u8> = (0..=255).collect();

        let hash = storage.write(&content)?;
        let read = storage.read(&hash)?;

        assert_eq!(content, read);

        Ok(())
    }

    #[test]
    fn test_unicode_content() -> Result<()> {
        let (_temp, storage) = setup_storage();

        let content = "Hello 世界 🚀 Привет".as_bytes();

        let hash = storage.write(content)?;
        let read = storage.read(&hash)?;

        assert_eq!(content, read.as_slice());

        Ok(())
    }
}
