//! Path utilities for flux
//!
//! Handles local and remote path resolution.
//! Remote paths are prefixed with `:`.

use std::path::PathBuf;

/// Represents a path that can be local or remote
#[derive(Debug, Clone, PartialEq)]
pub enum FluxPath {
    /// Local path
    Local(PathBuf),
    /// Remote path (on SSH server)
    Remote(String),
}

impl FluxPath {
    /// Parse a path string into FluxPath
    ///
    /// - `:` prefix indicates remote path
    /// - No prefix indicates local path
    pub fn parse(path: &str) -> Self {
        if let Some(remote_path) = path.strip_prefix(':') {
            FluxPath::Remote(remote_path.to_string())
        } else {
            FluxPath::Local(PathBuf::from(path))
        }
    }

    /// Check if this is a remote path
    pub fn is_remote(&self) -> bool {
        matches!(self, FluxPath::Remote(_))
    }

    /// Check if this is a local path
    pub fn is_local(&self) -> bool {
        matches!(self, FluxPath::Local(_))
    }

    /// Get the path as a string
    pub fn as_str(&self) -> String {
        match self {
            FluxPath::Local(p) => p.to_string_lossy().to_string(),
            FluxPath::Remote(p) => p.clone(),
        }
    }

    /// Resolve local path to absolute path
    ///
    /// - `~` is expanded to home directory
    /// - Relative paths are resolved from current working directory
    pub fn resolve_local(&self) -> anyhow::Result<PathBuf> {
        match self {
            FluxPath::Local(path) => {
                let path_str = path.to_string_lossy();
                if path_str.starts_with('~') {
                    let home = dirs::home_dir()
                        .ok_or_else(|| anyhow::anyhow!("Cannot find home directory"))?;
                    let rest = path_str.strip_prefix("~/").unwrap_or(&path_str[1..]);
                    Ok(home.join(rest))
                } else if path.is_absolute() {
                    Ok(path.clone())
                } else {
                    Ok(std::env::current_dir()?.join(path))
                }
            }
            FluxPath::Remote(_) => {
                anyhow::bail!("Cannot resolve remote path as local")
            }
        }
    }

    /// Resolve remote path
    ///
    /// - `~` is kept as-is (resolved on remote)
    /// - Relative paths are treated as relative to remote home directory
    pub fn resolve_remote(&self) -> anyhow::Result<String> {
        match self {
            FluxPath::Remote(path) => {
                if path.starts_with('~') || path.starts_with('/') {
                    Ok(path.clone())
                } else {
                    // Relative path -> relative to home
                    Ok(format!("~/{}", path))
                }
            }
            FluxPath::Local(_) => {
                anyhow::bail!("Cannot resolve local path as remote")
            }
        }
    }
}

/// Display implementation for nice output
impl std::fmt::Display for FluxPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FluxPath::Local(p) => write!(f, "{}", p.display()),
            FluxPath::Remote(p) => write!(f, ":{}", p),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_remote() {
        let path = FluxPath::parse(":~/.zshrc");
        assert!(path.is_remote());
        assert_eq!(path.as_str(), "~/.zshrc");
    }

    #[test]
    fn test_parse_local() {
        let path = FluxPath::parse("~/.bashrc");
        assert!(path.is_local());
    }

    #[test]
    fn test_parse_remote_absolute() {
        let path = FluxPath::parse(":/etc/hosts");
        assert!(path.is_remote());
        assert_eq!(path.as_str(), "/etc/hosts");
    }
}
