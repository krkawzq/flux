//! Script execution stage.

use crate::config::ScriptItem;
use crate::remote::RemoteOps;
use crate::reporter::{ItemOutcome, Reporter, Stage};
use crate::sync::plan::ScriptAction;
use std::path::{Path, PathBuf};
use std::sync::Arc;

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
) -> Vec<ScriptAction> {
    let mut actions = Vec::with_capacity(items.len());
    for item in items {
        actions.push(plan_one_script(
            item,
            asset_root,
            default_interpreter,
            default_flags,
        ));
    }
    actions
}

fn plan_one_script(
    item: &ScriptItem,
    asset_root: &Path,
    default_interpreter: &str,
    default_flags: &[String],
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
    let mut argv = vec![interpreter.to_string()];
    argv.extend(flags);
    argv.push(upload_to.clone());
    argv.extend(item.args.iter().cloned());

    ScriptAction::Run {
        item_name,
        upload_to,
        local_script_bytes: bytes,
        command_argv: argv,
    }
}

pub async fn execute_script<R: RemoteOps + ?Sized>(
    action: &ScriptAction,
    remote: &R,
    reporter: &dyn Reporter,
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
            if let Err(err) = remote.write_file(upload_to, local_script_bytes).await {
                return finish_script(
                    reporter,
                    &name,
                    ItemOutcome::Failed(Arc::new(err.into())),
                );
            }
            if let Err(err) = remote.chmod(upload_to, 0o755).await {
                return finish_script(
                    reporter,
                    &name,
                    ItemOutcome::Failed(Arc::new(err.into())),
                );
            }
            let command = command_argv
                .iter()
                .map(|arg| shell_quote(arg))
                .collect::<Vec<_>>()
                .join(" ");
            match remote.interactive_exec(&command).await {
                Ok(0) => ItemOutcome::Applied,
                Ok(code) => ItemOutcome::Failed(Arc::new(ScriptError::ExitCode(code).into())),
                Err(err) => ItemOutcome::Failed(Arc::new(err.into())),
            }
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
    use crate::remote::fake::InMemoryRemote;
    use crate::reporter::memory::CapturedReporter;
    use tempfile::TempDir;

    fn item(_name: &str, path: &str) -> ScriptItem {
        ScriptItem {
            path: path.into(),
            args: vec![],
            interpreter: None,
            flags: None,
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
        let actions = plan_scripts(&[item("s", "s.sh")], tmp.path(), "/bin/bash", &[]).await;
        let remote = InMemoryRemote::new();
        let reporter = CapturedReporter::new();
        let outcome = execute_script::<InMemoryRemote>(&actions[0], &remote, &reporter).await;
        assert!(matches!(outcome, ItemOutcome::Applied));
        let writes = remote.write_calls();
        assert_eq!(writes.len(), 1);
        assert!(writes[0].0.starts_with("/tmp/flux_script_"));
        let interactive = remote.interactive_calls();
        assert_eq!(interactive.len(), 1);
        assert!(interactive[0].contains("'/bin/bash'"));
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
}
