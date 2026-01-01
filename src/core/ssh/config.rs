//! SSH config file management

use crate::core::error::{RemoteError, Result};
use std::fs;
use std::io::Write;
use std::path::PathBuf;

/// SSH config entry
#[derive(Debug, Clone)]
pub struct SshConfigEntry {
    pub host_alias: String,
    pub hostname: String,
    pub user: String,
    pub port: u16,
    pub identity_file: Option<PathBuf>,
}

impl SshConfigEntry {
    /// Format as SSH config entry
    pub fn to_config_block(&self) -> String {
        let mut lines = vec![
            format!("Host {}", self.host_alias),
            format!("    HostName {}", self.hostname),
            format!("    User {}", self.user),
            format!("    Port {}", self.port),
        ];

        if let Some(ref key) = self.identity_file {
            lines.push(format!("    IdentityFile {}", key.display()));
            lines.push("    IdentitiesOnly yes".to_string());
        }

        lines.join("\n")
    }
}

/// Append SSH config entry to ~/.ssh/config
pub fn append_ssh_config(entry: &SshConfigEntry) -> Result<()> {
    let ssh_dir = dirs::home_dir()
        .ok_or_else(|| RemoteError::Config("Cannot find home directory".to_string()))?
        .join(".ssh");

    // Ensure .ssh directory exists
    fs::create_dir_all(&ssh_dir)?;

    let config_path = ssh_dir.join("config");

    // Read existing content
    let existing_content = if config_path.exists() {
        fs::read_to_string(&config_path)?
    } else {
        String::new()
    };

    // Check if entry already exists
    let host_pattern = format!("Host {}", entry.host_alias);
    if existing_content.contains(&host_pattern) {
        // Update existing entry
        update_ssh_config(&config_path, entry, &existing_content)?;
    } else {
        // Append new entry
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&config_path)?;

        // Add newlines before entry if file has content
        if !existing_content.is_empty() && !existing_content.ends_with('\n') {
            writeln!(file)?;
        }
        if !existing_content.is_empty() {
            writeln!(file)?;
        }

        writeln!(file, "{}", entry.to_config_block())?;
    }

    Ok(())
}

/// Update existing SSH config entry
fn update_ssh_config(
    config_path: &PathBuf,
    entry: &SshConfigEntry,
    existing_content: &str,
) -> Result<()> {
    let host_pattern = format!("Host {}", entry.host_alias);
    let lines: Vec<&str> = existing_content.lines().collect();

    let mut new_content = String::new();
    let mut i = 0;
    let mut found = false;

    while i < lines.len() {
        let line = lines[i];

        // Found the host entry to replace
        if line.trim() == host_pattern.trim() {
            found = true;
            // Add new entry
            new_content.push_str(&entry.to_config_block());
            new_content.push('\n');

            // Skip old entry lines (until next Host or end)
            i += 1;
            while i < lines.len() {
                let next_line = lines[i];
                if next_line.trim().starts_with("Host ") {
                    break;
                }
                i += 1;
            }
        } else {
            new_content.push_str(line);
            new_content.push('\n');
            i += 1;
        }
    }

    if !found {
        // Entry was not found, append it
        if !new_content.ends_with('\n') {
            new_content.push('\n');
        }
        new_content.push('\n');
        new_content.push_str(&entry.to_config_block());
        new_content.push('\n');
    }

    fs::write(config_path, new_content)?;
    Ok(())
}

