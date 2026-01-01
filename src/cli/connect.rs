//! Connect CLI command - SSH connection

use crate::cli::common::*;
use crate::config::resolver::ConfigResolver;
use console::style;
use std::process::Command;

/// Run connect command
pub async fn run_connect(
    config_name: Option<String>,
    with_proxy: bool,
) -> anyhow::Result<()> {
    let resolver = ConfigResolver::new();

    // Load configuration
    let config = match config_name {
        Some(name) => resolver.load(&name)?,
        None => resolver.load_default()?,
    };

    println!(
        "{} Connecting to {}@{}:{}",
        CONNECT,
        style(&config.connection.user).cyan(),
        style(&config.connection.host).green(),
        config.connection.port
    );

    // Start proxy if requested
    if with_proxy {
        println!(
            "{} Starting proxy tunnel (remote:{} -> local:{})",
            TUNNEL,
            config.proxy.remote_port,
            config.proxy.local_port
        );
        // TODO: Start proxy in background before connecting
        print_warning("Proxy during connect not yet implemented");
    }

    // Build SSH command
    let mut cmd = Command::new("ssh");

    // Add port if not default
    if config.connection.port != 22 {
        cmd.arg("-p").arg(config.connection.port.to_string());
    }

    // Add key if specified
    if let Some(key) = &config.connection.key {
        cmd.arg("-i").arg(key);
    }

    // Add user@host
    cmd.arg(format!("{}@{}", config.connection.user, config.connection.host));

    // Execute SSH
    let status = cmd.status()?;

    if !status.success() {
        print_error("SSH connection failed");
    }

    Ok(())
}
