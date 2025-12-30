//! System shell backend - uses the system's native shell

use super::executor::{ShellBackend, ShellOutput};
use crate::core::error::{RemoteError, Result};
use std::path::Path;
use std::process::{Command, Stdio};

/// System shell backend - uses bash/sh on Unix, cmd/powershell on Windows
pub struct SystemShellBackend {
    shell_path: Option<String>,
}

impl SystemShellBackend {
    pub fn new() -> Self {
        Self { shell_path: None }
    }

    /// Create with a specific shell path
    pub fn with_shell(shell_path: impl Into<String>) -> Self {
        Self {
            shell_path: Some(shell_path.into()),
        }
    }

    /// Get the shell command to use
    fn get_shell(&self) -> (&str, Vec<&str>) {
        if let Some(ref path) = self.shell_path {
            return (path.as_str(), vec!["-c"]);
        }

        #[cfg(unix)]
        {
            // Try bash first, then sh
            if Path::new("/bin/bash").exists() {
                ("/bin/bash", vec!["-c"])
            } else if Path::new("/usr/bin/bash").exists() {
                ("/usr/bin/bash", vec!["-c"])
            } else {
                ("/bin/sh", vec!["-c"])
            }
        }

        #[cfg(windows)]
        {
            // Use PowerShell on Windows for better bash compatibility
            // Or fall back to cmd
            if let Ok(ps_path) = std::env::var("SystemRoot") {
                let ps = format!(
                    "{}\\System32\\WindowsPowerShell\\v1.0\\powershell.exe",
                    ps_path
                );
                if Path::new(&ps).exists() {
                    return ("powershell.exe", vec!["-NoProfile", "-Command"]);
                }
            }
            ("cmd.exe", vec!["/C"])
        }
    }
}

impl Default for SystemShellBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl ShellBackend for SystemShellBackend {
    fn name(&self) -> &str {
        "system"
    }

    fn is_available(&self) -> bool {
        let (shell, _) = self.get_shell();

        #[cfg(unix)]
        {
            Path::new(shell).exists()
        }

        #[cfg(windows)]
        {
            // On Windows, cmd.exe is always available
            true
        }
    }

    fn execute_script(&self, script: &str, env: &[(String, String)]) -> Result<ShellOutput> {
        let (shell, args) = self.get_shell();

        let mut cmd = Command::new(shell);
        cmd.args(&args);
        cmd.arg(script);
        cmd.stdin(Stdio::null());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        // Add environment variables
        for (key, value) in env {
            cmd.env(key, value);
        }

        let output = cmd.output().map_err(|e| RemoteError::ScriptExecution {
            script: script.to_string(),
            code: -1,
            stderr: e.to_string(),
        })?;

        Ok(ShellOutput {
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            exit_code: output.status.code().unwrap_or(-1),
        })
    }

    fn execute_file(
        &self,
        path: &Path,
        args: &[String],
        env: &[(String, String)],
    ) -> Result<ShellOutput> {
        if !path.exists() {
            return Err(RemoteError::ScriptNotFound {
                path: path.to_path_buf(),
            });
        }

        let (shell, shell_args) = self.get_shell();

        // Build command string: "script_path arg1 arg2 ..."
        let script_cmd = if args.is_empty() {
            format!("\"{}\"", path.display())
        } else {
            format!("\"{}\" {}", path.display(), args.join(" "))
        };

        let mut cmd = Command::new(shell);
        cmd.args(&shell_args);
        cmd.arg(&script_cmd);
        cmd.stdin(Stdio::null());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        // Add environment variables
        for (key, value) in env {
            cmd.env(key, value);
        }

        let output = cmd.output().map_err(|e| RemoteError::ScriptExecution {
            script: path.display().to_string(),
            code: -1,
            stderr: e.to_string(),
        })?;

        Ok(ShellOutput {
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            exit_code: output.status.code().unwrap_or(-1),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_system_backend_available() {
        let backend = SystemShellBackend::new();
        assert!(backend.is_available());
    }

    #[test]
    fn test_execute_simple_script() {
        let backend = SystemShellBackend::new();

        #[cfg(unix)]
        let result = backend.execute_script("echo hello", &[]);

        #[cfg(windows)]
        let result = backend.execute_script("echo hello", &[]);

        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(output.stdout.contains("hello"));
    }
}
