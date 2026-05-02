//! Append-only structured audit log at ~/.flux/audit.jsonl.

use crate::reporter::PipelineSummary;
use chrono::Utc;
use serde::Serialize;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;

#[derive(Debug, Serialize)]
pub struct AuditEntry<'a> {
    pub ts: String,
    pub host: &'a str,
    pub config_name: &'a str,
    pub duration_ms: u128,
    pub interrupted: bool,
    pub dry_run: bool,
    pub stages: Vec<StageRecord>,
}

#[derive(Debug, Serialize)]
pub struct StageRecord {
    pub stage: String,
    pub applied: usize,
    pub skipped: usize,
    pub failed: usize,
}

pub fn audit_path() -> Option<PathBuf> {
    dirs::home_dir().map(|home| home.join(".flux").join("audit.jsonl"))
}

pub fn append(
    host: &str,
    config_name: &str,
    duration_ms: u128,
    summary: &PipelineSummary,
) -> std::io::Result<()> {
    let Some(path) = audit_path() else {
        return Ok(());
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let entry = AuditEntry {
        ts: Utc::now().to_rfc3339(),
        host,
        config_name,
        duration_ms,
        interrupted: summary.interrupted,
        dry_run: summary.dry_run,
        stages: summary
            .stages
            .iter()
            .map(|stage| StageRecord {
                stage: format!("{:?}", stage.stage),
                applied: stage.applied,
                skipped: stage.skipped,
                failed: stage.failed,
            })
            .collect(),
    };
    let line = serde_json::to_string(&entry).map_err(std::io::Error::other)?;
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(file, "{line}")
}
