// /* Defines the methods and functions that create and update blob storage for hashed files
// Content addressed storage: same content = same hash = auto deduplication*/
// use anyhow::{Context, Result};
// use helix_protocol::hash::{hash_bytes, Hash};
// use rayon::prelude::*;
// use std::{
//     fs,
//     path::{Path, PathBuf},
// };

// /// Blob storage in .helix/objects/blobs/
// pub struct BlobStorage {
//     root: PathBuf,
// }

// impl BlobStorage {
//     /// Create new blob storage
//     pub fn new(root: PathBuf) -> Self {
//         Self { root }
//     }

//     /// Create blob storage for a repository
//     pub fn create_blob_storage(repo_path: &Path) -> Self {
//         let root = repo_path.join(".helix/objects/blobs");
//         Self::new(root)
//     }

//     /// Write blob content (working directory files) and return its hash
//     /// Atomic write (temp + rename)
//     pub fn write(&self, content: &[u8]) -> Result<Hash> {
//         let hash = hash_bytes(content);
//         let path = self.get_blob_path(&hash);

//         if path.exists() {
//             return Ok(hash);
//         }

//         // Ensure directory exists
//         fs::create_dir_all(&self.root).context("Failed to create blob storage directory")?;

//         // Compress with Zstd level 3
//         let compressed = zstd::encode_all(content, 3).context("Failed to compress blob")?;

//         let temp_path = path.with_extension("tmp");
//         fs::write(&temp_path, &compressed).context("Failed to write temporary blob")?;
//         fs::rename(&temp_path, &path).context("Failed to rename blob to final location")?;

//         Ok(hash)
//     }

//     /// Read blob content by hash
//     pub fn read(&self, hash: &Hash) -> Result<Vec<u8>> {
//         let path = self.get_blob_path(hash);

//         // Read compressed data
//         let compressed = fs::read(&path)
//             .with_context(|| format!("Failed to read blob {}", self.hash_to_filename(hash)))?;

//         // Decompress
//         let decompressed =
//             zstd::decode_all(&compressed[..]).context("Failed to decompress blob")?;

//         Ok(decompressed)
//     }

//     /// Check if blob exists
//     pub fn exists(&self, hash: &Hash) -> bool {
//         self.get_blob_path(hash).exists()
//     }

//     /// Delete blob
//     /// TODO: safe delete her to check that the blob isn't currently referenced
//     /// user can override with --f
//     pub fn delete(&self, hash: &Hash) -> Result<()> {
//         let path = self.get_blob_path(hash);
//         if path.exists() {
//             fs::remove_file(path).context("Failed to delete blob")?;
//         }
//         Ok(())
//     }

//     /// Get blob size (compressed)
//     pub fn get_size_compressed(&self, hash: &Hash) -> Result<u64> {
//         let path = self.get_blob_path(hash);
//         let metadata = fs::metadata(path).context("Failed to get blob metadata")?;
//         Ok(metadata.len())
//     }

//     /// Get blob path
//     fn get_blob_path(&self, hash: &Hash) -> PathBuf {
//         self.root.join(self.hash_to_filename(hash))
//     }

//     /// Convert hash to filename
//     fn hash_to_filename(&self, hash: &Hash) -> String {
//         hex::encode(hash)
//     }

//     /// List all blobs (for gc, debugging)
//     pub fn list_all(&self) -> Result<Vec<Hash>> {
//         if !self.root.exists() {
//             return Ok(vec![]);
//         }

//         let mut hashes = Vec::new();

//         for entry in fs::read_dir(&self.root)? {
//             let entry = entry?;
//             if entry.file_type()?.is_file() {
//                 let filename = entry.file_name();
//                 let filename_str = filename.to_string_lossy();

//                 // Skip temp files
//                 if filename_str.ends_with(".tmp") {
//                     continue;
//                 }

//                 // Parse hash from filename
//                 if let Ok(bytes) = hex::decode(filename_str.as_ref()) {
//                     if bytes.len() == 32 {
//                         let mut hash = [0u8; 32];
//                         hash.copy_from_slice(&bytes);
//                         hashes.push(hash);
//                     }
//                 }
//             }
//         }

//         Ok(hashes)
//     }

//     /// Clean up temporary files (orphaned from crashes)
//     pub fn cleanup_temp_files(&self) -> Result<usize> {
//         if !self.root.exists() {
//             return Ok(0);
//         }

//         let mut cleaned = 0;

//         for entry in fs::read_dir(&self.root)? {
//             let entry = entry?;
//             let path = entry.path();

//             if path.extension().and_then(|s| s.to_str()) == Some("tmp") {
//                 fs::remove_file(path)?;
//                 cleaned += 1;
//             }
//         }

//         Ok(cleaned)
//     }
// }

// /// Batch blob operations for performance
// pub struct BlobBatch<'a> {
//     storage: &'a BlobStorage,
// }

// impl<'a> BlobBatch<'a> {
//     pub fn new(storage: &'a BlobStorage) -> Self {
//         Self { storage }
//     }

//     /// Write multiple blobs in parallel (10x faster for many files)
//     pub fn write_all(&self, contents: &[Vec<u8>]) -> Result<Vec<Hash>> {
//         fs::create_dir_all(&self.storage.root)
//             .context("Failed to create blob storage directory")?;

//         // Process in parallel
//         contents
//             .par_iter()
//             .map(|content| self.storage.write(content))
//             .collect()
//     }

//     /// Read multiple blobs in parallel
//     pub fn read_all(&self, hashes: &[Hash]) -> Result<Vec<Vec<u8>>> {
//         hashes
//             .par_iter()
//             .map(|hash| self.storage.read(hash))
//             .collect()
//     }

//     /// Check existence of multiple blobs in parallel
//     pub fn exists_all(&self, hashes: &[Hash]) -> Vec<bool> {
//         hashes
//             .par_iter()
//             .map(|hash| self.storage.exists(hash))
//             .collect()
//     }
// }
