//! Shell executor - manages multiple shell backends

use crate::core::error::{RemoteError, Result};
use std::path::Path;

/// Shell execution result
#[derive(Debug, Clone)]
pub struct ShellOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

impl ShellOutput {
    pub fn success(&self) -> bool {
        self.exit_code == 0
    }
}

/// Shell backend trait - implement this for different shell interpreters
pub trait ShellBackend: Send + Sync {
    /// Backend name (for logging and configuration)
    fn name(&self) -> &str;

    /// Check if this backend is available on the current system
    fn is_available(&self) -> bool;

    /// Execute a script string
    fn execute_script(&self, script: &str, env: &[(String, String)]) -> Result<ShellOutput>;

    /// Execute a script file
    fn execute_file(
        &self,
        path: &Path,
        args: &[String],
        env: &[(String, String)],
    ) -> Result<ShellOutput>;
}

/// Shell executor - manages backends and selects the best one
pub struct ShellExecutor {
    backends: Vec<Box<dyn ShellBackend>>,
    preferred: Option<String>,
}

impl ShellExecutor {
    /// Create a new executor with default backends
    pub fn new() -> Self {
        // Add system shell backend (always available as primary fallback)
        let backends: Vec<Box<dyn ShellBackend>> =
            vec![Box::new(super::system_backend::SystemShellBackend::new())];

        Self {
            backends,
            preferred: None,
        }
    }

    /// Set preferred backend by name
    pub fn prefer(&mut self, backend_name: &str) {
        self.preferred = Some(backend_name.to_string());
    }

    /// Add a custom backend
    pub fn add_backend(&mut self, backend: Box<dyn ShellBackend>) {
        self.backends.insert(0, backend); // Insert at front for priority
    }

    /// Get the best available backend
    fn get_backend(&self) -> Result<&dyn ShellBackend> {
        // Check preferred backend first
        if let Some(ref pref) = self.preferred {
            if let Some(backend) = self
                .backends
                .iter()
                .find(|b| b.name() == pref && b.is_available())
            {
                return Ok(backend.as_ref());
            }
            tracing::warn!("Preferred shell backend '{}' not available", pref);
        }

        // Fall back to first available
        self.backends
            .iter()
            .find(|b| b.is_available())
            .map(|b| b.as_ref())
            .ok_or(RemoteError::NoShellBackend)
    }

    /// Execute a script string
    pub fn execute_script(&self, script: &str, env: &[(String, String)]) -> Result<ShellOutput> {
        let backend = self.get_backend()?;
        tracing::debug!("Executing script with backend: {}", backend.name());
        backend.execute_script(script, env)
    }

    /// Execute a script file
    pub fn execute_file(
        &self,
        path: &Path,
        args: &[String],
        env: &[(String, String)],
    ) -> Result<ShellOutput> {
        let backend = self.get_backend()?;
        tracing::debug!("Executing file {:?} with backend: {}", path, backend.name());
        backend.execute_file(path, args, env)
    }

    /// List all backends and their availability
    pub fn list_backends(&self) -> Vec<(&str, bool)> {
        self.backends
            .iter()
            .map(|b| (b.name(), b.is_available()))
            .collect()
    }

    /// Get the currently selected backend name
    pub fn current_backend(&self) -> Option<&str> {
        self.get_backend().ok().map(|b| b.name())
    }
}

impl Default for ShellExecutor {
    fn default() -> Self {
        Self::new()
    }
}
