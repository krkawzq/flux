//! Sync domain models

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// File synchronization mode
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum SyncMode {
    /// Only sync if target doesn't exist
    Init,
    /// Sync if source is newer
    #[default]
    Update,
    /// Force overwrite target
    Cover,
    /// Bidirectional sync based on mtime
    Sync,
    /// Mirror mode - delete extra files in target
    Mirror,
}

/// Conflict resolution strategy
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ConflictStrategy {
    /// Reject update and report error
    #[default]
    Reject,
    /// Force overwrite
    Force,
    /// Backup before overwriting
    Backup,
    /// Ask user interactively
    Ask,
    /// Attempt to merge (text files only)
    Merge,
}

/// File sync configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileSync {
    /// Source path (local or remote with ":" prefix)
    pub src: String,
    /// Destination path (local or remote with ":" prefix)
    pub dist: String,
    /// Sync mode
    #[serde(default)]
    pub mode: SyncMode,
    /// Conflict resolution strategy
    #[serde(default)]
    pub conflict: ConflictStrategy,
    /// Conditional expression (e.g., "is_first_connect")
    #[serde(default)]
    pub condition: Option<String>,
    /// Exclude patterns (glob)
    #[serde(default)]
    pub excludes: Vec<String>,
}

/// Text block for incremental config sync
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextBlock {
    /// Source file paths
    pub src: Vec<String>,
    /// Sync mode
    #[serde(default)]
    pub mode: SyncMode,
    /// Conflict strategy
    #[serde(default)]
    pub conflict: ConflictStrategy,
}

impl TextBlock {
    /// Generate block name from first source path
    pub fn get_name(&self) -> String {
        self.src
            .first().cloned()
            .unwrap_or_else(|| "unnamed".to_string())
    }
}

/// Block group - multiple blocks targeting one file
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockGroup {
    /// Destination file path (remote)
    pub dist: String,
    /// Group mode: incremental or overwrite
    #[serde(default)]
    pub mode: BlockGroupMode,
    /// Blocks in this group
    pub blocks: Vec<TextBlock>,
}

/// Block group mode
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum BlockGroupMode {
    /// Preserve unknown blocks
    #[default]
    Incremental,
    /// Delete unknown blocks
    Overwrite,
}

/// Script execution configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScriptExec {
    /// Script source path (local or remote with ":" prefix)
    pub src: String,
    /// Execution timing
    #[serde(default)]
    pub mode: ScriptMode,
    /// Execution method
    #[serde(default)]
    pub exec_mode: ExecMode,
    /// Interpreter path (optional)
    pub interpreter: Option<String>,
    /// Interpreter flags
    #[serde(default)]
    pub flags: Vec<String>,
    /// Script arguments
    #[serde(default)]
    pub args: Vec<String>,
    /// Allow non-zero exit codes
    #[serde(default)]
    pub allow_fail: bool,
}

/// Script execution timing
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ScriptMode {
    /// Only on first connection
    Init,
    /// Every sync
    #[default]
    Always,
}

/// Script execution method
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ExecMode {
    /// Direct execution
    #[default]
    Exec,
    /// Source the script
    Source,
}

/// Global interpreter environment
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GlobalEnv {
    /// Default interpreter path
    #[serde(default = "default_interpreter")]
    pub interpreter: String,
    /// Default interpreter flags
    #[serde(default)]
    pub flags: Vec<String>,
}

fn default_interpreter() -> String {
    "/bin/bash".to_string()
}

impl Default for GlobalEnv {
    fn default() -> Self {
        Self {
            interpreter: default_interpreter(),
            flags: vec![],
        }
    }
}

// === Version Tracking ===

/// Block version information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockVersion {
    pub hash: String,
    pub mtime: i64,
    pub version: u32,
    pub synced_at: i64,
}

/// File version information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileVersion {
    pub hash: String,
    pub mtime: i64,
    pub size: u64,
}

/// Sync manifest - tracks versions across syncs
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SyncManifest {
    /// Machine identifier
    pub machine_id: String,
    /// Last sync timestamp
    pub last_sync: i64,
    /// Block versions
    #[serde(default)]
    pub blocks: HashMap<String, BlockVersion>,
    /// File versions
    #[serde(default)]
    pub files: HashMap<String, FileVersion>,
}

// === Sync Configuration ===

/// Complete sync configuration (parsed from TOML)
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SyncConfig {
    // Connection
    pub host: Option<String>,
    pub user: Option<String>,
    pub port: Option<u16>,
    pub password: Option<String>,
    pub key: Option<String>,
    pub ssh_config: Option<String>,

    // Options
    #[serde(default)]
    pub add_authorized_key: bool,

    // Paths
    pub block_home: Option<String>,
    pub script_home: Option<String>,

    // Sync items
    #[serde(default, rename = "file")]
    pub files: Vec<FileSync>,

    // Block configuration
    pub block: Option<BlockGroup>,

    // Scripts
    #[serde(default, rename = "script")]
    pub scripts: Vec<ScriptExec>,

    // Global environment
    #[serde(default)]
    pub env: GlobalEnv,
}
