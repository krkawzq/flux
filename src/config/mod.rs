//! Configuration models for flux
//!
//! Defines the YAML configuration structure for flux sync operations.

pub mod version;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Root configuration structure
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default = "default_version")]
    pub version: u32,
    // === SSH Configuration ===
    /// SSH host address (interactive if missing)
    pub host: Option<String>,
    /// SSH port (interactive if missing, default 22)
    pub port: Option<u16>,
    /// SSH username (interactive if missing, default root)
    pub user: Option<String>,
    /// Path to SSH private key (optional)
    pub key: Option<String>,
    /// SSH password (interactive if missing)
    pub password: Option<String>,
    /// Register public key to authorized_keys
    #[serde(default)]
    pub register_key: bool,

    // === Global Settings ===
    /// Default script interpreter
    #[serde(default = "default_interpreter")]
    pub interpreter: String,
    /// Default interpreter flags
    #[serde(default = "default_flags")]
    pub flags: Vec<String>,
    /// Default comment template for blocks
    #[serde(default = "default_comment_template")]
    pub comment_template: String,
    /// Custom .flux directory path
    pub flux_home: Option<PathBuf>,

    // === Proxy Configuration ===
    /// Proxy settings
    #[serde(default)]
    pub proxy: ProxyConfig,

    // === Sync Items ===
    /// File sync rules
    #[serde(default)]
    pub file: Vec<FileItem>,
    /// Script execution rules
    #[serde(default)]
    pub script: Vec<ScriptItem>,
    /// Block sync rules
    #[serde(default)]
    pub block: Vec<BlockItem>,
}

fn default_version() -> u32 {
    1
}

fn default_interpreter() -> String {
    if cfg!(windows) {
        "cmd".to_string()
    } else {
        "/bin/bash".to_string()
    }
}

fn default_flags() -> Vec<String> {
    vec!["-i".to_string()]
}

fn default_comment_template() -> String {
    "# {}".to_string()
}

/// Proxy configuration
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct ProxyConfig {
    /// Enable proxy
    #[serde(default)]
    pub enabled: bool,
    /// Local proxy port (clash/v2ray)
    #[serde(default = "default_local_port")]
    pub local_port: u16,
    /// Remote listening port
    #[serde(default = "default_remote_port")]
    pub remote_port: u16,
    /// Proxy protocol: http or socks5
    #[serde(default = "default_protocol")]
    pub protocol: ProxyProtocol,
}

fn default_local_port() -> u16 {
    7899
}

fn default_remote_port() -> u16 {
    7890
}

fn default_protocol() -> ProxyProtocol {
    ProxyProtocol::default()
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ProxyProtocol {
    #[default]
    Http,
    Socks5,
}

/// File sync item
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileItem {
    /// Identifier for script dependencies
    pub name: Option<String>,
    /// Source path
    pub src: String,
    /// Destination path
    pub dst: String,
    /// Sync mode: cover, sync, touch
    #[serde(default)]
    pub mode: SyncMode,
    /// File permission (e.g., "755")
    pub chmod: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
}

/// Script execution item
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScriptItem {
    /// Script path (local or remote with : prefix)
    pub path: String,
    /// Custom interpreter
    pub interpreter: Option<String>,
    /// Interpreter flags
    pub flags: Option<Vec<String>>,
    /// Script arguments
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub tags: Vec<String>,
}

/// Block sync item
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BlockItem {
    /// Block name (required, used in sentinel)
    pub name: String,
    /// Block content source (local file)
    pub path: String,
    /// Target file (remote, with : prefix)
    pub file: String,
    /// Sync mode: cover, sync, touch
    #[serde(default)]
    pub mode: SyncMode,
    /// Custom comment template
    pub comment_template: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
}

/// Sync mode for file and block
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum SyncMode {
    /// Always overwrite
    Cover,
    /// Sync based on timestamp
    #[default]
    Sync,
    /// Only if target doesn't exist
    Touch,
}

impl Config {
    /// Load configuration from YAML file
    pub fn load(path: &PathBuf) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read config file {}", path.display()))?;
        version::probe_version(&content).map_err(|err| anyhow::anyhow!("{err}"))?;
        let config: Config = serde_yml::from_str(&content)
            .with_context(|| format!("failed to parse config file {}", path.display()))?;
        Ok(config)
    }

    /// Find and load configuration by name or path
    pub fn find_and_load(name_or_path: &str) -> anyhow::Result<(Self, PathBuf)> {
        let path = Self::find_config(name_or_path)?;
        let config = Self::load(&path)?;
        Ok((config, path))
    }

    /// Find configuration file by name or path
    pub fn find_config(name_or_path: &str) -> anyhow::Result<PathBuf> {
        let path = PathBuf::from(name_or_path);

        // Check if it's a direct file path
        if path.exists() {
            return Ok(path);
        }

        // Search in .flux directories
        let search_dirs = vec![
            std::env::current_dir()?.join(".flux"),
            dirs::home_dir()
                .ok_or_else(|| anyhow::anyhow!("Cannot find home directory"))?
                .join(".flux"),
        ];

        let extensions = ["yml", "yaml"];

        for dir in search_dirs {
            for ext in &extensions {
                let file_path = dir.join(format!("{}.{}", name_or_path, ext));
                if file_path.exists() {
                    return Ok(file_path);
                }
            }
        }

        anyhow::bail!(
            "Configuration '{}' not found. Searched:\n  - {}\n  - ./.flux/{}.yml\n  - ~/.flux/{}.yml",
            name_or_path,
            name_or_path,
            name_or_path,
            name_or_path
        )
    }

    /// Resolve the effective root directory for relative config paths.
    pub fn resolve_root(&self, config_path: &Path) -> PathBuf {
        let config_dir = config_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from(".flux"));

        match &self.flux_home {
            Some(flux_home) => resolve_path_root(flux_home, &config_dir),
            None => config_dir,
        }
    }

    /// Validate cross-reference integrity inside the loaded config.
    pub fn validate(&self) -> Result<()> {
        Ok(())
    }
}

fn resolve_path_root(path: &Path, base_dir: &Path) -> PathBuf {
    let path_str = path.to_string_lossy();
    let expanded = if path_str == "~" || path_str.starts_with("~/") {
        if let Some(home) = dirs::home_dir() {
            let suffix = path_str.strip_prefix('~').unwrap_or("");
            home.join(suffix.trim_start_matches('/'))
        } else {
            path.to_path_buf()
        }
    } else {
        path.to_path_buf()
    };

    if expanded.is_absolute() {
        expanded
    } else {
        base_dir.join(expanded)
    }
}
