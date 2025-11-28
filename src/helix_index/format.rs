/*
This is the format of the helix.idx binary file - the primary index for Helix.

This file is the source of truth for Helix operations.

We synchronize Git's .git/index from this file for git compatibility.

Binary format for helix.idx V1.0

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

 This is modeled after the .git/index file format.
*/

use std::path::PathBuf;

pub const MAGIC: [u8; 4] = *b"HLIX";
pub const HEADER_SIZE: usize = 116;
pub const VERSION: u32 = 1;
pub const FOOTER_SIZE: usize = 32;
pub const ENTRY_RESERVED_SIZE: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    pub magic: [u8; 4],
    pub version: u32,               // 1
    pub generation: u64,            // Incremented on every write
    pub repo_fingerprint: [u8; 16], // Prevents cross-repo reuse
    pub entry_count: u32,
    pub created_at: u64,
    pub last_modified: u64,
    pub reserved: [u8; 64], // reserved for future fields
}

impl Header {
    pub fn new(generation: u64, repo_fingerprint: [u8; 16], entry_count: u32) -> Self {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        Self {
            magic: MAGIC,
            version: VERSION,
            generation,
            repo_fingerprint,
            entry_count,
            created_at: if generation == 1 { now } else { 0 },
            last_modified: now,
            reserved: [0; 64],
        }
    }
    /// Serialize header to bytes (140 bytes fixed)
    pub fn to_bytes(&self) -> [u8; HEADER_SIZE] {
        let mut buf = [0u8; HEADER_SIZE];
        let mut offset = 0;

        buf[offset..offset + 4].copy_from_slice(&self.magic);
        offset += 4;

        buf[offset..offset + 4].copy_from_slice(&self.version.to_le_bytes());
        offset += 4;

        buf[offset..offset + 8].copy_from_slice(&self.generation.to_le_bytes());
        offset += 8;

        buf[offset..offset + 16].copy_from_slice(&self.repo_fingerprint);
        offset += 16;

        buf[offset..offset + 4].copy_from_slice(&self.entry_count.to_le_bytes());
        offset += 4;

        buf[offset..offset + 8].copy_from_slice(&self.created_at.to_le_bytes());
        offset += 8;

        buf[offset..offset + 8].copy_from_slice(&self.last_modified.to_le_bytes());
        offset += 8;

        buf[offset..offset + 64].copy_from_slice(&self.reserved);
        offset += 64;

        assert_eq!(offset, HEADER_SIZE);
        buf
    }

    /// Deserialize header from bytes
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, FormatError> {
        if bytes.len() < HEADER_SIZE {
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

        let mut repo_fingerprint = [0u8; 16];
        repo_fingerprint.copy_from_slice(&bytes[offset..offset + 16]);
        offset += 16;

        let entry_count = u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
        offset += 4;

        let created_at = u64::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
        offset += 8;

        let last_modified = u64::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
        offset += 8;

        let mut reserved = [0u8; 64];
        reserved.copy_from_slice(&bytes[offset..offset + 64]);
        offset += 64;

        assert_eq!(offset, HEADER_SIZE);

        Ok(Self {
            magic,
            version,
            generation,
            repo_fingerprint,
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
    pub oid: [u8; 20],
    pub merge_conflict_stage: u8, // (0 = normal, 1-3 = conflict stages)
    pub file_mode: u32,           // (0o100644 = regular, 0o100755 = executable, 0o120000 = symlink)
    pub reserved: [u8; 57],
}

impl Entry {
    /// [path_len: u16][path: bytes][size: u64][mtime_sec: u64][mtime_nsec: u32][flags: u16][oid: 20 bytes][reserved: 64 bytes]
    pub fn to_bytes(&self) -> Vec<u8> {
        let path_bytes = self.path.to_string_lossy();
        let path_len = path_bytes.len() as u16;

        let mut buf = Vec::with_capacity(2 + path_bytes.len() + 8 + 8 + 4 + 2 + 20 + 1 + 4 + 57);

        buf.extend_from_slice(&path_len.to_le_bytes());
        buf.extend_from_slice(path_bytes.as_bytes());
        buf.extend_from_slice(&self.size.to_le_bytes());
        buf.extend_from_slice(&self.mtime_sec.to_le_bytes());
        buf.extend_from_slice(&self.mtime_nsec.to_le_bytes());
        buf.extend_from_slice(&self.flags.bits().to_le_bytes());
        buf.extend_from_slice(&self.oid);
        buf.push(self.merge_conflict_stage);
        buf.extend_from_slice(&self.file_mode.to_le_bytes());
        buf.extend_from_slice(&self.reserved);

        buf
    }

    /// Deserialize entry from bytes, returns (Entry, bytes_consumed)
    pub fn from_bytes(bytes: &[u8]) -> Result<(Self, usize), FormatError> {
        if bytes.len() < 2 {
            return Err(FormatError::InvalidEntry("Too short for path_len".into()));
        }

        let mut offset = 0;

        // path_len
        let path_len = u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap()) as usize;
        offset += 2;

        // path
        if bytes.len() < offset + path_len {
            return Err(FormatError::InvalidEntry("Too short for path".into()));
        }
        let path_bytes = &bytes[offset..offset + path_len];
        let path = PathBuf::from(String::from_utf8_lossy(path_bytes).to_string());
        offset += path_len;

        // Need at least 8 + 8 + 4 + 2 + 20 + 64 = 106 more bytes
        if bytes.len() < offset + 106 {
            return Err(FormatError::InvalidEntry(
                "Too short for entry fields".into(),
            ));
        }

        // size
        let size = u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap());
        offset += 8;

        // mtime_sec
        let mtime_sec = u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap());
        offset += 8;

        // mtime_nsec
        let mtime_nsec = u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
        offset += 4;

        // flags
        let flags_bits = u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap());
        let flags = EntryFlags::from_bits(flags_bits)
            .ok_or_else(|| FormatError::InvalidEntry(format!("Invalid flags: {}", flags_bits)))?;
        offset += 2;

        let mut oid = [0u8; 20];
        oid.copy_from_slice(&bytes[offset..offset + 20]);
        offset += 20;

        let mut reserved = [0u8; ENTRY_RESERVED_SIZE];
        reserved.copy_from_slice(&bytes[offset..offset + ENTRY_RESERVED_SIZE]);
        offset += ENTRY_RESERVED_SIZE;

        let merge_conflict_stage = bytes[offset];
        offset += 1;

        let file_mode = u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
        offset += 4;

        let mut reserved = [0u8; 57];
        reserved.copy_from_slice(&bytes[offset..offset + 57]);
        offset += 57;

        Ok((
            Self {
                path,
                size,
                mtime_sec,
                mtime_nsec,
                flags,
                oid,
                merge_conflict_stage,
                file_mode,
                reserved,
            },
            offset,
        ))
    }

    /// Create a new tracked entry (from git index or helix add)
    pub fn new_tracked(
        path: PathBuf,
        oid: [u8; 20],
        size: u64,
        mtime_sec: u64,
        mtime_nsec: u32,
        file_mode: u32,
    ) -> Self {
        Self {
            path,
            size,
            mtime_sec,
            mtime_nsec,
            flags: EntryFlags::TRACKED,
            oid,
            merge_conflict_stage: 0,
            file_mode,
            reserved: [0; 57],
        }
    }

    /// Create a new untracked entry (file exists but not added)
    pub fn new_untracked(path: PathBuf, size: u64, mtime_sec: u64, mtime_nsec: u32) -> Self {
        Self {
            path,
            size,
            mtime_sec,
            mtime_nsec,
            flags: EntryFlags::UNTRACKED,
            oid: [0; 20], // No hash yet
            merge_conflict_stage: 0,
            file_mode: 0o100644, // Default to regular file
            reserved: [0; 57],
        }
    }

    /// Mark this entry as staged
    pub fn mark_staged(&mut self) {
        self.flags |= EntryFlags::STAGED;
    }

    /// Mark this entry as modified
    pub fn mark_modified(&mut self) {
        self.flags |= EntryFlags::MODIFIED;
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
    pub struct EntryFlags: u16 {
        // Core status (mutually exclusive base states)
        const TRACKED    = 1 << 0;  // File is in the index (committed or staged)
        const STAGED     = 1 << 1;  // File has staged changes (ready to commit)
        const MODIFIED   = 1 << 2;  // Working tree differs from index
        const DELETED    = 1 << 3;  // File deleted from working tree
        const UNTRACKED  = 1 << 4;  // File exists but not in index (new file)

        // Special states
        const CONFLICT   = 1 << 5;  // Merge conflict
        const ASSUME_UNCHANGED = 1 << 6;  // Git's "assume unchanged" bit
        const SKIP_WORKTREE = 1 << 7;     // Git's "skip worktree" bit

        // Reserved for future use
        const RESERVED1  = 1 << 8;
        const RESERVED2  = 1 << 9;
        const RESERVED3  = 1 << 10;
        const RESERVED4  = 1 << 11;
        const RESERVED5  = 1 << 12;
        const RESERVED6  = 1 << 13;
        const RESERVED7  = 1 << 14;
        const RESERVED8  = 1 << 15;
    }
}

impl EntryFlags {
    pub fn is_clean(self) -> bool {
        self.contains(EntryFlags::TRACKED)
            && !self.intersects(EntryFlags::MODIFIED | EntryFlags::STAGED | EntryFlags::DELETED)
    }

    pub fn is_partially_staged(self) -> bool {
        self.contains(EntryFlags::TRACKED)
            && self.contains(EntryFlags::MODIFIED)
            && self.contains(EntryFlags::STAGED)
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_header_size() {
        assert_eq!(HEADER_SIZE, 116);
    }

    #[test]
    fn test_header_roundtrip() {
        let header = Header::new(42, [0xaa; 16], 10);

        let bytes = header.to_bytes();
        assert_eq!(bytes.len(), HEADER_SIZE);

        let decoded = Header::from_bytes(&bytes).unwrap();
        assert_eq!(decoded.magic, header.magic);
        assert_eq!(decoded.version, header.version);
        assert_eq!(decoded.generation, header.generation);
        assert_eq!(decoded.repo_fingerprint, header.repo_fingerprint);
        assert_eq!(decoded.entry_count, header.entry_count);
    }

    #[test]
    fn test_entry_roundtrip() {
        let entry = Entry {
            path: PathBuf::from("src/main.rs"),
            size: 1024,
            mtime_sec: 1234567890,
            mtime_nsec: 123456,
            flags: EntryFlags::TRACKED | EntryFlags::STAGED,
            oid: [0xcc; 20],
            merge_conflict_stage: 0,
            file_mode: 0o100644,
            reserved: [0; 57],
        };

        let bytes = entry.to_bytes();
        let (decoded, consumed) = Entry::from_bytes(&bytes).unwrap();
        assert_eq!(entry, decoded);
        assert_eq!(consumed, bytes.len());
    }

    #[test]
    fn test_entry_new_tracked() {
        let entry = Entry::new_tracked(
            PathBuf::from("test.txt"),
            [0xaa; 20],
            100,
            1234567890,
            0,
            0o100644,
        );

        assert_eq!(entry.path, PathBuf::from("test.txt"));
        assert_eq!(entry.size, 100);
        assert_eq!(entry.flags, EntryFlags::TRACKED);
        assert_eq!(entry.merge_conflict_stage, 0);
        assert_eq!(entry.file_mode, 0o100644);
    }

    #[test]
    fn test_entry_new_untracked() {
        let entry = Entry::new_untracked(PathBuf::from("new.txt"), 200, 1234567890, 0);

        assert_eq!(entry.path, PathBuf::from("new.txt"));
        assert_eq!(entry.size, 200);
        assert_eq!(entry.flags, EntryFlags::UNTRACKED);
        assert_eq!(entry.oid, [0; 20]);
        assert_eq!(entry.merge_conflict_stage, 0);
        assert_eq!(entry.file_mode, 0o100644);
    }

    #[test]
    fn test_entry_mark_staged() {
        let mut entry = Entry::new_tracked(
            PathBuf::from("test.txt"),
            [0xaa; 20],
            100,
            1234567890,
            0,
            0o100644,
        );

        entry.mark_staged();
        assert!(entry.flags.contains(EntryFlags::STAGED));
        assert!(entry.flags.contains(EntryFlags::TRACKED));
    }

    #[test]
    fn test_entry_mark_modified() {
        let mut entry = Entry::new_tracked(
            PathBuf::from("test.txt"),
            [0xaa; 20],
            100,
            1234567890,
            0,
            0o100644,
        );

        entry.mark_modified();
        assert!(entry.flags.contains(EntryFlags::MODIFIED));
        assert!(entry.flags.contains(EntryFlags::TRACKED));
    }

    #[test]
    fn test_entry_flags_is_clean() {
        let flags = EntryFlags::TRACKED;
        assert!(flags.is_clean());

        let flags = EntryFlags::TRACKED | EntryFlags::MODIFIED;
        assert!(!flags.is_clean());

        let flags = EntryFlags::TRACKED | EntryFlags::STAGED;
        assert!(!flags.is_clean());
    }

    #[test]
    fn test_entry_flags_is_partially_staged() {
        let flags = EntryFlags::TRACKED | EntryFlags::MODIFIED | EntryFlags::STAGED;
        assert!(flags.is_partially_staged());

        let flags = EntryFlags::TRACKED | EntryFlags::STAGED;
        assert!(!flags.is_partially_staged());

        let flags = EntryFlags::TRACKED | EntryFlags::MODIFIED;
        assert!(!flags.is_partially_staged());
    }

    #[test]
    fn test_entry_executable_mode() {
        let entry = Entry::new_tracked(
            PathBuf::from("script.sh"),
            [0xaa; 20],
            100,
            1234567890,
            0,
            0o100755, // Executable
        );

        assert_eq!(entry.file_mode, 0o100755);
    }

    #[test]
    fn test_entry_symlink_mode() {
        let entry = Entry::new_tracked(
            PathBuf::from("link"),
            [0xaa; 20],
            100,
            1234567890,
            0,
            0o120000, // Symlink
        );

        assert_eq!(entry.file_mode, 0o120000);
    }

    #[test]
    fn test_entry_conflict_stage() {
        let mut entry = Entry::new_tracked(
            PathBuf::from("conflict.txt"),
            [0xaa; 20],
            100,
            1234567890,
            0,
            0o100644,
        );

        // Simulate merge conflict
        entry.merge_conflict_stage = 1; // Stage 1 = common ancestor
        entry.flags |= EntryFlags::CONFLICT;

        assert_eq!(entry.merge_conflict_stage, 1);
        assert!(entry.flags.contains(EntryFlags::CONFLICT));
    }

    #[test]
    fn test_header_generation_increment() {
        let header1 = Header::new(1, [0xaa; 16], 10);
        let header2 = Header::new(2, [0xaa; 16], 10);

        assert_eq!(header1.generation, 1);
        assert_eq!(header2.generation, 2);
    }

    #[test]
    fn test_header_timestamps() {
        let header = Header::new(1, [0xaa; 16], 10);

        // Created_at should be set for generation 1
        assert!(header.created_at > 0);
        assert!(header.last_modified > 0);
    }

    #[test]
    fn test_invalid_magic() {
        let mut bytes = [0u8; HEADER_SIZE];
        bytes[0..4].copy_from_slice(b"BAAD");

        let result = Header::from_bytes(&bytes);
        assert!(matches!(result, Err(FormatError::InvalidMagic(_))));
    }

    #[test]
    fn test_invalid_version() {
        let mut bytes = [0u8; HEADER_SIZE];
        bytes[0..4].copy_from_slice(&MAGIC);
        bytes[4..8].copy_from_slice(&999u32.to_le_bytes());

        let result = Header::from_bytes(&bytes);
        assert!(matches!(result, Err(FormatError::UnsupportedVersion(999))));
    }

    #[test]
    fn test_entry_too_short() {
        let bytes = [0u8; 10]; // Way too short
        let result = Entry::from_bytes(&bytes);
        assert!(result.is_err());
    }

    #[test]
    fn test_footer_roundtrip() {
        let footer = Footer::new([0xdd; 32]);
        let bytes = footer.to_bytes();
        assert_eq!(bytes.len(), FOOTER_SIZE);

        let decoded = Footer::from_bytes(&bytes).unwrap();
        assert_eq!(footer, decoded);
    }
}
