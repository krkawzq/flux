//! Block injection stage.

use crate::config::{BlockItem, SyncMode};
use crate::path::FluxPath;
use crate::remote::{with_retry, RemoteOps, RetryPolicy};
use crate::reporter::{ItemOutcome, Reporter, Stage};
use crate::sync::plan::{BlockAction, Sentinel, SkipReason};
use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum BlockError {
    #[error("comment template missing {{}} placeholder")]
    BadTemplate,
    #[error("malformed sentinel for block '{name}'")]
    MalformedSentinel { name: String },
    #[error("local block source not found: {0}")]
    SourceNotFound(String),
    #[error("local io: {0}")]
    LocalIo(String),
}

pub fn build_markers(
    template: &str,
    name: &str,
    timestamp: i64,
) -> Result<(String, String), BlockError> {
    if !template.contains("{}") {
        return Err(BlockError::BadTemplate);
    }
    let open = template.replace("{}", &format!(">>> {name}:{timestamp} >>>"));
    let close = template.replace("{}", &format!("<<< {name}:{timestamp} <<<"));
    Ok((open, close))
}

pub fn find_block(
    template: &str,
    name: &str,
    content: &str,
) -> Result<Option<FoundBlock>, BlockError> {
    if !template.contains("{}") {
        return Err(BlockError::BadTemplate);
    }
    let prefix_open = template
        .replace("{}", &format!(">>> {name}:"))
        .trim_end()
        .to_string();
    let prefix_close = template
        .replace("{}", &format!("<<< {name}:"))
        .trim_end()
        .to_string();
    let suffix_open = " >>>";
    let suffix_close = " <<<";

    let mut byte = 0usize;
    let mut open = None;
    let mut close = None;
    for piece in split_keep_terminators(content) {
        let line = piece.trim_end_matches(['\n', '\r']);
        if open.is_none() {
            if line.starts_with(&prefix_open) && line.ends_with(suffix_open) {
                let mid = &line[prefix_open.len()..line.len() - suffix_open.len()];
                if let Ok(timestamp) = mid.parse::<i64>() {
                    open = Some((byte, byte + piece.len(), timestamp));
                }
            }
        } else if close.is_none() && line.starts_with(&prefix_close) && line.ends_with(suffix_close)
        {
            let mid = &line[prefix_close.len()..line.len() - suffix_close.len()];
            if mid.parse::<i64>().is_ok() {
                close = Some((byte, byte + piece.len()));
                break;
            }
        }
        byte += piece.len();
    }

    match (open, close) {
        (Some((open_start, _, timestamp)), Some((_, close_end))) => Ok(Some(FoundBlock {
            byte_range: open_start..close_end,
            timestamp,
        })),
        (Some(_), None) => Err(BlockError::MalformedSentinel { name: name.into() }),
        _ => Ok(None),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FoundBlock {
    pub byte_range: std::ops::Range<usize>,
    pub timestamp: i64,
}

fn split_keep_terminators(input: &str) -> Vec<&str> {
    input.split_inclusive('\n').collect()
}

pub async fn plan_blocks<R: RemoteOps + ?Sized>(
    items: &[BlockItem],
    asset_root: &Path,
    template: &str,
    remote: &R,
) -> Vec<BlockAction> {
    plan_blocks_with_concurrency(
        items,
        asset_root,
        template,
        remote,
        1,
        RetryPolicy::no_retry(),
    )
    .await
}

pub async fn plan_blocks_with_concurrency<R: RemoteOps + ?Sized>(
    items: &[BlockItem],
    asset_root: &Path,
    template: &str,
    remote: &R,
    max_concurrency: usize,
    policy: RetryPolicy,
) -> Vec<BlockAction> {
    use futures::stream::{self, StreamExt};

    let indexed: Vec<(usize, &BlockItem)> = items.iter().enumerate().collect();
    let mut results: Vec<Option<BlockAction>> = (0..items.len()).map(|_| None).collect();
    let mut stream = stream::iter(indexed)
        .map(|(idx, item)| async move {
            (
                idx,
                plan_one_block(item, asset_root, template, remote, policy).await,
            )
        })
        .buffer_unordered(max_concurrency.max(1));

    while let Some((idx, action)) = stream.next().await {
        results[idx] = Some(action);
    }

    results.into_iter().map(|result| result.unwrap()).collect()
}

async fn plan_one_block<R: RemoteOps + ?Sized>(
    item: &BlockItem,
    asset_root: &Path,
    template: &str,
    remote: &R,
    policy: RetryPolicy,
) -> BlockAction {
    let item_name = item.name.clone();
    let local_path = resolve_block_path(asset_root, &item.path);
    let local_body = match std::fs::read_to_string(&local_path) {
        Ok(body) => body,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return BlockAction::Failed {
                item_name,
                error: BlockError::SourceNotFound(local_path.display().to_string()).into(),
            };
        }
        Err(err) => {
            return BlockAction::Failed {
                item_name,
                error: BlockError::LocalIo(err.to_string()).into(),
            };
        }
    };

    let target = match FluxPath::parse(&item.file) {
        FluxPath::Remote(path) => path,
        FluxPath::Local(_) => {
            return BlockAction::Failed {
                item_name,
                error: BlockError::LocalIo(format!("block target must be remote: {}", item.file))
                    .into(),
            };
        }
    };

    let exists_remote = match with_retry(policy, || remote.exists(&target)).await {
        Ok(exists) => exists,
        Err(err) => {
            return BlockAction::Failed {
                item_name,
                error: err.into(),
            };
        }
    };
    let timestamp = std::fs::metadata(&local_path)
        .and_then(|m| m.modified())
        .map(|t| chrono::DateTime::<chrono::Utc>::from(t).timestamp())
        .unwrap_or_else(|_| chrono::Utc::now().timestamp());
    let chosen_template = item.comment_template.as_deref().unwrap_or(template);
    let (open_marker, close_marker) = match build_markers(chosen_template, &item_name, timestamp) {
        Ok(markers) => markers,
        Err(err) => {
            return BlockAction::Failed {
                item_name,
                error: err.into(),
            };
        }
    };
    let sentinel = Sentinel {
        name: item_name.clone(),
        timestamp,
        open_marker,
        close_marker,
    };

    if !exists_remote {
        return BlockAction::Apply {
            item_name,
            target,
            body: local_body,
            sentinel,
        };
    }

    let remote_content = match with_retry(policy, || remote.read_file(&target)).await {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(err) => {
            return BlockAction::Failed {
                item_name,
                error: err.into(),
            };
        }
    };
    let found = match find_block(chosen_template, &item_name, &remote_content) {
        Ok(found) => found,
        Err(err) => {
            return BlockAction::Failed {
                item_name,
                error: err.into(),
            };
        }
    };

    match (item.mode.clone(), found) {
        (SyncMode::Touch, Some(_)) => BlockAction::Skip {
            item_name,
            reason: SkipReason::AlreadyExists,
        },
        (SyncMode::Sync, Some(found_block)) => {
            let existing_body = extract_body(&remote_content, &found_block);
            if hash(existing_body.as_bytes()) == hash(local_body.as_bytes()) {
                BlockAction::Skip {
                    item_name,
                    reason: SkipReason::ContentUnchanged,
                }
            } else {
                let local_mtime = std::fs::metadata(&local_path)
                    .and_then(|m| m.modified())
                    .ok();
                let remote_mtime = with_retry(policy, || remote.mtime(&target)).await.ok();
                if let (Some(remote_time), Some(local_time)) = (remote_mtime, local_mtime) {
                    let local_time: DateTime<Utc> = local_time.into();
                    if remote_time > local_time {
                        return BlockAction::Skip {
                            item_name,
                            reason: SkipReason::RemoteNewer,
                        };
                    }
                }
                BlockAction::Apply {
                    item_name,
                    target,
                    body: local_body,
                    sentinel,
                }
            }
        }
        _ => BlockAction::Apply {
            item_name,
            target,
            body: local_body,
            sentinel,
        },
    }
}

fn extract_body(content: &str, found: &FoundBlock) -> String {
    let block = &content[found.byte_range.clone()];
    let mut lines: Vec<&str> = block.split_inclusive('\n').collect();
    if !lines.is_empty() {
        lines.remove(0);
    }
    if !lines.is_empty() {
        lines.pop();
    }
    lines.concat()
}

fn hash(bytes: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher.finalize().into()
}

pub async fn execute_block<R: RemoteOps + ?Sized>(
    action: &BlockAction,
    remote: &R,
    template: &str,
    reporter: &dyn Reporter,
    policy: RetryPolicy,
) -> ItemOutcome {
    let name = action_name(action);
    reporter.item_started(Stage::Block, &name);
    let outcome = match action {
        BlockAction::Skip { reason, .. } => ItemOutcome::Skipped(reason.clone()),
        BlockAction::Failed { error, .. } => ItemOutcome::Failed(Arc::new(error.clone())),
        BlockAction::Apply {
            target,
            body,
            sentinel,
            ..
        } => {
            let current = match with_retry(policy, || remote.exists(target)).await {
                Ok(true) => match with_retry(policy, || remote.read_file(target)).await {
                    Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
                    Err(err) => {
                        return finish_block(
                            reporter,
                            &name,
                            ItemOutcome::Failed(Arc::new(err.into())),
                        );
                    }
                },
                Ok(false) => String::new(),
                Err(err) => {
                    return finish_block(
                        reporter,
                        &name,
                        ItemOutcome::Failed(Arc::new(err.into())),
                    );
                }
            };
            match compose(&current, body, sentinel, template, &name) {
                Ok(content) => {
                    match with_retry(policy, || remote.write_file(target, content.as_bytes())).await
                    {
                        Ok(()) => ItemOutcome::Applied,
                        Err(err) => ItemOutcome::Failed(Arc::new(err.into())),
                    }
                }
                Err(err) => ItemOutcome::Failed(Arc::new(err.into())),
            }
        }
    };
    reporter.item_finished(Stage::Block, &name, &outcome);
    outcome
}

fn finish_block(reporter: &dyn Reporter, name: &str, outcome: ItemOutcome) -> ItemOutcome {
    reporter.item_finished(Stage::Block, name, &outcome);
    outcome
}

fn compose(
    existing: &str,
    body: &str,
    sentinel: &Sentinel,
    template: &str,
    name: &str,
) -> Result<String, BlockError> {
    let injected = format!(
        "{}\n{}{}{}\n",
        sentinel.open_marker,
        body,
        if body.ends_with('\n') { "" } else { "\n" },
        sentinel.close_marker,
    );
    match find_block(template, name, existing)? {
        Some(found) => {
            let mut out = String::with_capacity(existing.len() + injected.len());
            out.push_str(&existing[..found.byte_range.start]);
            out.push_str(&injected);
            out.push_str(&existing[found.byte_range.end..]);
            Ok(out)
        }
        None => {
            let mut out = String::from(existing);
            if !out.ends_with('\n') && !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&injected);
            Ok(out)
        }
    }
}

fn resolve_block_path(asset_root: &Path, path: &str) -> PathBuf {
    if let Some(remote) = path.strip_prefix(':') {
        asset_root.join(remote)
    } else {
        asset_root.join(path)
    }
}

fn action_name(action: &BlockAction) -> String {
    match action {
        BlockAction::Skip { item_name, .. }
        | BlockAction::Apply { item_name, .. }
        | BlockAction::Failed { item_name, .. } => item_name.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{BlockItem, SyncMode};
    use crate::remote::fake::InMemoryRemote;
    use std::time::Duration;
    use tempfile::TempDir;

    #[test]
    fn build_markers_basic() {
        let (open, close) = build_markers("# {}", "aliases", 1_700_000_000).unwrap();
        assert_eq!(open, "# >>> aliases:1700000000 >>>");
        assert_eq!(close, "# <<< aliases:1700000000 <<<");
    }

    #[test]
    fn build_markers_bad_template() {
        let err = build_markers("no placeholder", "x", 1).unwrap_err();
        assert!(matches!(err, BlockError::BadTemplate));
    }

    #[test]
    fn find_block_missing_returns_none() {
        let found = find_block("# {}", "aliases", "echo hi\n").unwrap();
        assert!(found.is_none());
    }

    #[test]
    fn find_block_round_trip() {
        let content = "before\n# >>> aliases:42 >>>\nalias x='1'\n# <<< aliases:42 <<<\nafter\n";
        let found = find_block("# {}", "aliases", content).unwrap().unwrap();
        assert_eq!(found.timestamp, 42);
        assert_eq!(
            &content[found.byte_range],
            "# >>> aliases:42 >>>\nalias x='1'\n# <<< aliases:42 <<<\n"
        );
    }

    #[test]
    fn find_block_crlf() {
        let content = "before\r\n# >>> n:1 >>>\r\nbody\r\n# <<< n:1 <<<\r\nafter\r\n";
        let found = find_block("# {}", "n", content).unwrap().unwrap();
        assert_eq!(found.timestamp, 1);
    }

    #[test]
    fn find_block_orphan_open_is_error() {
        let content = "# >>> n:1 >>>\nbody\n";
        let err = find_block("# {}", "n", content).unwrap_err();
        assert!(matches!(err, BlockError::MalformedSentinel { .. }));
    }

    #[test]
    fn find_block_does_not_match_inside_body() {
        let content = "# >>> a:1 >>>\nthis line says >>> a:2 >>> as text\n# <<< a:1 <<<\n";
        let found = find_block("# {}", "a", content).unwrap().unwrap();
        assert_eq!(found.timestamp, 1);
    }

    #[test]
    fn compose_inserts_when_missing() {
        let output = compose(
            "alpha\n",
            "beta\n",
            &Sentinel {
                name: "n".into(),
                timestamp: 1,
                open_marker: "# >>> n:1 >>>".into(),
                close_marker: "# <<< n:1 <<<".into(),
            },
            "# {}",
            "n",
        )
        .unwrap();
        assert_eq!(output, "alpha\n# >>> n:1 >>>\nbeta\n# <<< n:1 <<<\n");
    }

    #[test]
    fn compose_replaces_existing() {
        let pre = "x\n# >>> n:1 >>>\nold body\n# <<< n:1 <<<\ny\n";
        let output = compose(
            pre,
            "new body\n",
            &Sentinel {
                name: "n".into(),
                timestamp: 2,
                open_marker: "# >>> n:2 >>>".into(),
                close_marker: "# <<< n:2 <<<".into(),
            },
            "# {}",
            "n",
        )
        .unwrap();
        assert_eq!(output, "x\n# >>> n:2 >>>\nnew body\n# <<< n:2 <<<\ny\n");
    }

    #[tokio::test]
    async fn block_plan_is_idempotent_across_runs() {
        let tmp = TempDir::new().unwrap();
        let block_path = tmp.path().join("aliases.sh");
        std::fs::write(&block_path, b"alias x='1'\n").unwrap();
        let remote = InMemoryRemote::new();
        let item = BlockItem {
            name: "aliases".into(),
            path: "aliases.sh".into(),
            file: ":/remote/.bashrc".into(),
            mode: SyncMode::Sync,
            comment_template: None,
        };

        let actions1 = plan_blocks_with_concurrency(
            std::slice::from_ref(&item),
            tmp.path(),
            "# {}",
            &remote,
            1,
            RetryPolicy::no_retry(),
        )
        .await;
        tokio::time::sleep(Duration::from_millis(20)).await;
        let actions2 = plan_blocks_with_concurrency(
            &[item],
            tmp.path(),
            "# {}",
            &remote,
            1,
            RetryPolicy::no_retry(),
        )
        .await;

        let ts1 = match &actions1[0] {
            BlockAction::Apply { sentinel, .. } => sentinel.timestamp,
            _ => panic!("expected apply"),
        };
        let ts2 = match &actions2[0] {
            BlockAction::Apply { sentinel, .. } => sentinel.timestamp,
            _ => panic!("expected apply"),
        };
        assert_eq!(ts1, ts2);
    }
}
