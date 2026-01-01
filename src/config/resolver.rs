//! Configuration resolver
//!
//! Handles:
//! - Loading and parsing TOML config files
//! - Variable placeholder resolution ({{var}} and {{var:default}})
//! - Interactive input for missing values
//! - Configuration inheritance

use crate::config::finder::ConfigFinder;
use crate::config::models::*;
use crate::core::error::{RemoteError, Result};
use std::collections::HashMap;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// Configuration resolver
pub struct ConfigResolver {
    finder: ConfigFinder,
    /// Pre-defined variables (from CLI args, etc.)
    variables: HashMap<String, String>,
    /// Whether to prompt for missing variables
    interactive: bool,
}

impl ConfigResolver {
    /// Create a new resolver
    pub fn new() -> Self {
        Self {
            finder: ConfigFinder::new(),
            variables: HashMap::new(),
            interactive: true,
        }
    }

    /// Set interactive mode
    pub fn with_interactive(mut self, interactive: bool) -> Self {
        self.interactive = interactive;
        self
    }

    /// Add pre-defined variable
    pub fn with_variable(mut self, name: &str, value: &str) -> Self {
        self.variables.insert(name.to_string(), value.to_string());
        self
    }

    /// Add multiple pre-defined variables
    pub fn with_variables(mut self, vars: HashMap<String, String>) -> Self {
        self.variables.extend(vars);
        self
    }

    /// Load and resolve configuration by name or path
    pub fn load(&self, name_or_path: &str) -> Result<ResolvedConfig> {
        let config_path = self.finder.find(name_or_path)?;
        self.load_from_path(&config_path)
    }

    /// Load and resolve default configuration
    pub fn load_default(&self) -> Result<ResolvedConfig> {
        let config_path = self.finder.find_default()?;
        self.load_from_path(&config_path)
    }

    /// Load configuration from path
    pub fn load_from_path(&self, path: &Path) -> Result<ResolvedConfig> {
        let content = std::fs::read_to_string(path)?;
        let mut config: FluxConfig = toml::from_str(&content)?;

        // Handle inheritance
        if let Some(inherit) = &config.inherit {
            let parent_config = self.load_raw(inherit)?;
            config = merge_configs(parent_config, config);
        }

        // Resolve variables
        self.resolve_config(config)
    }

    /// Load raw config without resolving (for inheritance)
    fn load_raw(&self, name_or_path: &str) -> Result<FluxConfig> {
        let config_path = self.finder.find(name_or_path)?;
        let content = std::fs::read_to_string(&config_path)?;
        Ok(toml::from_str(&content)?)
    }

    /// Resolve all variables in configuration
    fn resolve_config(&self, config: FluxConfig) -> Result<ResolvedConfig> {
        // Get effective connection (flat fields override nested)
        let conn = config.effective_connection();

        // Sensitive fields: prompt if empty, use directly if set
        let host = self.prompt_if_empty(&conn.host, "Host", None)?;
        let user = self.prompt_if_empty(&conn.user, "User", Some("root"))?;
        let password = self.prompt_if_empty_optional(conn.password.as_deref(), "Password", None)?;

        // Port - prompt if 0 (not set), default to 22
        let port = if conn.port == 0 {
            let port_str = self.prompt_if_empty("", "Port", Some("22"))?;
            port_str.parse::<u16>().map_err(|_| {
                RemoteError::Config(format!("Invalid port number: {}", port_str))
            })?
        } else {
            conn.port
        };

        // Expand key path
        let key = conn.key.map(|k| expand_tilde(&k));

        let connection = ResolvedConnection {
            host,
            user,
            port,
            key,
            password,
        };

        // Convert config models to sync models
        let files = config.files.into_iter().map(|f| crate::sync::models::FileSync {
            src: f.src,
            dist: f.dist,
            mode: f.mode,
            conflict: f.conflict,
            condition: f.condition,
            excludes: f.excludes,
        }).collect();

        let blocks = config.blocks.into_iter().map(|b| crate::sync::models::BlockGroup {
            dist: b.dist,
            mode: b.mode,
            blocks: b.blocks,
        }).collect();

        let scripts = config.scripts.into_iter().map(|s| crate::sync::models::ScriptExec {
            src: s.src,
            mode: s.mode,
            exec_mode: s.exec_mode,
            interpreter: s.interpreter,
            flags: s.flags,
            args: s.args,
            allow_fail: s.allow_fail,
        }).collect();

        let env = crate::sync::models::GlobalEnv {
            interpreter: config.env.interpreter,
            flags: config.env.flags,
        };

        Ok(ResolvedConfig {
            connection,
            proxy: config.proxy,
            files,
            blocks,
            scripts,
            env,
            block_home: config.block_home,
            script_home: config.script_home,
            add_authorized_key: config.add_authorized_key,
        })
    }

    /// Prompt if value is empty (for required fields)
    /// Shows default in brackets, waits for user input
    fn prompt_if_empty(&self, value: &str, name: &str, default: Option<&str>) -> Result<String> {
        // If value is not empty, use it directly
        if !value.is_empty() {
            return Ok(value.to_string());
        }

        // Only prompt if interactive mode is enabled
        if !self.interactive {
            // Non-interactive: use default if available, error otherwise
            if let Some(def) = default {
                return Ok(def.to_string());
            }
            return Err(RemoteError::Config(format!(
                "{} is required but not provided",
                name
            )));
        }

        // Interactive mode: prompt user
        let prompt = match default {
            Some(def) => format!("{} [{}]: ", name, def),
            None => format!("{}: ", name),
        };

        print!("{}", prompt);
        io::stdout().flush()?;

        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        let input = input.trim();

        // Empty input: use default or error
        if input.is_empty() {
            if let Some(def) = default {
                return Ok(def.to_string());
            }
            return Err(RemoteError::Config(format!(
                "{} is required",
                name
            )));
        }

        Ok(input.to_string())
    }

    /// Prompt if value is empty (for optional fields like password)
    fn prompt_if_empty_optional(&self, value: Option<&str>, name: &str, default: Option<&str>) -> Result<Option<String>> {
        // If value is set and not empty, use it directly
        if let Some(val) = value {
            if !val.is_empty() {
                return Ok(Some(val.to_string()));
            }
        }

        // Only prompt if interactive mode is enabled
        if !self.interactive {
            // Non-interactive: use default if available, None otherwise
            return Ok(default.map(|s| s.to_string()));
        }

        // Interactive mode: prompt user
        let prompt = match default {
            Some(def) => format!("{} [{}]: ", name, def),
            None => format!("{} (optional, press Enter to skip): ", name),
        };

        print!("{}", prompt);
        io::stdout().flush()?;

        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        let input = input.trim();

        // Empty input: use default or None
        if input.is_empty() {
            return Ok(default.map(|s| s.to_string()));
        }

        Ok(Some(input.to_string()))
    }

    /// Get the config finder
    pub fn finder(&self) -> &ConfigFinder {
        &self.finder
    }
}

impl Default for ConfigResolver {
    fn default() -> Self {
        Self::new()
    }
}

/// Merge parent config with child config (child overrides parent)
/// 
/// Merge strategy:
/// - Connection: child values override parent (empty strings inherit from parent)
/// - Proxy: child values override parent (unless child uses all defaults)
/// - Files/Blocks/Scripts: child values extend parent (merge, not replace)
/// - Env: child values override parent
fn merge_configs(parent: FluxConfig, child: FluxConfig) -> FluxConfig {
    // Get effective connection configs
    let parent_conn = parent.effective_connection();
    let child_conn = child.effective_connection();

    // Merge files: combine parent and child, child entries with same src override
    let mut merged_files = parent.files.clone();
    for child_file in child.files {
        if let Some(pos) = merged_files.iter().position(|f| f.src == child_file.src) {
            merged_files[pos] = child_file;
        } else {
            merged_files.push(child_file);
        }
    }

    // Merge blocks: combine parent and child, child entries with same dist override
    let mut merged_blocks = parent.blocks.clone();
    for child_block in child.blocks {
        if let Some(pos) = merged_blocks.iter().position(|b| b.dist == child_block.dist) {
            merged_blocks[pos] = child_block;
        } else {
            merged_blocks.push(child_block);
        }
    }

    // Merge scripts: combine parent and child, child entries with same src override
    let mut merged_scripts = parent.scripts.clone();
    for child_script in child.scripts {
        if let Some(pos) = merged_scripts.iter().position(|s| s.src == child_script.src) {
            merged_scripts[pos] = child_script;
        } else {
            merged_scripts.push(child_script);
        }
    }

    FluxConfig {
        inherit: None, // Already processed
        // Store merged connection in flat fields (preferred format)
        host: Some(if child_conn.host.is_empty() {
            parent_conn.host
        } else {
            child_conn.host
        }),
        user: Some(child_conn.user),
        port: Some(child_conn.port),
        key: child_conn.key.or(parent_conn.key),
        password: child_conn.password.or(parent_conn.password),
        ssh_config: child_conn.ssh_config.or(parent_conn.ssh_config),
        // Clear nested connection (use flat fields)
        connection: ConnectionConfig::default(),
        // Proxy: merge settings, child overrides individual fields
        proxy: ProxyConfigSection {
            enabled: child.proxy.enabled || parent.proxy.enabled,
            remote_port: if child.proxy.remote_port != 1081 {
                child.proxy.remote_port
            } else {
                parent.proxy.remote_port
            },
            local_port: if child.proxy.local_port != 7890 {
                child.proxy.local_port
            } else {
                parent.proxy.local_port
            },
            mode: if child.proxy.mode != "socks5" {
                child.proxy.mode
            } else {
                parent.proxy.mode
            },
            builtin: child.proxy.builtin || parent.proxy.builtin,
            set_env: child.proxy.set_env && parent.proxy.set_env,
        },
        files: merged_files,
        blocks: merged_blocks,
        scripts: merged_scripts,
        env: EnvConfig {
            interpreter: if child.env.interpreter != "/bin/bash" {
                child.env.interpreter
            } else {
                parent.env.interpreter
            },
            flags: if child.env.flags.is_empty() {
                parent.env.flags
            } else {
                child.env.flags
            },
        },
        block_home: child.block_home.or(parent.block_home),
        script_home: child.script_home.or(parent.script_home),
        add_authorized_key: child.add_authorized_key || parent.add_authorized_key,
    }
}

/// Expand ~ to home directory
fn expand_tilde(path: &str) -> PathBuf {
    if path == "~" {
        // Single ~ returns home directory
        if let Some(home) = dirs::home_dir() {
            return home;
        }
    } else if let Some(stripped) = path.strip_prefix("~/") {
        // ~/xxx joins path to home
        if let Some(home) = dirs::home_dir() {
            return home.join(stripped);
        }
    }
    PathBuf::from(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_expand_tilde() {
        let path = expand_tilde("~/.ssh/id_rsa");
        assert!(path.to_string_lossy().contains(".ssh"));
    }
}
