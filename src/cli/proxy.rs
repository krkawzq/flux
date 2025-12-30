//! Proxy CLI adapter

use crate::cli::common::*;
use crate::core::error::RemoteError;
use crate::core::ssh::SshConfig;
use crate::proxy::models::{ProxyConfig, ProxyMode, ProxyState, ProxyStatus};
use crate::proxy::service::{DefaultProxyCallbacks, ProxyCallbacks, ProxyService};
use comfy_table::{Cell, Color};
use console::style;

/// CLI callbacks for proxy operations
pub struct CliProxyCallbacks;

impl ProxyCallbacks for CliProxyCallbacks {
    fn on_starting(&self, name: &str) {
        println!("{} Starting proxy: {}", TUNNEL, style(name).cyan());
    }

    fn on_connected(&self, name: &str) {
        print_success(&format!("SSH connected for {}", name));
    }

    fn on_tunnel_established(&self, name: &str, remote_port: u16) {
        print_success(&format!(
            "Tunnel established: {} on remote port {}",
            name, remote_port
        ));
    }

    fn on_reconnecting(&self, name: &str, attempt: u32) {
        print_warning(&format!("Reconnecting {} (attempt {})...", name, attempt));
    }

    fn on_stopped(&self, name: &str) {
        print_info(&format!("Stopped proxy: {}", name));
    }

    fn on_error(&self, name: &str, error: &RemoteError) {
        print_error(&format!("Proxy {} error: {}", name, error));
    }
}

/// Run proxy start command
pub async fn run_proxy_start(
    name: &str,
    local_port: Option<u16>,
    remote_port: u16,
    mode: &str,
    builtin: bool,
    foreground: bool,
    config_path: Option<String>,
) -> anyhow::Result<()> {
    let service = ProxyService::new();
    let callbacks = CliProxyCallbacks;

    // Parse mode
    let proxy_mode = match mode.to_lowercase().as_str() {
        "http" => ProxyMode::Http,
        _ => ProxyMode::Socks5,
    };

    // Build config
    let config = ProxyConfig {
        remote_port,
        local_port,
        local_host: "127.0.0.1".to_string(),
        mode: proxy_mode,
        use_builtin: builtin,
        ..Default::default()
    };

    // Build SSH config (placeholder - would load from ~/.ssh/config)
    let ssh_config = SshConfig::new(name, "root").with_port(22);

    println!();
    println!("{}", style("=== Proxy Configuration ===").bold());
    println!("  Name:        {}", style(name).cyan());
    println!("  Remote Port: {}", style(remote_port).green());
    println!(
        "  Local Port:  {}",
        style(local_port.unwrap_or(7890)).green()
    );
    println!("  Mode:        {}", style(mode).yellow());
    println!(
        "  Built-in:    {}",
        if builtin {
            style("yes").green()
        } else {
            style("no").dim()
        }
    );
    println!(
        "  Foreground:  {}",
        if foreground {
            style("yes").green()
        } else {
            style("no").dim()
        }
    );
    println!();

    match service
        .start(name, name, config, ssh_config, foreground, &callbacks)
        .await
    {
        Ok(state) => {
            if foreground {
                // Foreground mode - blocks until stopped
                println!("Press Ctrl+C to stop the proxy...");
            } else {
                println!();
                print_success(&format!("Proxy started in background (PID: {})", state.pid));
                println!();
                println!(
                    "Use '{}' to check status",
                    style("remote proxy status").cyan()
                );
                println!(
                    "Use '{}' to stop",
                    style(format!("remote proxy stop {}", name)).cyan()
                );
            }
        }
        Err(e) => {
            print_error(&format!("Failed to start proxy: {}", e));
            std::process::exit(1);
        }
    }

    Ok(())
}

/// Run proxy stop command
pub async fn run_proxy_stop(name: Option<String>) -> anyhow::Result<()> {
    let service = ProxyService::new();
    let callbacks = CliProxyCallbacks;

    match name {
        Some(n) => match service.stop(&n, &callbacks).await {
            Ok(_) => print_success(&format!("Stopped proxy: {}", n)),
            Err(e) => {
                print_error(&format!("Failed to stop {}: {}", n, e));
                std::process::exit(1);
            }
        },
        None => {
            // Stop all
            println!("{} Stopping all proxies...", INFO);
            match service.stop_all(&callbacks).await {
                Ok(_) => print_success("All proxies stopped"),
                Err(e) => {
                    print_error(&format!("Error stopping proxies: {}", e));
                }
            }
        }
    }

    Ok(())
}

/// Run proxy status command
pub async fn run_proxy_status(name: Option<String>) -> anyhow::Result<()> {
    let service = ProxyService::new();

    let states: Vec<ProxyState> = match name {
        Some(n) => match service.get_state(&n)? {
            Some(state) => vec![state],
            None => {
                print_warning(&format!("No proxy found with name: {}", n));
                return Ok(());
            }
        },
        None => service.get_all_states()?,
    };

    if states.is_empty() {
        print_info("No proxies running");
        return Ok(());
    }

    let mut table = create_table(vec![
        "Name",
        "Status",
        "Remote Port",
        "Local Port",
        "Mode",
        "PID",
        "Uptime",
    ]);

    for state in states {
        let status_str = format!("{}", state.status);
        let status_color = match state.status {
            ProxyStatus::Running => Color::Green,
            ProxyStatus::Starting => Color::Yellow,
            ProxyStatus::Reconnecting { .. } => Color::Yellow,
            ProxyStatus::Degraded { .. } => Color::Red,
            ProxyStatus::Stopped => Color::DarkGrey,
        };

        let uptime = chrono::Utc::now().timestamp() - state.started_at;

        table.add_row(vec![
            Cell::new(&state.name).fg(Color::Cyan),
            Cell::new(&status_str).fg(status_color),
            Cell::new(state.config.remote_port),
            Cell::new(state.config.local_port.unwrap_or(7890)),
            Cell::new(format!("{:?}", state.config.mode)),
            Cell::new(state.pid),
            Cell::new(format_duration(uptime)),
        ]);
    }

    println!();
    println!("{}", table);
    println!();

    Ok(())
}

/// Run proxy restart command
pub async fn run_proxy_restart(name: &str) -> anyhow::Result<()> {
    let service = ProxyService::new();
    let callbacks = CliProxyCallbacks;

    // Get current state
    let state = match service.get_state(name)? {
        Some(s) => s,
        None => {
            print_error(&format!("Proxy not found: {}", name));
            std::process::exit(1);
        }
    };

    // Stop
    println!("{} Restarting {}...", SYNC, style(name).cyan());
    service.stop(name, &callbacks).await?;

    // TODO: Re-start with saved config
    print_info("Restart functionality requires saved configuration. Use 'proxy start' instead.");

    Ok(())
}

/// Run proxy logs command
pub async fn run_proxy_logs(name: &str, lines: usize, follow: bool) -> anyhow::Result<()> {
    use crate::core::platform::get_background_service;

    let bg_service = get_background_service();
    let log_path = bg_service.log_path(name);

    if !log_path.exists() {
        print_warning(&format!("No logs found for: {}", name));
        return Ok(());
    }

    println!("{} Logs for: {}", INFO, style(name).cyan());
    println!("{}", style(format!("File: {}", log_path.display())).dim());
    println!();

    // Read last N lines
    let content = std::fs::read_to_string(&log_path)?;
    let log_lines: Vec<&str> = content.lines().collect();
    let start = if log_lines.len() > lines {
        log_lines.len() - lines
    } else {
        0
    };

    for line in &log_lines[start..] {
        println!("{}", line);
    }

    if follow {
        print_info("Follow mode not yet implemented. Use 'tail -f' on the log file.");
    }

    Ok(())
}
