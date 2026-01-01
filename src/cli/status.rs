//! Status CLI command - show overall status

use crate::cli::common::*;
use crate::config::finder::ConfigFinder;
use crate::proxy::models::ProxyStatus;
use crate::proxy::service::ProxyService;
use comfy_table::{Cell, Color};
use console::style;

/// Run status command
pub async fn run_status(verbose: bool) -> anyhow::Result<()> {
    let finder = ConfigFinder::new();
    let proxy_service = ProxyService::new();

    println!("{} Flux Status", INFO);
    println!();

    // Workspace info
    print_workspace_info(&finder);

    // Proxy status
    println!();
    print_proxy_status(&proxy_service, verbose)?;

    Ok(())
}

/// Print workspace information
fn print_workspace_info(finder: &ConfigFinder) {
    println!("{}", style("Workspace").bold());

    if let Some(local) = finder.local_dir() {
        println!(
            "  Local:  {}",
            style(local.display()).cyan()
        );
    } else {
        println!(
            "  Local:  {}",
            style("not initialized").dim()
        );
    }

    let global = finder.global_dir();
    if global.exists() {
        println!(
            "  Global: {}",
            style(global.display()).cyan()
        );
    }
}

/// Print proxy status
fn print_proxy_status(service: &ProxyService, _verbose: bool) -> anyhow::Result<()> {
    println!("{}", style("Proxies").bold());

    let states = service.get_all_states()?;

    if states.is_empty() {
        println!("  {}", style("No active proxies").dim());
        return Ok(());
    }

    let mut table = create_table(vec![
        "Name",
        "Status",
        "Remote",
        "Local",
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
            Cell::new(format!("{}", state.config.mode)),
            Cell::new(state.pid),
            Cell::new(format_duration(uptime)),
        ]);
    }

    println!("{}", table);

    Ok(())
}

/// Print configs summary
pub fn print_config_summary(finder: &ConfigFinder) {
    let configs = finder.list_configs();

    if configs.is_empty() {
        println!("  {}", style("No configurations found").dim());
        return;
    }

    println!("{}", style("Configurations").bold());
    for config in configs {
        println!(
            "  {} {} ({})",
            style("•").dim(),
            style(&config.name).green(),
            style(config.scope).dim()
        );
    }
}
