//! File sync stage.

use crate::config::{FileItem, SyncMode};
use crate::path::FluxPath;
use crate::remote::{with_retry, RemoteOps, RemoteOpsError, RetryPolicy};
use crate::reporter::{ItemOutcome, Reporter, Stage};
use crate::sync::plan::{FileAction, SkipReason};
use sha2::{Digest, Sha256};
use std::path::Path;
use std::sync::Arc;

#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum FileError {
    #[error("source not found: {0}")]
    SourceNotFound(String),
    #[error("source is a directory, not a file: {0}")]
    SourceIsDirectory(String),
    #[error("local io: {0}")]
    LocalIo(String),
    #[error("invalid path: {0}")]
    InvalidPath(String),
    #[error("only local->remote sync is supported (got src={src} dst={dst})")]
    UnsupportedDirection { src: String, dst: String },
}

/// Compute file actions without touching the remote write surface.
pub async fn plan_files<R: RemoteOps + ?Sized>(items: &[FileItem], remote: &R) -> Vec<FileAction> {
    plan_files_with_concurrency(items, remote, 1, RetryPolicy::no_retry()).await
}

pub async fn plan_files_with_concurrency<R: RemoteOps + ?Sized>(
    items: &[FileItem],
    remote: &R,
    max_concurrency: usize,
    policy: RetryPolicy,
) -> Vec<FileAction> {
    use futures::stream::{self, StreamExt};

    let indexed: Vec<(usize, &FileItem)> = items.iter().enumerate().collect();
    let mut results: Vec<Option<FileAction>> = (0..items.len()).map(|_| None).collect();
    let mut stream = stream::iter(indexed)
        .map(|(idx, item)| async move { (idx, plan_one_file(item, remote, policy).await) })
        .buffer_unordered(max_concurrency.max(1));

    while let Some((idx, action)) = stream.next().await {
        results[idx] = Some(action);
    }

    results.into_iter().map(|result| result.unwrap()).collect()
}

async fn plan_one_file<R: RemoteOps + ?Sized>(
    item: &FileItem,
    remote: &R,
    policy: RetryPolicy,
) -> FileAction {
    let item_name = item.name.clone().unwrap_or_else(|| item.src.clone());
    let src = FluxPath::parse(&item.src);
    let dst = FluxPath::parse(&item.dst);

    let local_path = match src {
        FluxPath::Local(path) => path,
        FluxPath::Remote(_) => {
            return FileAction::Failed {
                item_name,
                error: FileError::UnsupportedDirection {
                    src: item.src.clone(),
                    dst: item.dst.clone(),
                }
                .into(),
            };
        }
    };
    let remote_path = match dst {
        FluxPath::Remote(path) => path,
        FluxPath::Local(_) => {
            return FileAction::Failed {
                item_name,
                error: FileError::UnsupportedDirection {
                    src: item.src.clone(),
                    dst: item.dst.clone(),
                }
                .into(),
            };
        }
    };

    let metadata = match std::fs::metadata(&local_path) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return FileAction::Failed {
                item_name,
                error: FileError::SourceNotFound(local_path.display().to_string()).into(),
            };
        }
        Err(err) => {
            return FileAction::Failed {
                item_name,
                error: FileError::LocalIo(err.to_string()).into(),
            };
        }
    };
    if metadata.is_dir() {
        return FileAction::Failed {
            item_name,
            error: FileError::SourceIsDirectory(local_path.display().to_string()).into(),
        };
    }

    let len = metadata.len();
    let chmod = item
        .chmod
        .as_deref()
        .and_then(|value| u32::from_str_radix(value, 8).ok());
    let exists_remote = match with_retry(policy, || remote.exists(&remote_path)).await {
        Ok(exists) => exists,
        Err(err) => {
            return FileAction::Failed {
                item_name,
                error: err.into(),
            };
        }
    };

    match item.mode {
        SyncMode::Touch if exists_remote => FileAction::Skip {
            item_name,
            reason: SkipReason::AlreadyExists,
        },
        SyncMode::Sync if exists_remote => {
            let local_mtime = match local_mtime(&local_path) {
                Ok(mtime) => mtime,
                Err(err) => {
                    return FileAction::Failed {
                        item_name,
                        error: err.into(),
                    };
                }
            };
            match with_retry(policy, || remote.mtime(&remote_path)).await {
                Ok(remote_mtime) if remote_mtime > local_mtime => FileAction::Skip {
                    item_name,
                    reason: SkipReason::RemoteNewer,
                },
                Ok(remote_mtime) if remote_mtime == local_mtime => {
                    let local_bytes = match read_local_bytes(&local_path) {
                        Ok(bytes) => bytes,
                        Err(err) => {
                            return FileAction::Failed {
                                item_name,
                                error: err.into(),
                            };
                        }
                    };
                    match with_retry(policy, || remote.read_file(&remote_path)).await {
                        Ok(remote_bytes) if hash(&remote_bytes) == hash(&local_bytes) => {
                            FileAction::Skip {
                                item_name,
                                reason: SkipReason::ContentUnchanged,
                            }
                        }
                        Ok(_) => FileAction::Apply {
                            item_name,
                            src: local_path,
                            dst: remote_path,
                            len,
                            chmod,
                        },
                        Err(err) => FileAction::Failed {
                            item_name,
                            error: err.into(),
                        },
                    }
                }
                Ok(_) => FileAction::Apply {
                    item_name,
                    src: local_path,
                    dst: remote_path,
                    len,
                    chmod,
                },
                Err(err) => FileAction::Failed {
                    item_name,
                    error: err.into(),
                },
            }
        }
        _ => FileAction::Apply {
            item_name,
            src: local_path,
            dst: remote_path,
            len,
            chmod,
        },
    }
}

fn read_local_bytes(path: &Path) -> Result<Vec<u8>, FileError> {
    std::fs::read(path).map_err(|err| FileError::LocalIo(err.to_string()))
}

fn local_mtime(path: &Path) -> Result<chrono::DateTime<chrono::Utc>, RemoteOpsError> {
    let metadata = std::fs::metadata(path).map_err(|err| RemoteOpsError::Io(err.to_string()))?;
    let modified = metadata
        .modified()
        .map_err(|err| RemoteOpsError::Io(err.to_string()))?;
    Ok(chrono::DateTime::<chrono::Utc>::from(modified))
}

fn hash(bytes: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher.finalize().into()
}

pub async fn execute_file<R: RemoteOps + ?Sized>(
    action: &FileAction,
    remote: &R,
    reporter: &dyn Reporter,
    policy: RetryPolicy,
) -> ItemOutcome {
    let name = action_name(action);
    reporter.item_started(Stage::File, &name);
    let outcome = match action {
        FileAction::Skip { reason, .. } => ItemOutcome::Skipped(reason.clone()),
        FileAction::Failed { error, .. } => ItemOutcome::Failed(Arc::new(error.clone())),
        FileAction::Apply {
            src, dst, chmod, ..
        } => {
            if let Some(parent) = parent_dir(dst) {
                if let Err(err) = with_retry(policy, || remote.ensure_dir(parent)).await {
                    return finish(reporter, &name, ItemOutcome::Failed(Arc::new(err.into())));
                }
            }
            let bytes = match read_local_bytes(src) {
                Ok(bytes) => bytes,
                Err(err) => {
                    return finish(reporter, &name, ItemOutcome::Failed(Arc::new(err.into())));
                }
            };
            if let Err(err) = with_retry(policy, || remote.write_file(dst, &bytes)).await {
                return finish(reporter, &name, ItemOutcome::Failed(Arc::new(err.into())));
            }
            if let Some(mode) = chmod {
                let current_mode = with_retry(policy, || remote.stat_mode(dst)).await.ok();
                if current_mode != Some(*mode) {
                    if let Err(err) = with_retry(policy, || remote.chmod(dst, *mode)).await {
                        return finish(reporter, &name, ItemOutcome::Failed(Arc::new(err.into())));
                    }
                }
            }
            ItemOutcome::Applied
        }
    };
    reporter.item_finished(Stage::File, &name, &outcome);
    outcome
}

fn finish(reporter: &dyn Reporter, name: &str, outcome: ItemOutcome) -> ItemOutcome {
    reporter.item_finished(Stage::File, name, &outcome);
    outcome
}

fn action_name(action: &FileAction) -> String {
    match action {
        FileAction::Skip { item_name, .. }
        | FileAction::Apply { item_name, .. }
        | FileAction::Failed { item_name, .. } => item_name.clone(),
    }
}

fn parent_dir(path: &str) -> Option<&str> {
    path.rfind('/').and_then(|idx| {
        if idx == 0 {
            Some("/")
        } else if idx > 0 {
            Some(&path[..idx])
        } else {
            None
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote::fake::InMemoryRemote;
    use crate::reporter::memory::CapturedReporter;
    use tempfile::TempDir;

    fn local_file(dir: &TempDir, name: &str, content: &[u8]) -> String {
        let path = dir.path().join(name);
        std::fs::write(&path, content).unwrap();
        path.to_string_lossy().into_owned()
    }

    fn item(name: &str, src: &str, dst: &str, mode: SyncMode) -> FileItem {
        FileItem {
            name: Some(name.into()),
            src: src.into(),
            dst: dst.into(),
            mode,
            chmod: None,
        }
    }

    #[tokio::test]
    async fn touch_skips_when_remote_exists() {
        let tmp = TempDir::new().unwrap();
        let src = local_file(&tmp, "a.txt", b"x");
        let remote = InMemoryRemote::with_files([("/r/a.txt", b"old".to_vec())]);
        let actions = plan_files(&[item("a", &src, ":/r/a.txt", SyncMode::Touch)], &remote).await;
        assert!(matches!(
            &actions[0],
            FileAction::Skip {
                reason: SkipReason::AlreadyExists,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn cover_always_applies() {
        let tmp = TempDir::new().unwrap();
        let src = local_file(&tmp, "a.txt", b"new");
        let remote = InMemoryRemote::with_files([("/r/a.txt", b"old".to_vec())]);
        let actions = plan_files(&[item("a", &src, ":/r/a.txt", SyncMode::Cover)], &remote).await;
        match &actions[0] {
            FileAction::Apply {
                src: planned_src,
                len,
                ..
            } => {
                assert_eq!(planned_src, &std::path::PathBuf::from(&src));
                assert_eq!(*len, 3);
            }
            other => panic!("expected apply action, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn sync_skip_when_remote_newer() {
        use chrono::{Duration, Utc};
        let tmp = TempDir::new().unwrap();
        let src = local_file(&tmp, "a.txt", b"x");
        let remote = InMemoryRemote::with_files([("/r/a.txt", b"old".to_vec())]);
        remote.set_mtime("/r/a.txt", Utc::now() + Duration::seconds(60));
        let actions = plan_files(&[item("a", &src, ":/r/a.txt", SyncMode::Sync)], &remote).await;
        assert!(matches!(
            &actions[0],
            FileAction::Skip {
                reason: SkipReason::RemoteNewer,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn sync_skip_when_content_identical_with_equal_mtime() {
        let tmp = TempDir::new().unwrap();
        let src = local_file(&tmp, "a.txt", b"same");
        let local_modified = std::fs::metadata(&src).unwrap().modified().unwrap();
        let remote = InMemoryRemote::with_files([("/r/a.txt", b"same".to_vec())]);
        remote.set_mtime(
            "/r/a.txt",
            chrono::DateTime::<chrono::Utc>::from(local_modified),
        );
        let actions = plan_files(&[item("a", &src, ":/r/a.txt", SyncMode::Sync)], &remote).await;
        assert!(matches!(
            &actions[0],
            FileAction::Skip {
                reason: SkipReason::ContentUnchanged,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn missing_source_returns_failed() {
        let remote = InMemoryRemote::new();
        let actions = plan_files(
            &[item("a", "/no/such/file", ":/r/a.txt", SyncMode::Cover)],
            &remote,
        )
        .await;
        assert!(matches!(&actions[0], FileAction::Failed { .. }));
    }

    #[tokio::test]
    async fn execute_apply_writes_bytes_and_chmod() {
        let tmp = TempDir::new().unwrap();
        let src = local_file(&tmp, "a.txt", b"hello");
        let remote = InMemoryRemote::new();
        let mut file = item("a", &src, ":/r/a.txt", SyncMode::Cover);
        file.chmod = Some("600".into());
        let actions = plan_files(&[file], &remote).await;
        let reporter = CapturedReporter::new();
        let outcome = execute_file(&actions[0], &remote, &reporter, RetryPolicy::no_retry()).await;
        assert!(matches!(outcome, ItemOutcome::Applied));
        assert_eq!(remote.file_contents("/r/a.txt"), Some(b"hello".to_vec()));
        assert_eq!(remote.file_mode("/r/a.txt"), Some(0o600));
    }

    #[tokio::test]
    async fn execute_apply_skips_redundant_chmod() {
        let tmp = TempDir::new().unwrap();
        let src = local_file(&tmp, "a.txt", b"hello");
        let remote = InMemoryRemote::new();
        remote.write_file("/r/a.txt", b"old").await.unwrap();
        remote.chmod("/r/a.txt", 0o600).await.unwrap();
        let mut file = item("a", &src, ":/r/a.txt", SyncMode::Cover);
        file.chmod = Some("600".into());
        let actions = plan_files(&[file], &remote).await;
        let reporter = CapturedReporter::new();
        let outcome = execute_file(&actions[0], &remote, &reporter, RetryPolicy::no_retry()).await;
        assert!(matches!(outcome, ItemOutcome::Applied));
        assert_eq!(remote.file_mode("/r/a.txt"), Some(0o600));
    }

    #[tokio::test]
    async fn plan_retries_transient_exists_error() {
        let tmp = TempDir::new().unwrap();
        let src = local_file(&tmp, "a.txt", b"hello");
        let remote = InMemoryRemote::new();
        remote.fail_next("exists", RemoteOpsError::Transport("flake".into()));
        let actions = plan_files_with_concurrency(
            &[item("a", &src, ":/r/a.txt", SyncMode::Cover)],
            &remote,
            1,
            RetryPolicy::default(),
        )
        .await;
        assert!(matches!(&actions[0], FileAction::Apply { .. }));
    }
}
