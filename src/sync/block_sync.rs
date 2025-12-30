//! Block synchronization logic
//!
//! Handles incremental configuration block sync with conflict detection

use crate::core::error::{RemoteError, Result};
use crate::core::ssh::{SshClient, SshClientTrait};
use crate::sync::models::{BlockGroup, BlockGroupMode, ConflictStrategy, SyncMode, TextBlock};
use crate::sync::version::{hash_content, VersionTracker};
use regex::Regex;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

/// Block marker format
const BLOCK_START_FMT: &str = "# >>> remote-block:{} src={} mtime={} hash={} <<<";
const BLOCK_END_FMT: &str = "# <<< remote-block:{} <<<";

/// Parsed block from remote file
#[derive(Debug, Clone)]
pub struct ParsedBlock {
    pub name: String,
    pub src: String,
    pub mtime: i64,
    pub hash: String,
    pub content: String,
}

/// Block sync context
pub struct BlockSyncContext<'a> {
    pub client: &'a SshClient,
    pub version_tracker: &'a mut VersionTracker,
    pub block_home: Option<String>,
    pub force_init: bool,
    pub dry_run: bool,
}

/// Result of a block sync operation
#[derive(Debug, Clone)]
pub enum BlockSyncResult {
    /// Block was synced
    Synced { name: String, file: String },
    /// Block was skipped
    Skipped { name: String, reason: String },
    /// Block has conflict
    Conflict {
        name: String,
        local_hash: String,
        remote_hash: String,
    },
    /// Dry run - would sync
    WouldSync { name: String, file: String },
}

/// Sync block groups
pub async fn sync_block_groups(
    groups: &[BlockGroup],
    ctx: &mut BlockSyncContext<'_>,
) -> Result<Vec<BlockSyncResult>> {
    let mut results = Vec::new();

    for group in groups {
        let group_results = sync_one_group(group, ctx).await?;
        results.extend(group_results);
    }

    Ok(results)
}

/// Sync a single block group
async fn sync_one_group(
    group: &BlockGroup,
    ctx: &mut BlockSyncContext<'_>,
) -> Result<Vec<BlockSyncResult>> {
    let mut results = Vec::new();

    // Read remote file content
    let remote_path = &group.dist;
    let remote_content = read_remote_file(ctx.client, remote_path)
        .await
        .unwrap_or_default();

    // Parse existing blocks from remote
    let existing_blocks = parse_remote_blocks(&remote_content);

    // Process each block in the group
    let mut new_blocks: Vec<(String, String)> = Vec::new(); // (name, content)

    for block in &group.blocks {
        let block_name = block.get_name();

        // Read local block content
        let local_content = read_local_block(block, ctx.block_home.as_deref())?;
        let local_hash = hash_content(&local_content);
        let local_mtime = get_latest_mtime(&block.src, ctx.block_home.as_deref())?;

        // Check if block already exists in remote
        let existing = existing_blocks.get(&block_name);

        let should_update = match &block.mode {
            SyncMode::Init => {
                // Only sync if block doesn't exist
                existing.is_none() || ctx.force_init
            }
            SyncMode::Update => {
                // Sync if local is newer
                match existing {
                    Some(eb) => {
                        // Check for conflict: remote was modified manually
                        if eb.hash != local_hash && eb.mtime > local_mtime {
                            // Remote was modified after our last sync
                            results.push(BlockSyncResult::Conflict {
                                name: block_name.clone(),
                                local_hash: local_hash.clone(),
                                remote_hash: eb.hash.clone(),
                            });
                            continue;
                        }
                        local_mtime > eb.mtime || local_hash != eb.hash
                    }
                    None => true,
                }
            }
            SyncMode::Cover => true,
            _ => true,
        };

        if !should_update {
            results.push(BlockSyncResult::Skipped {
                name: block_name.clone(),
                reason: "Block is up-to-date".to_string(),
            });
            continue;
        }

        if ctx.dry_run {
            results.push(BlockSyncResult::WouldSync {
                name: block_name.clone(),
                file: remote_path.clone(),
            });
        } else {
            // Add block to new_blocks
            let formatted = format_block(
                &block_name,
                &block.src.join(","),
                local_mtime,
                &local_hash,
                &local_content,
            );
            new_blocks.push((block_name.clone(), formatted));

            // Update version tracker
            ctx.version_tracker
                .update_block_version(&block_name, local_hash.clone(), local_mtime);

            results.push(BlockSyncResult::Synced {
                name: block_name.clone(),
                file: remote_path.clone(),
            });
        }
    }

    // Build new file content if not dry run
    if !ctx.dry_run && !new_blocks.is_empty() {
        let new_content =
            build_file_content(&remote_content, &existing_blocks, &new_blocks, &group.mode);
        write_remote_file(ctx.client, remote_path, &new_content).await?;
    }

    Ok(results)
}

/// Parse blocks from remote file content
fn parse_remote_blocks(content: &str) -> HashMap<String, ParsedBlock> {
    let mut blocks = HashMap::new();

    // Pattern for block markers
    let start_pattern =
        Regex::new(r"(?m)^# >>> remote-block:(.+?) src=(.+?) mtime=(\d+) hash=([a-f0-9]+) <<<$")
            .unwrap();
    let end_pattern = Regex::new(r"(?m)^# <<< remote-block:(.+?) <<<\s*$").unwrap();

    // Find all blocks
    for start_match in start_pattern.captures_iter(content) {
        let name = start_match.get(1).unwrap().as_str();
        let src = start_match.get(2).unwrap().as_str();
        let mtime: i64 = start_match.get(3).unwrap().as_str().parse().unwrap_or(0);
        let hash = start_match.get(4).unwrap().as_str();

        let start_pos = start_match.get(0).unwrap().end();

        // Find matching end
        let end_pattern_for_block = Regex::new(&format!(
            r"(?m)^# <<< remote-block:{} <<<\s*$",
            regex::escape(name)
        ))
        .unwrap();
        if let Some(end_match) = end_pattern_for_block.find(&content[start_pos..]) {
            let block_content = content[start_pos..start_pos + end_match.start()].trim();

            blocks.insert(
                name.to_string(),
                ParsedBlock {
                    name: name.to_string(),
                    src: src.to_string(),
                    mtime,
                    hash: hash.to_string(),
                    content: block_content.to_string(),
                },
            );
        }
    }

    blocks
}

/// Read local block content from source files
fn read_local_block(block: &TextBlock, block_home: Option<&str>) -> Result<String> {
    let mut content = String::new();

    for src in &block.src {
        let path = if let Some(home) = block_home {
            PathBuf::from(home).join(src)
        } else {
            PathBuf::from(src)
        };

        let file_content = fs::read_to_string(&path).map_err(|e| {
            RemoteError::Sync(format!(
                "Failed to read block source {}: {}",
                path.display(),
                e
            ))
        })?;

        if !content.is_empty() {
            content.push('\n');
        }
        content.push_str(&file_content);
    }

    Ok(content)
}

/// Get latest mtime from source files
fn get_latest_mtime(sources: &[String], block_home: Option<&str>) -> Result<i64> {
    let mut latest: i64 = 0;

    for src in sources {
        let path = if let Some(home) = block_home {
            PathBuf::from(home).join(src)
        } else {
            PathBuf::from(src)
        };

        if let Ok(metadata) = fs::metadata(&path) {
            if let Ok(mtime) = metadata.modified() {
                if let Ok(duration) = mtime.duration_since(std::time::UNIX_EPOCH) {
                    let ts = duration.as_secs() as i64;
                    if ts > latest {
                        latest = ts;
                    }
                }
            }
        }
    }

    Ok(latest)
}

/// Format a block with markers
fn format_block(name: &str, src: &str, mtime: i64, hash: &str, content: &str) -> String {
    format!(
        "{}\n{}\n{}",
        format!(
            "# >>> remote-block:{} src={} mtime={} hash={} <<<",
            name, src, mtime, hash
        ),
        content.trim(),
        format!("# <<< remote-block:{} <<<", name)
    )
}

/// Build new file content with updated blocks
fn build_file_content(
    original: &str,
    existing: &HashMap<String, ParsedBlock>,
    new_blocks: &[(String, String)],
    mode: &BlockGroupMode,
) -> String {
    // Start with non-block content
    let mut content = strip_all_blocks(original);

    // Add global region markers if not present
    if !content.contains("# ========== REMOTE MANAGED REGION START ==========") {
        content.push_str("\n# ========== REMOTE MANAGED REGION START ==========\n");
    }

    // In incremental mode, keep existing blocks that aren't being updated
    if matches!(mode, BlockGroupMode::Incremental) {
        for (name, block) in existing {
            if !new_blocks.iter().any(|(n, _)| n == name) {
                content.push_str(&format_block(
                    &block.name,
                    &block.src,
                    block.mtime,
                    &block.hash,
                    &block.content,
                ));
                content.push('\n');
            }
        }
    }

    // Add new/updated blocks
    for (_, block_content) in new_blocks {
        content.push_str(block_content);
        content.push('\n');
    }

    // Close global region
    if !content.contains("# ========== REMOTE MANAGED REGION END ==========") {
        content.push_str("# ========== REMOTE MANAGED REGION END ==========\n");
    }

    content
}

/// Strip all blocks from content
fn strip_all_blocks(content: &str) -> String {
    let block_pattern =
        Regex::new(r"(?ms)# >>> remote-block:.+? <<<\n.*?# <<< remote-block:.+? <<<\s*\n?")
            .unwrap();

    block_pattern.replace_all(content, "").to_string()
}

/// Read remote file content
async fn read_remote_file(client: &SshClient, path: &str) -> Result<String> {
    let result = client.exec(&format!("cat '{}'", path)).await?;
    Ok(result.stdout)
}

/// Write remote file content
async fn write_remote_file(client: &SshClient, path: &str, content: &str) -> Result<()> {
    // Ensure directory exists
    let dir = std::path::Path::new(path)
        .parent()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();

    if !dir.is_empty() {
        client.exec(&format!("mkdir -p '{}'", dir)).await?;
    }

    // Write content using heredoc
    let cmd = format!("cat > '{}' << 'REMOTE_EOF'\n{}\nREMOTE_EOF", path, content);
    client.exec(&cmd).await?;

    Ok(())
}
