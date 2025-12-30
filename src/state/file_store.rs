//! File-based state storage

use crate::core::config::get_state_dir;
use crate::core::error::{RemoteError, Result};
use serde::{de::DeserializeOwned, Serialize};
use std::fs;
use std::path::PathBuf;

/// File-based state store for proxy instances and manifests
pub struct FileStateStore {
    state_dir: PathBuf,
}

impl FileStateStore {
    /// Create a new file state store
    pub fn new() -> Self {
        let state_dir = get_state_dir();

        // Ensure directory exists
        let _ = fs::create_dir_all(&state_dir);

        Self { state_dir }
    }

    /// Get the state directory path
    pub fn state_dir(&self) -> &PathBuf {
        &self.state_dir
    }

    /// Save state for a named instance
    pub fn save<T: Serialize>(&self, name: &str, state: &T) -> Result<()> {
        let path = self.state_path(name);
        let json = serde_json::to_string_pretty(state)?;
        fs::write(&path, json)
            .map_err(|e| RemoteError::State(format!("Failed to save state: {}", e)))?;
        Ok(())
    }

    /// Load state for a named instance
    pub fn load<T: DeserializeOwned>(&self, name: &str) -> Result<Option<T>> {
        let path = self.state_path(name);
        if !path.exists() {
            return Ok(None);
        }

        let content = fs::read_to_string(&path)
            .map_err(|e| RemoteError::State(format!("Failed to read state: {}", e)))?;
        let state: T = serde_json::from_str(&content)?;
        Ok(Some(state))
    }

    /// Delete state for a named instance
    pub fn delete(&self, name: &str) -> Result<()> {
        let path = self.state_path(name);
        if path.exists() {
            fs::remove_file(&path)
                .map_err(|e| RemoteError::State(format!("Failed to delete state: {}", e)))?;
        }

        // Also remove PID file
        let pid_path = self.pid_path(name);
        if pid_path.exists() {
            let _ = fs::remove_file(&pid_path);
        }

        Ok(())
    }

    /// Check if state exists
    pub fn exists(&self, name: &str) -> bool {
        self.state_path(name).exists()
    }

    /// List all instance names
    pub fn list(&self) -> Result<Vec<String>> {
        let mut names = Vec::new();

        if !self.state_dir.exists() {
            return Ok(names);
        }

        for entry in fs::read_dir(&self.state_dir)
            .map_err(|e| RemoteError::State(format!("Failed to read state dir: {}", e)))?
        {
            let entry =
                entry.map_err(|e| RemoteError::State(format!("Failed to read entry: {}", e)))?;
            let path = entry.path();

            if path.extension().map(|e| e == "json").unwrap_or(false) {
                if let Some(name) = path.file_stem() {
                    names.push(name.to_string_lossy().to_string());
                }
            }
        }

        Ok(names)
    }

    /// Save PID for a named instance
    pub fn save_pid(&self, name: &str, pid: u32) -> Result<()> {
        let path = self.pid_path(name);
        fs::write(&path, pid.to_string())
            .map_err(|e| RemoteError::State(format!("Failed to save PID: {}", e)))?;
        Ok(())
    }

    /// Load PID for a named instance
    pub fn load_pid(&self, name: &str) -> Result<Option<u32>> {
        let path = self.pid_path(name);
        if !path.exists() {
            return Ok(None);
        }

        let content = fs::read_to_string(&path)
            .map_err(|e| RemoteError::State(format!("Failed to read PID: {}", e)))?;
        let pid: u32 = content
            .trim()
            .parse()
            .map_err(|e| RemoteError::State(format!("Invalid PID: {}", e)))?;
        Ok(Some(pid))
    }

    fn state_path(&self, name: &str) -> PathBuf {
        self.state_dir.join(format!("{}.json", name))
    }

    fn pid_path(&self, name: &str) -> PathBuf {
        self.state_dir.join(format!("{}.pid", name))
    }
}

impl Default for FileStateStore {
    fn default() -> Self {
        Self::new()
    }
}
