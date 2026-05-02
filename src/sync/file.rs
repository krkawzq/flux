//! File sync stage.

use crate::config::{FileItem, ItemKind, SyncMode};
use crate::path::FluxPath;
use crate::remote::{with_retry, ExecOutput, RemoteOps, RemoteOpsError, RetryPolicy};
use crate::reporter::{ItemOutcome, Reporter, Stage};
use crate::sync::plan::{FileAction, SkipReason};
use chrono::Utc;
use globset::Glob;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use walkdir::WalkDir;

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
    let mut results: Vec<Option<Vec<FileAction>>> = (0..items.len()).map(|_| None).collect();
    let mut stream = stream::iter(indexed)
        .map(|(idx, item)| async move { (idx, plan_one_file(item, remote, policy).await) })
        .buffer_unordered(max_concurrency.max(1));

    while let Some((idx, actions)) = stream.next().await {
        results[idx] = Some(actions);
    }

    results
        .into_iter()
        .flat_map(|result| result.unwrap_or_default())
        .collect()
}

pub fn auto_detect_kind(item: &FileItem) -> ItemKind {
    if contains_glob_meta(&item.src) {
        return ItemKind::Glob;
    }
    let src = FluxPath::parse(&item.src);
    if let FluxPath::Local(path) = src {
        if std::fs::metadata(&path)
            .map(|meta| meta.is_dir())
            .unwrap_or(false)
        {
            return ItemKind::Dir;
        }
    }
    ItemKind::File
}

async fn plan_one_file<R: RemoteOps + ?Sized>(
    item: &FileItem,
    remote: &R,
    policy: RetryPolicy,
) -> Vec<FileAction> {
    match effective_kind(item) {
        ItemKind::Auto => unreachable!("auto should resolve before dispatch"),
        ItemKind::File => vec![plan_single_file(item, remote, policy).await],
        ItemKind::Glob => plan_glob(item, remote, policy).await,
        ItemKind::Dir => vec![plan_dir(item)],
        ItemKind::Link => vec![plan_link(item)],
    }
}

fn effective_kind(item: &FileItem) -> ItemKind {
    match item.kind {
        ItemKind::Auto => auto_detect_kind(item),
        _ => item.kind.clone(),
    }
}

async fn plan_single_file<R: RemoteOps + ?Sized>(
    item: &FileItem,
    remote: &R,
    policy: RetryPolicy,
) -> FileAction {
    let item_name = item.name.clone().unwrap_or_else(|| item.src.clone());
    let chmod = item
        .chmod
        .as_deref()
        .and_then(|value| u32::from_str_radix(value, 8).ok());
    match resolve_local_remote(&item.src, &item.dst) {
        Ok((local_path, remote_path)) => {
            plan_regular_file(
                item_name,
                local_path,
                remote_path,
                item.mode.clone(),
                chmod,
                remote,
                policy,
            )
            .await
        }
        Err(error) => FileAction::Failed {
            item_name,
            error: error.into(),
        },
    }
}

async fn plan_glob<R: RemoteOps + ?Sized>(
    item: &FileItem,
    remote: &R,
    policy: RetryPolicy,
) -> Vec<FileAction> {
    let item_name = item.name.clone().unwrap_or_else(|| item.src.clone());
    let src = FluxPath::parse(&item.src);
    let dst = FluxPath::parse(&item.dst);
    let local_pattern = match src {
        FluxPath::Local(path) => path,
        FluxPath::Remote(_) => {
            return vec![FileAction::Failed {
                item_name,
                error: FileError::UnsupportedDirection {
                    src: item.src.clone(),
                    dst: item.dst.clone(),
                }
                .into(),
            }]
        }
    };
    let remote_base = match dst {
        FluxPath::Remote(path) => path,
        FluxPath::Local(_) => {
            return vec![FileAction::Failed {
                item_name,
                error: FileError::UnsupportedDirection {
                    src: item.src.clone(),
                    dst: item.dst.clone(),
                }
                .into(),
            }]
        }
    };

    let matcher = match Glob::new(&local_pattern.to_string_lossy()) {
        Ok(glob) => glob.compile_matcher(),
        Err(err) => {
            return vec![FileAction::Failed {
                item_name,
                error: FileError::InvalidPath(err.to_string()).into(),
            }]
        }
    };
    let root = glob_search_root(&local_pattern);
    let chmod = item
        .chmod
        .as_deref()
        .and_then(|value| u32::from_str_radix(value, 8).ok());

    let mut actions = Vec::new();
    for entry in WalkDir::new(&root).into_iter().filter_map(Result::ok) {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path().to_path_buf();
        if !matcher.is_match(&path) {
            continue;
        }
        let relative = path
            .strip_prefix(&root)
            .ok()
            .map(path_to_unix)
            .unwrap_or_else(|| {
                path.file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned()
            });
        let dst = join_remote_path(&remote_base, &relative);
        let action_name = format!("{item_name}:{relative}");
        actions.push(
            plan_regular_file(
                action_name,
                path,
                dst,
                item.mode.clone(),
                chmod,
                remote,
                policy,
            )
            .await,
        );
    }

    if actions.is_empty() {
        vec![FileAction::Failed {
            item_name,
            error: FileError::SourceNotFound(local_pattern.display().to_string()).into(),
        }]
    } else {
        actions
    }
}

fn plan_dir(item: &FileItem) -> FileAction {
    let item_name = item.name.clone().unwrap_or_else(|| item.src.clone());
    let chmod = item
        .chmod
        .as_deref()
        .and_then(|value| u32::from_str_radix(value, 8).ok());
    let (src_dir, dst_dir) = match resolve_local_remote(&item.src, &item.dst) {
        Ok(paths) => paths,
        Err(error) => {
            return FileAction::Failed {
                item_name,
                error: error.into(),
            };
        }
    };
    match std::fs::metadata(&src_dir) {
        Ok(meta) if meta.is_dir() => {}
        Ok(_) => {
            return FileAction::Failed {
                item_name,
                error: FileError::SourceIsDirectory(src_dir.display().to_string()).into(),
            };
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return FileAction::Failed {
                item_name,
                error: FileError::SourceNotFound(src_dir.display().to_string()).into(),
            };
        }
        Err(err) => {
            return FileAction::Failed {
                item_name,
                error: FileError::LocalIo(err.to_string()).into(),
            };
        }
    }

    let mut files = Vec::new();
    for entry in WalkDir::new(&src_dir).into_iter().filter_map(Result::ok) {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path().to_path_buf();
        let relative = path
            .strip_prefix(&src_dir)
            .ok()
            .map(path_to_unix)
            .unwrap_or_else(|| {
                path.file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned()
            });
        files.push((path, relative));
    }

    FileAction::ApplyDir {
        item_name,
        src_dir,
        dst_dir,
        files,
        chmod,
    }
}

fn plan_link(item: &FileItem) -> FileAction {
    let item_name = item.name.clone().unwrap_or_else(|| item.dst.clone());
    let dst = match FluxPath::parse(&item.dst) {
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
    let Some(target) = item.target.clone() else {
        return FileAction::Failed {
            item_name,
            error: FileError::InvalidPath("link kind requires target".into()).into(),
        };
    };
    FileAction::ApplyLink {
        item_name,
        dst,
        target,
    }
}

async fn plan_regular_file<R: RemoteOps + ?Sized>(
    item_name: String,
    local_path: PathBuf,
    remote_path: String,
    mode: SyncMode,
    chmod: Option<u32>,
    remote: &R,
    policy: RetryPolicy,
) -> FileAction {
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
    let exists_remote = match with_retry(policy, || remote.exists(&remote_path)).await {
        Ok(exists) => exists,
        Err(err) => {
            return FileAction::Failed {
                item_name,
                error: err.into(),
            };
        }
    };

    match mode {
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
        } => match apply_file_path(remote, src, dst, *chmod, policy).await {
            Ok(()) => ItemOutcome::Applied,
            Err(err) => ItemOutcome::Failed(Arc::new(err.into())),
        },
        FileAction::ApplyDir {
            files,
            dst_dir,
            chmod,
            ..
        } => {
            let result = async {
                for (src, relative) in files {
                    let dst = join_remote_path(dst_dir, relative);
                    apply_file_path(remote, src, &dst, *chmod, policy).await?;
                }
                Ok::<(), RemoteOpsError>(())
            }
            .await;
            match result {
                Ok(()) => ItemOutcome::Applied,
                Err(err) => ItemOutcome::Failed(Arc::new(err.into())),
            }
        }
        FileAction::ApplyLink { dst, target, .. } => {
            match apply_link(remote, dst, target, policy).await {
                Ok(()) => ItemOutcome::Applied,
                Err(err) => ItemOutcome::Failed(Arc::new(err.into())),
            }
        }
    };
    reporter.item_finished(Stage::File, &name, &outcome);
    outcome
}

async fn apply_file_path<R: RemoteOps + ?Sized>(
    remote: &R,
    src: &Path,
    dst: &str,
    chmod: Option<u32>,
    policy: RetryPolicy,
) -> Result<(), RemoteOpsError> {
    if let Some(parent) = parent_dir(dst) {
        with_retry(policy, || remote.ensure_dir(parent)).await?;
    }
    let bytes = read_local_bytes(src).map_err(|err| RemoteOpsError::Io(err.to_string()))?;
    let tmp = format!("{dst}.flux.tmp.{}", std::process::id());
    with_retry(policy, || remote.write_file(&tmp, &bytes)).await?;
    if let Some(mode) = chmod {
        let current_mode = with_retry(policy, || remote.stat_mode(&tmp)).await.ok();
        if current_mode != Some(mode) {
            with_retry(policy, || remote.chmod(&tmp, mode)).await?;
        }
    }
    if with_retry(policy, || remote.exists(dst)).await? {
        let backup = format!("{dst}.flux-{}.bak", Utc::now().timestamp());
        with_retry(policy, || remote.rename(dst, &backup)).await?;
        rotate_backups(remote, dst, 3, policy).await?;
    }
    with_retry(policy, || remote.rename(&tmp, dst)).await?;
    Ok(())
}

async fn apply_link<R: RemoteOps + ?Sized>(
    remote: &R,
    dst: &str,
    target: &str,
    policy: RetryPolicy,
) -> Result<(), RemoteOpsError> {
    if let Some(parent) = parent_dir(dst) {
        with_retry(policy, || remote.ensure_dir(parent)).await?;
    }
    let command = format!("ln -sfn {} {}", shell_escape(target), shell_escape(dst));
    let out = with_retry(policy, || remote.exec(&command)).await?;
    ensure_success(&out)
}

fn action_name(action: &FileAction) -> String {
    match action {
        FileAction::Skip { item_name, .. }
        | FileAction::Apply { item_name, .. }
        | FileAction::ApplyDir { item_name, .. }
        | FileAction::ApplyLink { item_name, .. }
        | FileAction::Failed { item_name, .. } => item_name.clone(),
    }
}

fn resolve_local_remote(src: &str, dst: &str) -> Result<(PathBuf, String), FileError> {
    let src_raw = src.to_string();
    let dst_raw = dst.to_string();
    let src = FluxPath::parse(src);
    let dst = FluxPath::parse(dst);
    let local_path = match src {
        FluxPath::Local(path) => path,
        FluxPath::Remote(_) => {
            return Err(FileError::UnsupportedDirection {
                src: src_raw,
                dst: dst_raw,
            });
        }
    };
    let remote_path = match dst {
        FluxPath::Remote(path) => path,
        FluxPath::Local(_) => {
            return Err(FileError::UnsupportedDirection {
                src: src_raw,
                dst: dst_raw,
            });
        }
    };
    Ok((local_path, remote_path))
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

async fn rotate_backups<R: RemoteOps + ?Sized>(
    remote: &R,
    dst: &str,
    keep: usize,
    policy: RetryPolicy,
) -> Result<(), RemoteOpsError> {
    let quoted_dst = shell_escape(dst);
    let pattern = format!("\"$(dirname {quoted_dst})/$(basename {quoted_dst})\".flux-*.bak");
    let command = format!(
        "ls -1 {pattern} 2>/dev/null | sort -t- -k2,2nr | tail -n +{}",
        keep + 1
    );
    let out = with_retry(policy, || remote.exec(&command)).await?;
    ensure_success(&out)?;
    for backup in out
        .stdout_string()
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        with_retry(policy, || remote.remove_file(backup)).await?;
    }
    Ok(())
}

fn ensure_success(out: &ExecOutput) -> Result<(), RemoteOpsError> {
    if out.success() {
        Ok(())
    } else {
        Err(RemoteOpsError::NonZeroExit {
            status: out.status,
            stderr: out.stderr_string(),
        })
    }
}

fn shell_escape(input: &str) -> String {
    let mut out = String::with_capacity(input.len() + 2);
    out.push('\'');
    for ch in input.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

fn contains_glob_meta(path: &str) -> bool {
    path.contains('*') || path.contains('?') || path.contains('[')
}

fn glob_search_root(pattern: &Path) -> PathBuf {
    let mut root = PathBuf::new();
    for component in pattern.components() {
        let value = component.as_os_str().to_string_lossy();
        if contains_glob_meta(&value) {
            break;
        }
        root.push(component.as_os_str());
    }
    if root.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        root
    }
}

fn join_remote_path(base: &str, relative: &str) -> String {
    if base == "/" {
        format!("/{relative}")
    } else {
        format!("{}/{}", base.trim_end_matches('/'), relative)
    }
}

fn path_to_unix(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote::fake::InMemoryRemote;
    use crate::reporter::memory::CapturedReporter;
    use tempfile::TempDir;

    fn local_file(dir: &TempDir, name: &str, content: &[u8]) -> String {
        let path = dir.path().join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&path, content).unwrap();
        path.to_string_lossy().into_owned()
    }

    fn item(name: &str, src: &str, dst: &str, mode: SyncMode) -> FileItem {
        FileItem {
            name: Some(name.into()),
            src: src.into(),
            dst: dst.into(),
            kind: ItemKind::Auto,
            target: None,
            mode,
            chmod: None,
            tags: vec![],
        }
    }

    #[test]
    fn auto_detect_picks_glob_dir_and_file() {
        let tmp = TempDir::new().unwrap();
        let file = local_file(&tmp, "a.txt", b"x");
        std::fs::create_dir_all(tmp.path().join("dir")).unwrap();
        let file_item = item("f", &file, ":/r/a.txt", SyncMode::Cover);
        assert_eq!(auto_detect_kind(&file_item), ItemKind::File);

        let dir_item = item(
            "d",
            &tmp.path().join("dir").to_string_lossy(),
            ":/r/dir",
            SyncMode::Cover,
        );
        assert_eq!(auto_detect_kind(&dir_item), ItemKind::Dir);

        let glob_item = item(
            "g",
            &tmp.path().join("*.txt").to_string_lossy(),
            ":/r/dir",
            SyncMode::Cover,
        );
        assert_eq!(auto_detect_kind(&glob_item), ItemKind::Glob);
    }

    #[tokio::test]
    async fn glob_expands_to_multiple_apply_actions() {
        let tmp = TempDir::new().unwrap();
        local_file(&tmp, "a.txt", b"a");
        local_file(&tmp, "b.txt", b"b");
        let mut glob_item = item(
            "glob",
            &tmp.path().join("*.txt").to_string_lossy(),
            ":/r/glob",
            SyncMode::Cover,
        );
        glob_item.kind = ItemKind::Glob;
        let remote = InMemoryRemote::new();
        let actions = plan_files(&[glob_item], &remote).await;
        assert_eq!(actions.len(), 2);
        assert!(actions
            .iter()
            .all(|action| matches!(action, FileAction::Apply { .. })));
    }

    #[tokio::test]
    async fn dir_recursion_plans_apply_dir() {
        let tmp = TempDir::new().unwrap();
        local_file(&tmp, "dir/a.txt", b"a");
        local_file(&tmp, "dir/nested/b.txt", b"b");
        let mut dir_item = item(
            "dir",
            &tmp.path().join("dir").to_string_lossy(),
            ":/r/dir",
            SyncMode::Cover,
        );
        dir_item.kind = ItemKind::Dir;
        let remote = InMemoryRemote::new();
        let actions = plan_files(&[dir_item], &remote).await;
        match &actions[0] {
            FileAction::ApplyDir { files, .. } => {
                assert_eq!(files.len(), 2);
                assert!(files.iter().any(|(_, dst)| dst == "a.txt"));
                assert!(files.iter().any(|(_, dst)| dst == "nested/b.txt"));
            }
            other => panic!("expected ApplyDir, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn link_kind_is_explicit() {
        let mut link_item = item("link", "/unused", ":/r/link", SyncMode::Cover);
        link_item.kind = ItemKind::Link;
        link_item.target = Some("/target/path".into());
        let remote = InMemoryRemote::new();
        let actions = plan_files(&[link_item], &remote).await;
        assert!(matches!(
            &actions[0],
            FileAction::ApplyLink { target, .. } if target == "/target/path"
        ));
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
                assert_eq!(planned_src, &PathBuf::from(&src));
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
    async fn execute_apply_dir_writes_nested_files() {
        let tmp = TempDir::new().unwrap();
        local_file(&tmp, "dir/a.txt", b"a");
        local_file(&tmp, "dir/nested/b.txt", b"b");
        let remote = InMemoryRemote::new();
        let reporter = CapturedReporter::new();
        let action = FileAction::ApplyDir {
            item_name: "dir".into(),
            src_dir: tmp.path().join("dir"),
            dst_dir: "/r/dir".into(),
            files: vec![
                (tmp.path().join("dir/a.txt"), "a.txt".into()),
                (tmp.path().join("dir/nested/b.txt"), "nested/b.txt".into()),
            ],
            chmod: None,
        };
        let outcome = execute_file(&action, &remote, &reporter, RetryPolicy::no_retry()).await;
        assert!(matches!(outcome, ItemOutcome::Applied));
        assert_eq!(remote.file_contents("/r/dir/a.txt"), Some(b"a".to_vec()));
        assert_eq!(
            remote.file_contents("/r/dir/nested/b.txt"),
            Some(b"b".to_vec())
        );
    }

    #[tokio::test]
    async fn execute_apply_link_runs_ln_sfn() {
        let remote = InMemoryRemote::new();
        let reporter = CapturedReporter::new();
        let action = FileAction::ApplyLink {
            item_name: "link".into(),
            dst: "/r/link".into(),
            target: "/target".into(),
        };
        let outcome = execute_file(&action, &remote, &reporter, RetryPolicy::no_retry()).await;
        assert!(matches!(outcome, ItemOutcome::Applied));
        assert!(remote
            .exec_calls()
            .iter()
            .any(|cmd| cmd.contains("ln -sfn")));
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
    async fn execute_apply_creates_backup_when_dst_existed() {
        let tmp = TempDir::new().unwrap();
        let src = local_file(&tmp, "a.txt", b"new");
        let remote = InMemoryRemote::with_files([("/r/a.txt", b"old".to_vec())]);
        remote.add_exec_rule(crate::remote::fake::ExecRule {
            matcher: Box::new(|cmd| cmd.starts_with("ls -1 ")),
            status: 0,
            stdout: vec![],
            stderr: vec![],
        });
        let actions = plan_files(&[item("a", &src, ":/r/a.txt", SyncMode::Cover)], &remote).await;
        let reporter = CapturedReporter::new();
        let outcome = execute_file(&actions[0], &remote, &reporter, RetryPolicy::no_retry()).await;
        assert!(matches!(outcome, ItemOutcome::Applied));
        assert_eq!(remote.file_contents("/r/a.txt"), Some(b"new".to_vec()));
        assert!(remote
            .file_paths()
            .iter()
            .any(|path| path.starts_with("/r/a.txt.flux-") && path.ends_with(".bak")));
    }

    #[tokio::test]
    async fn rotate_keeps_latest_3_backups() {
        let remote = InMemoryRemote::new();
        remote.add_exec_rule(crate::remote::fake::ExecRule {
            matcher: Box::new(|cmd| cmd.starts_with("ls -1 ")),
            status: 0,
            stdout: b"/r/a.txt.flux-2.bak\n/r/a.txt.flux-1.bak\n".to_vec(),
            stderr: vec![],
        });
        for ts in 1..=5 {
            remote
                .write_file(&format!("/r/a.txt.flux-{ts}.bak"), b"x")
                .await
                .unwrap();
        }
        rotate_backups(&remote, "/r/a.txt", 3, RetryPolicy::no_retry())
            .await
            .unwrap();
        assert!(remote.file_contents("/r/a.txt.flux-1.bak").is_none());
        assert!(remote.file_contents("/r/a.txt.flux-2.bak").is_none());
        assert!(remote.file_contents("/r/a.txt.flux-3.bak").is_some());
        assert!(remote.file_contents("/r/a.txt.flux-4.bak").is_some());
        assert!(remote.file_contents("/r/a.txt.flux-5.bak").is_some());
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
