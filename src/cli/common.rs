//! Common CLI utilities

use comfy_table::{Cell, Color, Table};
use console::{style, Emoji};
use indicatif::{ProgressBar, ProgressStyle};
use std::time::Duration;

// Emoji constants
pub static SUCCESS: Emoji<'_, '_> = Emoji("✅ ", "[OK] ");
pub static FAILED: Emoji<'_, '_> = Emoji("❌ ", "[FAIL] ");
pub static WARNING: Emoji<'_, '_> = Emoji("⚠️  ", "[WARN] ");
pub static INFO: Emoji<'_, '_> = Emoji("ℹ️  ", "[INFO] ");
pub static SYNC: Emoji<'_, '_> = Emoji("🔄 ", "[SYNC] ");
pub static UPLOAD: Emoji<'_, '_> = Emoji("⬆️  ", "[UP] ");
pub static DOWNLOAD: Emoji<'_, '_> = Emoji("⬇️  ", "[DOWN] ");
pub static SKIP: Emoji<'_, '_> = Emoji("⏭️  ", "[SKIP] ");
pub static CONNECT: Emoji<'_, '_> = Emoji("🔗 ", "[CONN] ");
pub static TUNNEL: Emoji<'_, '_> = Emoji("🚇 ", "[TUN] ");
pub static SCRIPT: Emoji<'_, '_> = Emoji("📜 ", "[SCR] ");

/// Print a success message
pub fn print_success(msg: &str) {
    println!("{} {}", SUCCESS, style(msg).green());
}

/// Print an error message
pub fn print_error(msg: &str) {
    eprintln!("{} {}", FAILED, style(msg).red());
}

/// Print a warning message  
pub fn print_warning(msg: &str) {
    println!("{} {}", WARNING, style(msg).yellow());
}

/// Print an info message
pub fn print_info(msg: &str) {
    println!("{} {}", INFO, style(msg).cyan());
}

/// Create a spinner with a message
pub fn create_spinner(msg: &str) -> ProgressBar {
    let pb = ProgressBar::new_spinner();
    pb.set_style(
        ProgressStyle::default_spinner()
            .tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏")
            .template("{spinner:.cyan} {msg}")
            .unwrap(),
    );
    pb.set_message(msg.to_string());
    pb.enable_steady_tick(Duration::from_millis(100));
    pb
}

/// Create a progress bar
pub fn create_progress_bar(len: u64, msg: &str) -> ProgressBar {
    let pb = ProgressBar::new(len);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("{msg} [{bar:40.cyan/blue}] {pos}/{len} ({eta})")
            .unwrap()
            .progress_chars("█▓▒░"),
    );
    pb.set_message(msg.to_string());
    pb
}

/// Create a styled table
pub fn create_table(headers: Vec<&str>) -> Table {
    let mut table = Table::new();
    table.set_header(
        headers
            .into_iter()
            .map(|h| Cell::new(h).fg(Color::Cyan))
            .collect::<Vec<_>>(),
    );
    table
}

/// Format bytes to human readable
pub fn format_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;

    if bytes >= GB {
        format!("{:.2} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.2} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.2} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} B", bytes)
    }
}

/// Format duration to human readable
pub fn format_duration(secs: i64) -> String {
    if secs < 60 {
        format!("{}s", secs)
    } else if secs < 3600 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else {
        format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
    }
}

/// Format timestamp to human readable
pub fn format_timestamp(ts: i64) -> String {
    chrono::DateTime::from_timestamp(ts, 0)
        .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_else(|| "N/A".to_string())
}
