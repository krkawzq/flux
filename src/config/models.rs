//! Configuration data models

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Root configuration structure
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FluxConfig {
    /// Inherit from another config file
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inherit: Option<String>,

    /// Connection settings
    #[serde(default)]
    pub connection: ConnectionConfig,

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

/// File synchronization mode
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum SyncMode {
    /// Only sync if target doesn't exist
    Init,
    /// Sync if source is newer (default)
    #[default]
    Update,
    /// Force overwrite target
    Cover,
    /// Bidirectional sync based on mtime
    Sync,
    /// Mirror mode - delete extra files
    Mirror,
}

/// Conflict resolution strategy
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ConflictStrategy {
    /// Reject and report error (default)
    #[default]
    Reject,
    /// Force overwrite
    Force,
    /// Backup before overwriting
    Backup,
    /// Ask user interactively
    Ask,
    /// Attempt to merge
    Merge,
}

/// Block group mode
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum BlockGroupMode {
    /// Preserve unknown blocks (default)
    #[default]
    Incremental,
    /// Delete unknown blocks
    Overwrite,
}

/// Script execution timing
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ScriptMode {
    /// Only on first connection
    Init,
    /// Every sync (default)
    #[default]
    Always,
}

/// Script execution method
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ExecMode {
    /// Direct execution (default)
    #[default]
    Exec,
    /// Source the script
    Source,
}

// === Resolved Configuration ===

/// Fully resolved configuration (no placeholders)
#[derive(Debug, Clone)]
pub struct ResolvedConfig {
    pub connection: ResolvedConnection,
    pub proxy: ProxyConfigSection,
    pub files: Vec<FileSyncRule>,
    pub blocks: Vec<BlockSyncRule>,
    pub scripts: Vec<ScriptRule>,
    pub env: EnvConfig,
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
