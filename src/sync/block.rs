//! Block injection stage.

use crate::cli::state::HostState;
use crate::config::{BlockItem, SyncMode};
use crate::path::FluxPath;
use crate::remote::{with_retry, RemoteOps, RetryPolicy};
use crate::reporter::{ItemOutcome, Reporter, Stage};
use crate::sync::file::apply_atomic_bytes;
use crate::sync::plan::{BlockAction, Sentinel, SkipReason};
use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
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
        None,
        false,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub async fn plan_blocks_with_concurrency<R: RemoteOps + ?Sized>(
    items: &[BlockItem],
    asset_root: &Path,
    template: &str,
    remote: &R,
    max_concurrency: usize,
    policy: RetryPolicy,
    state: Option<&HostState>,
    use_cache: bool,
) -> Vec<BlockAction> {
    use futures::stream::{self, StreamExt};

    let indexed: Vec<(usize, &BlockItem)> = items.iter().enumerate().collect();
    let mut results: Vec<Option<BlockAction>> = (0..items.len()).map(|_| None).collect();
    let mut stream = stream::iter(indexed)
        .map(|(idx, item)| async move {
            (
                idx,
                plan_one_block(item, asset_root, template, remote, policy, state, use_cache).await,
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
    state: Option<&HostState>,
    use_cache: bool,
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
    let chosen_template = item.comment_template.as_deref().unwrap_or(template);
    let local_hash = block_cache_key(
        &item_name,
        &local_body,
        &target,
        &item.mode,
        chosen_template,
    );
    if use_cache
        && state
            .and_then(|state| state.item_hashes.get(&item_name))
            .is_some_and(|cached| cached == &local_hash)
    {
        return BlockAction::Skip {
            item_name,
            reason: SkipReason::ContentUnchanged,
        };
    }

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
            observed_remote_mtime: None,
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
                    observed_remote_mtime: remote_mtime,
                }
            }
        }
        _ => BlockAction::Apply {
            item_name,
            target: target.clone(),
            body: local_body,
            sentinel,
            observed_remote_mtime: with_retry(policy, || remote.mtime(&target)).await.ok(),
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

pub fn collect_item_hashes(
    items: &[BlockItem],
    asset_root: &Path,
    template: &str,
) -> HashMap<String, String> {
    let mut hashes = HashMap::new();
    for item in items {
        let local_path = resolve_block_path(asset_root, &item.path);
        if let Ok(body) = std::fs::read_to_string(local_path) {
            if let FluxPath::Remote(target) = FluxPath::parse(&item.file) {
                let chosen_template = item.comment_template.as_deref().unwrap_or(template);
                hashes.insert(
                    item.name.clone(),
                    block_cache_key(&item.name, &body, &target, &item.mode, chosen_template),
                );
            }
        }
    }
    hashes
}

fn block_cache_key(
    item_name: &str,
    body: &str,
    target: &str,
    mode: &SyncMode,
    comment_template: &str,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(body.as_bytes());
    hasher.update([0]);
    hasher.update(target.as_bytes());
    hasher.update([0]);
    hasher.update(match mode {
        SyncMode::Cover => b"cover".as_slice(),
        SyncMode::Sync => b"sync".as_slice(),
        SyncMode::Touch => b"touch".as_slice(),
    });
    hasher.update([0]);
    hasher.update(comment_template.as_bytes());
    hasher.update([0]);
    hasher.update(item_name.as_bytes());
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
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
            // No remote_changed_since_plan check: multiple blocks targeting
            // the same file are executed sequentially, each re-reading the
            // current content before composing — this is safe and expected.
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
                    match apply_atomic_bytes(remote, target, content.as_bytes(), None, policy, 3)
                        .await
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
    let eol = detect_eol(existing);
    let normalized_body = normalize_eol(body, eol);
    let injected = format!(
        "{}{eol}{}{}{}{eol}",
        sentinel.open_marker,
        normalized_body,
        if normalized_body.ends_with(eol) {
            ""
        } else {
            eol
        },
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
            if !out.ends_with('\n') && !out.ends_with("\r\n") && !out.is_empty() {
                out.push_str(eol);
            }
            out.push_str(&injected);
            Ok(out)
        }
    }
}

fn detect_eol(existing: &str) -> &'static str {
    if existing.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    }
}

fn normalize_eol(input: &str, eol: &str) -> String {
    input.replace("\r\n", "\n").replace('\n', eol)
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
    use crate::cli::state::HostState;
    use crate::config::{BlockItem, SyncMode};
    use crate::remote::fake::InMemoryRemote;
    use std::collections::HashMap;
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
            tags: vec![],
        };

        let actions1 = plan_blocks_with_concurrency(
            std::slice::from_ref(&item),
            tmp.path(),
            "# {}",
            &remote,
            1,
            RetryPolicy::no_retry(),
            None,
            false,
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
            None,
            false,
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

    #[tokio::test]
    async fn changing_block_target_invalidates_block_cache() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("aliases.sh"), b"alias x='1'\n").unwrap();
        let old_item = BlockItem {
            name: "aliases".into(),
            path: "aliases.sh".into(),
            file: ":/remote/.bashrc".into(),
            mode: SyncMode::Sync,
            comment_template: None,
            tags: vec![],
        };
        let new_item = BlockItem {
            file: ":/remote/.zshrc".into(),
            ..old_item.clone()
        };
        let state = HostState {
            host: "h".into(),
            last_sync_ts: 0,
            item_hashes: collect_item_hashes(&[old_item], tmp.path(), "# {}")
                .into_iter()
                .collect::<HashMap<_, _>>(),
            last_failed_item: None,
        };
        let remote = InMemoryRemote::new();
        let actions = plan_blocks_with_concurrency(
            &[new_item],
            tmp.path(),
            "# {}",
            &remote,
            1,
            RetryPolicy::no_retry(),
            Some(&state),
            true,
        )
        .await;
        assert!(
            matches!(&actions[0], BlockAction::Apply { target, .. } if target == "/remote/.zshrc")
        );
    }

    #[tokio::test]
    async fn execute_block_creates_backup_and_can_be_undone() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("aliases.sh"), b"alias x='1'\n").unwrap();
        let remote =
            InMemoryRemote::with_files([("/remote/.bashrc", b"export PATH=/bin\n".to_vec())]);
        remote.add_exec_rule(crate::remote::fake::ExecRule {
            matcher: Box::new(|cmd| cmd.starts_with("ls -1 ")),
            status: 0,
            stdout: vec![],
            stderr: vec![],
        });
        let item = BlockItem {
            name: "aliases".into(),
            path: "aliases.sh".into(),
            file: ":/remote/.bashrc".into(),
            mode: SyncMode::Cover,
            comment_template: None,
            tags: vec![],
        };
        let actions = plan_blocks_with_concurrency(
            &[item],
            tmp.path(),
            "# {}",
            &remote,
            1,
            RetryPolicy::no_retry(),
            None,
            false,
        )
        .await;
        let reporter = crate::reporter::memory::CapturedReporter::new();
        let outcome = execute_block(
            &actions[0],
            &remote,
            "# {}",
            &reporter,
            RetryPolicy::no_retry(),
        )
        .await;
        assert!(matches!(outcome, ItemOutcome::Applied));
        let applied = String::from_utf8(remote.file_contents("/remote/.bashrc").unwrap()).unwrap();
        assert!(applied.contains("alias x='1'"));
        let backup = remote
            .file_paths()
            .into_iter()
            .find(|path| path.starts_with("/remote/.bashrc.flux-") && path.ends_with(".bak"))
            .expect("backup path");
        remote.rename(&backup, "/remote/.bashrc").await.unwrap();
        assert_eq!(
            remote.file_contents("/remote/.bashrc"),
            Some(b"export PATH=/bin\n".to_vec())
        );
    }

    #[test]
    fn compose_preserves_crlf_style() {
        let output = compose(
            "alpha\r\n",
            "beta\nline2\n",
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
        assert!(output.contains("\r\n# >>> n:1 >>>\r\nbeta\r\nline2\r\n# <<< n:1 <<<\r\n"));
    }
}
