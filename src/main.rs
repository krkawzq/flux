//! Flux - SSH remote server configuration sync tool
//!
//! A tool for managing personal configurations on remote temporary environments.

mod config;
mod output;
mod path;
mod ssh;
mod sync;

use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "flux")]
#[command(version, about = "SSH remote server configuration sync tool")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Initialize .flux directory structure
    Init,

    /// Sync configuration to remote server
    Sync {
        /// Configuration name or path
        config: String,

        /// Save connection to ~/.ssh/config with this name
        #[arg(long)]
        save: Option<String>,
    },

    /// Create SSH reverse proxy tunnel (standalone)
    Proxy {
        /// SSH host (from ~/.ssh/config or user@host:port)
        host: String,

        /// Local proxy port (your clash/v2ray port)
        #[arg(short, long, default_value = "7890")]
        local: u16,

        /// Remote listening port
        #[arg(short, long, default_value = "1081")]
        remote: u16,

        /// SSH private key path
        #[arg(short, long)]
        key: Option<String>,

        /// Retry interval in seconds (0 to disable)
        #[arg(long, default_value = "5")]
        retry: u64,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Init => {
            run_init()?;
        }
        Commands::Sync { config, save } => {
            run_sync(&config, save).await?;
        }
        Commands::Proxy {
            host,
            local,
            remote,
            key,
            retry,
        } => {
            run_proxy(&host, local, remote, key, retry).await?;
        }
    }

    Ok(())
}

/// Initialize .flux directory structure
fn run_init() -> anyhow::Result<()> {
    let flux_dir = PathBuf::from(".flux");

    let dirs = ["scripts", "blocks", "files"];

    for dir in &dirs {
        let path = flux_dir.join(dir);
        if !path.exists() {
            std::fs::create_dir_all(&path)?;
            output::print_info(&format!("Created {}", path.display()));
        }
    }

    output::print_status(output::Status::Success, ".flux directory initialized");

    Ok(())
}

/// Run sync command
async fn run_sync(config_name: &str, save_name: Option<String>) -> anyhow::Result<()> {
    // Find and load config
    let (config, config_path) = config::Config::find_and_load(config_name)?;

    output::print_info(&format!("Using config: {}", config_path.display()));

    // Run sync pipeline
    let ssh_config = sync::run_sync(config, &config_path).await?;

    // Save to ~/.ssh/config if requested
    if let Some(name) = save_name {
        save_ssh_config(&name, &ssh_config)?;
    }

    Ok(())
}

/// Save SSH connection to ~/.ssh/config
fn save_ssh_config(name: &str, config: &sync::SshConfigInfo) -> anyhow::Result<()> {
    let ssh_config_path = dirs::home_dir()
        .ok_or_else(|| anyhow::anyhow!("Cannot find home directory"))?
        .join(".ssh")
        .join("config");

    // Create .ssh directory if needed
    if let Some(parent) = ssh_config_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Read existing config
    let existing = std::fs::read_to_string(&ssh_config_path).unwrap_or_default();

    // Check if host already exists and remove it
    let host_pattern = format!("Host {}", name);
    let mut new_content = String::new();
    let mut skip_until_next_host = false;
    let mut found_existing = false;

    for line in existing.lines() {
        if line.trim() == host_pattern {
            skip_until_next_host = true;
            found_existing = true;
            // Also skip the "# Added by flux" comment before it
            if new_content.ends_with("# Added by flux\n") {
                new_content.truncate(new_content.len() - "# Added by flux\n".len());
            }
            continue;
        }
        if skip_until_next_host {
            if line.trim().starts_with("Host ") || line.trim().starts_with("# ") {
                skip_until_next_host = false;
            } else {
                continue;
            }
        }
        new_content.push_str(line);
        new_content.push('\n');
    }

    // Write back the cleaned content if we removed something
    if found_existing {
        std::fs::write(&ssh_config_path, new_content.trim_end())?;
    }

    // Build new entry
    let mut entry = format!("\n# Added by flux\nHost {}\n", name);
    entry.push_str(&format!("    HostName {}\n", config.host));
    entry.push_str(&format!("    User {}\n", config.user));
    if config.port != 22 {
        entry.push_str(&format!("    Port {}\n", config.port));
    }
    if let Some(key) = &config.key {
        // Expand ~ to absolute path for cross-platform compatibility
        let expanded_key = expand_tilde(key);
        entry.push_str(&format!("    IdentityFile {}\n", expanded_key));
        entry.push_str("    IdentitiesOnly yes\n");
    }

    // Append to config
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&ssh_config_path)?;

    use std::io::Write;
    file.write_all(entry.as_bytes())?;

    output::print_status(
        output::Status::Success,
        &format!("Saved to ~/.ssh/config as '{}'", name),
    );
    output::print_info(&format!("You can now connect with: ssh {}", name));

    Ok(())
}

/// Expand ~ to home directory (cross-platform)
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

/// Run standalone proxy command
async fn run_proxy(
    host: &str,
    local_port: u16,
    remote_port: u16,
    key_override: Option<String>,
    retry_interval: u64,
) -> anyhow::Result<()> {
    use dialoguer::Password;
    use std::net::TcpStream as StdTcpStream;

    // Check if local port is listening
    let local_available = StdTcpStream::connect(format!("127.0.0.1:{}", local_port)).is_ok();
    if !local_available {
        output::print_warning(&format!(
            "Local port {} is not listening (no proxy service?)",
            local_port
        ));
    }

    // Parse host - check SSH config first, then parse as address
    let (user, hostname, port, key_from_config) = parse_ssh_host_with_config(host);

    // Use key override if provided, otherwise use key from config or default paths
    let key_path = key_override
        .map(|k| expand_tilde(&k))
        .or(key_from_config)
        .or_else(find_default_key);

    output::print_header("SSH Reverse Proxy Tunnel");
    println!();
    output::print_info(&format!("Remote: {}@{}:{}", user, hostname, port));
    output::print_info(&format!(
        "Tunnel: remote:{} ← local:{}",
        remote_port, local_port
    ));
    if let Some(ref k) = key_path {
        output::print_info(&format!("Key: {}", k));
    }
    println!();

    let mut retry_count = 0u32;
    let mut cached_password: Option<String> = None;

    loop {
        retry_count += 1;

        if retry_count > 1 {
            output::print_info(&format!("Reconnecting (attempt {})...", retry_count));
        } else {
            output::print_info("Connecting...");
        }

        // Only ask for password if no key available and not cached
        let password = if key_path.is_none() && cached_password.is_none() {
            match Password::new().with_prompt("Password").interact() {
                Ok(pw) => {
                    cached_password = Some(pw.clone());
                    Some(pw)
                }
                Err(_) => None,
            }
        } else {
            cached_password.clone()
        };

        let ssh_config = ssh::SshConfig {
            host: hostname.clone(),
            port,
            user: user.clone(),
            key_path: key_path.clone(),
            password,
        };

        // Connect
        match ssh::SshClient::connect(&ssh_config).await {
            Ok(mut client) => {
                output::print_status(output::Status::Success, "Connected");

                // Start reverse forwarding
                match client.start_reverse_forward(local_port, remote_port).await {
                    Ok(_) => {
                        output::print_status(
                            output::Status::Success,
                            &format!(
                                "Tunnel active (remote:{} ← local:{})",
                                remote_port, local_port
                            ),
                        );
                        output::print_info("Press Ctrl+C to stop");

                        // Keep connection alive - wait for disconnect or interrupt
                        loop {
                            tokio::select! {
                                _ = tokio::signal::ctrl_c() => {
                                    println!();
                                    output::print_info("Interrupted, closing...");
                                    client.close().await.ok();
                                    return Ok(());
                                }
                                _ = tokio::time::sleep(std::time::Duration::from_secs(30)) => {
                                    // Keepalive - the SSH client should handle this internally
                                }
                            }
                        }
                    }
                    Err(e) => {
                        output::print_error(&format!("Failed to setup tunnel: {}", e));
                        client.close().await.ok();
                    }
                }
            }
            Err(e) => {
                output::print_error(&format!("Connection failed: {}", e));
            }
        }

        // Retry logic
        if retry_interval == 0 {
            output::print_info("Retry disabled, exiting");
            return Ok(());
        }

        output::print_info(&format!("Retrying in {} seconds...", retry_interval));

        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                println!();
                output::print_info("Interrupted, exiting");
                return Ok(());
            }
            _ = tokio::time::sleep(std::time::Duration::from_secs(retry_interval)) => {}
        }
    }
}

/// Parse SSH host with config file support
/// Returns (user, host, port, key_path)
fn parse_ssh_host_with_config(input: &str) -> (String, String, u16, Option<String>) {
    // First try to read from ~/.ssh/config
    if let Some(config) = read_ssh_config_entry(input) {
        return config;
    }

    // Fallback to parsing the input string
    let (user, host, port) = parse_ssh_host(input);
    (user, host, port, None)
}

/// Read SSH config entry
fn read_ssh_config_entry(name: &str) -> Option<(String, String, u16, Option<String>)> {
    let config_path = dirs::home_dir()?.join(".ssh").join("config");
    let content = std::fs::read_to_string(&config_path).ok()?;

    let mut in_target_host = false;
    let mut hostname = None;
    let mut user = None;
    let mut port = None;
    let mut identity_file = None;

    for line in content.lines() {
        let line = line.trim();

        if line.to_lowercase().starts_with("host ") {
            let host_pattern = line[5..].trim();
            in_target_host = host_pattern == name;
            continue;
        }

        if in_target_host {
            let lower = line.to_lowercase();
            if lower.starts_with("hostname ") {
                hostname = Some(line[9..].trim().to_string());
            } else if lower.starts_with("user ") {
                user = Some(line[5..].trim().to_string());
            } else if lower.starts_with("port ") {
                port = line[5..].trim().parse().ok();
            } else if lower.starts_with("identityfile ") {
                identity_file = Some(expand_tilde(line[13..].trim()));
            }
        }
    }

    // If we found hostname, we have a valid entry
    hostname.map(|h| {
        (
            user.unwrap_or_else(|| "root".to_string()),
            h,
            port.unwrap_or(22),
            identity_file,
        )
    })
}

/// Find default SSH key
fn find_default_key() -> Option<String> {
    let home = dirs::home_dir()?;
    let ssh_dir = home.join(".ssh");

    // Check common key files in order of preference
    let key_names = ["id_ed25519", "id_rsa", "id_ecdsa"];

    for name in key_names {
        let key_path = ssh_dir.join(name);
        if key_path.exists() {
            return Some(key_path.to_string_lossy().to_string());
        }
    }

    None
}

/// Parse SSH host string into (user, host, port)
/// Supports: host, user@host, user@host:port
fn parse_ssh_host(input: &str) -> (String, String, u16) {
    let default_user = "root".to_string();
    let default_port = 22u16;

    // Check if it's user@host format
    if let Some(at_pos) = input.find('@') {
        let user = input[..at_pos].to_string();
        let rest = &input[at_pos + 1..];

        // Check for port
        if let Some(colon_pos) = rest.rfind(':') {
            let host = rest[..colon_pos].to_string();
            let port = rest[colon_pos + 1..].parse().unwrap_or(default_port);
            (user, host, port)
        } else {
            (user, rest.to_string(), default_port)
        }
    } else if let Some(colon_pos) = input.rfind(':') {
        // host:port format
        let host = input[..colon_pos].to_string();
        let port = input[colon_pos + 1..].parse().unwrap_or(default_port);
        (default_user, host, port)
    } else {
        // Just hostname
        (default_user, input.to_string(), default_port)
    }
}
