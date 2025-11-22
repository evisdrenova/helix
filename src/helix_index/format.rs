/*
This is the format of the .helix.idx binary file that will be the read-only optimized version of .git/index.
This file isn't meant to replace .git/index but to sync with it and provide a read-optimized, crash-resilient cache that
accelerates git workflows without altering the canonical .git/index file.

Binary format for helix.idx V1.0

┌─────────────────────────────────────┐
 │ Header                              │
 ├─────────────────────────────────────┤
 │ Entry 1                             │
 │ Entry 2                             │
 │ ...                                 │
 │ Entry N                             │
 ├─────────────────────────────────────┤
 │ Reserved Metadata Zone (future)     │
 ├─────────────────────────────────────┤
 │ Footer                              │
 └─────────────────────────────────────┘

 This is modeled after the .git/index file format.
*/

// Magic bytes: "HLIX"
pub const MAGIC: [u8; 4] = *b"HLIX";

// Header is 140 bytes (fixed)
pub const HEADER_SIZE: usize = 4 + 4 + 8 + 16 + 8 + 4 + 8 + 20 + 4 + 64;

// Footer size in bytes (fixed)
pub const FOOTER_SIZE: usize = 32;

// Reserved bytes for future Entry metadata
pub const ENTRY_RESERVED_SIZE: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    pub magic: [u8; 4],
    pub version: u32,

    // Incremented on every successful sync
    pub generation: u64,

    // Hash of repo path + initial HEAD (prevents cross-repo reuse)
    pub repo_fingerprint: [u8; 16],

    // Git index metadata for drift detection
    pub git_index_mtime_sec: u64,
    pub git_index_mtime_nsec: u32,
    pub git_index_size: u64,
    pub git_index_checksum: [u8; 20],

    pub entry_count: u32,

    pub reserved: [u8; 64],
}

impl Header {
    pub fn new(
        generation: u64,
        repo_fingerprint: [u8; 16],
        git_index_mtime_sec: u64,
        git_index_mtime_nsec: u32,
        git_index_size: u64,
        git_index_checksum: [u8; 20],
        entry_count: u32,
    ) -> Self {
        Self {
            magic: MAGIC,
            version: VERSION,
            generation,
            repo_fingerprint,
            git_index_mtime_sec,
            git_index_mtime_nsec,
            git_index_size,
            git_index_checksum,
            entry_count,
            reserved: [0; 64],
        }
    }
    /// Serialize header to bytes (140 bytes fixed)
    pub fn to_bytes(&self) -> [u8; HEADER_SIZE] {
        let mut buf = [0u8; HEADER_SIZE];
        let mut offset = 0;

        // magic (4 bytes)
        buf[offset..offset + 4].copy_from_slice(&self.magic);
        offset += 4;

        // version (4 bytes)
        buf[offset..offset + 4].copy_from_slice(&self.version.to_le_bytes());
        offset += 4;

        // generation (8 bytes)
        buf[offset..offset + 8].copy_from_slice(&self.generation.to_le_bytes());
        offset += 8;

        // repo_fingerprint (16 bytes)
        buf[offset..offset + 16].copy_from_slice(&self.repo_fingerprint);
        offset += 16;

        // git_index_mtime_sec (8 bytes)
        buf[offset..offset + 8].copy_from_slice(&self.git_index_mtime_sec.to_le_bytes());
        offset += 8;

        // git_index_mtime_nsec (4 bytes)
        buf[offset..offset + 4].copy_from_slice(&self.git_index_mtime_nsec.to_le_bytes());
        offset += 4;

        // git_index_size (8 bytes)
        buf[offset..offset + 8].copy_from_slice(&self.git_index_size.to_le_bytes());
        offset += 8;

        // git_index_checksum (20 bytes)
        buf[offset..offset + 20].copy_from_slice(&self.git_index_checksum);
        offset += 20;

        // entry_count (4 bytes)
        buf[offset..offset + 4].copy_from_slice(&self.entry_count.to_le_bytes());
        offset += 4;

        // reserved (64 bytes)
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

        // magic
        let mut magic = [0u8; 4];
        magic.copy_from_slice(&bytes[offset..offset + 4]);
        if magic != MAGIC {
            return Err(FormatError::InvalidMagic(magic));
        }
        offset += 4;

        // version
        let version = u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
        if version != VERSION {
            return Err(FormatError::UnsupportedVersion(version));
        }
        offset += 4;

        // generation
        let generation = u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap());
        offset += 8;

        // repo_fingerprint
        let mut repo_fingerprint = [0u8; 16];
        repo_fingerprint.copy_from_slice(&bytes[offset..offset + 16]);
        offset += 16;

        // git_index_mtime_sec
        let git_index_mtime_sec = u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap());
        offset += 8;

        // git_index_mtime_nsec
        let git_index_mtime_nsec =
            u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
        offset += 4;

        // git_index_size
        let git_index_size = u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap());
        offset += 8;

        // git_index_checksum
        let mut git_index_checksum = [0u8; 20];
        git_index_checksum.copy_from_slice(&bytes[offset..offset + 20]);
        offset += 20;

        // entry_count
        let entry_count = u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
        offset += 4;

        // reserved
        let mut reserved = [0u8; 64];
        reserved.copy_from_slice(&bytes[offset..offset + 64]);
        offset += 64;

        assert_eq!(offset, HEADER_SIZE);

        Ok(Self {
            magic,
            version,
            generation,
            repo_fingerprint,
            git_index_mtime_sec,
            git_index_mtime_nsec,
            git_index_size,
            git_index_checksum,
            entry_count,
            reserved,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub path: PathBuf,
    pub size: u64,
    pub mtime_sec: u64,
    pub mtime_nsec: u64,
    pub flags: EntryFlags,
    pub oid: [u8; 20],
    pub reserved: [u8; ENTRY_RESERVED_SIZE],
}

impl Entry {
    /// Serialize entry to bytes (variable length)
    /// Format: [path_len: u16][path: bytes][size: u64][mtime_sec: u64][mtime_nsec: u32][flags: u16][oid: 20 bytes][reserved: 64 bytes]
    pub fn to_bytes(&self) -> Vec<u8> {
        let path_bytes = self.path.to_string_lossy().as_bytes();
        let path_len = path_bytes.len() as u16;

        let mut buf =
            Vec::with_capacity(2 + path_bytes.len() + 8 + 8 + 4 + 2 + 20 + ENTRY_RESERVED_SIZE);

        // path_len (2 bytes)
        buf.extend_from_slice(&path_len.to_le_bytes());

        // path (variable)
        buf.extend_from_slice(path_bytes);

        // size (8 bytes)
        buf.extend_from_slice(&self.size.to_le_bytes());

        // mtime_sec (8 bytes)
        buf.extend_from_slice(&self.mtime_sec.to_le_bytes());

        // mtime_nsec (4 bytes)
        buf.extend_from_slice(&self.mtime_nsec.to_le_bytes());

        // flags (2 bytes)
        buf.extend_from_slice(&self.flags.bits().to_le_bytes());

        // oid (20 bytes)
        buf.extend_from_slice(&self.oid);

        // reserved (64 bytes)
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

        // oid
        let mut oid = [0u8; 20];
        oid.copy_from_slice(&bytes[offset..offset + 20]);
        offset += 20;

        // reserved
        let mut reserved = [0u8; ENTRY_RESERVED_SIZE];
        reserved.copy_from_slice(&bytes[offset..offset + ENTRY_RESERVED_SIZE]);
        offset += ENTRY_RESERVED_SIZE;

        Ok((
            Self {
                path,
                size,
                mtime_sec,
                mtime_nsec,
                flags,
                oid,
                reserved,
            },
            offset,
        ))
    }
}
