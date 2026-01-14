//! File synchronization module
//!
//! Handles file sync between local and remote.

use crate::config::{FileItem, SyncMode};
use crate::output::{self, Status};
use crate::path::FluxPath;
use crate::ssh::SshClient;
use anyhow::Result;
use std::collections::HashMap;
use std::path::Path;

/// Result of a file sync operation
#[derive(Debug)]
pub struct FileSyncResult {
    #[allow(dead_code)]
    pub name: Option<String>,
    pub status: Status,
    pub reason: Option<String>,
}

/// Sync all files
pub async fn sync_files(
    client: &SshClient,
    files: &[FileItem],
) -> Result<(Vec<FileSyncResult>, HashMap<String, bool>)> {
    let mut results = Vec::new();
    let mut file_status: HashMap<String, bool> = HashMap::new();

    for file in files {
        let result = sync_file(client, file).await;

        // Track named files for dependency checking
        if let Some(name) = &file.name {
            file_status.insert(name.clone(), result.status == Status::Success);
        }

        // Print output
        let src_path = FluxPath::parse(&file.src);
        let dst_path = FluxPath::parse(&file.dst);
        output::print_file(&src_path.as_str(), &dst_path.to_string());
        output::print_file_result(result.status, result.reason.as_deref());

        results.push(result);
    }

    Ok((results, file_status))
}

/// Sync a single file
async fn sync_file(client: &SshClient, file: &FileItem) -> FileSyncResult {
    let name = file.name.clone();

    match sync_file_inner(client, file).await {
        Ok((status, reason)) => FileSyncResult {
            name,
            status,
            reason,
        },
        Err(e) => FileSyncResult {
            name,
            status: Status::Failed,
            reason: Some(e.to_string()),
        },
    }
}

async fn sync_file_inner(client: &SshClient, file: &FileItem) -> Result<(Status, Option<String>)> {
    let src = FluxPath::parse(&file.src);
    let dst = FluxPath::parse(&file.dst);

    // Currently only support local -> remote
    if src.is_remote() {
        anyhow::bail!("Remote source not yet supported");
    }
    if dst.is_local() {
        anyhow::bail!("Local destination not yet supported");
    }

    let local_path = src.resolve_local()?;
    let remote_path = dst.resolve_remote()?;
    let remote_path = client.expand_remote_path(&remote_path).await?;

    // Check if local file exists
    if !local_path.exists() {
        return Ok((Status::Failed, Some("source file not found".to_string())));
    }

    // Handle different sync modes
    match file.mode {
        SyncMode::Touch => {
            // Only sync if remote doesn't exist
            if client.file_exists(&remote_path).await? {
                return Ok((Status::Skip, Some("file exists, mode: touch".to_string())));
            }
        }
        SyncMode::Sync => {
            // Check timestamps
            if let Some(remote_mtime) = client.get_mtime(&remote_path).await? {
                let local_mtime = get_local_mtime(&local_path)?;
                if remote_mtime >= local_mtime {
                    return Ok((
                        Status::Skip,
                        Some("remote is newer, mode: sync".to_string()),
                    ));
                }
            }
        }
        SyncMode::Cover => {
            // Always overwrite, no checks needed
        }
    }

    // Upload the file
    client.upload_file(&local_path, &remote_path).await?;

    // Set permissions if specified
    if let Some(chmod) = &file.chmod {
        client.chmod(&remote_path, chmod).await?;
    }

    Ok((Status::Success, None))
}

/// Get local file modification time as unix timestamp
fn get_local_mtime(path: &Path) -> Result<i64> {
    let metadata = std::fs::metadata(path)?;
    let mtime = metadata.modified()?;
    let duration = mtime.duration_since(std::time::UNIX_EPOCH)?;
    Ok(duration.as_secs() as i64)
}
