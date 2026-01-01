/*
Binary file format for helix.idx V1.0

┌─────────────────────────────────────┐
 │ Header                              │
 ├─────────────────────────────────────┤
 │ Entry 1                             │
 │ Entry 2                             │
 │ ...                                 │
 │ Entry N                             │
 ├─────────────────────────────────────┤
 │ Reserved Metadata                   │
 ├─────────────────────────────────────┤
 │ Footer                              │
 └─────────────────────────────────────┘
*/

use helix_protocol::hash::Hash;
use std::{
    path::{Path, PathBuf},
    str::Utf8Error,
};

use crate::add_command::get_file_mode;

pub const MAGIC: [u8; 4] = *b"HLIX";
pub const VERSION: u32 = 1;
pub const FOOTER_SIZE: usize = 32;
pub const ENTRY_RESERVED_SIZE: usize = 64;

#[repr(C)] // ensures file layout is guaranteed in memory w/o extra padding
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    pub magic: [u8; 4],
    pub version: u32,    // 1
    pub generation: u64, // Incremented on every write
    pub checksum: Hash,  // Checksum of entire file; 32 bytes
    pub entry_count: u32,
    pub created_at: u64,
    pub last_modified: u64,
    pub reserved: [u8; 60], // reserved for future fields
}

impl Header {
    pub const HEADER_SIZE: usize = 128;
    pub fn new(generation: u64, entry_count: u32) -> Self {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        Self {
            magic: MAGIC,
            version: VERSION,
            generation,
            checksum: [0u8; 32],
            entry_count,
            created_at: if generation == 1 { now } else { 0 },
            last_modified: now,
            reserved: [0; 60],
        }
    }
    /// Serialize header to bytes
    pub fn to_bytes(&self) -> [u8; Self::HEADER_SIZE] {
        let mut buf = [0u8; Self::HEADER_SIZE];
        let mut offset = 0;

        buf[offset..offset + 4].copy_from_slice(&self.magic);
        offset += 4;

        buf[offset..offset + 4].copy_from_slice(&self.version.to_le_bytes());
        offset += 4;

        buf[offset..offset + 8].copy_from_slice(&self.generation.to_le_bytes());
        offset += 8;

        buf[offset..offset + 32].copy_from_slice(&self.checksum);
        offset += 32;

        buf[offset..offset + 4].copy_from_slice(&self.entry_count.to_le_bytes());
        offset += 4;

        buf[offset..offset + 8].copy_from_slice(&self.created_at.to_le_bytes());
        offset += 8;

        buf[offset..offset + 8].copy_from_slice(&self.last_modified.to_le_bytes());
        offset += 8;

        buf[offset..offset + 60].copy_from_slice(&self.reserved);
        offset += 60;

        assert_eq!(offset, Self::HEADER_SIZE);
        buf
    }

    /// Deserialize header from bytes
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, FormatError> {
        if bytes.len() < Self::HEADER_SIZE {
            return Err(FormatError::InvalidHeader("Too short".into()));
        }

        let mut offset = 0;

        let mut magic = [0u8; 4];
        magic.copy_from_slice(&bytes[offset..offset + 4]);
        if magic != MAGIC {
            return Err(FormatError::InvalidMagic(magic));
        }
        offset += 4;

        let version = u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
        if version != VERSION {
            return Err(FormatError::UnsupportedVersion(version));
        }
        offset += 4;

        let generation = u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap());
        offset += 8;

        let mut checksum = [0u8; 32];
        checksum.copy_from_slice(&bytes[offset..offset + 32]);
        offset += 32;

        let entry_count = u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
        offset += 4;

        let created_at = u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap());
        offset += 8;

        let last_modified = u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap());
        offset += 8;

        let mut reserved = [0u8; 60];
        reserved.copy_from_slice(&bytes[offset..offset + 60]);
        offset += 60;

        assert_eq!(offset, Self::HEADER_SIZE);

        Ok(Self {
            magic,
            version,
            generation,
            checksum,
            entry_count,
            created_at,
            last_modified,
            reserved,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub path: PathBuf,
    pub size: u64,
    pub mtime_sec: u64,
    pub mtime_nsec: u32,
    pub flags: EntryFlags,
    pub oid: Hash,
    pub merge_conflict_stage: u8, // (0 = normal, 1-3 = conflict stages)
    pub file_mode: u32,           // (0o100644 = regular, 0o100755 = executable, 0o120000 = symlink)
    pub reserved: [u8; 33],
}

impl Entry {
    pub const ENTRY_MAX_SIZE: usize = 296;
    pub const ENTRY_MAX_PATH_LEN: usize = 200;

    /// Create a new Helix index entry
    pub fn new(path: PathBuf, size: u64, mtime_sec: u64, oid: Hash, file_mode: u32) -> Self {
        Self {
            path,
            size,
            mtime_sec,
            mtime_nsec: 0,
            flags: EntryFlags::empty(),
            oid,
            file_mode,
            merge_conflict_stage: 0,
            reserved: [0u8; 33],
        }
    }

    /// Serialize entry to bytes
    pub fn to_bytes(&self) -> Result<Vec<u8>, FormatError> {
        let path_str = self.path.to_string_lossy();
        let path_bytes = path_str.as_bytes();

        if path_bytes.len() > Self::ENTRY_MAX_PATH_LEN {
            return Err(FormatError::PathLengthLongerThanMaxSize(path_bytes.len()));
        }

        let mut buf = Vec::with_capacity(Self::ENTRY_MAX_SIZE);

        // Path length (2 bytes)
        buf.extend_from_slice(&(path_bytes.len() as u16).to_le_bytes());

        // Path (variable, up to MAX_PATH_LEN)
        buf.extend_from_slice(path_bytes);

        // Pad path to MAX_PATH_LEN
        buf.resize(2 + Self::ENTRY_MAX_PATH_LEN, 0);

        // Size (8 bytes)
        buf.extend_from_slice(&self.size.to_le_bytes());

        // Mtime sec (8 bytes)
        buf.extend_from_slice(&self.mtime_sec.to_le_bytes());

        // Mtime nsec (4 bytes)
        buf.extend_from_slice(&self.mtime_nsec.to_le_bytes());

        // Flags (4 bytes)
        buf.extend_from_slice(&self.flags.bits().to_le_bytes());

        // OID (32 bytes) - BLAKE3 hash
        buf.extend_from_slice(&self.oid);

        // File mode (4 bytes)
        buf.extend_from_slice(&self.file_mode.to_le_bytes());

        // Merge conflict stage (1 byte)
        buf.push(self.merge_conflict_stage);

        // Reserved (33 bytes)
        buf.extend_from_slice(&self.reserved);

        assert_eq!(
            buf.len(),
            Self::ENTRY_MAX_SIZE,
            "Entry serialization size mismatch"
        );
        Ok(buf)
    }

    /// Deserialize entry from bytes
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, FormatError> {
        if bytes.len() < Self::ENTRY_MAX_SIZE {
            return Err(FormatError::PathLengthLongerThanMaxSize(bytes.len()));
        }

        let mut offset = 0;

        // Path length (2 bytes)
        let path_len = u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap()) as usize;
        offset += 2;

        if path_len > Self::ENTRY_MAX_PATH_LEN {
            return Err(FormatError::PathLengthTooLong(bytes.len()));
        }

        // Path (variable)
        let path_bytes = &bytes[offset..offset + path_len];
        let path_str =
            std::str::from_utf8(path_bytes).map_err(|e| FormatError::InvalidPathEncoding(e))?;
        let path = PathBuf::from(path_str);
        offset += Self::ENTRY_MAX_PATH_LEN; // Skip past padded path

        // Size (8 bytes)
        let size = u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap());
        offset += 8;

        // Mtime sec (8 bytes)
        let mtime_sec = u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap());
        offset += 8;

        // Mtime nsec (4 bytes)
        let mtime_nsec = u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
        offset += 4;

        // Flags (4 bytes)
        let flags_bits = u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
        let flags = EntryFlags::from_bits(flags_bits)
            .ok_or_else(|| FormatError::InvalidEntry("Invalid flag bits".to_string()))?;
        offset += 4;

        // OID (32 bytes) - BLAKE3 hash
        let mut oid = [0u8; 32];
        oid.copy_from_slice(&bytes[offset..offset + 32]);
        offset += 32;

        // File mode (4 bytes)
        let file_mode = u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
        offset += 4;

        // Merge conflict stage (1 byte)
        let merge_conflict_stage = bytes[offset];
        offset += 1;

        // Reserved (33 bytes)
        let mut reserved = [0u8; 33];
        reserved.copy_from_slice(&bytes[offset..offset + 33]);

        Ok(Self {
            path,
            size,
            mtime_sec,
            mtime_nsec,
            flags,
            oid,
            file_mode,
            merge_conflict_stage,
            reserved,
        })
    }

    pub fn from_blob(path: PathBuf, oid: Hash, workdir: &Path) -> Self {
        let full_path = workdir.join(&path);

        let (size, mtime_sec, file_mode) = full_path
            .metadata()
            .ok()
            .map(|m| {
                let mtime = m
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                (m.len(), mtime, get_file_mode(&m))
            })
            .unwrap_or((0, 0, 0o100644));

        Self {
            path,
            oid,
            flags: EntryFlags::TRACKED,
            size,
            mtime_sec,
            mtime_nsec: 0,
            file_mode,
            merge_conflict_stage: 0,
            reserved: [0u8; 33],
        }
    }
}

/*
tracked && modified && !staged = unstaged
tracked && staged && !modified = staged
tracked && !modified && !staged = clean
tracked && staged && modified = partially staged
untracked && !staged = untracked
untracked && staged = staged (new file)
*/

bitflags::bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct EntryFlags: u32 {
        // Core status (mutually exclusive base states)
        const TRACKED    = 1 << 0;  // File is in the index (committed or staged)
        const STAGED     = 1 << 1;  // File has staged changes (ready to commit)
        const MODIFIED   = 1 << 2;  // Working tree differs from index
        const DELETED    = 1 << 3;  // File deleted from working tree
        const UNTRACKED  = 1 << 4;  // File exists but not in index (new file)

        // Special states
        const CONFLICT   = 1 << 5;  // Merge conflict
        const ASSUME_UNCHANGED = 1 << 6;
        const IGNORED = 1 << 7;
        const SYMLINK = 1 << 8;

        // Reserved for future use
        const RESERVED1  = 1 << 9;
        const RESERVED2  = 1 << 10;
        const RESERVED3  = 1 << 11;
        const RESERVED4  = 1 << 12;
        const RESERVED5  = 1 << 13;
        const RESERVED6  = 1 << 14;
        const RESERVED7  = 1 << 15;
    }
}

impl EntryFlags {
    /// Check if file is in a clean state (tracked, not modified)
    pub fn is_clean(&self) -> bool {
        self.contains(Self::TRACKED)
            && !self.intersects(Self::MODIFIED | Self::DELETED | Self::CONFLICT)
    }

    /// Check if file needs attention (modified, deleted, or conflict)
    pub fn needs_attention(&self) -> bool {
        self.intersects(Self::MODIFIED | Self::DELETED | Self::CONFLICT)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Footer {
    /// SHA-256 checksum of header + all entries
    pub checksum: [u8; 32],
}

impl Footer {
    pub fn new(checksum: [u8; 32]) -> Self {
        Self { checksum }
    }

    pub fn to_bytes(&self) -> [u8; FOOTER_SIZE] {
        self.checksum
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, FormatError> {
        if bytes.len() < FOOTER_SIZE {
            return Err(FormatError::InvalidFooter("Too short".into()));
        }

        let mut checksum = [0u8; 32];
        checksum.copy_from_slice(&bytes[0..32]);

        Ok(Self { checksum })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum FormatError {
    #[error("Invalid magic bytes: {0:?}")]
    InvalidMagic([u8; 4]),

    #[error("Unsupported version: {0}")]
    UnsupportedVersion(u32),

    #[error("Invalid header: {0}")]
    InvalidHeader(String),

    #[error("Invalid entry: {0}")]
    InvalidEntry(String),

    #[error("Invalid footer: {0}")]
    InvalidFooter(String),

    #[error("Checksum mismatch")]
    ChecksumMismatch,

    #[error("Path length exceeds max size: {0}")]
    PathLengthLongerThanMaxSize(usize),

    #[error("Path length too long: {0}")]
    PathLengthTooLong(usize),

    #[error("Path is not encoded correctly: {0}")]
    InvalidPathEncoding(Utf8Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use helix_protocol::hash::hash_bytes;

    #[test]
    fn test_header_serialization() {
        let header = Header::new(1, 10);

        let bytes = header.to_bytes();
        assert_eq!(bytes.len(), Header::HEADER_SIZE);

        let parsed = Header::from_bytes(&bytes).unwrap();
        assert_eq!(parsed.magic, header.magic);
        assert_eq!(parsed.version, header.version);
        assert_eq!(parsed.generation, header.generation);
        assert_eq!(parsed.entry_count, header.entry_count);
    }

    #[test]
    fn test_header_magic_validation() {
        let mut bytes = vec![0u8; Header::HEADER_SIZE];
        bytes[0..6].copy_from_slice(b"WRONG!");

        let result = Header::from_bytes(&bytes);
        assert!(result.is_err());
        match result.unwrap_err() {
            FormatError::InvalidMagic(_) => {}
            other => panic!("Expected InvalidMagic FormatError Type, got {:?}", other),
        }
    }

    #[test]
    fn test_header_version_validation() {
        let mut bytes = vec![0u8; Header::HEADER_SIZE];
        bytes[0..6].copy_from_slice(&MAGIC);
        bytes[6..10].copy_from_slice(&999u32.to_le_bytes()); // Wrong version

        let result = Header::from_bytes(&bytes);
        assert!(result.is_err());
        match result.unwrap_err() {
            FormatError::InvalidMagic(_) => {}
            other => panic!("Expected InvalidMagic, got {:?}", other),
        }
    }

    #[test]
    fn test_entry_serialization() {
        let oid = hash_bytes(b"test content");
        let entry = Entry::new(
            PathBuf::from("src/main.rs"),
            1024,
            1234567890,
            oid,
            0o100644,
        );

        let bytes = entry.to_bytes().unwrap();
        assert_eq!(bytes.len(), Entry::ENTRY_MAX_SIZE);

        let parsed = Entry::from_bytes(&bytes).unwrap();
        assert_eq!(parsed.path, entry.path);
        assert_eq!(parsed.size, entry.size);
        assert_eq!(parsed.mtime_sec, entry.mtime_sec);
        assert_eq!(parsed.oid, entry.oid);
        assert_eq!(parsed.file_mode, entry.file_mode);
    }

    #[test]
    fn test_entry_path_too_long() {
        let long_path = "a".repeat(Entry::ENTRY_MAX_PATH_LEN + 1);
        let oid = hash_bytes(b"test");
        let entry = Entry::new(PathBuf::from(long_path), 1024, 1234567890, oid, 0o100644);

        let result = entry.to_bytes();
        assert!(result.is_err());
        match result.unwrap_err() {
            FormatError::PathLengthLongerThanMaxSize(_) => {}
            other => panic!("Path too long, got {:?}", other),
        }
    }

    #[test]
    fn test_entry_flags() {
        let mut flags = EntryFlags::TRACKED;
        assert!(flags.contains(EntryFlags::TRACKED));
        assert!(!flags.contains(EntryFlags::STAGED));

        flags |= EntryFlags::STAGED;
        assert!(flags.contains(EntryFlags::TRACKED | EntryFlags::STAGED));

        flags.remove(EntryFlags::TRACKED);
        assert!(!flags.contains(EntryFlags::TRACKED));
        assert!(flags.contains(EntryFlags::STAGED));
    }

    #[test]
    fn test_entry_is_clean() {
        let clean = EntryFlags::TRACKED;
        assert!(clean.is_clean());

        let modified = EntryFlags::TRACKED | EntryFlags::MODIFIED;
        assert!(!modified.is_clean());

        let deleted = EntryFlags::TRACKED | EntryFlags::DELETED;
        assert!(!deleted.is_clean());
    }

    #[test]
    fn test_entry_needs_attention() {
        let clean = EntryFlags::TRACKED;
        assert!(!clean.needs_attention());

        let modified = EntryFlags::TRACKED | EntryFlags::MODIFIED;
        assert!(modified.needs_attention());

        let deleted = EntryFlags::TRACKED | EntryFlags::DELETED;
        assert!(deleted.needs_attention());

        let conflict = EntryFlags::TRACKED | EntryFlags::CONFLICT;
        assert!(conflict.needs_attention());
    }

    #[test]
    fn test_oid_size() {
        // Verify OID is now 32 bytes (BLAKE3) not 20 bytes (SHA-1)
        let oid = hash_bytes(b"test");
        assert_eq!(oid.len(), 32, "OID should be 32 bytes for BLAKE3");
    }

    #[test]
    fn test_entry_roundtrip_with_blake3() {
        // Test that BLAKE3 hashes work correctly in entries
        let content = b"hello world from helix";
        let oid = hash_bytes(content);

        let entry = Entry::new(
            PathBuf::from("test.txt"),
            content.len() as u64,
            1234567890,
            oid,
            0o100644,
        );

        let bytes = entry.to_bytes().unwrap();
        let parsed = Entry::from_bytes(&bytes).unwrap();

        assert_eq!(parsed.oid, oid);
        assert_eq!(parsed.oid.len(), 32);
    }

    #[test]
    fn test_entry_max_path() {
        // Test with maximum allowed path length
        let max_path = "a".repeat(Entry::ENTRY_MAX_PATH_LEN);
        let oid = hash_bytes(b"test");

        let entry = Entry::new(PathBuf::from(&max_path), 1024, 1234567890, oid, 0o100644);

        let bytes = entry.to_bytes().unwrap();
        let parsed = Entry::from_bytes(&bytes).unwrap();

        assert_eq!(
            parsed.path.to_string_lossy().len(),
            Entry::ENTRY_MAX_PATH_LEN
        );
    }

    #[test]
    fn test_header_constants() {
        assert_eq!(MAGIC, *b"HLIX");
        assert_eq!(VERSION, 2, "Version should be 2 for BLAKE3 format");
        assert_eq!(Header::HEADER_SIZE, 128);
    }

    #[test]
    fn test_entry_constants() {
        assert_eq!(Entry::ENTRY_MAX_SIZE, 256);
        assert_eq!(Entry::ENTRY_MAX_PATH_LEN, 200);
    }

    #[test]
    fn test_multiple_entries_roundtrip() {
        let entries = vec![
            Entry::new(
                PathBuf::from("file1.txt"),
                100,
                111,
                hash_bytes(b"1"),
                0o100644,
            ),
            Entry::new(
                PathBuf::from("file2.txt"),
                200,
                222,
                hash_bytes(b"2"),
                0o100644,
            ),
            Entry::new(
                PathBuf::from("file3.txt"),
                300,
                333,
                hash_bytes(b"3"),
                0o100755,
            ),
        ];

        for entry in &entries {
            let bytes = entry.to_bytes().unwrap();
            let parsed = Entry::from_bytes(&bytes).unwrap();
            assert_eq!(&parsed, entry);
        }
    }
}
