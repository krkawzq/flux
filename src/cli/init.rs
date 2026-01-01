//! Init CLI command - initialize .flux directory

use crate::cli::common::*;
use crate::config::finder::{init_flux_dir, init_global_flux_dir, ConfigFinder};
use console::style;

/// Run init command
pub fn run_init(global: bool, no_example: bool) -> anyhow::Result<()> {
    if global {
        init_global()?;
    } else {
        init_local(no_example)?;
    }
    Ok(())
}

/// Initialize local .flux directory
fn init_local(no_example: bool) -> anyhow::Result<()> {
    let current_dir = std::env::current_dir()?;
    let flux_dir = current_dir.join(".flux");

    if flux_dir.exists() {
        print_warning("Directory .flux already exists");
        return Ok(());
    }

    println!(
        "{} Initializing flux workspace in {}",
        INFO,
        style(current_dir.display()).cyan()
    );

    init_flux_dir(&current_dir, !no_example)?;

    println!();
    print_success("Initialized .flux directory");
    println!();
    println!("Created structure:");
    println!("  {}", style(".flux/").cyan());
    println!("  ├── {}", style("config/").dim());
    if !no_example {
        println!("  │   └── {}", style("default.toml").green());
    }
    println!("  ├── {}", style("scripts/").dim());
    if !no_example {
        println!("  │   └── {}", style("example.sh").dim());
    }
    println!("  ├── {}", style("blocks/").dim());
    if !no_example {
        println!("  │   └── {}", style("example.block").dim());
    }
    println!("  └── {}", style("state/").dim());
    println!();

    if !no_example {
        println!(
            "Edit {} to configure your server connection.",
            style(".flux/config/default.toml").cyan()
        );
    } else {
        println!(
            "Create a config file in {} to get started.",
            style(".flux/config/").cyan()
        );
    }
    println!(
        "Then run {} to sync.",
        style("flux sync").green()
    );

    Ok(())
}

/// Initialize global ~/.flux directory
fn init_global() -> anyhow::Result<()> {
    let finder = ConfigFinder::new();
    let global_dir = finder.global_dir();

    if global_dir.exists() {
        print_warning(&format!(
            "Global directory {} already exists",
            global_dir.display()
        ));
        return Ok(());
    }

    println!(
        "{} Initializing global flux directory at {}",
        INFO,
        style(global_dir.display()).cyan()
    );

    let path = init_global_flux_dir()?;

    println!();
    print_success(&format!("Initialized {}", path.display()));
    println!();
    println!("Created structure:");
    println!("  {}", style("~/.flux/").cyan());
    println!("  ├── {}", style("config/").dim());
    println!("  ├── {}", style("scripts/").dim());
    println!("  └── {}", style("state/").dim());

    Ok(())
}

/// Show current flux directory info
pub fn run_info() -> anyhow::Result<()> {
    let finder = ConfigFinder::new();

    println!("{} Flux workspace info", INFO);
    println!();

    // Local directory
    if let Some(local) = finder.local_dir() {
        println!(
            "  Local:  {} {}",
            style(local.display()).cyan(),
            style("(active)").green()
        );
    } else {
        println!(
            "  Local:  {} (run {} to create)",
            style("not found").dim(),
            style("flux init").cyan()
        );
    }

    // Global directory
    let global = finder.global_dir();
    if global.exists() {
        println!("  Global: {}", style(global.display()).cyan());
    } else {
        println!(
            "  Global: {} (run {} to create)",
            style("not found").dim(),
            style("flux init --global").cyan()
        );
    }

    // List available configs
    let configs = finder.list_configs();
    if !configs.is_empty() {
        println!();
        println!("Available configurations:");
        for config in configs {
            println!(
                "  {} {} ({})",
                style("•").dim(),
                style(&config.name).green(),
                style(config.scope).dim()
            );
        }
    }

    Ok(())
}
