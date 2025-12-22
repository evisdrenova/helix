use anyhow::Context;
use anyhow::Result;
use helix_protocol::hash::Hash;
use helix_protocol::message::ObjectType;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
#[derive(Clone)]
pub struct FsObjectStore {
    root: PathBuf, // e.g. /var/lib/helix/repos/<repo>
}

impl FsObjectStore {
    pub fn new(root: &str) -> Self {
        let base = PathBuf::from(root).join(".helix").join("objects");
        std::fs::create_dir_all(base.join("commits")).ok();
        std::fs::create_dir_all(base.join("trees")).ok();
        std::fs::create_dir_all(base.join("blobs")).ok();

        Self {
            root: PathBuf::from(root),
        }
    }

    fn obj_path(&self, ty: &ObjectType, hash: &Hash) -> PathBuf {
        let subdir = match ty {
            ObjectType::Blob => "blobs",
            ObjectType::Tree => "trees",
            ObjectType::Commit => "commits",
        };
        let hex = hex::encode(hash);
        self.root
            .join(".helix")
            .join("objects")
            .join(subdir)
            .join(hex)
    }

    pub fn has_object(&self, ty: &ObjectType, hash: &Hash) -> bool {
        self.obj_path(ty, hash).exists()
    }

    pub fn write_object(&self, ty: &ObjectType, hash: &Hash, data: &[u8]) -> Result<()> {
        let path = self.obj_path(ty, hash);

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        if path.exists() {
            return Ok(());
        }

        // Temp file in same directory so rename is atomic (on POSIX).
        let tmp_path = tmp_path_for(&path);

        // Create temp exclusively so we don't clobber anything.
        let mut f = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp_path)
            .with_context(|| format!("open temp object file {:?}", tmp_path))?;

        f.write_all(data)
            .with_context(|| format!("write temp object file {:?}", tmp_path))?;

        // Ensure bytes are on disk before rename (optional, but good for durability).
        f.sync_all()
            .with_context(|| format!("sync temp object file {:?}", tmp_path))?;

        drop(f);

        // Atomic replace is annoying on Windows if destination exists; we already checked `path.exists()`.
        // Rename is atomic on POSIX when source+dest are on same filesystem.
        fs::rename(&tmp_path, &path)
            .with_context(|| format!("rename {:?} -> {:?}", tmp_path, path))?;

        Ok(())
    }

    pub fn read_object(&self, ty: &ObjectType, hash: &Hash) -> Result<Vec<u8>> {
        let path = self.obj_path(ty, hash);
        let data = fs::read(path)?;
        Ok(data)
    }
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
