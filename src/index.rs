use anyhow::{bail, Result};
use memmap2::{Mmap, MmapOptions};
use std::path::Path;
use std::{fs::File, io};

use crate::Oid;

pub struct ReadOnlyMmap {
    _file: File,
    map: Mmap,
}

/*  - this creates a safer boundary around the unsafe mmap
    - the _file field ensures that the file is not dropped while the mapping is active
    - checks that it's a file and we're not trying to mmap() a directory or empty file
    - mapping borrows implicitly from the structs lifetime, can't accidently outlive it
    - should have no real peformance hit, it's just an extra field in the struct
*/
impl ReadOnlyMmap {
    pub fn open(path: &Path) -> io::Result<Self> {
        let file = File::open(path)?;
        let meta = file.metadata()?;
        if !meta.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "not a regular file",
            ));
        }

        // check to make sure file isn't empty
        if meta.len() == 0 {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "empty file"));
        }

        let map = unsafe { MmapOptions::new().len(meta.len() as usize).map(&file)? };

        Ok(Self { _file: file, map })
    }

    #[inline]
    pub fn bytes(&self) -> &[u8] {
        &self.map
    }
}

pub struct Index {
    mmap: ReadOnlyMmap,
    entry_count: u32,
}

// index entries are usually files
pub struct IndexEntry<'a> {
    pub path: &'a str,
    pub oid: Oid,
    pub mtime: u64,
    pub size: u64,
}

pub struct IndexEntryIter<'a> {
    index: &'a Index,
    offset: usize,
    seen: u32,
}

impl Index {
    // open a git index file from a repository root
    pub fn open(repo_root: &Path) -> Result<Self> {
        let mmap = ReadOnlyMmap::open(&repo_root.join(".git/index"))?;
        let buf = mmap.bytes();

        if buf.len() < 12 {
            bail!("index file too small");
        }

        if &buf[0..4] != b"DIRC" {
            bail!("invalid index signature");
        }

        // doc: https://git-scm.com/docs/index-format
        let version = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]);
        if version != 2 {
            bail!("unsupported index version: {}", version);
        }

        let entry_count = u32::from_be_bytes([buf[8], buf[9], buf[10], buf[11]]);

        Ok(Self { mmap, entry_count })
    }

    // iterator over all index entries
    pub fn entries(&self) -> impl Iterator<Item = IndexEntry> + '_ {
        IndexEntryIter {
            index: self,
            offset: 12, // skip 12-byte header
            seen: 0,
        }
    }
}

// todo: probably optimize this, it's pretty restrictive in the way that it checks the bounds
impl<'a> Iterator for IndexEntryIter<'a> {
    type Item = IndexEntry<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.seen >= self.index.entry_count {
            return None;
        }

        let buf = self.index.mmap.bytes();
        const FIXED_HEADER_SIZE: usize = 62; // fixed part of each entry

        // ensure we have enough data for the fixed header
        if self.offset + FIXED_HEADER_SIZE > buf.len() {
            self.seen = self.index.entry_count; // reached the end, stop iterating
            return None;
        }

        // starts at 12 after the header
        let base = self.offset;

        // mtime (offset 8-15: seconds + nanoseconds)
        let mtime_secs = u32::from_be_bytes(buf[base + 8..base + 12].try_into().ok()?);
        let mtime_nsecs = u32::from_be_bytes(buf[base + 12..base + 16].try_into().ok()?);
        let mtime = ((mtime_secs as u64) << 32) | (mtime_nsecs as u64);

        // file size (offset 36-39)
        let size = u32::from_be_bytes(buf[base + 36..base + 40].try_into().ok()?) as u64;

        // SHA-1 hash (offset 40-59, 20 bytes)
        let oid_start = base + 40;
        let oid_end = oid_start + 20;
        if oid_end > buf.len() {
            self.seen = self.index.entry_count;
            return None;
        }
        let oid = Oid::from_bytes(&buf[oid_start..oid_end]);

        // flags (offset 60-61)
        let flags = u16::from_be_bytes(buf[base + 60..base + 62].try_into().ok()?);
        let name_hint = (flags & 0x0FFF) as usize;

        // path (starts at offset 62, NUL-terminated)
        let name_start = base + FIXED_HEADER_SIZE;
        if name_start >= buf.len() {
            self.seen = self.index.entry_count;
            return None;
        }

        // find NUL terminator
        let remaining = buf.len() - name_start;
        let max_scan = if name_hint > 0 && name_hint < 0x0FFF {
            (name_hint + 1).min(remaining)
        } else {
            remaining.min(1 << 20) // safety limit: 1MB max path
        };

        // scan for NUL byte
        let nul_offset = (0..max_scan).find(|&i| buf[name_start + i] == 0)?;

        let path_bytes = &buf[name_start..name_start + nul_offset];
        let path = std::str::from_utf8(path_bytes).ok()?;

        // calculate next entry offset with 8-byte alignment padding
        let entry_len = FIXED_HEADER_SIZE + nul_offset + 1; // +1 for NUL
        let padding = (8 - (entry_len % 8)) % 8;
        self.offset += entry_len + padding;
        self.seen += 1;

        Some(IndexEntry {
            path,
            oid,
            mtime,
            size,
        })
    }
}
