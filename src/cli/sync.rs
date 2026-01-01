//! Sync CLI adapter

use crate::cli::common::*;
use crate::config::resolver::ConfigResolver;
use crate::core::error::RemoteError;
use crate::sync::block_sync::BlockSyncResult;
use crate::sync::file_sync::FileSyncResult;
use crate::sync::script_exec::ScriptExecResult;
use crate::sync::service::{
    load_sync_config, SyncCallbacks, SyncResult, SyncService, SyncServiceConfig,
};
use comfy_table::Cell;
use console::style;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

/// CLI callbacks for sync operations
pub struct CliSyncCallbacks {
    spinner: Option<indicatif::ProgressBar>,
    file_count: AtomicUsize,
    block_count: AtomicUsize,
    script_count: AtomicUsize,
}

impl Default for CliSyncCallbacks {
    fn default() -> Self {
        Self::new()
    }
}

impl CliSyncCallbacks {
    pub fn new() -> Self {
        Self {
            spinner: None,
            file_count: AtomicUsize::new(0),
            block_count: AtomicUsize::new(0),
            script_count: AtomicUsize::new(0),
        }
    }
}

impl SyncCallbacks for CliSyncCallbacks {
    fn on_connecting(&self, host: &str) {
        println!("{} Connecting to {}...", CONNECT, style(host).cyan());
    }

    fn on_connected(&self, host: &str) {
        print_success(&format!("Connected to {}", host));
    }

    fn on_key_generated(&self, key_path: &str) {
        print_info(&format!("Generated SSH key: {}", key_path));
    }

    fn on_first_connect(&self, host: &str) {
        print_info(&format!(
            "First connection to {} - running init scripts",
            host
        ));
    }

    fn on_file_sync(&self, result: &FileSyncResult) {
        self.file_count.fetch_add(1, Ordering::SeqCst);

        match result {
            FileSyncResult::Synced { src, dst } => {
                println!(
                    "  {} {} -> {}",
                    UPLOAD,
                    style(src).dim(),
                    style(dst).green()
                );
            }
            FileSyncResult::Skipped { path, reason } => {
                println!(
                    "  {} {} ({})",
                    SKIP,
                    style(path).dim(),
                    style(reason).yellow()
                );
            }
            FileSyncResult::Conflict {
                path,
                local_hash,
                remote_hash,
            } => {
                println!(
                    "  {} {} conflict: local={} remote={}",
                    WARNING,
                    style(path).red(),
                    &local_hash[..8],
                    &remote_hash[..8]
                );
            }
            FileSyncResult::WouldSync { src, dst } => {
                println!(
                    "  {} [dry-run] {} -> {}",
                    SYNC,
                    style(src).dim(),
                    style(dst).cyan()
                );
            }
        }
    }

    fn on_block_sync(&self, result: &BlockSyncResult) {
        self.block_count.fetch_add(1, Ordering::SeqCst);

        match result {
            BlockSyncResult::Synced { name, file } => {
                println!(
                    "  {} Block {} -> {}",
                    SYNC,
                    style(name).green(),
                    style(file).dim()
                );
            }
            BlockSyncResult::Skipped { name, reason } => {
                println!(
                    "  {} Block {} ({})",
                    SKIP,
                    style(name).dim(),
                    style(reason).yellow()
                );
            }
            BlockSyncResult::Conflict {
                name,
                local_hash,
                remote_hash,
            } => {
                println!(
                    "  {} Block {} conflict: local={} remote={}",
                    WARNING,
                    style(name).red(),
                    &local_hash[..8],
                    &remote_hash[..8]
                );
            }
            BlockSyncResult::WouldSync { name, file } => {
                println!(
                    "  {} [dry-run] Block {} -> {}",
                    SYNC,
                    style(name).cyan(),
                    style(file).dim()
                );
            }
        }
    }

    fn on_script_start(&self, script: &str) {
        println!("  {} Running {}...", SCRIPT, style(script).cyan());
    }

    fn on_script_end(&self, result: &ScriptExecResult) {
        self.script_count.fetch_add(1, Ordering::SeqCst);

        match result {
            ScriptExecResult::Success { script, output } => {
                println!("  {} {} completed", SUCCESS, style(script).green());
                if !output.is_empty() {
                    for line in output.lines().take(5) {
                        println!("    {}", style(line).dim());
                    }
                }
            }
            ScriptExecResult::Skipped { script, reason } => {
                println!(
                    "  {} {} ({})",
                    SKIP,
                    style(script).dim(),
                    style(reason).yellow()
                );
            }
            ScriptExecResult::FailedAllowed {
                script,
                code,
                stderr,
            } => {
                println!(
                    "  {} {} failed (exit {}) - allowed",
                    WARNING,
                    style(script).yellow(),
                    code
                );
                if !stderr.is_empty() {
                    for line in stderr.lines().take(3) {
                        println!("    {}", style(line).red());
                    }
                }
            }
            ScriptExecResult::WouldExecute { script } => {
                println!("  {} [dry-run] Would run {}", SCRIPT, style(script).cyan());
            }
        }
    }

    fn on_complete(&self, _result: &SyncResult) {
        println!();
        print_success("Sync complete!");

        let mut table = create_table(vec!["Category", "Count"]);
        table.add_row(vec![
            Cell::new("Files"),
            Cell::new(self.file_count.load(Ordering::SeqCst)),
        ]);
        table.add_row(vec![
            Cell::new("Blocks"),
            Cell::new(self.block_count.load(Ordering::SeqCst)),
        ]);
        table.add_row(vec![
            Cell::new("Scripts"),
            Cell::new(self.script_count.load(Ordering::SeqCst)),
        ]);

        println!("{}", table);
    }

    fn on_error(&self, error: &RemoteError) {
        print_error(&format!("Sync failed: {}", error));
    }
}

/// Run sync command
pub async fn run_sync(
    config_path: &str,
    _ssh_config_name: Option<String>,
    force_init: bool,
    dry_run: bool,
    conflict_override: Option<String>,
) -> anyhow::Result<()> {
    let config_path = PathBuf::from(config_path);

    if !config_path.exists() {
        print_error(&format!("Config file not found: {}", config_path.display()));
        return Ok(());
    }

    println!(
        "{} Loading config: {}",
        INFO,
        style(config_path.display()).cyan()
    );

    // Load config
    let sync_config = load_sync_config(&config_path)?;

    // Create service
    let service_config = SyncServiceConfig {
        force_init,
        dry_run,
        conflict_override,
    };
    let service = SyncService::new(service_config);

    // Create callbacks
    let callbacks = CliSyncCallbacks::new();

    // Show dry-run banner
    if dry_run {
        println!();
        print_warning("DRY-RUN MODE - No changes will be made");
        println!();
    }

    // Run sync
    println!();
    println!("{}", style("=== File Sync ===").bold());

    let result = service.sync(&sync_config, &callbacks).await;

    match result {
        Ok(_) => {
            println!();
            if dry_run {
                print_info("Dry-run complete. Run without --dry-run to apply changes.");
            }
        }
        Err(e) => {
            print_error(&format!("{}", e));
            std::process::exit(1);
        }
    }

    Ok(())
}

/// Run sync command (new version with config name support)
pub async fn run_sync_v2(
    config_name: Option<String>,
    with_proxy: bool,
    force_init: bool,
    dry_run: bool,
    conflict_override: Option<String>,
    _ssh_config_name: Option<String>,
) -> anyhow::Result<()> {
    let resolver = ConfigResolver::new();

    // Load configuration
    let config_name_str = config_name.as_deref().unwrap_or("default");

    println!(
        "{} Loading config: {}",
        INFO,
        style(config_name_str).cyan()
    );

    let flux_config = resolver.load(config_name_str)?;

    // Show connection info
    println!(
        "{} Target: {}@{}:{}",
        CONNECT,
        style(&flux_config.connection.user).cyan(),
        style(&flux_config.connection.host).green(),
        flux_config.connection.port
    );

    // Handle proxy forwarding
    if with_proxy || flux_config.proxy.enabled {
        println!(
            "{} Proxy forwarding enabled (remote:{} -> local:{})",
            TUNNEL,
            flux_config.proxy.remote_port,
            flux_config.proxy.local_port
        );
        // TODO: Start proxy tunnel before sync
        print_warning("Proxy during sync not yet fully implemented");
    }

    // Show dry-run banner
    if dry_run {
        println!();
        print_warning("DRY-RUN MODE - No changes will be made");
    }

    // For now, fall back to legacy sync if we find a config file path
    // This maintains compatibility while we migrate
    let finder = resolver.finder();
    if let Ok(config_path) = finder.find(config_name_str) {
        // Use legacy sync path
        let sync_config = load_sync_config(&config_path)?;

        let service_config = SyncServiceConfig {
            force_init,
            dry_run,
            conflict_override,
        };
        let service = SyncService::new(service_config);
        let callbacks = CliSyncCallbacks::new();

        println!();
        println!("{}", style("=== File Sync ===").bold());

        let result = service.sync(&sync_config, &callbacks).await;

        match result {
            Ok(_) => {
                println!();
                if dry_run {
                    print_info("Dry-run complete. Run without --dry-run to apply changes.");
                }
            }
            Err(e) => {
                print_error(&format!("{}", e));
                std::process::exit(1);
            }
        }
    } else {
        print_error(&format!("Config not found: {}", config_name_str));
        std::process::exit(1);
    }

    Ok(())
}
