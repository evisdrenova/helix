/// Unified storage for Helix objects.
/// Invariants:
/// - ObjectId/Hash = BLAKE3(raw bytes)
/// - On-disk representation may be encoded (e.g. zstd), but API always reads/writes RAW bytes.
use anyhow::{Context, Result};
use std::fs;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::hash::{hash_bytes, Hash};
use crate::message::ObjectType;

#[derive(Clone)]
pub struct FsObjectStore {
    repo_root: PathBuf, // path to repo root (contains .helix/)
}

impl FsObjectStore {
    pub fn new(repo_root: impl AsRef<Path>) -> Self {
        let repo_root = repo_root.as_ref().to_path_buf();
        let base = repo_root.join(".helix").join("objects");
        // best-effort create
        let _ = fs::create_dir_all(base.join("commits"));
        let _ = fs::create_dir_all(base.join("trees"));
        let _ = fs::create_dir_all(base.join("blobs"));
        Self { repo_root }
    }

    fn obj_path(&self, ty: &ObjectType, hash: &Hash) -> PathBuf {
        let subdir = match ty {
            ObjectType::Blob => "blobs",
            ObjectType::Tree => "trees",
            ObjectType::Commit => "commits",
        };
        self.repo_root
            .join(".helix")
            .join("objects")
            .join(subdir)
            .join(hex::encode(hash))
    }

    pub fn has_object(&self, ty: &ObjectType, hash: &Hash) -> bool {
        self.obj_path(ty, hash).exists()
    }

    /// Canonical API: write RAW bytes, compute hash, store encoded appropriately.
    pub fn write_object(&self, ty: &ObjectType, raw: &[u8]) -> Result<Hash> {
        let hash = hash_bytes(raw);
        self.write_object_with_hash(ty, &hash, raw)?;
        Ok(hash)
    }

    /// Write RAW bytes for a claimed hash. Validates hash == BLAKE3(raw).
    pub fn write_object_with_hash(&self, ty: &ObjectType, hash: &Hash, raw: &[u8]) -> Result<()> {
        let computed = hash_bytes(raw);
        anyhow::ensure!(
            &computed == hash,
            "object hash mismatch: ty={:?} claimed={} computed={}",
            ty,
            hex::encode(hash),
            hex::encode(computed),
        );

        let path = self.obj_path(ty, hash);
        if path.exists() {
            return Ok(());
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        // Encode for storage
        let on_disk: Vec<u8> = match ty {
            ObjectType::Blob => zstd::encode_all(raw, 3).context("Failed to compress blob")?,
            ObjectType::Tree | ObjectType::Commit => {
                // TODO: this is just raw bytes, we should compress here
                raw.to_vec()
            }
        };

        atomic_write(&path, &on_disk)
            .with_context(|| format!("write object ty={:?} {}", ty, hex::encode(hash)))?;
        Ok(())
    }

    /// Read RAW bytes; decodes/decompresses based on object type.
    pub fn read_object(&self, ty: &ObjectType, hash: &Hash) -> Result<Vec<u8>> {
        let path = self.obj_path(ty, hash);
        let data = fs::read(&path).with_context(|| format!("read {}", path.display()))?;

        let raw = match ty {
            ObjectType::Blob => zstd::decode_all(&data[..]).context("Failed to decompress blob")?,
            ObjectType::Tree | ObjectType::Commit => data,
        };

        // verify integrity at read time too.
        let computed = hash_bytes(&raw);
        anyhow::ensure!(
            &computed == hash,
            "corrupt object on disk: ty={:?} expected={} got={}",
            ty,
            hex::encode(hash),
            hex::encode(computed),
        );

        Ok(raw)
    }

    pub fn list_object_hashes(&self, ty: &ObjectType) -> Result<Vec<Hash>> {
        let dir = self
            .repo_root
            .join(".helix")
            .join("objects")
            .join(match ty {
                ObjectType::Blob => "blobs",
                ObjectType::Tree => "trees",
                ObjectType::Commit => "commits",
            });

        if !dir.exists() {
            return Ok(vec![]);
        }

        let mut out = Vec::new();
        for entry in fs::read_dir(&dir).with_context(|| format!("read_dir {}", dir.display()))? {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            let path = entry.path();
            let name = match path.file_name().and_then(|s| s.to_str()) {
                Some(s) => s,
                None => continue,
            };

            // skip temp / junk
            if name.ends_with(".tmp") || name.starts_with('.') || name.len() != 64 {
                continue;
            }

            let bytes = match hex::decode(name) {
                Ok(b) if b.len() == 32 => b,
                _ => continue,
            };

            let mut h = [0u8; 32];
            h.copy_from_slice(&bytes);
            out.push(h);
        }

        Ok(out)
    }
}

fn atomic_write(final_path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp_path = tmp_path_for(final_path);

    let mut f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp_path)
        .with_context(|| format!("open temp object file {:?}", tmp_path))?;

    f.write_all(bytes)
        .with_context(|| format!("write temp object file {:?}", tmp_path))?;

    f.sync_all()
        .with_context(|| format!("sync temp object file {:?}", tmp_path))?;

    drop(f);

    fs::rename(&tmp_path, final_path)
        .with_context(|| format!("rename {:?} -> {:?}", tmp_path, final_path))?;

    Ok(())
}

fn tmp_path_for(final_path: &Path) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let file_name = final_path.file_name().unwrap_or_default().to_string_lossy();
    final_path.with_file_name(format!(".{}.tmp.{}", file_name, nanos))
}

#[derive(Clone)]
pub struct FsRefStore {
    root: PathBuf,
}

impl FsRefStore {
    pub fn new(root: impl AsRef<Path>) -> Self {
        Self {
            root: root.as_ref().to_path_buf(),
        }
    }

    fn ref_path(&self, name: &str) -> PathBuf {
        self.root.join(".helix").join("refs").join(name)
    }

    pub fn get_ref(&self, name: &str) -> Result<Option<Hash>> {
        let path = self.ref_path(name);
        if !path.exists() {
            return Ok(None);
        }
        let s = fs::read_to_string(path)?;
        let s = s.trim();
        let bytes = hex::decode(s)?;
        let mut hash = [0u8; 32];
        hash.copy_from_slice(&bytes);
        Ok(Some(hash))
    }

    pub fn set_ref(&self, name: &str, new: Hash) -> Result<()> {
        let path = self.ref_path(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, hex::encode(new))?;
        Ok(())
    }
}

// #[cfg(test)]
// mod tests {
//     use super::*;
//     use tempfile::TempDir;

//     fn setup_storage() -> (TempDir, BlobStorage) {
//         let temp_dir = TempDir::new().unwrap();
//         let storage = BlobStorage::create_blob_storage(temp_dir.path());
//         (temp_dir, storage)
//     }

//     #[test]
//     fn test_write_and_read_blob() -> Result<()> {
//         let (_temp, storage) = setup_storage();

//         let content = b"hello world";
//         let hash = storage.write(content)?;

//         let read_content = storage.read(&hash)?;
//         assert_eq!(content, read_content.as_slice());

//         Ok(())
//     }

//     #[test]
//     fn test_blob_deduplication() -> Result<()> {
//         let (_temp, storage) = setup_storage();

//         let content = b"duplicate content";

//         // Write same content twice
//         let hash1 = storage.write(content)?;
//         let hash2 = storage.write(content)?;

//         // Should have same hash
//         assert_eq!(hash1, hash2);

//         // Should only exist once on disk
//         assert!(storage.exists(&hash1));

//         Ok(())
//     }

//     #[test]
//     fn test_different_content_different_hash() -> Result<()> {
//         let (_temp, storage) = setup_storage();

//         let hash1 = storage.write(b"content 1")?;
//         let hash2 = storage.write(b"content 2")?;

//         assert_ne!(hash1, hash2);

//         Ok(())
//     }

//     #[test]
//     fn test_blob_exists() -> Result<()> {
//         let (_temp, storage) = setup_storage();

//         let content = b"test content";
//         let hash = storage.write(content)?;

//         assert!(storage.exists(&hash));

//         // Non-existent blob
//         let fake_hash = [0u8; 32];
//         assert!(!storage.exists(&fake_hash));

//         Ok(())
//     }

//     #[test]
//     fn test_empty_content() -> Result<()> {
//         let (_temp, storage) = setup_storage();

//         let hash = storage.write(b"")?;
//         let read = storage.read(&hash)?;

//         assert_eq!(read.len(), 0);

//         Ok(())
//     }

//     #[test]
//     fn test_large_content() -> Result<()> {
//         let (_temp, storage) = setup_storage();

//         // 1MB of data
//         let content = vec![0xAB; 1024 * 1024];

//         let hash = storage.write(&content)?;
//         let read = storage.read(&hash)?;

//         assert_eq!(content, read);

//         Ok(())
//     }

//     #[test]
//     fn test_compression_saves_space() -> Result<()> {
//         let (_temp, storage) = setup_storage();

//         // Highly compressible data (repeated pattern)
//         let content = vec![0x00; 10 * 1024]; // 10KB of zeros

//         let hash = storage.write(&content)?;
//         let compressed_size = storage.get_size_compressed(&hash)?;

//         // Should compress significantly (expect <100 bytes for 10KB of zeros)
//         assert!(
//             compressed_size < 200,
//             "Compressed size {} should be much smaller than 10KB",
//             compressed_size
//         );

//         // Verify we can still read it back
//         let read = storage.read(&hash)?;
//         assert_eq!(content, read);

//         Ok(())
//     }

//     #[test]
//     fn test_atomic_write() -> Result<()> {
//         let (_temp, storage) = setup_storage();

//         let content = b"atomic test";
//         let hash = storage.write(content)?;

//         // Temp file should not exist after successful write
//         let temp_path = storage.get_blob_path(&hash).with_extension("tmp");
//         assert!(!temp_path.exists());

//         // Final file should exist
//         assert!(storage.get_blob_path(&hash).exists());

//         Ok(())
//     }

//     #[test]
//     fn test_delete_blob() -> Result<()> {
//         let (_temp, storage) = setup_storage();

//         let content = b"to be deleted";
//         let hash = storage.write(content)?;

//         assert!(storage.exists(&hash));

//         storage.delete(&hash)?;

//         assert!(!storage.exists(&hash));

//         Ok(())
//     }

//     #[test]
//     fn test_list_all_blobs() -> Result<()> {
//         let (_temp, storage) = setup_storage();

//         // Write multiple blobs
//         let hash1 = storage.write(b"blob 1")?;
//         let hash2 = storage.write(b"blob 2")?;
//         let hash3 = storage.write(b"blob 3")?;

//         let all_hashes = storage.list_all()?;

//         assert_eq!(all_hashes.len(), 3);
//         assert!(all_hashes.contains(&hash1));
//         assert!(all_hashes.contains(&hash2));
//         assert!(all_hashes.contains(&hash3));

//         Ok(())
//     }

//     #[test]
//     fn test_cleanup_temp_files() -> Result<()> {
//         let (_temp, storage) = setup_storage();

//         // Create directory
//         fs::create_dir_all(&storage.root)?;

//         // Manually create temp files (simulating crashed writes)
//         fs::write(storage.root.join("orphaned1.tmp"), b"temp1")?;
//         fs::write(storage.root.join("orphaned2.tmp"), b"temp2")?;

//         // Write a real blob
//         storage.write(b"real content")?;

//         let cleaned = storage.cleanup_temp_files()?;

//         assert_eq!(cleaned, 2);
//         assert!(!storage.root.join("orphaned1.tmp").exists());
//         assert!(!storage.root.join("orphaned2.tmp").exists());

//         Ok(())
//     }

//     #[test]
//     fn test_batch_write() -> Result<()> {
//         let (_temp, storage) = setup_storage();

//         let contents = vec![
//             b"content 1".to_vec(),
//             b"content 2".to_vec(),
//             b"content 3".to_vec(),
//         ];

//         let batch = BlobBatch::new(&storage);
//         let hashes = batch.write_all(&contents)?;

//         assert_eq!(hashes.len(), 3);

//         // Verify all written
//         for hash in &hashes {
//             assert!(storage.exists(hash));
//         }

//         Ok(())
//     }

//     #[test]
//     fn test_batch_read() -> Result<()> {
//         let (_temp, storage) = setup_storage();

//         // Write blobs
//         let hash1 = storage.write(b"content 1")?;
//         let hash2 = storage.write(b"content 2")?;
//         let hash3 = storage.write(b"content 3")?;

//         // Batch read
//         let batch = BlobBatch::new(&storage);
//         let contents = batch.read_all(&[hash1, hash2, hash3])?;

//         assert_eq!(contents.len(), 3);
//         assert_eq!(contents[0], b"content 1");
//         assert_eq!(contents[1], b"content 2");
//         assert_eq!(contents[2], b"content 3");

//         Ok(())
//     }

//     #[test]
//     fn test_batch_exists() -> Result<()> {
//         let (_temp, storage) = setup_storage();

//         let hash1 = storage.write(b"exists")?;
//         let hash2 = [0u8; 32]; // Doesn't exist
//         let hash3 = storage.write(b"also exists")?;

//         let batch = BlobBatch::new(&storage);
//         let exists = batch.exists_all(&[hash1, hash2, hash3]);

//         assert_eq!(exists, vec![true, false, true]);

//         Ok(())
//     }

//     #[test]
//     fn test_batch_write_performance() -> Result<()> {
//         let (_temp, storage) = setup_storage();

//         // Create 100 blobs
//         let contents: Vec<Vec<u8>> = (0..100)
//             .map(|i| format!("content {}", i).into_bytes())
//             .collect();

//         let start = std::time::Instant::now();
//         let batch = BlobBatch::new(&storage);
//         let hashes = batch.write_all(&contents)?;
//         let elapsed = start.elapsed();

//         assert_eq!(hashes.len(), 100);
//         println!("Batch wrote 100 blobs in {:?}", elapsed);

//         // Should be fast (expect <100ms)
//         assert!(
//             elapsed.as_millis() < 200,
//             "Batch write took {}ms, expected <200ms",
//             elapsed.as_millis()
//         );

//         Ok(())
//     }

//     #[test]
//     fn test_concurrent_writes_same_content() -> Result<()> {
//         let (_temp, storage) = setup_storage();

//         let content = b"concurrent content";

//         // Write same content multiple times in parallel
//         let hashes: Vec<_> = (0..10)
//             .into_par_iter()
//             .map(|_| storage.write(content))
//             .collect::<Result<Vec<_>>>()?;

//         // All hashes should be identical
//         assert!(hashes.windows(2).all(|w| w[0] == w[1]));

//         // Only one blob should exist on disk
//         assert_eq!(storage.list_all()?.len(), 1);

//         Ok(())
//     }

//     #[test]
//     fn test_binary_content() -> Result<()> {
//         let (_temp, storage) = setup_storage();

//         // Binary data with all byte values
//         let content: Vec<u8> = (0..=255).collect();

//         let hash = storage.write(&content)?;
//         let read = storage.read(&hash)?;

//         assert_eq!(content, read);

//         Ok(())
//     }

//     #[test]
//     fn test_unicode_content() -> Result<()> {
//         let (_temp, storage) = setup_storage();

//         let content = "Hello 世界 🚀 Привет".as_bytes();

//         let hash = storage.write(content)?;
//         let read = storage.read(&hash)?;

//         assert_eq!(content, read.as_slice());

//         Ok(())
//     }
// }
