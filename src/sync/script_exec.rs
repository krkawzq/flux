//! Script execution logic
//!
//! Handles remote script execution with interpreter detection

use crate::core::error::{RemoteError, Result};
use crate::core::platform::expand_tilde;
use crate::core::ssh::{SshClient, SshClientTrait};
use crate::sync::file_sync::{is_remote_path, strip_remote_prefix};
use crate::sync::models::{ExecMode, GlobalEnv, ScriptExec, ScriptMode};
use std::fs;
use std::path::PathBuf;

/// Script execution context
pub struct ScriptExecContext<'a> {
    pub client: &'a SshClient,
    pub global_env: &'a GlobalEnv,
    pub script_home: Option<String>,
    pub is_first_connect: bool,
    pub dry_run: bool,
    /// Verbose mode - stream output in real-time
    pub verbose: bool,
    /// .flux directory for resolving relative script paths
    pub flux_dir: Option<PathBuf>,
}

/// Result of script execution
#[derive(Debug, Clone)]
pub enum ScriptExecResult {
    /// Script executed successfully
    Success { script: String, output: String },
    /// Script was skipped
    Skipped { script: String, reason: String },
    /// Script failed (with allow_fail = true)
    FailedAllowed {
        script: String,
        code: i32,
        stderr: String,
    },
    /// Dry run - would execute
    WouldExecute { script: String },
}

/// Execute a list of scripts
pub async fn execute_scripts(
    scripts: &[ScriptExec],
    ctx: &ScriptExecContext<'_>,
) -> Result<Vec<ScriptExecResult>> {
    let mut results = Vec::new();

    for script in scripts {
        // Filter based on mode
        match script.mode {
            ScriptMode::Init if !ctx.is_first_connect => {
                results.push(ScriptExecResult::Skipped {
                    script: script.src.clone(),
                    reason: "Init mode - not first connection".to_string(),
                });
                continue;
            }
            _ => {}
        }

        let result = execute_one_script(script, ctx).await?;
        results.push(result);
    }

    Ok(results)
}

/// Execute a single script
async fn execute_one_script(
    script: &ScriptExec,
    ctx: &ScriptExecContext<'_>,
) -> Result<ScriptExecResult> {
    if ctx.dry_run {
        return Ok(ScriptExecResult::WouldExecute {
            script: script.src.clone(),
        });
    }

    let src_is_remote = is_remote_path(&script.src);

    // Build command
    let cmd = if src_is_remote {
        build_remote_script_cmd(script, ctx)?
    } else {
        build_local_script_cmd(script, ctx).await?
    };

    tracing::info!("Executing script: {}", script.src);
    tracing::debug!("Command: {}", cmd);

    // Execute - always stream output in real-time (like Python version)
    let result = if ctx.verbose {
        // Stream output in real-time
        ctx.client.exec_streaming(
            &cmd,
            |line| print!("{}", line),  // stdout
            |line| eprint!("{}", line), // stderr to stderr
        ).await?
    } else {
        // Still capture for legacy compatibility
        ctx.client.exec(&cmd).await?
    };

    if result.exit_code != 0 {
        if script.allow_fail {
            return Ok(ScriptExecResult::FailedAllowed {
                script: script.src.clone(),
                code: result.exit_code,
                stderr: result.stderr,
            });
        } else {
            return Err(RemoteError::ScriptExecution {
                script: script.src.clone(),
                code: result.exit_code,
                stderr: result.stderr,
            });
        }
    }

    Ok(ScriptExecResult::Success {
        script: script.src.clone(),
        output: result.stdout,
    })
}

/// Build command for remote script
fn build_remote_script_cmd(script: &ScriptExec, ctx: &ScriptExecContext<'_>) -> Result<String> {
    let script_path = strip_remote_prefix(&script.src);

    let interpreter = resolve_interpreter(script, ctx);
    let flags = script.flags.join(" ");
    let args = script.args.join(" ");

    let cmd = match script.exec_mode {
        ExecMode::Exec => {
            if interpreter.is_empty() {
                format!("'{}' {}", script_path, args)
            } else {
                format!("{} {} '{}' {}", interpreter, flags, script_path, args)
            }
        }
        ExecMode::Source => {
            format!("source '{}'", script_path)
        }
    };

    Ok(cmd)
}

/// Check if path is relative (not starting with / or ~)
fn is_relative_path(path: &str) -> bool {
    !path.starts_with('/') && !path.starts_with('~') && !path.starts_with(':')
}

/// Build command for local script (upload first)
async fn build_local_script_cmd(
    script: &ScriptExec,
    ctx: &ScriptExecContext<'_>,
) -> Result<String> {
    // Resolve local path
    let local_path = if let Some(ref home) = ctx.script_home {
        // Explicit script_home takes precedence
        PathBuf::from(home).join(&script.src)
    } else if is_relative_path(&script.src) {
        // Relative path: resolve against .flux/scripts directory
        if let Some(ref flux_dir) = ctx.flux_dir {
            flux_dir.join("scripts").join(&script.src)
        } else {
            expand_tilde(&script.src)
        }
    } else {
        // Absolute or ~ path
        expand_tilde(&script.src)
    };

    if !local_path.exists() {
        return Err(RemoteError::ScriptNotFound { path: local_path });
    }

    // Read script content
    let content = fs::read_to_string(&local_path)?;

    // Detect shebang
    let shebang = detect_shebang(&content);

    // Upload to remote /tmp
    let remote_tmp = format!("/tmp/remote_script_{}", std::process::id());
    upload_script(ctx.client, &remote_tmp, &content).await?;

    // Resolve interpreter
    let interpreter = if let Some(ref int) = script.interpreter {
        int.clone()
    } else if let Some(shb) = shebang {
        shb
    } else {
        ctx.global_env.interpreter.clone()
    };

    let flags = script.flags.join(" ");
    let args = script.args.join(" ");

    let cmd = match script.exec_mode {
        ExecMode::Exec => {
            format!(
                "{} {} '{}' {}; rm -f '{}'",
                interpreter, flags, remote_tmp, args, remote_tmp
            )
        }
        ExecMode::Source => {
            format!("source '{}'; rm -f '{}'", remote_tmp, remote_tmp)
        }
    };

    Ok(cmd)
}

/// Detect shebang line from script content
fn detect_shebang(content: &str) -> Option<String> {
    let first_line = content.lines().next()?;

    if let Some(interpreter) = first_line.strip_prefix("#!") {
        let interpreter = interpreter.trim();
        // Handle "#!/usr/bin/env bash" style
        if let Some(cmd) = interpreter.strip_prefix("/usr/bin/env ") {
            Some(cmd.trim().to_string())
        } else {
            Some(interpreter.to_string())
        }
    } else {
        None
    }
}

/// Resolve interpreter for a script
fn resolve_interpreter(script: &ScriptExec, ctx: &ScriptExecContext<'_>) -> String {
    script
        .interpreter
        .clone()
        .unwrap_or_else(|| ctx.global_env.interpreter.clone())
}

/// Upload script to remote
async fn upload_script(client: &SshClient, remote_path: &str, content: &str) -> Result<()> {
    // Write content using heredoc
    let cmd = format!(
        "cat > '{}' << 'REMOTE_SCRIPT_EOF'\n{}\nREMOTE_SCRIPT_EOF\nchmod +x '{}'",
        remote_path, content, remote_path
    );

    client.exec(&cmd).await?;

    Ok(())
}

/// Delete remote file
pub async fn delete_remote_file(client: &SshClient, path: &str) -> Result<()> {
    client.exec(&format!("rm -f '{}'", path)).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_shebang() {
        assert_eq!(
            detect_shebang("#!/bin/bash\necho hello"),
            Some("/bin/bash".to_string())
        );
        assert_eq!(
            detect_shebang("#!/usr/bin/env python3\nprint('hi')"),
            Some("python3".to_string())
        );
        assert_eq!(detect_shebang("echo hello"), None);
    }
}
