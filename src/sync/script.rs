//! Script execution module
//!
//! Handles script execution on remote server.

use crate::config::ScriptItem;
use crate::output::{self, Status};
use crate::path::FluxPath;
use crate::ssh::SshClient;
use anyhow::Result;
use sha2::{Digest, Sha256};
use std::collections::HashMap;

/// Result of a script execution
#[derive(Debug)]
pub struct ScriptResult {
    pub status: Status,
    pub reason: Option<String>,
}

/// Execute all scripts
pub async fn exec_scripts(
    client: &SshClient,
    scripts: &[ScriptItem],
    file_status: &HashMap<String, bool>,
    default_interpreter: &str,
    default_flags: &[String],
) -> Result<Vec<ScriptResult>> {
    let mut results = Vec::new();

    for script in scripts {
        // Check dependencies
        let deps_ok = script.dependencies.iter().all(|dep| {
            file_status
                .get(dep)
                .copied()
                .expect("validated by Config::validate")
        });

        if !deps_ok {
            output::print_script(&script.path);
            output::print_script_result(Status::Skip, Some("dependency failed"));
            results.push(ScriptResult {
                status: Status::Skip,
                reason: Some("dependency failed".to_string()),
            });
            continue;
        }

        let result = exec_script(client, script, default_interpreter, default_flags).await;

        output::print_script(&script.path);
        output::print_script_result(result.status, result.reason.as_deref());

        results.push(result);
    }

    Ok(results)
}

/// Execute a single script
async fn exec_script(
    client: &SshClient,
    script: &ScriptItem,
    default_interpreter: &str,
    default_flags: &[String],
) -> ScriptResult {
    match exec_script_inner(client, script, default_interpreter, default_flags).await {
        Ok((status, reason)) => ScriptResult { status, reason },
        Err(e) => ScriptResult {
            status: Status::Failed,
            reason: Some(e.to_string()),
        },
    }
}

async fn exec_script_inner(
    client: &SshClient,
    script: &ScriptItem,
    default_interpreter: &str,
    default_flags: &[String],
) -> Result<(Status, Option<String>)> {
    let path = FluxPath::parse(&script.path);
    let interpreter = script.interpreter.as_deref().unwrap_or(default_interpreter);
    let flags = script.flags.as_deref().unwrap_or(default_flags);

    let remote_script_path = if path.is_remote() {
        // Script is already on remote
        let remote_path = path.resolve_remote()?;
        let remote_path = client.expand_remote_path(&remote_path).await?;

        // Check if script exists
        if !client.file_exists(&remote_path).await? {
            return Ok((
                Status::Failed,
                Some("script not found on remote".to_string()),
            ));
        }

        remote_path
    } else {
        // Script is local, upload to /tmp
        let local_path = path.resolve_local()?;

        if !local_path.exists() {
            return Ok((Status::Failed, Some("script not found locally".to_string())));
        }

        // Read script content and compute hash for unique filename
        let content = std::fs::read(&local_path)?;
        let hash = compute_hash(&content);
        let remote_path = format!("/tmp/flux_script_{}.sh", &hash[..8]);

        // Upload script
        client.write_remote_file(&remote_path, &content).await?;
        client.chmod(&remote_path, "755").await?;

        remote_path
    };

    // Build command
    let mut command_parts = Vec::with_capacity(2 + flags.len() + script.args.len());
    command_parts.push(shell_quote(interpreter));
    command_parts.extend(flags.iter().map(|flag| shell_quote(flag)));
    command_parts.push(shell_quote(&remote_script_path));
    command_parts.extend(script.args.iter().map(|arg| shell_quote(arg)));
    let command = command_parts.join(" ");

    // Execute with streaming output
    let exit_code = client.exec_interactive(&command).await?;

    if exit_code != 0 {
        return Ok((Status::Failed, Some(format!("exit code: {}", exit_code))));
    }

    Ok((Status::Success, None))
}

/// Compute SHA256 hash of content
fn compute_hash(content: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content);
    let result = hasher.finalize();
    hex::encode(result)
}

/// Simple hex encoding
mod hex {
    pub fn encode(bytes: impl AsRef<[u8]>) -> String {
        bytes
            .as_ref()
            .iter()
            .map(|b| format!("{:02x}", b))
            .collect()
    }
}

fn shell_quote(value: &str) -> String {
    if value.is_empty() {
        return "''".to_string();
    }

    format!("'{}'", value.replace('\'', r#"'"'"'"#))
}
