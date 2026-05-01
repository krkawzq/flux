//! Flux - SSH remote server configuration sync tool
//!
//! A tool for managing personal configurations on remote temporary environments.

mod config;
mod output;
mod path;
mod ssh;
mod sync;

use anyhow::Context;
use clap::{Parser, Subcommand};
use std::collections::HashSet;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

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
        #[arg(short, long, default_value = "7899")]
        local: u16,

        /// Remote listening port
        #[arg(short, long, default_value = "7890")]
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
    let existing = match std::fs::read_to_string(&ssh_config_path) {
        Ok(content) => content,
        Err(err) if err.kind() == ErrorKind::NotFound => String::new(),
        Err(err) => {
            return Err(err).context(format!(
                "failed to read SSH config {}",
                ssh_config_path.display()
            ))
        }
    };

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
            if line.trim().starts_with("Host ") {
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
    let (user, hostname, port, key_from_config) = parse_ssh_host_with_config(host)?;

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
fn parse_ssh_host_with_config(
    input: &str,
) -> anyhow::Result<(String, String, u16, Option<String>)> {
    // First try to read from ~/.ssh/config
    if let Some(config) = read_ssh_config_entry(input)? {
        return Ok(config);
    }

    // Fallback to parsing the input string
    let (user, host, port) = parse_ssh_host(input)?;
    Ok((user, host, port, None))
}

/// Read SSH config entry
fn read_ssh_config_entry(
    name: &str,
) -> anyhow::Result<Option<(String, String, u16, Option<String>)>> {
    let config_path = match dirs::home_dir() {
        Some(home) => home.join(".ssh").join("config"),
        None => return Ok(None),
    };

    let mut visited = HashSet::new();
    read_ssh_config_entry_from(&config_path, name, &mut visited)
}

fn read_ssh_config_entry_from(
    config_path: &Path,
    name: &str,
    visited: &mut HashSet<PathBuf>,
) -> anyhow::Result<Option<(String, String, u16, Option<String>)>> {
    let visit_key =
        std::fs::canonicalize(config_path).unwrap_or_else(|_| config_path.to_path_buf());
    if !visited.insert(visit_key) {
        return Ok(None);
    }

    let content = match std::fs::read_to_string(config_path) {
        Ok(content) => content,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(err).context(format!(
                "failed to read SSH config {}",
                config_path.display()
            ))
        }
    };

    let base_dir = config_path.parent().unwrap_or_else(|| Path::new("."));

    let mut in_target_host = false;
    let mut hostname = None;
    let mut user = None;
    let mut port = None;
    let mut identity_file = None;

    for line in content.lines() {
        let line = line.trim();

        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        if starts_with_keyword(line, "include") {
            let patterns = line["include".len()..].split_whitespace();
            for pattern in patterns {
                for include_path in expand_include_pattern(pattern, base_dir)? {
                    if let Some(entry) = read_ssh_config_entry_from(&include_path, name, visited)? {
                        return Ok(Some(entry));
                    }
                }
            }
            continue;
        }

        if starts_with_keyword(line, "match") {
            eprintln!(
                "Warning: unsupported SSH config Match block in {}: {}",
                config_path.display(),
                line
            );
            in_target_host = false;
            continue;
        }

        if starts_with_keyword(line, "host") {
            let host_patterns = line["host".len()..].split_whitespace().collect::<Vec<_>>();
            if host_patterns.iter().any(|pattern| {
                pattern.contains('*') || pattern.contains('?') || pattern.starts_with('!')
            }) {
                eprintln!(
                    "Warning: unsupported SSH config Host pattern in {}: {}",
                    config_path.display(),
                    line
                );
            }
            in_target_host = host_patterns.iter().any(|pattern| *pattern == name);
            continue;
        }

        if in_target_host {
            if starts_with_keyword(line, "hostname") {
                hostname = Some(line["hostname".len()..].trim().to_string());
            } else if starts_with_keyword(line, "user") {
                user = Some(line["user".len()..].trim().to_string());
            } else if starts_with_keyword(line, "port") {
                let port_str = line["port".len()..].trim();
                port = Some(parse_port(port_str).map_err(|err| {
                    err.context(format!(
                        "invalid Port in SSH config {} for host '{}'",
                        config_path.display(),
                        name
                    ))
                })?);
            } else if starts_with_keyword(line, "identityfile") {
                identity_file = Some(expand_tilde(line["identityfile".len()..].trim()));
            }
        }
    }

    // If we found hostname, we have a valid entry
    Ok(hostname.map(|h| {
        (
            user.unwrap_or_else(|| "root".to_string()),
            h,
            port.unwrap_or(22),
            identity_file,
        )
    }))
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
/// Supports: host, host:port, [ipv6], [ipv6]:port, user@host, user@[ipv6]:port
fn parse_ssh_host(input: &str) -> anyhow::Result<(String, String, u16)> {
    let default_user = "root".to_string();
    let default_port = 22u16;

    // Check if it's user@host format
    if let Some(at_pos) = input.find('@') {
        let user = input[..at_pos].to_string();
        let rest = &input[at_pos + 1..];
        parse_host_and_port(rest, user, default_port)
    } else {
        parse_host_and_port(input, default_user, default_port)
    }
}

fn parse_host_and_port(
    rest: &str,
    user: String,
    default_port: u16,
) -> anyhow::Result<(String, String, u16)> {
    if let Some(closing_bracket) = rest.find(']') {
        if rest.starts_with('[') {
            let host = rest[1..closing_bracket].to_string();
            let remainder = &rest[closing_bracket + 1..];

            if remainder.is_empty() {
                return Ok((user, host, default_port));
            }

            if let Some(port_str) = remainder.strip_prefix(':') {
                let port = parse_port(port_str)?;
                return Ok((user, host, port));
            }
        }
    }

    if let Some(colon_pos) = rest.rfind(':') {
        if !rest[..colon_pos].contains(':') {
            let host = rest[..colon_pos].to_string();
            let port = parse_port(&rest[colon_pos + 1..])?;
            return Ok((user, host, port));
        }
    }

    Ok((user, rest.to_string(), default_port))
}

fn parse_port(port_str: &str) -> anyhow::Result<u16> {
    port_str
        .parse::<u16>()
        .map_err(|err| anyhow::anyhow!("invalid port '{}': {}", port_str, err))
}

fn starts_with_keyword(line: &str, keyword: &str) -> bool {
    if line.len() <= keyword.len() {
        return false;
    }

    let (prefix, remainder) = line.split_at(keyword.len());
    prefix.eq_ignore_ascii_case(keyword)
        && remainder.chars().next().is_some_and(char::is_whitespace)
}

fn expand_include_pattern(pattern: &str, base_dir: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let expanded = PathBuf::from(expand_tilde(pattern));
    let resolved = if expanded.is_absolute() {
        expanded
    } else {
        base_dir.join(expanded)
    };

    if !contains_wildcards(&resolved) {
        return Ok(vec![resolved]);
    }

    let components = resolved
        .components()
        .map(|component| component.as_os_str().to_string_lossy().to_string())
        .filter(|component| component != &std::path::MAIN_SEPARATOR.to_string())
        .collect::<Vec<_>>();

    let mut results = Vec::new();
    let mut current = if resolved.is_absolute() {
        PathBuf::from(std::path::MAIN_SEPARATOR.to_string())
    } else {
        PathBuf::new()
    };

    expand_include_components(&mut current, &components, 0, &mut results)?;
    results.sort();
    results.dedup();
    Ok(results)
}

fn expand_include_components(
    current: &mut PathBuf,
    components: &[String],
    index: usize,
    results: &mut Vec<PathBuf>,
) -> anyhow::Result<()> {
    if index >= components.len() {
        if current.exists() {
            results.push(current.clone());
        }
        return Ok(());
    }

    let component = &components[index];
    if component.is_empty() {
        return expand_include_components(current, components, index + 1, results);
    }

    if contains_wildcards_str(component) {
        let search_dir = if current.as_os_str().is_empty() {
            Path::new(".")
        } else {
            current.as_path()
        };

        for entry in std::fs::read_dir(search_dir)? {
            let entry = entry?;
            let file_name = entry.file_name().to_string_lossy().to_string();
            if wildcard_matches(component, &file_name) {
                let previous = current.clone();
                *current = previous.join(&file_name);
                expand_include_components(current, components, index + 1, results)?;
                *current = previous;
            }
        }
    } else {
        let previous = current.clone();
        *current = if current.as_os_str().is_empty() {
            PathBuf::from(component)
        } else {
            current.join(component)
        };
        expand_include_components(current, components, index + 1, results)?;
        *current = previous;
    }

    Ok(())
}

fn contains_wildcards(path: &Path) -> bool {
    path.to_string_lossy().contains(['*', '?'])
}

fn contains_wildcards_str(value: &str) -> bool {
    value.contains('*') || value.contains('?')
}

fn wildcard_matches(pattern: &str, candidate: &str) -> bool {
    wildcard_matches_bytes(pattern.as_bytes(), candidate.as_bytes())
}

fn wildcard_matches_bytes(pattern: &[u8], candidate: &[u8]) -> bool {
    if pattern.is_empty() {
        return candidate.is_empty();
    }

    match pattern[0] {
        b'*' => {
            wildcard_matches_bytes(&pattern[1..], candidate)
                || (!candidate.is_empty() && wildcard_matches_bytes(pattern, &candidate[1..]))
        }
        b'?' => !candidate.is_empty() && wildcard_matches_bytes(&pattern[1..], &candidate[1..]),
        ch => {
            !candidate.is_empty()
                && ch == candidate[0]
                && wildcard_matches_bytes(&pattern[1..], &candidate[1..])
        }
    }
}
