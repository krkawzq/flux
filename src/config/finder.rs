//! Configuration file finder
//!
//! Supports finding config by:
//! - Absolute path: /path/to/config.toml
//! - Relative path: ./config.toml or ../config.toml
//! - Name: myserver (searches .flux/config/ and ~/.flux/config/)

use crate::core::error::{RemoteError, Result};
use std::path::{Path, PathBuf};

/// Configuration file finder
pub struct ConfigFinder {
    /// Local .flux directory (current workspace)
    local_dir: Option<PathBuf>,
    /// Global ~/.flux directory
    global_dir: PathBuf,
}

impl ConfigFinder {
    /// Create a new config finder
    pub fn new() -> Self {
        Self {
            local_dir: find_local_flux_dir(),
            global_dir: get_global_flux_dir(),
        }
    }

    /// Find configuration file by name or path
    ///
    /// Search order:
    /// 1. If path is absolute -> use directly
    /// 2. If path starts with ./ or ../ -> relative to current dir
    /// 3. If path contains / or \ -> relative to current dir
    /// 4. Otherwise treat as name:
    ///    a. .flux/config/{name}.toml (local)
    ///    b. ~/.flux/config/{name}.toml (global)
    pub fn find(&self, name_or_path: &str) -> Result<PathBuf> {
        let path = Path::new(name_or_path);

        // Absolute path
        if path.is_absolute() {
            return self.validate_path(path);
        }

        // Relative path (starts with ./ or ../ or contains path separator)
        if name_or_path.starts_with("./")
            || name_or_path.starts_with(".\\")
            || name_or_path.starts_with("../")
            || name_or_path.starts_with("..\\")
            || name_or_path.contains('/')
            || name_or_path.contains('\\')
        {
            let full_path = std::env::current_dir()?.join(path);
            return self.validate_path(&full_path);
        }

        // Treat as name - search in config directories
        self.find_by_name(name_or_path)
    }

    /// Find config by name in .flux/config/ directories
    fn find_by_name(&self, name: &str) -> Result<PathBuf> {
        let filename = if name.ends_with(".toml") {
            name.to_string()
        } else {
            format!("{}.toml", name)
        };

        // Search local .flux/config/ first
        if let Some(local_dir) = &self.local_dir {
            let local_config = local_dir.join("config").join(&filename);
            if local_config.exists() {
                return Ok(local_config);
            }
        }

        // Search global ~/.flux/config/
        let global_config = self.global_dir.join("config").join(&filename);
        if global_config.exists() {
            return Ok(global_config);
        }

        Err(RemoteError::Config(format!(
            "Configuration '{}' not found. Searched:\n  - .flux/config/{}\n  - ~/.flux/config/{}",
            name, filename, filename
        )))
    }

    /// Find default config (default.toml)
    pub fn find_default(&self) -> Result<PathBuf> {
        self.find("default")
    }

    /// Validate that path exists and is a file
    fn validate_path(&self, path: &Path) -> Result<PathBuf> {
        let canonical = if path.exists() {
            path.canonicalize()?
        } else {
            return Err(RemoteError::Config(format!(
                "Configuration file not found: {}",
                path.display()
            )));
        };

        if !canonical.is_file() {
            return Err(RemoteError::Config(format!(
                "Path is not a file: {}",
                canonical.display()
            )));
        }

        Ok(canonical)
    }

    /// Get local .flux directory if exists
    pub fn local_dir(&self) -> Option<&PathBuf> {
        self.local_dir.as_ref()
    }

    /// Get global ~/.flux directory
    pub fn global_dir(&self) -> &PathBuf {
        &self.global_dir
    }

    /// Check if local .flux directory exists
    pub fn has_local(&self) -> bool {
        self.local_dir.is_some()
    }

    /// List all available config names
    pub fn list_configs(&self) -> Vec<ConfigInfo> {
        let mut configs = Vec::new();

        // List local configs
        if let Some(local_dir) = &self.local_dir {
            let config_dir = local_dir.join("config");
            if let Ok(entries) = std::fs::read_dir(&config_dir) {
                for entry in entries.flatten() {
                    if let Some(name) = entry.path().file_stem() {
                        if entry.path().extension().is_some_and(|e| e == "toml") {
                            configs.push(ConfigInfo {
                                name: name.to_string_lossy().to_string(),
                                path: entry.path(),
                                scope: ConfigScope::Local,
                            });
                        }
                    }
                }
            }
        }

        // List global configs
        let config_dir = self.global_dir.join("config");
        if let Ok(entries) = std::fs::read_dir(&config_dir) {
            for entry in entries.flatten() {
                if let Some(name) = entry.path().file_stem() {
                    if entry.path().extension().is_some_and(|e| e == "toml") {
                        let name_str = name.to_string_lossy().to_string();
                        // Skip if already found in local
                        if !configs.iter().any(|c| c.name == name_str) {
                            configs.push(ConfigInfo {
                                name: name_str,
                                path: entry.path(),
                                scope: ConfigScope::Global,
                            });
                        }
                    }
                }
            }
        }

        configs.sort_by(|a, b| a.name.cmp(&b.name));
        configs
    }
}

impl Default for ConfigFinder {
    fn default() -> Self {
        Self::new()
    }
}

/// Configuration info
#[derive(Debug, Clone)]
pub struct ConfigInfo {
    pub name: String,
    pub path: PathBuf,
    pub scope: ConfigScope,
}

/// Configuration scope
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ConfigScope {
    Local,
    Global,
}

impl std::fmt::Display for ConfigScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigScope::Local => write!(f, "local"),
            ConfigScope::Global => write!(f, "global"),
        }
    }
}

/// Find local .flux directory by searching upward from current dir
fn find_local_flux_dir() -> Option<PathBuf> {
    let current_dir = std::env::current_dir().ok()?;
    let mut dir = current_dir.as_path();

    loop {
        let flux_dir = dir.join(".flux");
        if flux_dir.is_dir() {
            return Some(flux_dir);
        }

        match dir.parent() {
            Some(parent) => dir = parent,
            None => break,
        }
    }

    None
}

/// Get global ~/.flux directory path
fn get_global_flux_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".flux")
}

/// Initialize .flux directory structure
pub fn init_flux_dir(path: &Path, with_example: bool) -> Result<()> {
    let flux_dir = path.join(".flux");

    // Create directory structure
    std::fs::create_dir_all(flux_dir.join("config"))?;
    std::fs::create_dir_all(flux_dir.join("scripts"))?;
    std::fs::create_dir_all(flux_dir.join("blocks"))?;
    std::fs::create_dir_all(flux_dir.join("files"))?;  // 新增 files 目录
    std::fs::create_dir_all(flux_dir.join("state"))?;

    // Create .gitignore for state directory
    std::fs::write(
        flux_dir.join("state").join(".gitignore"),
        "*\n!.gitignore\n",
    )?;

    if with_example {
        // Create example config
        let example_config = r#"# Flux Configuration
# ========================================
# Placeholders: {{var}} or {{var:default}} for interactive input
#
# Path resolution rules:
# - Relative paths: resolved from .flux/{type}/ directory
#   - files -> .flux/files/
#   - blocks -> .flux/blocks/
#   - scripts -> .flux/scripts/
# - Absolute paths or ~/: used as-is
# - Remote paths: prefixed with ":" (e.g., ":~/.bashrc")

[connection]
host = "{{host}}"           # Will prompt for input
user = "{{user:root}}"      # Default: root
port = 22
# key = "~/.ssh/id_rsa"     # Uncomment to use SSH key

[proxy]
enabled = false             # Enable proxy during sync
remote_port = 1081
local_port = 7890
mode = "socks5"

# ========================================
# File Sync Examples
# ========================================
# Sync files from local to remote

# Example 1: Relative path (resolved from .flux/files/)
# [[file]]
# src = "bashrc"                   # -> .flux/files/bashrc
# dist = ":~/.bashrc"              # -> remote ~/.bashrc
# mode = "update"                  # Only sync if newer

# Example 2: Subdirectory in .flux/files/
# [[file]]
# src = "config/tmux.conf"         # -> .flux/files/config/tmux.conf
# dist = ":~/.tmux.conf"
# mode = "cover"                   # Always overwrite

# Example 3: Absolute path with tilde
# [[file]]
# src = "~/.ssh/id_rsa.pub"        # Use absolute path
# dist = ":~/.ssh/authorized_keys"
# mode = "init"                    # Only sync if target doesn't exist

# ========================================
# Block Sync Examples
# ========================================
# Merge configuration blocks into remote files

# [[block]]
# dist = ":~/.bashrc"
# blocks = ["bashrc/aliases.block", "bashrc/env.block"]
# mode = "incremental"             # Preserve unknown blocks

# ========================================
# Script Execution Examples
# ========================================
# Run scripts on remote server

# Example 1: Init script (only runs on first connection)
# [[script]]
# src = "setup.sh"                 # -> .flux/scripts/setup.sh
# mode = "init"

# Example 2: Always run
# [[script]]
# src = "update.sh"
# mode = "always"
# allow_fail = true                # Don't fail if script returns non-zero
"#;
        std::fs::write(flux_dir.join("config").join("default.toml"), example_config)?;

        // Create example script
        let example_script = r#"#!/bin/bash
# Flux initialization script
# This runs only on first connection (mode = "init")

echo "Flux init script executed"
echo "Setting up environment..."

# Add your initialization commands here
# For example:
# - Install packages
# - Create directories
# - Set permissions
"#;
        std::fs::write(flux_dir.join("scripts").join("setup.sh"), example_script)?;

        // Create example files
        let example_bashrc = r#"# Flux managed bashrc
# Put your bash configuration here

# Aliases
alias ll='ls -la'
alias ..='cd ..'

# Environment
export EDITOR=vim
"#;
        std::fs::write(flux_dir.join("files").join("bashrc"), example_bashrc)?;

        // Create example block
        let example_block = r#"# Flux managed aliases
alias ll='ls -la'
alias la='ls -A'
alias l='ls -CF'
alias ..='cd ..'
alias ...='cd ../..'
"#;
        std::fs::write(flux_dir.join("blocks").join("aliases.block"), example_block)?;
    }

    Ok(())
}

/// Initialize global ~/.flux directory
pub fn init_global_flux_dir() -> Result<PathBuf> {
    let global_dir = get_global_flux_dir();

    std::fs::create_dir_all(global_dir.join("config"))?;
    std::fs::create_dir_all(global_dir.join("scripts"))?;
    std::fs::create_dir_all(global_dir.join("state"))?;

    Ok(global_dir)
}
