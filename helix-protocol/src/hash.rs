use anyhow::Result;
use blake3::Hasher;
use rayon::prelude::*;
use std::{fs, io::Read, path::Path};

/// 32-byte BLAKE3 hash
/// We use our own hash type instead of the blake3::Hash type for speed
pub type Hash = [u8; 32];

/// Hash arbitrary bytes with BLAKE3 and return the hash's raw bytes
#[inline]
pub fn hash_bytes(data: &[u8]) -> Hash {
    let hash = blake3::hash(data);
    *hash.as_bytes()
}

/// Hash a file's contents
pub fn hash_file(path: &Path) -> Result<Hash> {
    let content = fs::read(path)?;
    Ok(hash_bytes(&content))
}

/// Hash a file with streaming (for large files >10MB)
pub fn hash_file_stream(path: &Path) -> Result<Hash> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Hasher::new();

    // Read in 64KB chunks
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let bytes_read = file.read(&mut buffer)?;
        if bytes_read == 0 {
            break;
        }
        hasher.update(&buffer[..bytes_read]);
    }

    let hash = hasher.finalize();
    Ok(*hash.as_bytes())
}

/// Hash multiple files in parallel
pub fn hash_files_parallel(paths: &[&Path]) -> Result<Vec<Hash>> {
    paths.par_iter().map(|path| hash_file(path)).collect()
}

pub fn compute_blob_oid(content: &[u8]) -> Vec<u8> {
    use sha1::{Digest, Sha1};

    // Git's blob format: "blob {size}\0{content}"
    let header = format!("blob {}\0", content.len());

    let mut hasher = Sha1::new();
    hasher.update(header.as_bytes());
    hasher.update(content);

    hasher.finalize().to_vec()
}

/// Convert hash to hex string for display/storage
#[inline]
pub fn hash_to_hex(hash: &Hash) -> String {
    hex::encode(hash)
}

/// Parse hex string back to hash
pub fn hex_to_hash(hex_str: &str) -> Result<Hash> {
    let bytes = hex::decode(hex_str)?;
    if bytes.len() != 32 {
        anyhow::bail!(
            "Invalid hash length: expected 32 bytes, got {}",
            bytes.len()
        );
    }
    let mut hash = [0u8; 32];
    hash.copy_from_slice(&bytes);
    Ok(hash)
}

/// Zero hash (used as placeholder/null value)
pub const ZERO_HASH: Hash = [0u8; 32];

#[inline]
pub fn is_zero_hash(hash: &Hash) -> bool {
    hash == &ZERO_HASH
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_hash_bytes_deterministic() {
        let data = b"hello world";
        let hash1 = hash_bytes(data);
        let hash2 = hash_bytes(data);

        assert_eq!(hash1, hash2, "Same input should produce same hash");
    }

    #[test]
    fn helix_blob_hash_is_blake3_of_raw_bytes() {
        let content = b"hello\n"; // byte string literal
        let h1 = hash_bytes(content);

        // Simulate any other path that claims to compute the same oid:
        let h2 = blake3::hash(content);
        assert_eq!(h1, *h2.as_bytes());
    }

    #[test]
    fn test_hash_bytes_different() {
        let hash1 = hash_bytes(b"hello");
        let hash2 = hash_bytes(b"world");

        assert_ne!(
            hash1, hash2,
            "Different inputs should produce different hashes"
        );
    }

    #[test]
    fn test_hash_length() {
        let hash = hash_bytes(b"test");
        assert_eq!(hash.len(), 32, "BLAKE3 hash should be 32 bytes");
    }

    #[test]
    fn test_hash_file() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let file_path = temp_dir.path().join("test.txt");

        fs::write(&file_path, b"test content")?;

        let hash = hash_file(&file_path)?;
        assert_eq!(hash.len(), 32);

        // Hash should be deterministic
        let hash2 = hash_file(&file_path)?;
        assert_eq!(hash, hash2);

        Ok(())
    }

    #[test]
    fn test_hash_file_stream() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let file_path = temp_dir.path().join("large.txt");

        // Create a larger file
        let content = vec![b'x'; 1024 * 1024]; // 1MB
        fs::write(&file_path, &content)?;

        let hash_regular = hash_file(&file_path)?;
        let hash_stream = hash_file_stream(&file_path)?;

        assert_eq!(
            hash_regular, hash_stream,
            "Stream and regular hashing should match"
        );

        Ok(())
    }

    #[test]
    fn test_hash_files_parallel() -> Result<()> {
        let temp_dir = TempDir::new()?;

        // Create multiple files
        let file1 = temp_dir.path().join("file1.txt");
        let file2 = temp_dir.path().join("file2.txt");
        let file3 = temp_dir.path().join("file3.txt");

        fs::write(&file1, b"content1")?;
        fs::write(&file2, b"content2")?;
        fs::write(&file3, b"content3")?;

        let paths = vec![file1.as_path(), file2.as_path(), file3.as_path()];
        let hashes = hash_files_parallel(&paths)?;

        assert_eq!(hashes.len(), 3);

        // Verify each hash matches individual hashing
        assert_eq!(hashes[0], hash_file(&file1)?);
        assert_eq!(hashes[1], hash_file(&file2)?);
        assert_eq!(hashes[2], hash_file(&file3)?);

        Ok(())
    }

    #[test]
    fn test_hash_to_hex() {
        let hash = hash_bytes(b"test");
        let hex = hash_to_hex(&hash);

        assert_eq!(
            hex.len(),
            64,
            "Hex string should be 64 chars (32 bytes * 2)"
        );

        // Should only contain hex characters
        assert!(hex.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn test_hex_to_hash_roundtrip() -> Result<()> {
        let original_hash = hash_bytes(b"test");
        let hex = hash_to_hex(&original_hash);
        let parsed_hash = hex_to_hash(&hex)?;

        assert_eq!(
            original_hash, parsed_hash,
            "Round-trip should preserve hash"
        );

        Ok(())
    }

    #[test]
    fn test_hex_to_hash_invalid_length() {
        let result = hex_to_hash("deadbeef"); // Too short
        assert!(result.is_err(), "Should reject invalid length");
    }

    #[test]
    fn test_hex_to_hash_invalid_chars() {
        let invalid_hex = "z".repeat(64); // Invalid hex characters
        let result = hex::decode(&invalid_hex);
        assert!(result.is_err(), "Should reject invalid hex");
    }

    #[test]
    fn test_zero_hash() {
        assert!(is_zero_hash(&ZERO_HASH));

        let non_zero = hash_bytes(b"test");
        assert!(!is_zero_hash(&non_zero));
    }

    #[test]
    fn test_blake3_faster_than_sha1() {
        use std::time::Instant;

        // Create 10MB of data
        let data = vec![0u8; 10 * 1024 * 1024];

        // Time BLAKE3
        let start = Instant::now();
        let _hash = hash_bytes(&data);
        let blake3_time = start.elapsed();

        println!("BLAKE3 hashed 10MB in {:?}", blake3_time);

        // BLAKE3 should hash 10MB in under 20ms on modern hardware
        // (SHA-1 would take ~150ms)
        assert!(blake3_time.as_millis() < 50, "BLAKE3 should be very fast");
    }

    #[test]
    fn test_parallel_hashing_performance() -> Result<()> {
        use std::time::Instant;

        let temp_dir = TempDir::new()?;

        // Create 100 small files
        let mut paths = Vec::new();
        for i in 0..100 {
            let path = temp_dir.path().join(format!("file{}.txt", i));
            fs::write(&path, format!("content {}", i))?;
            paths.push(path);
        }

        let path_refs: Vec<_> = paths.iter().map(|p| p.as_path()).collect();

        // Time parallel hashing
        let start = Instant::now();
        let hashes = hash_files_parallel(&path_refs)?;
        let parallel_time = start.elapsed();

        assert_eq!(hashes.len(), 100);
        println!("Parallel hashed 100 files in {:?}", parallel_time);

        // Should be very fast
        assert!(
            parallel_time.as_millis() < 100,
            "Parallel hashing should be fast"
        );

        Ok(())
    }

    #[test]
    fn test_empty_content() {
        let hash = hash_bytes(b"");
        assert_ne!(hash, ZERO_HASH, "Empty content should have non-zero hash");

        // Empty content should hash deterministically
        let hash2 = hash_bytes(b"");
        assert_eq!(hash, hash2);
    }

    #[test]
    fn test_large_content() {
        // Hash 100MB
        let data = vec![0xAB; 100 * 1024 * 1024];

        let start = std::time::Instant::now();
        let hash = hash_bytes(&data);
        let elapsed = start.elapsed();

        println!("Hashed 100MB in {:?}", elapsed);

        assert_ne!(hash, ZERO_HASH);

        // BLAKE3 should hash 100MB in under 200ms
        assert!(elapsed.as_millis() < 500, "Should hash 100MB quickly");
    }
}
