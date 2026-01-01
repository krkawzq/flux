//! CLI entry point for the flux tool

// Allow dead code during development - these functions will be used as the project matures
#![allow(dead_code)]

use clap::{Parser, Subcommand};

mod cli;
mod config;
mod core;
mod proxy;
mod shell;
mod state;
mod sync;

use cli::{connect, init, proxy as proxy_cli, status, sync as sync_cli};

#[derive(Parser)]
#[command(name = "flux")]
#[command(author, version, about = "SSH remote server management tool")]
#[command(
    long_about = "Flux - A powerful tool for managing remote servers with configuration synchronization, SSH proxy tunnels, and more."
)]
struct Cli {
    /// Enable verbose logging
    #[arg(short, long, global = true)]
    verbose: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Initialize .flux directory structure
    Init {
        /// Initialize global ~/.flux directory instead
        #[arg(long)]
        global: bool,

        /// Don't create example files
        #[arg(long)]
        no_example: bool,
    },

    /// Show flux workspace info
    Info,

    /// Sync configuration to remote server
    Sync {
        /// Configuration name or path (default: "default")
        config: Option<String>,

        /// Enable proxy forwarding during sync
        #[arg(short, long)]
        proxy: bool,

        /// Force init mode (treat as first connection)
        #[arg(long)]
        force_init: bool,

        /// Preview mode (don't make changes)
        #[arg(long)]
        dry_run: bool,

        /// Override conflict strategy
        #[arg(long)]
        conflict: Option<String>,

        /// Save SSH config entry with this name
        #[arg(long)]
        ssh_config: Option<String>,
    },

    /// Start or manage SSH proxy tunnels
    Proxy {
        #[command(subcommand)]
        action: Option<ProxyAction>,

        /// Configuration name (when starting without subcommand)
        #[arg(value_name = "CONFIG")]
        config: Option<String>,
    },

    /// Show status of all services
    Status {
        /// Show verbose information
        #[arg(short, long)]
        verbose: bool,
    },

    /// Connect to remote server via SSH
    Connect {
        /// Configuration name or path
        config: Option<String>,

        /// Start proxy tunnel before connecting
        #[arg(short, long)]
        proxy: bool,
    },
}

#[derive(Subcommand)]
enum ProxyAction {
    /// Start proxy tunnel (alternative syntax)
    Start {
        /// Configuration name
        config: String,

        /// Run in foreground
        #[arg(short, long)]
        foreground: bool,

        /// Override local port
        #[arg(short, long)]
        local_port: Option<u16>,

        /// Override remote port
        #[arg(short, long)]
        remote_port: Option<u16>,
    },

    /// Stop proxy tunnel
    Stop {
        /// Proxy instance name (default: stop all)
        name: Option<String>,
    },

    /// Restart proxy tunnel
    Restart {
        /// Proxy instance name
        name: String,
    },

    /// Show proxy logs
    Logs {
        /// Proxy instance name
        name: String,

        /// Number of lines to show
        #[arg(short = 'n', long, default_value = "50")]
        lines: usize,

        /// Follow log output
        #[arg(short, long)]
        follow: bool,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // Initialize logging
    let log_level = if cli.verbose { "debug" } else { "info" };
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| log_level.into()),
        )
        .init();

    match cli.command {
        // Initialize .flux directory
        Commands::Init { global, no_example } => {
            init::run_init(global, no_example)?;
        }

        // Show workspace info
        Commands::Info => {
            init::run_info()?;
        }

        // Sync configuration
        Commands::Sync {
            config,
            proxy,
            force_init,
            dry_run,
            conflict,
            ssh_config,
        } => {
            sync_cli::run_sync_v2(
                config,
                proxy,
                force_init,
                dry_run,
                conflict,
                ssh_config,
            )
            .await?;
        }

        // Proxy commands
        Commands::Proxy { action, config } => {
            match action {
                // Subcommand style: flux proxy start/stop/logs
                Some(ProxyAction::Start {
                    config,
                    foreground,
                    local_port,
                    remote_port,
                }) => {
                    proxy_cli::run_proxy_start_v2(&config, foreground, local_port, remote_port)
                        .await?;
                }
                Some(ProxyAction::Stop { name }) => {
                    proxy_cli::run_proxy_stop(name).await?;
                }
                Some(ProxyAction::Restart { name }) => {
                    proxy_cli::run_proxy_restart(&name).await?;
                }
                Some(ProxyAction::Logs {
                    name,
                    lines,
                    follow,
                }) => {
                    proxy_cli::run_proxy_logs(&name, lines, follow).await?;
                }

                // Direct style: flux proxy <config>
                None => {
                    if let Some(config_name) = config {
                        // Start proxy with config
                        proxy_cli::run_proxy_start_v2(&config_name, false, None, None).await?;
                    } else {
                        // Show status
                        status::run_status(false).await?;
                    }
                }
            }
        }

        // Show status
        Commands::Status { verbose } => {
            status::run_status(verbose).await?;
        }

        // Connect via SSH
        Commands::Connect { config, proxy } => {
            connect::run_connect(config, proxy).await?;
        }
    }

    Ok(())
}
