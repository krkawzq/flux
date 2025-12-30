//! Configuration constants and global settings

use std::path::PathBuf;

/// Default SSH port
pub const DEFAULT_SSH_PORT: u16 = 22;

/// Default SSH connection timeout (seconds)
pub const DEFAULT_SSH_TIMEOUT: u64 = 10;

/// Default proxy local port
pub const DEFAULT_PROXY_LOCAL_PORT: u16 = 7890;

/// Default proxy remote port
pub const DEFAULT_PROXY_REMOTE_PORT: u16 = 1081;

/// Default proxy mode
pub const DEFAULT_PROXY_MODE: &str = "socks5";

/// Default proxy local host
pub const DEFAULT_PROXY_LOCAL_HOST: &str = "127.0.0.1";

/// Block marker prefix
pub const BLOCK_START_PREFIX: &str = "# >>> remote-block:";

/// Block marker suffix
pub const BLOCK_END_PREFIX: &str = "# <<< remote-block:";

/// Global region start marker
pub const GLOBAL_START_MARKER: &str = "# ========== REMOTE MANAGED REGION START ==========";

/// Global region end marker
pub const GLOBAL_END_MARKER: &str = "# ========== REMOTE MANAGED REGION END ==========";

/// Application name for directories
pub const APP_NAME: &str = "remote";

/// Get the data directory for the application
pub fn get_data_dir() -> PathBuf {
    #[cfg(unix)]
    {
        dirs::data_local_dir()
            .unwrap_or_else(|| PathBuf::from("~/.local/share"))
            .join(APP_NAME)
    }

    #[cfg(windows)]
    {
        dirs::data_local_dir()
            .unwrap_or_else(|| {
                std::env::var("APPDATA")
                    .map(PathBuf::from)
                    .unwrap_or_else(|_| PathBuf::from("C:\\ProgramData"))
            })
            .join(APP_NAME)
    }
}

/// Get the state directory (for proxy PIDs, etc.)
pub fn get_state_dir() -> PathBuf {
    get_data_dir().join("state")
}

/// Get the logs directory
pub fn get_logs_dir() -> PathBuf {
    get_data_dir().join("logs")
}

/// Get the manifests directory (for sync version tracking)
pub fn get_manifests_dir() -> PathBuf {
    get_data_dir().join("manifests")
}

/// Global configuration structure
#[derive(Debug, Clone)]
pub struct Config {
    /// Default shell backend preference
    pub shell_backend: Option<String>,

    /// Default conflict strategy for sync
    pub default_conflict_strategy: String,

    /// Enable verbose logging
    pub verbose: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            shell_backend: None,
            default_conflict_strategy: "reject".to_string(),
            verbose: false,
        }
    }
}
