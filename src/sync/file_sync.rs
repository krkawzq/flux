//! File synchronization logic
//!
//! Handles file sync between local and remote with various modes
//!
//! Path resolution rules:
//! - Remote paths: prefixed with ":" (e.g., ":~/.bashrc")
//! - Absolute paths: start with "/" or "~" (e.g., "~/.ssh/id_rsa")
//! - Relative paths: resolved relative to .flux directory (e.g., "files/config.sh")

use crate::core::error::{RemoteError, Result};
use crate::core::platform::expand_tilde;
use crate::core::ssh::{SshClient, SshClientTrait};
use crate::sync::models::{FileSync, SyncMode};
use crate::sync::version::VersionTracker;
use std::fs;
use std::path::PathBuf;

/// Check if a path is a remote path (prefixed with ":")
pub fn is_remote_path(path: &str) -> bool {
    path.starts_with(':')
}

/// Remove the remote prefix from a path
pub fn strip_remote_prefix(path: &str) -> &str {
    path.strip_prefix(':').unwrap_or(path)
}

/// Check if path is relative (not starting with / or ~)
fn is_relative_path(path: &str) -> bool {
    !path.starts_with('/') && !path.starts_with('~') && !path.starts_with(':')
}

/// Resolve a local path
/// - Absolute or ~ paths: expand tilde
/// - Relative paths: resolve relative to flux_dir (if provided)
pub fn resolve_local_path(path: &str) -> PathBuf {
    resolve_local_path_with_base(path, None)
}

/// Resolve a local path with optional base directory (for relative paths)
/// Relative paths are resolved against .flux/files/ directory
pub fn resolve_local_path_with_base(path: &str, flux_dir: Option<&PathBuf>) -> PathBuf {
    if is_relative_path(path) {
        // Relative path - resolve against .flux/files directory
        if let Some(base) = flux_dir {
            return base.join("files").join(path);
        }
        // Fall back to current directory if no flux_dir
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    } else {
        // Absolute or ~ path
        expand_tilde(path)
    }
}

/// File sync context
pub struct FileSyncContext<'a> {
    pub client: &'a SshClient,
    pub version_tracker: &'a mut VersionTracker,
    pub force_init: bool,
    pub dry_run: bool,
    /// .flux directory path for resolving relative paths
    pub flux_dir: Option<PathBuf>,
}

/// Result of a file sync operation
#[derive(Debug, Clone)]
pub enum FileSyncResult {
    /// File was synced successfully
    Synced { src: String, dst: String },
    /// File was skipped (already up-to-date)
    Skipped { path: String, reason: String },
    /// File sync was blocked due to conflict
    Conflict {
        path: String,
        local_hash: String,
        remote_hash: String,
    },
    /// Dry run - would sync
    WouldSync { src: String, dst: String },
}

/// Sync a list of files
pub async fn sync_files(
    files: &[FileSync],
    ctx: &mut FileSyncContext<'_>,
) -> Result<Vec<FileSyncResult>> {
    let mut results = Vec::new();

    for file in files {
        let result = sync_one_file(file, ctx).await?;
        results.push(result);
    }

    Ok(results)
}

/// Sync a single file
pub async fn sync_one_file(
    file: &FileSync,
    ctx: &mut FileSyncContext<'_>,
) -> Result<FileSyncResult> {
    let src_is_remote = is_remote_path(&file.src);
    let dst_is_remote = is_remote_path(&file.dist);

    // Parse paths - use flux_dir for relative paths
    let src_path = if src_is_remote {
        strip_remote_prefix(&file.src).to_string()
    } else {
        resolve_local_path_with_base(&file.src, ctx.flux_dir.as_ref())
            .to_string_lossy()
            .to_string()
    };

    let dst_path = if dst_is_remote {
        strip_remote_prefix(&file.dist).to_string()
    } else {
        resolve_local_path_with_base(&file.dist, ctx.flux_dir.as_ref())
            .to_string_lossy()
            .to_string()
    };

    // Handle based on mode
    match file.mode {
        SyncMode::Init => {
            sync_init(
                file,
                &src_path,
                &dst_path,
                src_is_remote,
                dst_is_remote,
                ctx,
            )
            .await
        }
        SyncMode::Update => {
            sync_update(
                file,
                &src_path,
                &dst_path,
                src_is_remote,
                dst_is_remote,
                ctx,
            )
            .await
        }
        SyncMode::Cover => {
            sync_cover(
                file,
                &src_path,
                &dst_path,
                src_is_remote,
                dst_is_remote,
                ctx,
            )
            .await
        }
        SyncMode::Sync => {
            sync_bidirectional(
                file,
                &src_path,
                &dst_path,
                src_is_remote,
                dst_is_remote,
                ctx,
            )
            .await
        }
        SyncMode::Mirror => {
            // Mirror mode is similar to cover for individual files
            sync_cover(
                file,
                &src_path,
                &dst_path,
                src_is_remote,
                dst_is_remote,
                ctx,
            )
            .await
        }
    }
}

/// Init mode: only sync if target doesn't exist
async fn sync_init(
    _file: &FileSync,
    src: &str,
    dst: &str,
    src_is_remote: bool,
    dst_is_remote: bool,
    ctx: &mut FileSyncContext<'_>,
) -> Result<FileSyncResult> {
    // Check if target exists
    let target_exists = if dst_is_remote {
        remote_file_exists(ctx.client, dst).await?
    } else {
        PathBuf::from(dst).exists()
    };

    // Skip if target exists (unless force_init)
    if target_exists && !ctx.force_init {
        return Ok(FileSyncResult::Skipped {
            path: dst.to_string(),
            reason: "Target already exists (init mode)".to_string(),
        });
    }

    // Perform sync
    if ctx.dry_run {
        return Ok(FileSyncResult::WouldSync {
            src: src.to_string(),
            dst: dst.to_string(),
        });
    }

    transfer_file(src, dst, src_is_remote, dst_is_remote, ctx).await?;

    Ok(FileSyncResult::Synced {
        src: src.to_string(),
        dst: dst.to_string(),
    })
}

/// Update mode: sync if source is newer
async fn sync_update(
    _file: &FileSync,
    src: &str,
    dst: &str,
    src_is_remote: bool,
    dst_is_remote: bool,
    ctx: &mut FileSyncContext<'_>,
) -> Result<FileSyncResult> {
    // Get source mtime
    let src_mtime = if src_is_remote {
        remote_file_mtime(ctx.client, src).await?
    } else {
        local_file_mtime(&PathBuf::from(src))?
    };

    // Get target mtime
    let dst_mtime = if dst_is_remote {
        remote_file_mtime(ctx.client, dst).await.ok()
    } else {
        local_file_mtime(&PathBuf::from(dst)).ok()
    };

    // Skip if target is newer or same
    if let Some(dm) = dst_mtime {
        if dm >= src_mtime {
            return Ok(FileSyncResult::Skipped {
                path: dst.to_string(),
                reason: "Target is up-to-date".to_string(),
            });
        }
    }

    // Perform sync
    if ctx.dry_run {
        return Ok(FileSyncResult::WouldSync {
            src: src.to_string(),
            dst: dst.to_string(),
        });
    }

    transfer_file(src, dst, src_is_remote, dst_is_remote, ctx).await?;

    Ok(FileSyncResult::Synced {
        src: src.to_string(),
        dst: dst.to_string(),
    })
}

/// Cover mode: force overwrite
async fn sync_cover(
    _file: &FileSync,
    src: &str,
    dst: &str,
    src_is_remote: bool,
    dst_is_remote: bool,
    ctx: &mut FileSyncContext<'_>,
) -> Result<FileSyncResult> {
    if ctx.dry_run {
        return Ok(FileSyncResult::WouldSync {
            src: src.to_string(),
            dst: dst.to_string(),
        });
    }

    transfer_file(src, dst, src_is_remote, dst_is_remote, ctx).await?;

    Ok(FileSyncResult::Synced {
        src: src.to_string(),
        dst: dst.to_string(),
    })
}

/// Bidirectional sync based on mtime
async fn sync_bidirectional(
    _file: &FileSync,
    src: &str,
    dst: &str,
    src_is_remote: bool,
    dst_is_remote: bool,
    ctx: &mut FileSyncContext<'_>,
) -> Result<FileSyncResult> {
    // Get mtimes
    let src_mtime = if src_is_remote {
        remote_file_mtime(ctx.client, src).await.ok()
    } else {
        local_file_mtime(&PathBuf::from(src)).ok()
    };

    let dst_mtime = if dst_is_remote {
        remote_file_mtime(ctx.client, dst).await.ok()
    } else {
        local_file_mtime(&PathBuf::from(dst)).ok()
    };

    match (src_mtime, dst_mtime) {
        (Some(sm), Some(dm)) if sm > dm => {
            // Source is newer, sync to destination
            if ctx.dry_run {
                return Ok(FileSyncResult::WouldSync {
                    src: src.to_string(),
                    dst: dst.to_string(),
                });
            }
            transfer_file(src, dst, src_is_remote, dst_is_remote, ctx).await?;
            Ok(FileSyncResult::Synced {
                src: src.to_string(),
                dst: dst.to_string(),
            })
        }
        (Some(sm), Some(dm)) if dm > sm => {
            // Destination is newer, sync to source
            if ctx.dry_run {
                return Ok(FileSyncResult::WouldSync {
                    src: dst.to_string(),
                    dst: src.to_string(),
                });
            }
            transfer_file(dst, src, dst_is_remote, src_is_remote, ctx).await?;
            Ok(FileSyncResult::Synced {
                src: dst.to_string(),
                dst: src.to_string(),
            })
        }
        (Some(_), None) => {
            // Destination doesn't exist, sync to destination
            if ctx.dry_run {
                return Ok(FileSyncResult::WouldSync {
                    src: src.to_string(),
                    dst: dst.to_string(),
                });
            }
            transfer_file(src, dst, src_is_remote, dst_is_remote, ctx).await?;
            Ok(FileSyncResult::Synced {
                src: src.to_string(),
                dst: dst.to_string(),
            })
        }
        (None, Some(_)) => {
            // Source doesn't exist, sync to source
            if ctx.dry_run {
                return Ok(FileSyncResult::WouldSync {
                    src: dst.to_string(),
                    dst: src.to_string(),
                });
            }
            transfer_file(dst, src, dst_is_remote, src_is_remote, ctx).await?;
            Ok(FileSyncResult::Synced {
                src: dst.to_string(),
                dst: src.to_string(),
            })
        }
        _ => Ok(FileSyncResult::Skipped {
            path: dst.to_string(),
            reason: "Files are in sync".to_string(),
        }),
    }
}

// === Helper Functions ===

/// Check if remote file exists
async fn remote_file_exists(client: &SshClient, path: &str) -> Result<bool> {
    let result = client
        .exec(&format!("test -f '{}' && echo 1 || echo 0", path))
        .await?;
    Ok(result.stdout.trim() == "1")
}

/// Get remote file mtime
async fn remote_file_mtime(client: &SshClient, path: &str) -> Result<i64> {
    let result = client.exec(&format!("stat -c %Y '{}'", path)).await?;
    result
        .stdout
        .trim()
        .parse::<i64>()
        .map_err(|_| RemoteError::Sftp(format!("Failed to get mtime for {}", path)))
}

/// Get local file mtime
fn local_file_mtime(path: &PathBuf) -> Result<i64> {
    let metadata = fs::metadata(path)?;
    let mtime = metadata.modified()?;
    let duration = mtime
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| RemoteError::Sync(e.to_string()))?;
    Ok(duration.as_secs() as i64)
}

/// Transfer file between local and remote
async fn transfer_file(
    src: &str,
    dst: &str,
    src_is_remote: bool,
    dst_is_remote: bool,
    ctx: &FileSyncContext<'_>,
) -> Result<()> {
    tracing::info!("Transfer: {} -> {}", src, dst);

    if !src_is_remote && dst_is_remote {
        // Upload: local -> remote
        tracing::debug!("Upload: {} -> {}", src, dst);
        let content = fs::read(src).map_err(|e| {
            RemoteError::Sync(format!("Failed to read local file {}: {}", src, e))
        })?;
        
        // Expand ~ in remote path
        let remote_dst = expand_remote_tilde(dst, ctx.client).await?;
        ctx.client.upload_file(&remote_dst, &content).await?;
    } else if src_is_remote && !dst_is_remote {
        // Download: remote -> local
        tracing::debug!("Download: {} -> {}", src, dst);
        
        // Expand ~ in remote path
        let remote_src = expand_remote_tilde(src, ctx.client).await?;
        let content = ctx.client.download_file(&remote_src).await?;
        
        // Ensure parent directory exists
        let dst_path = PathBuf::from(dst);
        if let Some(parent) = dst_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&dst_path, content)?;
    } else if !src_is_remote && !dst_is_remote {
        // Local copy
        let src_path = PathBuf::from(src);
        let dst_path = PathBuf::from(dst);
        if let Some(parent) = dst_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(&src_path, &dst_path)?;
    } else {
        // Remote to remote copy (via temp local file)
        tracing::debug!("Remote copy (via local): {} -> {}", src, dst);
        let remote_src = expand_remote_tilde(src, ctx.client).await?;
        let remote_dst = expand_remote_tilde(dst, ctx.client).await?;
        
        let content = ctx.client.download_file(&remote_src).await?;
        ctx.client.upload_file(&remote_dst, &content).await?;
    }

    Ok(())
}

/// Expand ~ in remote path to actual home directory
async fn expand_remote_tilde(path: &str, client: &SshClient) -> Result<String> {
    if path.starts_with("~/") {
        let home = client.get_home().await?;
        Ok(format!("{}{}", home, &path[1..]))
    } else if path == "~" {
        client.get_home().await
    } else {
        Ok(path.to_string())
    }
}

/// Ensure remote directory exists
pub async fn ensure_remote_dir(client: &SshClient, path: &str) -> Result<()> {
    let dir = std::path::Path::new(path)
        .parent()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();

    if !dir.is_empty() {
        client.exec(&format!("mkdir -p '{}'", dir)).await?;
    }

    Ok(())
}
