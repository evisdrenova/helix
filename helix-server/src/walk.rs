use anyhow::Result;

use crate::storage::storage::FsObjectStore;

pub fn collect_all_objects(objects: &FsObjectStore, remote_head: &[u8; 32]) -> Result<()> {
    Ok(())
}
