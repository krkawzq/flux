//! Block synchronization module
//!
//! Handles block injection into remote config files using sentinel comments.

use crate::config::{BlockItem, SyncMode};
use crate::output::{self, Status};
use crate::path::FluxPath;
use crate::remote::ssh::SshClient;
use anyhow::Result;
use sha2::{Digest, Sha256};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum BlockError {
    #[error("bad comment template")]
    BadTemplate,
}

/// Result of a block sync operation
#[derive(Debug)]
pub struct BlockResult {
    pub status: Status,
    pub reason: Option<String>,
}

/// Sync all blocks
pub async fn sync_blocks(
    client: &SshClient,
    blocks: &[BlockItem],
    default_comment_template: &str,
) -> Result<Vec<BlockResult>> {
    let mut results = Vec::new();

    for block in blocks {
        let result = sync_block(client, block, default_comment_template).await;

        output::print_block(&block.name, &block.file);
        output::print_block_result(result.status, result.reason.as_deref());

        results.push(result);
    }

    Ok(results)
}

/// Sync a single block
async fn sync_block(
    client: &SshClient,
    block: &BlockItem,
    default_comment_template: &str,
) -> BlockResult {
    match sync_block_inner(client, block, default_comment_template).await {
        Ok((status, reason)) => BlockResult { status, reason },
        Err(e) => BlockResult {
            status: Status::Failed,
            reason: Some(e.to_string()),
        },
    }
}

async fn sync_block_inner(
    client: &SshClient,
    block: &BlockItem,
    default_comment_template: &str,
) -> Result<(Status, Option<String>)> {
    // Parse paths
    let src = FluxPath::parse(&block.path);
    let dst = FluxPath::parse(&block.file);

    if src.is_remote() {
        anyhow::bail!("Block source must be local");
    }
    if dst.is_local() {
        anyhow::bail!("Block target must be remote");
    }

    // Read local block content
    let local_path = src.resolve_local()?;
    if !local_path.exists() {
        return Ok((Status::Failed, Some("block file not found".to_string())));
    }
    let block_content = std::fs::read_to_string(&local_path)?;
    let local_mtime = get_local_mtime(&local_path)?;

    // Get remote file path
    let remote_path = dst.resolve_remote()?;
    let remote_path = client.expand_remote_path(&remote_path).await?;

    // Check if remote file exists
    if !client.file_exists(&remote_path).await? {
        return Ok((Status::Skip, Some("target file not found".to_string())));
    }

    // Read remote file content
    let remote_content =
        String::from_utf8_lossy(&client.read_remote_file(&remote_path).await?).to_string();

    // Get comment template
    let comment_template = block
        .comment_template
        .as_deref()
        .unwrap_or(default_comment_template);

    // Generate sentinels
    let timestamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    let (start_sentinel, end_sentinel) =
        generate_sentinels(comment_template, &block.name, timestamp);

    // Find existing block
    let existing_block = find_existing_block(&remote_content, comment_template, &block.name);

    match (&existing_block, &block.mode) {
        // Block exists and mode is touch -> skip
        (Some(_), SyncMode::Touch) => {
            return Ok((Status::Skip, Some("block exists, mode: touch".to_string())));
        }
        // Block exists and mode is sync -> check timestamp
        (Some((_, _, existing_content)), SyncMode::Sync) => {
            // Compare hash to see if content changed
            let existing_hash = compute_hash(existing_content.trim().as_bytes());
            let new_hash = compute_hash(block_content.trim().as_bytes());

            if existing_hash == new_hash {
                return Ok((Status::Skip, Some("content unchanged".to_string())));
            }

            if let Some(remote_mtime) = client.get_mtime(&remote_path).await? {
                if remote_mtime >= local_mtime {
                    return Ok((
                        Status::Skip,
                        Some("remote is newer, mode: sync".to_string()),
                    ));
                }
            }
        }
        _ => {}
    }

    // Build new file content
    let new_content = if let Some((range, _, _)) = existing_block {
        // Replace existing block
        let mut content = remote_content.clone();
        let block_text = format!(
            "{}\n{}\n{}",
            start_sentinel,
            block_content.trim(),
            end_sentinel
        );
        content.replace_range(range, &block_text);
        content
    } else {
        // Append new block
        let block_text = format!(
            "\n{}\n{}\n{}\n",
            start_sentinel,
            block_content.trim(),
            end_sentinel
        );
        format!("{}{}", remote_content.trim_end(), block_text)
    };

    // Write back to remote
    client
        .write_remote_file(&remote_path, new_content.as_bytes())
        .await?;

    Ok((Status::Success, None))
}

/// Generate start and end sentinels
fn generate_sentinels(template: &str, name: &str, timestamp: u64) -> (String, String) {
    let marker = format!("{}:{}", name, timestamp);
    let start = template.replace("{}", &format!(">>> {} >>>", marker));
    let end = template.replace("{}", &format!("<<< {} <<<", marker));
    (start, end)
}

/// Find existing block in content
/// Returns (range, timestamp, content) if found
fn find_existing_block(
    content: &str,
    template: &str,
    name: &str,
) -> Option<(std::ops::Range<usize>, Option<i64>, String)> {
    let line_infos = collect_lines_with_offsets(content);
    let mut start_line = None;
    let mut start_timestamp = None;
    let mut end_line = None;

    for (i, line_info) in line_infos.iter().enumerate() {
        if let Some((true, ts)) = parse_sentinel_line(line_info.content, template, name) {
            start_line = Some(i);
            start_timestamp = Some(ts);
        }
        if start_line.is_some()
            && matches!(
                parse_sentinel_line(line_info.content, template, name),
                Some((false, _))
            )
        {
            end_line = Some(i);
            break;
        }
    }

    if let (Some(start), Some(end)) = (start_line, end_line) {
        let byte_start = line_infos[start].start;
        let byte_end = line_infos[end].end;

        // Extract content between sentinels
        let block_content: String = line_infos[start + 1..end]
            .iter()
            .map(|line| line.content)
            .collect::<Vec<_>>()
            .join("\n");

        return Some((byte_start..byte_end, start_timestamp, block_content));
    }

    None
}

/// Compute SHA256 hash
fn compute_hash(content: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content);
    let result = hasher.finalize();
    result.iter().map(|b| format!("{:02x}", b)).collect()
}

fn get_local_mtime(path: &Path) -> Result<i64> {
    let metadata = std::fs::metadata(path)?;
    let mtime = metadata.modified()?;
    let duration = mtime.duration_since(UNIX_EPOCH)?;
    Ok(duration.as_secs() as i64)
}

struct LineInfo<'a> {
    content: &'a str,
    start: usize,
    end: usize,
}

fn collect_lines_with_offsets(content: &str) -> Vec<LineInfo<'_>> {
    let mut lines = Vec::new();
    let mut start = 0;

    for segment in content.split_inclusive('\n') {
        let end = start + segment.len();
        let content = segment.trim_end_matches(['\n', '\r']);
        lines.push(LineInfo {
            content,
            start,
            end,
        });
        start = end;
    }

    if start < content.len() {
        lines.push(LineInfo {
            content: &content[start..],
            start,
            end: content.len(),
        });
    }

    lines
}

fn parse_sentinel_line(line: &str, template: &str, name: &str) -> Option<(bool, i64)> {
    let (prefix, suffix) = template.split_once("{}")?;
    let trimmed = line.trim();

    if !trimmed.starts_with(prefix) || !trimmed.ends_with(suffix) {
        return None;
    }

    let inner = &trimmed[prefix.len()..trimmed.len() - suffix.len()];
    let inner = inner.trim();

    if let Some(timestamp) = inner
        .strip_prefix(&format!(">>> {}:", name))
        .and_then(|rest| rest.strip_suffix(" >>>"))
    {
        return timestamp.trim().parse::<i64>().ok().map(|ts| (true, ts));
    }

    if let Some(timestamp) = inner
        .strip_prefix(&format!("<<< {}:", name))
        .and_then(|rest| rest.strip_suffix(" <<<"))
    {
        return timestamp.trim().parse::<i64>().ok().map(|ts| (false, ts));
    }

    None
}
