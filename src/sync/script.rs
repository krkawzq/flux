//! Script execution stage.

use crate::cli::state::HostState;
use crate::config::ScriptItem;
use crate::remote::{with_retry, RemoteOps, RetryPolicy, SharedCancellation};
use crate::reporter::{ItemOutcome, Reporter, Stage};
use crate::sync::plan::ScriptAction;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use std::{collections::HashMap, path::Path as StdPath};

#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum ScriptError {
    #[error("script source not found: {0}")]
    SourceNotFound(String),
    #[error("local io: {0}")]
    LocalIo(String),
    #[error("script exited with code {0}")]
    ExitCode(i32),
}

/// Quote a string for safe inclusion in a `/bin/sh` command.
pub fn shell_quote(input: &str) -> String {
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

pub async fn plan_scripts(
    items: &[ScriptItem],
    asset_root: &Path,
    default_interpreter: &str,
    default_flags: &[String],
    state: Option<&HostState>,
    use_cache: bool,
) -> Vec<ScriptAction> {
    let mut actions = Vec::with_capacity(items.len());
    for item in items {
        actions.push(plan_one_script(
            item,
            asset_root,
            default_interpreter,
            default_flags,
            state,
            use_cache,
        ));
    }
    actions
}

fn plan_one_script(
    item: &ScriptItem,
    asset_root: &Path,
    default_interpreter: &str,
    default_flags: &[String],
    state: Option<&HostState>,
    use_cache: bool,
) -> ScriptAction {
    let item_name = item.path.clone();

    let local_path = resolve_script_path(asset_root, &item.path);
    let bytes = match std::fs::read(&local_path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return ScriptAction::Failed {
                item_name,
                error: ScriptError::SourceNotFound(local_path.display().to_string()).into(),
            };
        }
        Err(err) => {
            return ScriptAction::Failed {
                item_name,
                error: ScriptError::LocalIo(err.to_string()).into(),
            };
        }
    };

    let upload_to = format!(
        "/tmp/flux_script_{}_{}.sh",
        std::process::id(),
        item_name.replace(['/', '.', ' '], "_"),
    );
    let interpreter = item.interpreter.as_deref().unwrap_or(default_interpreter);
    let flags = item
        .flags
        .clone()
        .filter(|flags| !flags.is_empty())
        .unwrap_or_else(|| default_flags.to_vec());
    let cache_key = script_cache_key(
        &item_name,
        &bytes,
        interpreter,
        flags.as_slice(),
        item.args.as_slice(),
    );
    let mut argv = vec![interpreter.to_string()];
    argv.extend(flags);
    argv.push(upload_to.clone());
    argv.extend(item.args.iter().cloned());
    if use_cache
        && state
            .and_then(|state| state.item_hashes.get(&item_name))
            .is_some_and(|cached| cached == &cache_key)
    {
        return ScriptAction::Skip {
            item_name,
            reason: crate::sync::plan::SkipReason::ContentUnchanged,
        };
    }

    ScriptAction::Run {
        item_name,
        upload_to,
        local_script_bytes: bytes,
        command_argv: argv,
    }
}

pub fn collect_item_hashes(
    items: &[ScriptItem],
    asset_root: &StdPath,
    default_interpreter: &str,
    default_flags: &[String],
) -> HashMap<String, String> {
    let mut hashes = HashMap::new();
    for item in items {
        let item_name = item.path.clone();
        let local_path = resolve_script_path(asset_root, &item.path);
        if let Ok(bytes) = std::fs::read(&local_path) {
            let interpreter = item.interpreter.as_deref().unwrap_or(default_interpreter);
            let flags = item
                .flags
                .clone()
                .filter(|flags| !flags.is_empty())
                .unwrap_or_else(|| default_flags.to_vec());
            hashes.insert(
                item_name.clone(),
                script_cache_key(
                    &item_name,
                    &bytes,
                    interpreter,
                    flags.as_slice(),
                    item.args.as_slice(),
                ),
            );
        }
    }
    hashes
}

fn script_cache_key(
    item_name: &str,
    bytes: &[u8],
    interpreter: &str,
    flags: &[String],
    args: &[String],
) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher.update([0]);
    hasher.update(item_name.as_bytes());
    hasher.update([0]);
    hasher.update(interpreter.as_bytes());
    hasher.update([0]);
    for arg in flags {
        hasher.update(arg.as_bytes());
        hasher.update([0]);
    }
    for arg in args {
        hasher.update(arg.as_bytes());
        hasher.update([0]);
    }
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub async fn execute_script<R: RemoteOps + ?Sized>(
    action: &ScriptAction,
    remote: &R,
    reporter: &dyn Reporter,
    policy: RetryPolicy,
    script_timeout: Option<Duration>,
    cancellation: Option<&SharedCancellation>,
) -> ItemOutcome {
    let name = action_name(action);
    reporter.item_started(Stage::Script, &name);
    let outcome = match action {
        ScriptAction::Skip { reason, .. } => ItemOutcome::Skipped(reason.clone()),
        ScriptAction::Failed { error, .. } => ItemOutcome::Failed(Arc::new(error.clone())),
        ScriptAction::Run {
            upload_to,
            local_script_bytes,
            command_argv,
            ..
        } => {
            if let Err(err) =
                with_retry(policy, || remote.write_file(upload_to, local_script_bytes)).await
            {
                return finish_script(reporter, &name, ItemOutcome::Failed(Arc::new(err.into())));
            }
            if let Err(err) = with_retry(policy, || remote.chmod(upload_to, 0o755)).await {
                return finish_script(reporter, &name, ItemOutcome::Failed(Arc::new(err.into())));
            }
            let command = command_argv
                .iter()
                .map(|arg| shell_quote(arg))
                .collect::<Vec<_>>()
                .join(" ");
            let outcome = match remote
                .interactive_exec(&command, script_timeout, cancellation)
                .await
            {
                Ok(0) => ItemOutcome::Applied,
                Ok(code) => ItemOutcome::Failed(Arc::new(ScriptError::ExitCode(code).into())),
                Err(err) => ItemOutcome::Failed(Arc::new(err.into())),
            };
            if let Err(err) = with_retry(policy, || remote.remove_file(upload_to)).await {
                reporter.warning(&format!(
                    "failed to remove remote temp script {upload_to}: {err}"
                ));
            }
            outcome
        }
    };
    reporter.item_finished(Stage::Script, &name, &outcome);
    outcome
}

fn action_name(action: &ScriptAction) -> String {
    match action {
        ScriptAction::Skip { item_name, .. }
        | ScriptAction::Run { item_name, .. }
        | ScriptAction::Failed { item_name, .. } => item_name.clone(),
    }
}

fn finish_script(reporter: &dyn Reporter, name: &str, outcome: ItemOutcome) -> ItemOutcome {
    reporter.item_finished(Stage::Script, name, &outcome);
    outcome
}

fn resolve_script_path(asset_root: &Path, path: &str) -> PathBuf {
    if let Some(remote) = path.strip_prefix(':') {
        asset_root.join(remote)
    } else {
        asset_root.join(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::state::HostState;
    use crate::remote::fake::InMemoryRemote;
    use crate::reporter::memory::CapturedReporter;
    use std::collections::HashMap;
    use tempfile::TempDir;

    fn item(_name: &str, path: &str) -> ScriptItem {
        ScriptItem {
            path: path.into(),
            args: vec![],
            interpreter: None,
            flags: None,
            tags: vec![],
        }
    }

    #[test]
    fn shell_quote_handles_single_quotes() {
        assert_eq!(shell_quote("a'b"), r#"'a'\''b'"#);
        assert_eq!(shell_quote("plain"), "'plain'");
        assert_eq!(shell_quote(""), "''");
    }

    #[tokio::test]
    async fn run_action_uploads_and_execs() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("s.sh"), b"#!/bin/sh\necho hi").unwrap();
        let actions = plan_scripts(
            &[item("s", "s.sh")],
            tmp.path(),
            "/bin/bash",
            &[],
            None,
            false,
        )
        .await;
        let remote = InMemoryRemote::new();
        let reporter = CapturedReporter::new();
        let outcome = execute_script::<InMemoryRemote>(
            &actions[0],
            &remote,
            &reporter,
            RetryPolicy::no_retry(),
            None,
            None,
        )
        .await;
        assert!(matches!(outcome, ItemOutcome::Applied));
        let writes = remote.write_calls();
        assert_eq!(writes.len(), 1);
        assert!(writes[0].0.starts_with("/tmp/flux_script_"));
        let interactive = remote.interactive_calls();
        assert_eq!(interactive.len(), 1);
        assert!(interactive[0].0.contains("'/bin/bash'"));
        assert_eq!(interactive[0].1, None);
        assert_eq!(remote.file_contents(&writes[0].0), None);
    }

    #[tokio::test]
    async fn run_action_passes_timeout_to_interactive_exec() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("s.sh"), b"#!/bin/sh\necho hi").unwrap();
        let actions = plan_scripts(
            &[item("s", "s.sh")],
            tmp.path(),
            "/bin/bash",
            &[],
            None,
            false,
        )
        .await;
        let remote = InMemoryRemote::new();
        let reporter = CapturedReporter::new();
        let timeout = Duration::from_secs(3);
        let _ = execute_script::<InMemoryRemote>(
            &actions[0],
            &remote,
            &reporter,
            RetryPolicy::no_retry(),
            Some(timeout),
            None,
        )
        .await;
        assert_eq!(remote.interactive_calls()[0].1, Some(timeout));
    }

    proptest::proptest! {
        #[test]
        fn shell_quote_round_trip(input in r#"[^\x00]{0,40}"#) {
            let quoted = shell_quote(&input);
            assert!(quoted.starts_with('\''));
            assert!(quoted.ends_with('\''));
            let inner = &quoted[1..quoted.len() - 1];
            let decoded = inner.replace(r#"'\''"#, "'");
            assert_eq!(decoded, input);
        }
    }

    #[tokio::test]
    async fn script_cache_hits_across_pids() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("s.sh"), b"#!/bin/sh\necho hi").unwrap();
        let script = item("s", "s.sh");
        let state = HostState {
            host: "h".into(),
            last_sync_ts: 0,
            item_hashes: collect_item_hashes(
                std::slice::from_ref(&script),
                tmp.path(),
                "/bin/bash",
                &[],
            )
            .into_iter()
            .collect::<HashMap<_, _>>(),
            last_failed_item: None,
        };
        let actions = plan_scripts(
            &[script],
            tmp.path(),
            "/bin/bash",
            &[],
            Some(&state),
            true,
        )
        .await;
        assert!(matches!(
            &actions[0],
            ScriptAction::Skip {
                reason: crate::sync::plan::SkipReason::ContentUnchanged,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn first_ctrl_c_waits_second_ctrl_c_closes() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("s.sh"), b"#!/bin/sh\nsleep 30").unwrap();
        let actions = plan_scripts(
            &[item("s", "s.sh")],
            tmp.path(),
            "/bin/bash",
            &[],
            None,
            false,
        )
        .await;
        let remote: &'static InMemoryRemote = Box::leak(Box::new(InMemoryRemote::new()));
        remote.set_interactive_wait_for_cancellation(true);
        let reporter: &'static CapturedReporter = Box::leak(Box::new(CapturedReporter::new()));
        let cancellation = SharedCancellation::new();

        let action = actions.into_iter().next().unwrap();
        let signal_cancellation = cancellation.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            signal_cancellation.press();
            tokio::time::sleep(Duration::from_millis(100)).await;
            signal_cancellation.press();
        });
        let task = execute_script::<InMemoryRemote>(
            &action,
            remote,
            reporter,
            RetryPolicy::no_retry(),
            None,
            Some(&cancellation),
        );
        tokio::pin!(task);

        assert!(tokio::time::timeout(Duration::from_millis(50), &mut task).await.is_err());
        assert_eq!(remote.interactive_cancel_log(), vec![1]);

        let outcome = task.await;
        assert!(matches!(outcome, ItemOutcome::Failed(_)));
        assert_eq!(remote.interactive_cancel_log(), vec![1, 2]);
    }
}
