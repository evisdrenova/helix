use anyhow::Result;
use std::fs;
use std::path::Path;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

pub fn system_time_to_parts(time: SystemTime) -> (u64, u32) {
    let duration = time.duration_since(UNIX_EPOCH).unwrap_or_default();
    (duration.as_secs(), duration.subsec_nanos())
}

pub fn read_git_index_checksum(path: &Path) -> Result<[u8; 20]> {
    let data = fs::read(path)?;
    if data.len() < 20 {
        anyhow::bail!(".git/index too small");
    }

    let mut checksum = [0u8; 20];
    checksum.copy_from_slice(&data[data.len() - 20..]);
    Ok(checksum)
}
