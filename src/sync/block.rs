//! Block synchronization module
//!
//! Handles block injection into remote config files using sentinel comments.

use crate::config::{BlockItem, SyncMode};
use crate::output::{self, Status};
use crate::path::FluxPath;
use crate::ssh::SshClient;
use anyhow::Result;
use sha2::{Digest, Sha256};
use std::time::{SystemTime, UNIX_EPOCH};

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

    // Get remote file path
    let remote_path = dst.resolve_remote()?;
    let remote_path = client.expand_remote_path(&remote_path).await?;

    // Check if remote file exists
    if !client.file_exists(&remote_path).await? {
        return Ok((Status::Skip, Some("target file not found".to_string())));
    }

    // Read remote file content
    let remote_content = String::from_utf8_lossy(
        &client.read_remote_file(&remote_path).await?
    ).to_string();

    // Get comment template
    let comment_template = block
        .comment_template
        .as_deref()
        .unwrap_or(default_comment_template);

    // Generate sentinels
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)?
        .as_secs();
    let (start_sentinel, end_sentinel) = generate_sentinels(
        comment_template,
        &block.name,
        timestamp,
    );

    // Find existing block
    let existing_block = find_existing_block(&remote_content, comment_template, &block.name);

    match (&existing_block, &block.mode) {
        // Block exists and mode is touch -> skip
        (Some(_), SyncMode::Touch) => {
            return Ok((Status::Skip, Some("block exists, mode: touch".to_string())));
        }
        // Block exists and mode is sync -> check timestamp
        (Some((_, existing_timestamp, existing_content)), SyncMode::Sync) => {
            // Compare hash to see if content changed
            let existing_hash = compute_hash(existing_content.trim().as_bytes());
            let new_hash = compute_hash(block_content.trim().as_bytes());
            
            if existing_hash == new_hash {
                return Ok((Status::Skip, Some("content unchanged".to_string())));
            }
            
            // If remote timestamp is newer and content differs, check mode behavior
            if let Some(ts) = existing_timestamp {
                if *ts >= timestamp as i64 {
                    return Ok((Status::Skip, Some("remote is newer, mode: sync".to_string())));
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
    // Build regex pattern for start sentinel
    // Template is like "# {}", we need to match "# >>> name:timestamp >>>"
    let prefix = template.split("{}").next().unwrap_or("");
    let suffix = template.split("{}").last().unwrap_or("");
    
    let _start_pattern = format!(
        r"{}>>> {}:(\d+) >>>{}",
        regex_escape(prefix.trim()),
        regex_escape(name),
        regex_escape(suffix.trim())
    );
    let _end_pattern = format!(
        r"{}<<< {}:\d+ <<<{}",
        regex_escape(prefix.trim()),
        regex_escape(name),
        regex_escape(suffix.trim())
    );

    // Simple line-by-line search
    let lines: Vec<&str> = content.lines().collect();
    let mut start_line = None;
    let mut start_timestamp = None;
    let mut end_line = None;

    for (i, line) in lines.iter().enumerate() {
        if line.contains(&format!(">>> {}:", name)) && line.contains(">>>") {
            start_line = Some(i);
            // Extract timestamp
            if let Some(ts_start) = line.find(&format!("{}:", name)) {
                let rest = &line[ts_start + name.len() + 1..];
                if let Some(ts_end) = rest.find(' ') {
                    start_timestamp = rest[..ts_end].parse::<i64>().ok();
                }
            }
        }
        if start_line.is_some() && line.contains(&format!("<<< {}:", name)) && line.contains("<<<") {
            end_line = Some(i);
            break;
        }
    }

    if let (Some(start), Some(end)) = (start_line, end_line) {
        // Calculate byte range
        let mut byte_start = 0;
        let mut byte_end = 0;
        let mut current_pos = 0;

        for (i, line) in lines.iter().enumerate() {
            if i == start {
                byte_start = current_pos;
            }
            current_pos += line.len() + 1; // +1 for newline
            if i == end {
                byte_end = current_pos;
                break;
            }
        }

        // Extract content between sentinels
        let block_content: String = lines[start + 1..end].join("\n");

        return Some((byte_start..byte_end, start_timestamp, block_content));
    }

    None
}

/// Escape special regex characters
fn regex_escape(s: &str) -> String {
    let special_chars = ['\\', '.', '+', '*', '?', '(', ')', '[', ']', '{', '}', '|', '^', '$'];
    let mut result = String::new();
    for c in s.chars() {
        if special_chars.contains(&c) {
            result.push('\\');
        }
        result.push(c);
    }
    result
}

/// Compute SHA256 hash
fn compute_hash(content: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content);
    let result = hasher.finalize();
    result.iter().map(|b| format!("{:02x}", b)).collect()
}
