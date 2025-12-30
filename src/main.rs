//! CLI entry point for the remote tool

use clap::{Parser, Subcommand};

mod cli;
mod core;
mod proxy;
mod shell;
mod state;
mod sync;

use cli::{proxy as proxy_cli, sync as sync_cli};

#[derive(Parser)]
#[command(name = "remote")]
#[command(author, version, about = "SSH remote server management tool", long_about = None)]
struct Cli {
    /// Enable verbose logging
    #[arg(short, long, global = true)]
    verbose: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Sync remote server configuration
    Sync {
        /// Configuration file path (TOML)
        config: String,

        /// Save SSH configuration to ~/.ssh/config with specified Host name
        #[arg(long)]
        ssh_config: Option<String>,

        /// Force init mode (treat as first connection)
        #[arg(long)]
        force_init: bool,

        /// Preview mode (don't make changes)
        #[arg(long)]
        dry_run: bool,

        /// Override conflict strategy
        #[arg(long)]
        conflict: Option<String>,
    },

    /// Manage SSH reverse proxy tunnels
    Proxy {
        #[command(subcommand)]
        action: ProxyAction,
    },
}

#[derive(Subcommand)]
enum ProxyAction {
    /// Start SSH proxy tunnel
    Start {
        /// Proxy instance name (SSH config host name)
        name: String,

        /// Local proxy port
        #[arg(short, long)]
        local_port: Option<u16>,

        /// Remote proxy port
        #[arg(short, long, default_value = "1081")]
        remote_port: u16,

        /// Proxy mode: http or socks5
        #[arg(short, long, default_value = "socks5")]
        mode: String,

        /// Use built-in proxy server
        #[arg(short, long)]
        builtin: bool,

        /// Run in foreground
        #[arg(short, long)]
        foreground: bool,

        /// Configuration file path
        #[arg(short, long)]
        config: Option<String>,
    },

    /// Stop SSH proxy tunnel
    Stop {
        /// Proxy instance name (default: stop all)
        name: Option<String>,
    },

    /// Show proxy tunnel status
    Status {
        /// Proxy instance name (default: show all)
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
        Commands::Sync {
            config,
            ssh_config,
            force_init,
            dry_run,
            conflict,
        } => {
            sync_cli::run_sync(&config, ssh_config, force_init, dry_run, conflict).await?;
        }
        Commands::Proxy { action } => match action {
            ProxyAction::Start {
                name,
                local_port,
                remote_port,
                mode,
                builtin,
                foreground,
                config,
            } => {
                proxy_cli::run_proxy_start(
                    &name,
                    local_port,
                    remote_port,
                    &mode,
                    builtin,
                    foreground,
                    config,
                )
                .await?;
            }
            ProxyAction::Stop { name } => {
                proxy_cli::run_proxy_stop(name).await?;
            }
            ProxyAction::Status { name } => {
                proxy_cli::run_proxy_status(name).await?;
            }
            ProxyAction::Restart { name } => {
                proxy_cli::run_proxy_restart(&name).await?;
            }
            ProxyAction::Logs {
                name,
                lines,
                follow,
            } => {
                proxy_cli::run_proxy_logs(&name, lines, follow).await?;
            }
        },
    }

    Ok(())
}
