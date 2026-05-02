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
        #[arg(long, help = "Show unified diffs for apply actions during dry-run")]
        diff: bool,
        #[arg(long, value_enum, default_value = "text")]
        log_format: flux::cli::LogFormat,
        #[arg(long, value_name = "N")]
        max_concurrency: Option<usize>,
        #[arg(long, default_value = "3")]
        retries: u8,
        #[arg(long, value_name = "SECS")]
        script_timeout: Option<u64>,
        #[arg(long, value_delimiter = ',')]
        only_stage: Vec<String>,
        #[arg(long, value_delimiter = ',')]
        skip_stage: Vec<String>,
        #[arg(long, value_delimiter = ',')]
        only_item: Vec<String>,
        #[arg(long, value_delimiter = ',')]
        tag: Vec<String>,
        #[arg(long, value_delimiter = ',')]
        hosts: Vec<String>,
        #[arg(long)]
        no_cache: bool,
        #[arg(long)]
        resume: bool,
        #[arg(long, default_value = "8")]
        max_hosts: usize,
    },
    Undo {
        config: String,
        #[arg(long)]
        yes: bool,
        #[arg(long, value_enum, default_value = "text")]
        log_format: flux::cli::LogFormat,
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
            diff,
            log_format,
            max_concurrency,
            retries,
            script_timeout,
            only_stage,
            skip_stage,
            only_item,
            tag,
            hosts,
            no_cache,
            resume,
            max_hosts,
        } => {
            flux::cli::run_sync(
                &config,
                save,
                flux::cli::SyncRunOptions {
                    dry_run,
                    diff,
                    log_format,
                    max_concurrency,
                    retries,
                    script_timeout,
                    only_stage,
                    skip_stage,
                    only_item,
                    tag,
                    hosts,
                    no_cache,
                    resume,
                    max_hosts,
                },
            )
            .await
        }
        Commands::Undo {
            config,
            yes,
            log_format,
        } => flux::cli::run_undo(&config, yes, log_format).await,
        Commands::Proxy {
            host,
            local,
            remote,
            key,
            retry,
        } => flux::cli::run_proxy(host, local, remote, key, retry).await,
    }
}
