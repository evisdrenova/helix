/// Unified storage for Helix objects.
/// Invariants:
/// - ObjectId/Hash = BLAKE3(raw bytes)
/// - On-disk representation may be encoded (e.g. zstd), but API always reads/writes RAW bytes.
use anyhow::{Context, Result};
use rayon::iter::*;
use std::fs;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::hash::{hash_bytes, Hash};
use crate::message::ObjectType;

#[derive(Clone, Debug)]
pub struct FsObjectStore {
    repo_root: PathBuf,
}

impl FsObjectStore {
    /// Creates a new ObjectStore for Commits, Blobs and Trees.
    pub fn new(repo_root: impl AsRef<Path>) -> Self {
        let repo_root = repo_root.as_ref().to_path_buf();
        let base = repo_root.join(".helix").join("objects");
        // best-effort create
        let _ = fs::create_dir_all(base.join("commits"));
        let _ = fs::create_dir_all(base.join("trees"));
        let _ = fs::create_dir_all(base.join("blobs"));
        Self { repo_root }
    }
    /// Given an ObjectType, returns the directory path where those objects are stored.
    fn get_obj_path(&self, ty: &ObjectType, hash: &Hash) -> PathBuf {
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

    /// Checks if a hash exists given an ObjectType. For example, given a Blob Hash, checks if the Hash exists within the .helix/objects/blobs/{} path.
    pub fn has_object(&self, ty: &ObjectType, hash: &Hash) -> bool {
        self.get_obj_path(ty, hash).exists()
    }

    /// Writes object bytes to disk and returns Hash of bytes.
    pub fn write_object(&self, ty: &ObjectType, raw: &[u8]) -> Result<Hash> {
        let hash = hash_bytes(raw);
        self.write_object_with_hash(ty, &hash, raw)?;
        Ok(hash)
    }

    /// Write RAW bytes for a claimed hash. Validates hash == Hash(raw).
    pub fn write_object_with_hash(&self, ty: &ObjectType, hash: &Hash, raw: &[u8]) -> Result<()> {
        let computed = hash_bytes(raw);
        anyhow::ensure!(
            &computed == hash,
            "object hash mismatch: ty={:?} claimed={} computed={}",
            ty,
            hex::encode(hash),
            hex::encode(computed),
        );

        let path = self.get_obj_path(ty, hash);
        if path.exists() {
            return Ok(());
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        // Encode for storage
        let on_disk = zstd::encode_all(raw, 3).context("Failed to compress object")?;

        atomic_write(&path, &on_disk)
            .with_context(|| format!("write object ty={:?} {}", ty, hex::encode(hash)))?;
        Ok(())
    }

    /// Read objects from disk by decompressing objects to get raw bytes
    pub fn read_object(&self, ty: &ObjectType, hash: &Hash) -> Result<Vec<u8>> {
        let path = self.get_obj_path(ty, hash);
        let data = fs::read(&path).with_context(|| format!("read {}", path.display()))?;

        let raw = zstd::decode_all(&data[..]).context("Failed to decompress object")?;

        // verify integrity at read time too
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

    /// Read compressed bytes directly from disk. Does not decompress bytes. Mainly used for transfer between client and server.
    pub fn read_object_compressed(&self, ty: &ObjectType, hash: &Hash) -> Result<Vec<u8>> {
        let path = self.get_obj_path(ty, hash);
        fs::read(&path).with_context(|| format!("read compressed {}", path.display()))
    }

    /// Write pre-compressed  bytes directly to disk.
    /// Decompresses only to verify hash == BLAKE3(raw), then stores compressed bytes as-is.
    /// Used to write object to disk after receiving from network to avoid recompression.
    pub fn write_object_compressed_with_hash(
        &self,
        ty: &ObjectType,
        hash: &Hash,
        compressed: &[u8],
    ) -> Result<()> {
        // Decompress to verify hash
        let raw =
            zstd::decode_all(compressed).context("Failed to decompress for hash verification")?;

        let computed = hash_bytes(&raw);
        anyhow::ensure!(
            &computed == hash,
            "object hash mismatch: ty={:?} claimed={} computed={}",
            ty,
            hex::encode(hash),
            hex::encode(computed),
        );

        let path = self.get_obj_path(ty, hash);
        if path.exists() {
            return Ok(());
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        // Store compressed bytes directly
        atomic_write(&path, compressed)
            .with_context(|| format!("write object ty={:?} {}", ty, hex::encode(hash)))?;
        Ok(())
    }

    /// List all object hashes on disk for a given ObjectType (not-recursive)
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

            // skip temp
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

    /// Write objects to disk in batch mode. This uses rayon for parallelization.
    pub fn write_objects_batch(&self, ty: &ObjectType, objects: &[Vec<u8>]) -> Result<Vec<Hash>> {
        objects
            .par_iter()
            .map(|raw| self.write_object(ty, raw))
            .collect()
    }

    /// Read multiple objects in parallel
    pub fn read_objects_batch(&self, ty: &ObjectType, hashes: &[Hash]) -> Result<Vec<Vec<u8>>> {
        hashes
            .par_iter()
            .map(|hash| self.read_object(ty, hash))
            .collect()
    }

    /// Read multiple objects in parallel (returns compressed bytes for network transfer)
    pub fn read_objects_compressed_batch(
        &self,
        ty: &ObjectType,
        hashes: &[Hash],
    ) -> Result<Vec<Vec<u8>>> {
        hashes
            .par_iter()
            .map(|hash| self.read_object_compressed(ty, hash))
            .collect()
    }

    /// Write multiple pre-compressed objects in parallel
    pub fn write_objects_compressed_batch(
        &self,
        ty: &ObjectType,
        objects: &[(Hash, Vec<u8>)],
    ) -> Result<()> {
        objects.par_iter().try_for_each(|(hash, compressed)| {
            self.write_object_compressed_with_hash(ty, hash, compressed)
        })
    }

    /// Check existence of multiple objects in parallel
    pub fn has_objects_batch(&self, ty: &ObjectType, hashes: &[Hash]) -> Vec<bool> {
        hashes
            .par_iter()
            .map(|hash| self.has_object(ty, hash))
            .collect()
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
        self.root.join(".helix").join(name)
    }

    pub fn get_ref(&self, name: &str) -> Result<Option<Hash>> {
        let path = self.ref_path(name);
        if !path.exists() {
            return Ok(None);
        }
        let s = fs::read_to_string(&path)?;
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
        fs::write(&path, hex::encode(new))?;
        Ok(())
    }
}
