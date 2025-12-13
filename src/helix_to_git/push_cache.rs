// Minimal push cache - tracks Helix BLAKE3 → Git SHA1 mappings
// Stored in .helix/git-push-cache as simple text file

use crate::helix_index::hash::{hash_to_hex, hex_to_hash, Hash};
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

/// Push cache - maps Helix hashes to Git SHAs
pub struct PushCache {
    cache_path: PathBuf,
    /// Helix BLAKE3 hash (32 bytes) → Git SHA1 (as hex string)
    mappings: HashMap<Hash, String>,
}

impl PushCache {
    /// Load cache from .helix/git-push-cache
    pub fn load(repo_path: &Path) -> Result<Self> {
        let cache_path = repo_path.join(".helix").join("git-push-cache");
        let mut mappings = HashMap::new();

        if cache_path.exists() {
            let content = fs::read_to_string(&cache_path).context("Failed to read push cache")?;

            for (line_num, line) in content.lines().enumerate() {
                let line = line.trim();

                // Skip empty lines and comments
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }

                // Parse: blake3_hex git_sha1_hex
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() != 2 {
                    eprintln!(
                        "Warning: Invalid line {} in push cache: '{}'",
                        line_num + 1,
                        line
                    );
                    continue;
                }

                let helix_hash = hex_to_hash(parts[0]).with_context(|| {
                    format!("Invalid Helix hash on line {}: {}", line_num + 1, parts[0])
                })?;

                let git_sha = parts[1].to_string();

                // Validate Git SHA format (40 hex chars)
                if git_sha.len() != 40 || !git_sha.chars().all(|c| c.is_ascii_hexdigit()) {
                    eprintln!(
                        "Warning: Invalid Git SHA on line {}: {}",
                        line_num + 1,
                        git_sha
                    );
                    continue;
                }

                mappings.insert(helix_hash, git_sha);
            }
        }

        Ok(Self {
            cache_path,
            mappings,
        })
    }

    /// Get Git SHA for a Helix hash
    pub fn get(&self, helix_hash: &Hash) -> Option<&str> {
        self.mappings.get(helix_hash).map(|s| s.as_str())
    }

    /// Check if we've already converted this commit
    pub fn contains(&self, helix_hash: &Hash) -> bool {
        self.mappings.contains_key(helix_hash)
    }

    /// Insert new mapping (in-memory only, call save() to persist)
    pub fn insert(&mut self, helix_hash: Hash, git_sha: String) {
        self.mappings.insert(helix_hash, git_sha);
    }

    /// Persist cache to disk (append-only)
    pub fn save(&self) -> Result<()> {
        // Ensure .helix directory exists
        if let Some(parent) = self.cache_path.parent() {
            fs::create_dir_all(parent)?;
        }

        // Write entire cache (simple approach for now)
        // Could optimize to append-only in the future
        let mut content = String::from("# Helix → Git push cache\n");
        content.push_str("# Format: helix_blake3_hash git_sha1_hash\n\n");

        // Sort for deterministic output
        let mut entries: Vec<_> = self.mappings.iter().collect();
        entries.sort_by_key(|(hash, _)| *hash);

        for (helix_hash, git_sha) in entries {
            content.push_str(&format!("{} {}\n", hash_to_hex(helix_hash), git_sha));
        }

        fs::write(&self.cache_path, content)
            .with_context(|| format!("Failed to write push cache to {:?}", self.cache_path))?;

        Ok(())
    }

    /// Append single entry to cache file (efficient for incremental updates)
    pub fn append(&self, helix_hash: &Hash, git_sha: &str) -> Result<()> {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.cache_path)
            .with_context(|| format!("Failed to open push cache {:?}", self.cache_path))?;

        writeln!(file, "{} {}", hash_to_hex(helix_hash), git_sha)
            .context("Failed to append to push cache")?;

        Ok(())
    }

    /// Get number of cached mappings
    pub fn len(&self) -> usize {
        self.mappings.len()
    }

    /// Check if cache is empty
    pub fn is_empty(&self) -> bool {
        self.mappings.is_empty()
    }

    /// Clear all mappings (useful for testing)
    #[allow(dead_code)]
    pub fn clear(&mut self) {
        self.mappings.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_push_cache_load_empty() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        let cache = PushCache::load(repo_path)?;
        assert!(cache.is_empty());

        Ok(())
    }

    #[test]
    fn test_push_cache_insert_and_get() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        let mut cache = PushCache::load(repo_path)?;

        let helix_hash = [1u8; 32];
        let git_sha = "a".repeat(40); // Valid 40-char hex

        cache.insert(helix_hash, git_sha.clone());

        assert_eq!(cache.get(&helix_hash), Some(git_sha.as_str()));
        assert!(cache.contains(&helix_hash));
        assert_eq!(cache.len(), 1);

        Ok(())
    }

    #[test]
    fn test_push_cache_save_and_load() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        // Create and populate cache
        let mut cache = PushCache::load(repo_path)?;
        let helix_hash1 = [1u8; 32];
        let helix_hash2 = [2u8; 32];

        cache.insert(helix_hash1, "a".repeat(40));
        cache.insert(helix_hash2, "b".repeat(40));

        // Save
        cache.save()?;

        // Load in new instance
        let loaded_cache = PushCache::load(repo_path)?;

        assert_eq!(loaded_cache.len(), 2);
        assert_eq!(loaded_cache.get(&helix_hash1), Some(&"a".repeat(40)[..]));
        assert_eq!(loaded_cache.get(&helix_hash2), Some(&"b".repeat(40)[..]));

        Ok(())
    }

    #[test]
    fn test_push_cache_append() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();

        let cache = PushCache::load(repo_path)?;
        let helix_hash = [1u8; 32];
        let git_sha = "c".repeat(40);

        cache.append(&helix_hash, &git_sha)?;

        // Reload to verify
        let loaded_cache = PushCache::load(repo_path)?;
        assert_eq!(loaded_cache.get(&helix_hash), Some(git_sha.as_str()));

        Ok(())
    }

    #[test]
    fn test_push_cache_handles_invalid_lines() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let repo_path = temp_dir.path();
        let cache_path = repo_path.join(".helix").join("git-push-cache");

        fs::create_dir_all(cache_path.parent().unwrap())?;

        // Write cache with invalid lines
        fs::write(
            &cache_path,
            "# Comment\n\
             0000000000000000000000000000000000000000000000000000000000000000 aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n\
             invalid line\n\
             0000000000000000000000000000000000000000000000000000000000000001 bad_sha\n\
             0000000000000000000000000000000000000000000000000000000000000002 bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\n",
        )?;

        let cache = PushCache::load(repo_path)?;

        // Should have loaded 2 valid entries, skipped invalid ones
        assert_eq!(cache.len(), 2);

        Ok(())
    }
}
