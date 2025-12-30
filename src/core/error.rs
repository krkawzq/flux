//! Error types for the remote tool

use std::path::PathBuf;
use thiserror::Error;

/// Main error type for the remote tool
#[derive(Error, Debug)]
pub enum RemoteError {
    // === SSH Errors ===
    #[error("SSH connection failed: {0}")]
    SshConnection(String),

    #[error("SSH authentication failed: {0}")]
    SshAuth(String),

    #[error("SSH channel error: {0}")]
    SshChannel(String),

    // === SFTP Errors ===
    #[error("SFTP operation failed: {0}")]
    Sftp(String),

    #[error("Remote file not found: {path}")]
    RemoteFileNotFound { path: String },

    #[error("Remote path is not a file: {path}")]
    RemoteNotAFile { path: String },

    // === Sync Errors ===
    #[error("Sync error: {0}")]
    Sync(String),

    #[error("Block conflict: '{block_name}' was modified remotely (local: {local_hash}, remote: {remote_hash})")]
    BlockConflict {
        block_name: String,
        local_hash: String,
        remote_hash: String,
    },

    #[error("File conflict: '{path}' was modified on both sides")]
    FileConflict { path: String },

    // === Script Errors ===
    #[error("Script execution failed: {script} (exit code: {code})")]
    ScriptExecution {
        script: String,
        code: i32,
        stderr: String,
    },

    #[error("Script not found: {path}")]
    ScriptNotFound { path: PathBuf },

    #[error("No shell backend available")]
    NoShellBackend,

    // === Proxy Errors ===
    #[error("Proxy error: {0}")]
    Proxy(String),

    #[error("Proxy '{name}' is already running (PID: {pid})")]
    ProxyAlreadyRunning { name: String, pid: u32 },

    #[error("Proxy '{name}' is not running")]
    ProxyNotRunning { name: String },

    #[error("Tunnel error: {0}")]
    Tunnel(String),

    #[error("Tunnel connection lost for '{name}'")]
    TunnelConnectionLost { name: String },

    // === Configuration Errors ===
    #[error("Configuration error: {0}")]
    Config(String),

    #[error("Invalid configuration file: {path}: {reason}")]
    InvalidConfig { path: PathBuf, reason: String },

    #[error("SSH config entry not found: {name}")]
    SshConfigNotFound { name: String },

    // === State Errors ===
    #[error("State error: {0}")]
    State(String),

    // === Platform Errors ===
    #[error("Platform error: {0}")]
    Platform(String),

    #[error("Unsupported platform for operation: {operation}")]
    UnsupportedPlatform { operation: String },

    // === Standard Errors ===
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("TOML parse error: {0}")]
    TomlParse(#[from] toml::de::Error),

    #[error("TOML serialize error: {0}")]
    TomlSerialize(#[from] toml::ser::Error),
}

/// Result type alias using RemoteError
pub type Result<T> = std::result::Result<T, RemoteError>;

impl RemoteError {
    /// Create a new SSH connection error
    pub fn ssh_connection(msg: impl Into<String>) -> Self {
        Self::SshConnection(msg.into())
    }

    /// Create a new sync error
    pub fn sync(msg: impl Into<String>) -> Self {
        Self::Sync(msg.into())
    }

    /// Create a new proxy error
    pub fn proxy(msg: impl Into<String>) -> Self {
        Self::Proxy(msg.into())
    }

    /// Create a new config error
    pub fn config(msg: impl Into<String>) -> Self {
        Self::Config(msg.into())
    }
}
