//! Version tracking for sync operations
//!
//! Tracks file and block versions for intelligent sync decisions

use crate::core::config::get_manifests_dir;
use crate::core::error::{RemoteError, Result};
use crate::sync::models::{BlockVersion, FileVersion, SyncManifest};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

/// Version tracker for a specific remote machine
pub struct VersionTracker {
    machine_id: String,
    manifest_path: PathBuf,
    manifest: SyncManifest,
}

impl VersionTracker {
    /// Load or create a version tracker for a machine
    pub fn load(machine_id: &str) -> Result<Self> {
        let manifest_dir = get_manifests_dir();
        let _ = fs::create_dir_all(&manifest_dir);

        let manifest_path = manifest_dir.join(format!("{}.json", machine_id));

        let manifest = if manifest_path.exists() {
            let content = fs::read_to_string(&manifest_path)
                .map_err(|e| RemoteError::State(format!("Failed to read manifest: {}", e)))?;
            serde_json::from_str(&content)?
        } else {
            SyncManifest {
                machine_id: machine_id.to_string(),
                last_sync: 0,
                blocks: HashMap::new(),
                files: HashMap::new(),
            }
        };

        Ok(Self {
            machine_id: machine_id.to_string(),
            manifest_path,
            manifest,
        })
    }

    /// Save the manifest to disk
    pub fn save(&self) -> Result<()> {
        let content = serde_json::to_string_pretty(&self.manifest)?;
        fs::write(&self.manifest_path, content)
            .map_err(|e| RemoteError::State(format!("Failed to save manifest: {}", e)))?;
        Ok(())
    }

    /// Get file version info
    pub fn get_file_version(&self, path: &str) -> Option<&FileVersion> {
        self.manifest.files.get(path)
    }

    /// Update file version
    pub fn update_file_version(&mut self, path: &str, hash: String, mtime: i64, size: u64) {
        self.manifest
            .files
            .insert(path.to_string(), FileVersion { hash, mtime, size });
    }

    /// Get block version info
    pub fn get_block_version(&self, name: &str) -> Option<&BlockVersion> {
        self.manifest.blocks.get(name)
    }

    /// Update block version
    pub fn update_block_version(&mut self, name: &str, hash: String, mtime: i64) {
        let version = self
            .manifest
            .blocks
            .get(name)
            .map(|v| v.version + 1)
            .unwrap_or(1);

        self.manifest.blocks.insert(
            name.to_string(),
            BlockVersion {
                hash,
                mtime,
                version,
                synced_at: chrono::Utc::now().timestamp(),
            },
        );
    }

    /// Update last sync timestamp
    pub fn update_last_sync(&mut self) {
        self.manifest.last_sync = chrono::Utc::now().timestamp();
    }

    /// Check if this is the first sync
    pub fn is_first_sync(&self) -> bool {
        self.manifest.last_sync == 0
    }

    /// Get the machine ID
    pub fn machine_id(&self) -> &str {
        &self.machine_id
    }
}

/// Calculate SHA256 hash of content
pub fn hash_content(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    let result = hasher.finalize();
    format!("{:x}", result)[..16].to_string()
}

/// Calculate SHA256 hash of a file
pub fn hash_file(path: &PathBuf) -> Result<String> {
    let content = fs::read_to_string(path)
        .map_err(|e| RemoteError::Sync(format!("Failed to read file for hashing: {}", e)))?;
    Ok(hash_content(&content))
}

/// Generate a machine ID from host and user
pub fn generate_machine_id(host: &str, user: &str) -> String {
    let input = format!("{}@{}", user, host);
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    let result = hasher.finalize();
    format!("{:x}", result)[..12].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hash_content() {
        let hash1 = hash_content("hello world");
        let hash2 = hash_content("hello world");
        let hash3 = hash_content("different content");

        assert_eq!(hash1, hash2);
        assert_ne!(hash1, hash3);
        assert_eq!(hash1.len(), 16);
    }

    #[test]
    fn test_generate_machine_id() {
        let id1 = generate_machine_id("example.com", "user");
        let id2 = generate_machine_id("example.com", "user");
        let id3 = generate_machine_id("other.com", "user");

        assert_eq!(id1, id2);
        assert_ne!(id1, id3);
        assert_eq!(id1.len(), 12);
    }
}
