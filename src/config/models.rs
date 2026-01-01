//! Configuration data models

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

// Re-export sync models for compatibility
pub use crate::sync::models::{
    SyncMode, ConflictStrategy, BlockGroupMode, ScriptMode, ExecMode
};

/// Root configuration structure
/// Supports both flat format (host = "...") and nested format ([connection] host = "...")
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FluxConfig {
    /// Inherit from another config file
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inherit: Option<String>,

    /// Connection settings (nested format)
    #[serde(default)]
    pub connection: ConnectionConfig,

    // === Flat connection fields (override nested [connection] if present) ===
    /// Host address (flat format)
    #[serde(default)]
    pub host: Option<String>,
    /// SSH user (flat format)
    #[serde(default)]
    pub user: Option<String>,
    /// SSH port (flat format)
    #[serde(default)]
    pub port: Option<u16>,
    /// SSH password (flat format)
    #[serde(default)]
    pub password: Option<String>,
    /// Path to SSH private key (flat format)
    #[serde(default)]
    pub key: Option<String>,
    /// Reference to SSH config host name (flat format)
    #[serde(default)]
    pub ssh_config: Option<String>,

    /// Proxy settings
    #[serde(default)]
    pub proxy: ProxyConfigSection,

    /// File sync rules
    #[serde(default, rename = "file")]
    pub files: Vec<FileSyncRule>,

    /// Block sync rules
    #[serde(default, rename = "block")]
    pub blocks: Vec<BlockSyncRule>,

    /// Script execution rules
    #[serde(default, rename = "script")]
    pub scripts: Vec<ScriptRule>,

    /// Global environment settings
    #[serde(default)]
    pub env: EnvConfig,

    // === Additional sync settings ===
    /// Block home directory (relative to .flux/)
    #[serde(default)]
    pub block_home: Option<String>,
    
    /// Script home directory (relative to .flux/)
    #[serde(default)]
    pub script_home: Option<String>,
    
    /// Whether to add public key to authorized_keys on first connect
    #[serde(default)]
    pub add_authorized_key: bool,
}

impl FluxConfig {
    /// Get effective connection config (flat fields override nested)
    pub fn effective_connection(&self) -> ConnectionConfig {
        // For port, use flat field if set, otherwise nested, with fallback to 22
        let port = self.port
            .or(if self.connection.port != 0 { Some(self.connection.port) } else { None })
            .unwrap_or(22);

        // For user, use flat field if set, otherwise nested, with fallback to "root"
        let user = self.user.clone()
            .or(if !self.connection.user.is_empty() { Some(self.connection.user.clone()) } else { None })
            .unwrap_or_else(|| "root".to_string());

        ConnectionConfig {
            host: self.host.clone().unwrap_or_else(|| self.connection.host.clone()),
            user,
            port,
            key: self.key.clone().or_else(|| self.connection.key.clone()),
            password: self.password.clone().or_else(|| self.connection.password.clone()),
            ssh_config: self.ssh_config.clone().or_else(|| self.connection.ssh_config.clone()),
        }
    }
}

/// SSH connection configuration
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ConnectionConfig {
    /// Host address (supports {{host}} placeholder)
    #[serde(default)]
    pub host: String,

    /// SSH user (supports {{user:root}} placeholder with default)
    #[serde(default = "default_user")]
    pub user: String,

    /// SSH port (supports {{port:22}} placeholder with default)
    #[serde(default = "default_port")]
    pub port: u16,

    /// Path to SSH private key
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,

    /// SSH password (not recommended, use key instead)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,

    /// Reference to SSH config host name
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ssh_config: Option<String>,
}

fn default_user() -> String {
    "root".to_string()
}

fn default_port() -> u16 {
    22
}

/// Proxy configuration section
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyConfigSection {
    /// Enable proxy during sync
    #[serde(default)]
    pub enabled: bool,

    /// Remote port on the server
    #[serde(default = "default_remote_port")]
    pub remote_port: u16,

    /// Local port for proxy
    #[serde(default = "default_local_port")]
    pub local_port: u16,

    /// Proxy mode: socks5 or http
    #[serde(default = "default_proxy_mode")]
    pub mode: String,

    /// Use built-in proxy server
    #[serde(default)]
    pub builtin: bool,

    /// Auto-set HTTP_PROXY environment variables
    #[serde(default = "default_true")]
    pub set_env: bool,
}

fn default_remote_port() -> u16 {
    1081
}

fn default_local_port() -> u16 {
    7890
}

fn default_proxy_mode() -> String {
    "socks5".to_string()
}

fn default_true() -> bool {
    true
}

impl Default for ProxyConfigSection {
    fn default() -> Self {
        Self {
            enabled: false,
            remote_port: default_remote_port(),
            local_port: default_local_port(),
            mode: default_proxy_mode(),
            builtin: false,
            set_env: true,
        }
    }
}

/// File synchronization rule
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileSyncRule {
    /// Source path (local file)
    /// - Relative path: resolved relative to .flux/files/ directory
    /// - Absolute path or ~/: resolved as-is
    /// - Example: "bashrc" -> .flux/files/bashrc
    /// - Example: "config/app.conf" -> .flux/files/config/app.conf
    /// - Example: "~/.ssh/id_rsa" -> ~/.ssh/id_rsa (absolute)
    pub src: String,

    /// Destination path (`:` prefix for remote)
    pub dist: String,

    /// Sync mode
    #[serde(default)]
    pub mode: SyncMode,

    /// Conflict resolution strategy
    #[serde(default)]
    pub conflict: ConflictStrategy,

    /// Conditional expression
    #[serde(skip_serializing_if = "Option::is_none")]
    pub condition: Option<String>,

    /// Exclude patterns (glob)
    #[serde(default)]
    pub excludes: Vec<String>,
}

/// Block synchronization rule
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockSyncRule {
    /// Destination file path (remote, : prefix)
    pub dist: String,

    /// Source block files
    /// - Relative path: resolved relative to .flux/blocks/ directory
    /// - Absolute path or ~/: resolved as-is
    /// - Example: "bashrc.block" -> .flux/blocks/bashrc.block
    #[serde(default)]
    pub blocks: Vec<String>,

    /// Block group mode
    #[serde(default)]
    pub mode: BlockGroupMode,

    /// Conflict strategy
    #[serde(default)]
    pub conflict: ConflictStrategy,
}

/// Script execution rule
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScriptRule {
    /// Script source path
    /// - Relative path: resolved relative to .flux/scripts/ directory
    /// - Absolute path or ~/: resolved as-is
    /// - Remote path (: prefix): executed directly on remote
    /// - Example: "init.sh" -> .flux/scripts/init.sh
    pub src: String,

    /// Execution timing
    #[serde(default)]
    pub mode: ScriptMode,

    /// Execution method
    #[serde(default)]
    pub exec_mode: ExecMode,

    /// Custom interpreter
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interpreter: Option<String>,

    /// Interpreter flags
    #[serde(default)]
    pub flags: Vec<String>,

    /// Script arguments
    #[serde(default)]
    pub args: Vec<String>,

    /// Allow non-zero exit
    #[serde(default)]
    pub allow_fail: bool,
}

/// Global environment configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvConfig {
    /// Default interpreter
    #[serde(default = "default_interpreter")]
    pub interpreter: String,

    /// Default interpreter flags
    #[serde(default)]
    pub flags: Vec<String>,
}

fn default_interpreter() -> String {
    "/bin/bash".to_string()
}

impl Default for EnvConfig {
    fn default() -> Self {
        Self {
            interpreter: default_interpreter(),
            flags: vec![],
        }
    }
}

// === Enums ===

// === Resolved Configuration ===
// Note: Enums are re-exported from sync::models at the top of this file

/// Fully resolved configuration (no placeholders)
/// Uses sync-compatible types for direct use in sync operations
#[derive(Debug, Clone)]
pub struct ResolvedConfig {
    pub connection: ResolvedConnection,
    pub proxy: ProxyConfigSection,
    pub files: Vec<crate::sync::models::FileSync>,
    pub blocks: Vec<crate::sync::models::BlockGroup>,
    pub scripts: Vec<crate::sync::models::ScriptExec>,
    pub env: crate::sync::models::GlobalEnv,
    pub block_home: Option<String>,
    pub script_home: Option<String>,
    pub add_authorized_key: bool,
}

/// Resolved connection (all values filled)
#[derive(Debug, Clone)]
pub struct ResolvedConnection {
    pub host: String,
    pub user: String,
    pub port: u16,
    pub key: Option<PathBuf>,
    pub password: Option<String>,
}

impl ResolvedConfig {
    /// Check if proxy should be enabled
    pub fn proxy_enabled(&self) -> bool {
        self.proxy.enabled
    }
}
