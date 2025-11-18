use anyhow::Result;
use memmap2::Mmap;
use std::fs::File;
use std::path::Path;

use crate::Oid;

// Git index structs
pub struct Index {
    mmap: Mmap,
    entry_count: u32,
}

pub struct IndexEntry<'a> {
    pub path: &'a str,
    pub oid: Oid,
    pub mtime: u64,
    pub size: u64,
}

pub struct IndexEntryIter<'a> {
    index: &'a Index,
    offset: usize,
    count: u32,
}




impl Index {
    pub fn open(path: &Path) -> Result<Self> {
        let file = File::open(path.join(".git/index"))?;
        let mmap = unsafe { Mmap::map(&file)? };

        // Parse the header
        // Git index format: 4-byte signature "DIRC", 4-byte version, 4-byte entry count
        let entry_count = u32::from_be_bytes([mmap[8], mmap[9], mmap[10], mmap[11]]);

        Ok(Self { mmap, entry_count })
    }

    pub fn entries(&self) -> impl Iterator<Item = IndexEntry> + '_ {
        IndexEntryIter {
            index: self,
            offset: 12, // Skip 12-byte header
            count: 0,
        }
    }
}





// doc: https://git-scm.com/docs/index-format
impl<'a> Iterator for IndexEntryIter<'a> {
    type Item = IndexEntry<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.count >= self.index.entry_count {
            return None;
        }

        let data = &self.index.mmap[self.offset..];

        // Git index entry format (version 2):
        // 32-bit ctime seconds, 32-bit ctime nanoseconds
        // 32-bit mtime seconds, 32-bit mtime nanoseconds
        // 32-bit dev, 32-bit ino, 32-bit mode, 32-bit uid, 32-bit gid
        // 32-bit file size
        // 20-byte SHA-1 hash
        // 16-bit flags
        // variable-length path name (null-terminated)
        // padding to align to 8-byte boundary

        // Parse mtime (skip ctime, get mtime at offset 8)
        let mtime = u32::from_be_bytes([data[8], data[9], data[10], data[11]]) as u64;

        // Parse file size (at offset 40)
        let size = u32::from_be_bytes([data[40], data[41], data[42], data[43]]) as u64;

        // Parse SHA-1 hash (at offset 44, 20 bytes)
        let mut oid_bytes = [0u8; 20];
        oid_bytes.copy_from_slice(&data[44..64]);
        let oid = Oid(oid_bytes);

        // Parse flags (at offset 64, 2 bytes)
        let flags = u16::from_be_bytes([data[64], data[65]]);
        let name_length = (flags & 0x0FFF) as usize;

        // Parse path name (at offset 66)
        let path_start = 66;
        let path_end = if name_length < 0x0FFF {
            path_start + name_length
        } else {
            // If name_length == 0x0FFF, the name is null-terminated
            path_start + data[path_start..].iter().position(|&b| b == 0).unwrap_or(0)
        };

        let path = std::str::from_utf8(&data[path_start..path_end]).ok()?;

        // Calculate next entry offset (entries are padded to 8-byte boundary)
        let entry_size = 62 + path.len() + 1; // 62 bytes fixed + path + null terminator
        let padding = (8 - (entry_size % 8)) % 8;
        self.offset += entry_size + padding;
        self.count += 1;

        Some(IndexEntry {
            path,
            oid,
            mtime,
            size,
        })
    }
}
