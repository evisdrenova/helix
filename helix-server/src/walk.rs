use anyhow::Result;
use storage::storage::{FsObjectStore, FsRefStore};

pub fn collect_all_objects(objects: &FsObjectStore, remote_head: &[u8; 32]) -> Result<()> {
    Ok(())
}
