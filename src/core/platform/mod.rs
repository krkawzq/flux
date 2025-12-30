//! Platform-specific abstractions
//!
//! This module provides cross-platform abstractions for:
//! - Background process management
//! - Directory paths
//! - Signal handling

#[cfg(unix)]
mod unix;

#[cfg(windows)]
mod windows;

use crate::core::error::Result;
use std::path::PathBuf;

/// Background service management trait
pub trait BackgroundService: Send + Sync {
    /// Spawn a background process
    fn spawn_background(&self, name: &str, args: Vec<String>) -> Result<u32>;

    /// Stop a background process by PID
    fn stop_background(&self, pid: u32) -> Result<()>;

    /// Check if a process is running
    fn is_running(&self, pid: u32) -> bool;

    /// Get log file path for a named service
    fn log_path(&self, name: &str) -> PathBuf;

    /// Get error log file path for a named service
    fn error_log_path(&self, name: &str) -> PathBuf;
}

/// Get the platform-specific background service implementation
pub fn get_background_service() -> Box<dyn BackgroundService> {
    #[cfg(unix)]
    {
        Box::new(unix::UnixBackgroundService::new())
    }

    #[cfg(windows)]
    {
        Box::new(windows::WindowsBackgroundService::new())
    }
}

/// Check if current platform is Windows
pub fn is_windows() -> bool {
    cfg!(windows)
}

/// Check if current platform is Unix-like
pub fn is_unix() -> bool {
    cfg!(unix)
}

/// Get the home directory
pub fn get_home_dir() -> Option<PathBuf> {
    dirs::home_dir()
}

/// Expand ~ in paths
pub fn expand_tilde(path: &str) -> PathBuf {
    if path.starts_with("~/") || path == "~" {
        if let Some(home) = get_home_dir() {
            return home.join(&path[2..]);
        }
    }
    PathBuf::from(path)
}
