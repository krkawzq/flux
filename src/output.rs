//! Terminal output utilities
//!
//! Provides colorful terminal output for flux operations.

use console::{style, Style};

/// Operation result status
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Status {
    Success,
    Failed,
    Skip,
}

impl Status {
    /// Get the symbol for this status
    pub fn symbol(&self) -> &'static str {
        match self {
            Status::Success => "✓",
            Status::Failed => "✗",
            Status::Skip => "○",
        }
    }

    /// Get the label for this status
    pub fn label(&self) -> &'static str {
        match self {
            Status::Success => "SUCCESS",
            Status::Failed => "FAILED",
            Status::Skip => "SKIP",
        }
    }

    /// Get the style for this status
    pub fn style(&self) -> Style {
        match self {
            Status::Success => Style::new().green(),
            Status::Failed => Style::new().red(),
            Status::Skip => Style::new().yellow(),
        }
    }
}

/// Print flux banner/header message
pub fn print_header(msg: &str) {
    println!("{} {}", style("[flux]").cyan().bold(), msg);
}

/// Print flux status message
pub fn print_status(status: Status, msg: &str) {
    let styled_status = status
        .style()
        .apply_to(format!("{} {}", status.symbol(), status.label()));
    println!("{} {}", style("[flux]").cyan().bold(), styled_status);
    if !msg.is_empty() {
        println!("       {}", msg);
    }
}

/// Print file sync operation
pub fn print_file(src: &str, dst: &str) {
    println!(
        "{} {} → {}",
        style("[file]").blue().bold(),
        style(src).dim(),
        style(dst).white()
    );
}

/// Print file sync result
pub fn print_file_result(status: Status, reason: Option<&str>) {
    let styled = status
        .style()
        .apply_to(format!("{} {}", status.symbol(), status.label()));
    match reason {
        Some(r) => println!("       {} ({})", styled, style(r).dim()),
        None => println!("       {}", styled),
    }
}

/// Print script execution
pub fn print_script(path: &str) {
    println!(
        "{} {}",
        style("[script]").magenta().bold(),
        style(path).white()
    );
}

/// Print script result
pub fn print_script_result(status: Status, reason: Option<&str>) {
    let styled = status
        .style()
        .apply_to(format!("{} {}", status.symbol(), status.label()));
    match reason {
        Some(r) => println!("         {} ({})", styled, style(r).dim()),
        None => println!("         {}", styled),
    }
}

/// Print block sync operation
pub fn print_block(name: &str, file: &str) {
    println!(
        "{} {} → {}",
        style("[block]").yellow().bold(),
        style(name).white(),
        style(file).dim()
    );
}

/// Print block result
pub fn print_block_result(status: Status, reason: Option<&str>) {
    let styled = status
        .style()
        .apply_to(format!("{} {}", status.symbol(), status.label()));
    match reason {
        Some(r) => println!("        {} ({})", styled, style(r).dim()),
        None => println!("        {}", styled),
    }
}

/// Print sync summary
pub fn print_summary(success: usize, failed: usize, skipped: usize) {
    println!();
    println!(
        "{} Sync completed: {} success, {} failed, {} skipped",
        style("[flux]").cyan().bold(),
        style(success).green(),
        style(failed).red(),
        style(skipped).yellow()
    );
}

/// Print error message
#[allow(dead_code)]
pub fn print_error(msg: &str) {
    eprintln!(
        "{} {} {}",
        style("[flux]").cyan().bold(),
        style("✗").red(),
        style(msg).red()
    );
}

/// Print warning message
pub fn print_warning(msg: &str) {
    println!(
        "{} {} {}",
        style("[flux]").cyan().bold(),
        style("!").yellow(),
        style(msg).yellow()
    );
}

/// Print info message
pub fn print_info(msg: &str) {
    println!("{} {}", style("[flux]").cyan().bold(), style(msg).dim());
}
