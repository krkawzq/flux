//! Sync module
//!
//! Orchestrates the sync pipeline: file -> script -> block

pub mod plan;
pub mod block;
pub mod file;
pub mod script;

use crate::config::Config;
use crate::output::{self, Status};
use crate::remote::ssh::{SshClient, SshConfig};
use anyhow::Result;
use dialoguer::{Input, Password};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SyncError {
    #[error("block: {0}")]
    Block(#[from] block::BlockError),
    #[error("file: {0}")]
    File(#[from] file::FileError),
    #[error("script: {0}")]
    Script(#[from] script::ScriptError),
    #[error("remote: {0}")]
    Remote(#[from] crate::remote::RemoteOpsError),
}

/// SSH config info for saving to ~/.ssh/config
pub struct SshConfigInfo {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub key: Option<String>,
}

/// Run the sync pipeline
pub async fn run_sync(config: Config, config_path: &std::path::Path) -> Result<SshConfigInfo> {
    config.validate()?;

    // Preprocess config paths - resolve relative paths to .flux subdirectories
    let config = resolve_config_paths(config, config_path);

    // Resolve SSH connection parameters (with interactive prompts)
    let ssh_config = resolve_ssh_config(&config)?;

    // Save config info for later
    let config_info = SshConfigInfo {
        host: ssh_config.host.clone(),
        port: ssh_config.port,
        user: ssh_config.user.clone(),
        key: ssh_config.key_path.clone(),
    };

    // Phase 1: Connect
    output::print_header(&format!(
        "Connecting to {}@{}:{}...",
        ssh_config.user, ssh_config.host, ssh_config.port
    ));

    let mut client = SshClient::connect(&ssh_config).await?;
    output::print_status(Status::Success, "SSH connection established");

    let mut total_success = 0;
    let mut total_failed = 0;
    let mut total_skipped = 0;

    // Register key first (before proxy, so it always runs)
    if config.register_key {
        if let Some(key_path) = &config.key {
            if let Err(err) = register_public_key(&client, key_path).await {
                output::print_warning(&format!("Failed to register public key: {}", err));
                total_failed += 1;
            }
        }
    }

    // Phase 1.5: Setup proxy if enabled
    if config.proxy.enabled {
        output::print_header(&format!(
            "Setting up proxy tunnel (local:{} → remote:{})...",
            config.proxy.local_port, config.proxy.remote_port
        ));

        // Start reverse port forwarding
        match client
            .start_reverse_forward(config.proxy.local_port, config.proxy.remote_port)
            .await
        {
            Ok(_) => {
                output::print_status(
                    Status::Success,
                    &format!(
                        "Proxy tunnel established (remote 127.0.0.1:{} → local {})",
                        config.proxy.remote_port, config.proxy.local_port
                    ),
                );
            }
            Err(e) => {
                output::print_error(&format!("Failed to setup proxy tunnel: {}", e));
                output::print_info("Proxy is required for this sync, exiting...");
                client.close().await?;
                anyhow::bail!("Proxy tunnel setup failed");
            }
        }
    }

    println!();

    // Phase 2: Sync pipeline
    // 2.1: File sync
    let mut file_status = std::collections::HashMap::new();
    if !config.file.is_empty() {
        let (file_results, statuses) = file::sync_files(&client, &config.file).await?;
        file_status = statuses;

        for r in &file_results {
            match r.status {
                Status::Success => total_success += 1,
                Status::Failed => total_failed += 1,
                Status::Skip => total_skipped += 1,
            }
        }

        println!();
    }

    // 2.2: Script execution
    if !config.script.is_empty() {
        let script_results = script::exec_scripts(
            &client,
            &config.script,
            &file_status,
            &config.interpreter,
            &config.flags,
        )
        .await?;

        for r in &script_results {
            match r.status {
                Status::Success => total_success += 1,
                Status::Failed => total_failed += 1,
                Status::Skip => total_skipped += 1,
            }
        }

        println!();
    }

    // 2.3: Block sync
    if !config.block.is_empty() {
        let block_results =
            block::sync_blocks(&client, &config.block, &config.comment_template).await?;

        for r in &block_results {
            match r.status {
                Status::Success => total_success += 1,
                Status::Failed => total_failed += 1,
                Status::Skip => total_skipped += 1,
            }
        }
    }

    // Summary
    output::print_summary(total_success, total_failed, total_skipped);

    // Close connection
    client.close().await?;

    Ok(config_info)
}

/// Resolve SSH config with interactive prompts for missing values
fn resolve_ssh_config(config: &Config) -> Result<SshConfig> {
    let host = match &config.host {
        Some(h) if !h.is_empty() => h.clone(),
        _ => Input::new().with_prompt("Host").interact_text()?,
    };

    let port = match config.port {
        Some(p) if p > 0 => p,
        _ => Input::new()
            .with_prompt("Port")
            .default(22u16)
            .interact_text()?,
    };

    let user = match &config.user {
        Some(u) if !u.is_empty() => u.clone(),
        _ => Input::new()
            .with_prompt("User")
            .default("root".to_string())
            .interact_text()?,
    };

    // Key is optional, no prompt
    let key_path = config.key.clone();

    // Password: only prompt if no key or key doesn't exist
    let password = match &config.password {
        Some(p) if !p.is_empty() => Some(p.clone()),
        _ => {
            let need_password = key_path.as_ref().map_or(true, |k| {
                let expanded = expand_tilde(k);
                !std::path::Path::new(&expanded).exists()
            });

            if need_password {
                let pw = Password::new().with_prompt("Password").interact()?;
                Some(pw)
            } else {
                None
            }
        }
    };

    Ok(SshConfig {
        host,
        port,
        user,
        key_path,
        password,
    })
}

/// Register public key to authorized_keys
async fn register_public_key(client: &SshClient, key_path: &str) -> Result<()> {
    let pub_key_path = format!("{}.pub", key_path);
    let expanded_path = expand_tilde(&pub_key_path);

    output::print_header("Registering public key...");

    if !std::path::Path::new(&expanded_path).exists() {
        output::print_warning(&format!("Public key not found: {}", expanded_path));
        return Ok(());
    }

    let pub_key = std::fs::read_to_string(&expanded_path)?;
    let pub_key = pub_key.trim();

    // Extract key fingerprint for checking
    let key_parts: Vec<&str> = pub_key.split_whitespace().collect();
    if key_parts.len() < 2 {
        output::print_warning("Invalid public key format");
        return Ok(());
    }
    let key_fingerprint = key_parts[1];

    // Ensure .ssh directory exists with correct permissions
    client.exec("mkdir -p ~/.ssh && chmod 700 ~/.ssh").await?;

    // Read current authorized_keys content via SFTP
    let auth_keys_path = "~/.ssh/authorized_keys";
    let current_content = match client.read_remote_file(auth_keys_path).await {
        Ok(content) => String::from_utf8_lossy(&content).to_string(),
        Err(_) => String::new(), // File doesn't exist
    };

    // Check if key already exists by searching for the fingerprint
    if current_content.contains(key_fingerprint) {
        output::print_info("Public key already registered");
        return Ok(());
    }

    // Write public key to a temp file first, then append
    let pid = std::process::id();
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let temp_path = format!("/tmp/.flux_pubkey_{}_{}", pid, nanos);
    client
        .write_remote_file(&temp_path, pub_key.as_bytes())
        .await?;

    // Append temp file to authorized_keys and set permissions
    let result = client
        .exec(&format!(
            "cat {} >> ~/.ssh/authorized_keys && chmod 600 ~/.ssh/authorized_keys && rm -f {}",
            shell_quote(&temp_path),
            shell_quote(&temp_path)
        ))
        .await?;

    if result.exit_code != 0 {
        let _ = client
            .exec(&format!("rm -f {}", shell_quote(&temp_path)))
            .await;
        output::print_warning(&format!("Failed to append key: {}", result.stderr));
        return Ok(());
    }

    output::print_status(Status::Success, "Public key registered");
    Ok(())
}

/// Expand ~ to home directory
fn expand_tilde(path: &str) -> String {
    if path.starts_with("~/") {
        if let Some(home) = dirs::home_dir() {
            return path.replacen("~", &home.to_string_lossy(), 1);
        }
    } else if path == "~" {
        if let Some(home) = dirs::home_dir() {
            return home.to_string_lossy().to_string();
        }
    }
    path.to_string()
}

/// Resolve config paths - map relative paths to .flux subdirectories
fn resolve_config_paths(mut config: Config, config_path: &Path) -> Config {
    let flux_dir = config.resolve_root(config_path);

    // Helper to resolve a local path
    let resolve_local = |path: &str, subdir: &str| -> String {
        // Skip remote paths (starting with :) and absolute paths
        if path.starts_with(':') || path.starts_with('/') || path.starts_with('~') {
            return path.to_string();
        }

        // Skip if path contains directory separators (already has path)
        if path.contains('/') || path.contains('\\') {
            // But still try to resolve relative to flux_dir
            let full_path = flux_dir.join(path);
            if full_path.exists() {
                return full_path.to_string_lossy().to_string();
            }
            return path.to_string();
        }

        // Try .flux/<subdir>/<path> first
        let subdir_path = flux_dir.join(subdir).join(path);
        if subdir_path.exists() {
            return subdir_path.to_string_lossy().to_string();
        }

        // Try .flux/<path> directly
        let direct_path = flux_dir.join(path);
        if direct_path.exists() {
            return direct_path.to_string_lossy().to_string();
        }

        // Fallback: return as-is (will fail later with clear error)
        path.to_string()
    };

    // Resolve file sources
    for file in &mut config.file {
        file.src = resolve_local(&file.src, "files");
    }

    // Resolve script paths
    for script in &mut config.script {
        script.path = resolve_local(&script.path, "scripts");
    }

    // Resolve block paths
    for block in &mut config.block {
        block.path = resolve_local(&block.path, "blocks");
    }

    config
}

fn shell_quote(value: &str) -> String {
    if value.is_empty() {
        return "''".to_string();
    }

    format!("'{}'", value.replace('\'', r#"'"'"'"#))
}
