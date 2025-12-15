use anyhow::Result;
use helix_protocol::{Hash32, ObjectType};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone)]
pub struct FsObjectStore {
    root: PathBuf, // e.g. /var/lib/helix/repos/<repo>
}

impl FsObjectStore {
    pub fn new(root: impl AsRef<Path>) -> Self {
        Self {
            root: root.as_ref().to_path_buf(),
        }
    }

    fn obj_path(&self, ty: &ObjectType, hash: &Hash32) -> PathBuf {
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

    pub fn has_object(&self, ty: &ObjectType, hash: &Hash32) -> bool {
        self.obj_path(ty, hash).exists()
    }

    pub fn write_object(&self, ty: &ObjectType, hash: &Hash32, data: &[u8]) -> Result<()> {
        let path = self.obj_path(ty, hash);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        // you might want atomic write here later
        fs::write(path, data)?;
        Ok(())
    }

    pub fn read_object(&self, ty: &ObjectType, hash: &Hash32) -> Result<Vec<u8>> {
        let path = self.obj_path(ty, hash);
        let data = fs::read(path)?;
        Ok(data)
    }
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

    pub fn get_ref(&self, name: &str) -> Result<Option<Hash32>> {
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

    pub fn set_ref(&self, name: &str, new: Hash32) -> Result<()> {
        let path = self.ref_path(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, hex::encode(new))?;
        Ok(())
    }
}
