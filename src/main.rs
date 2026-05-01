//! Flux CLI binary entry - thin dispatcher over `flux::cli::*`.

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "flux", version, about = "SSH remote configuration sync tool")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Init,
    Sync {
        config: String,
        #[arg(long)]
        save: Option<String>,
        #[arg(long, help = "Compute the plan and print it without applying changes")]
        dry_run: bool,
        #[arg(long, value_name = "N")]
        max_concurrency: Option<usize>,
    },
    Proxy {
        host: String,
        #[arg(short, long, default_value = "7899")]
        local: u16,
        #[arg(short, long, default_value = "7890")]
        remote: u16,
        #[arg(short, long)]
        key: Option<String>,
        #[arg(long, default_value = "5")]
        retry: u64,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Init => flux::cli::run_init().await,
        Commands::Sync {
            config,
            save,
            dry_run,
            max_concurrency,
        } => flux::cli::run_sync(&config, save, dry_run, max_concurrency).await,
        Commands::Proxy {
            host,
            local,
            remote,
            key,
            retry,
        } => flux::cli::run_proxy(host, local, remote, key, retry).await,
    }
}
