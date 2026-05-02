//! Sync pipeline orchestration.
//!
//! `Pipeline` holds references to `Config`, `RemoteOps`, and `Reporter`,
//! computes a Plan, and executes it stage by stage with stage-level
//! concurrency (file: parallel; script: serial; block: parallel by target).

pub mod block;
pub mod file;
pub mod plan;
pub mod script;

use crate::config::Config;
use crate::remote::ssh::{SshClient, SshConfig};
use crate::remote::{RemoteOps, RemoteOpsError};
use crate::reporter::{ItemOutcome, PipelineSummary, Reporter, Stage, StageSummary};
use crate::sync::plan::{BlockAction, FileAction, Plan, RegisterPubkeyAction, ScriptAction};
use anyhow::Result;
use dialoguer::{Input, Password};
use futures::stream::StreamExt;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum SyncError {
    #[error("file: {0}")]
    File(#[from] file::FileError),
    #[error("script: {0}")]
    Script(#[from] script::ScriptError),
    #[error("block: {0}")]
    Block(#[from] block::BlockError),
    #[error("remote: {0}")]
    Remote(#[from] RemoteOpsError),
}

#[derive(Debug, Clone)]
pub struct PipelineOpts {
    pub dry_run: bool,
    pub max_concurrency: usize,
}

impl Default for PipelineOpts {
    fn default() -> Self {
        Self {
            dry_run: false,
            max_concurrency: 8,
        }
    }
}

pub struct Pipeline<'a, R: RemoteOps + ?Sized> {
    pub config: &'a Config,
    pub asset_root: &'a Path,
    pub remote: &'a R,
    pub reporter: &'a dyn Reporter,
    pub opts: PipelineOpts,
}

impl<'a, R: RemoteOps + ?Sized> Pipeline<'a, R> {
    pub async fn plan(&self) -> Plan {
        let register_pubkey = self.config.register_key.then(|| RegisterPubkeyAction {
            local_pubkey_path: self
                .config
                .key
                .clone()
                .map(|key| format!("{key}.pub"))
                .unwrap_or_default(),
            remote_authorized_keys: "~/.ssh/authorized_keys".into(),
        });
        let file_actions =
            file::plan_files_with_concurrency(&self.config.file, self.remote, self.opts.max_concurrency)
                .await;
        let script_actions = script::plan_scripts(
            &self.config.script,
            self.asset_root,
            &self.config.interpreter,
            self.config.flags.as_slice(),
        )
        .await;
        let block_actions = block::plan_blocks_with_concurrency(
            &self.config.block,
            self.asset_root,
            &self.config.comment_template,
            self.remote,
            self.opts.max_concurrency,
        )
        .await;
        Plan {
            register_pubkey,
            file_actions,
            script_actions,
            block_actions,
        }
    }

    pub async fn run(&self) -> PipelineSummary {
        let plan = self.plan().await;
        if self.opts.dry_run {
            self.reporter.print_plan(&plan);
            return PipelineSummary {
                stages: vec![],
                interrupted: false,
                dry_run: true,
            };
        }
        self.execute(&plan).await
    }

    pub async fn execute(&self, plan: &Plan) -> PipelineSummary {
        let mut stages = Vec::new();
        if let Some(action) = &plan.register_pubkey {
            stages.push(self.execute_pubkey(action).await);
        }
        stages.push(self.execute_file_stage(&plan.file_actions).await);
        stages.push(self.execute_script_stage(&plan.script_actions).await);
        stages.push(self.execute_block_stage(&plan.block_actions).await);
        let summary = PipelineSummary {
            stages,
            interrupted: false,
            dry_run: false,
        };
        self.reporter.pipeline_summary(&summary);
        summary
    }

    async fn execute_file_stage(&self, actions: &[FileAction]) -> StageSummary {
        self.reporter.stage_started(Stage::File, actions.len());
        let outcomes: Vec<ItemOutcome> =
            futures::stream::iter(actions.iter())
                .map(|action| async move {
                    file::execute_file(action, self.remote, self.reporter).await
                })
                .buffer_unordered(self.opts.max_concurrency)
                .collect()
                .await;
        let summary = tally(Stage::File, &outcomes);
        self.reporter.stage_finished(&summary);
        summary
    }

    async fn execute_script_stage(&self, actions: &[ScriptAction]) -> StageSummary {
        self.reporter.stage_started(Stage::Script, actions.len());
        let mut outcomes = Vec::with_capacity(actions.len());
        for action in actions {
            outcomes.push(script::execute_script(action, self.remote, self.reporter).await);
        }
        let summary = tally(Stage::Script, &outcomes);
        self.reporter.stage_finished(&summary);
        summary
    }

    async fn execute_block_stage(&self, actions: &[BlockAction]) -> StageSummary {
        self.reporter.stage_started(Stage::Block, actions.len());
        let mut by_target: HashMap<String, Vec<&BlockAction>> = HashMap::new();
        for action in actions {
            let key = match action {
                BlockAction::Apply { target, .. } => target.clone(),
                BlockAction::Skip { item_name, .. } | BlockAction::Failed { item_name, .. } => {
                    format!("_special:{item_name}")
                }
            };
            by_target.entry(key).or_default().push(action);
        }

        let template = self.config.comment_template.clone();
        let outcomes_groups: Vec<Vec<ItemOutcome>> = futures::stream::iter(by_target.into_values())
            .map(|group| async {
                let mut outcomes = Vec::with_capacity(group.len());
                for action in group {
                    outcomes.push(
                        block::execute_block(action, self.remote, &template, self.reporter).await,
                    );
                }
                outcomes
            })
            .buffer_unordered(self.opts.max_concurrency)
            .collect()
            .await;
        let outcomes: Vec<ItemOutcome> = outcomes_groups.into_iter().flatten().collect();
        let summary = tally(Stage::Block, &outcomes);
        self.reporter.stage_finished(&summary);
        summary
    }

    async fn execute_pubkey(&self, action: &RegisterPubkeyAction) -> StageSummary {
        self.reporter.stage_started(Stage::Pubkey, 1);
        let result = async {
            let pub_bytes = std::fs::read(&action.local_pubkey_path)
                .map_err(|err| SyncError::Remote(RemoteOpsError::Io(err.to_string())))?;
            let pub_str = String::from_utf8(pub_bytes)
                .map_err(|err| SyncError::Remote(RemoteOpsError::Encoding(err.to_string())))?
                .trim()
                .to_string();
            let target = action.remote_authorized_keys.clone();
            let existing = match self.remote.read_file(&target).await {
                Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
                Err(RemoteOpsError::NotFound(_)) => String::new(),
                Err(err) => return Err(SyncError::Remote(err)),
            };
            if existing.lines().any(|line| line.trim() == pub_str) {
                return Ok::<_, SyncError>(ItemOutcome::Skipped(
                    crate::sync::plan::SkipReason::AlreadyExists,
                ));
            }
            let mut new_content = existing;
            if !new_content.is_empty() && !new_content.ends_with('\n') {
                new_content.push('\n');
            }
            new_content.push_str(&pub_str);
            new_content.push('\n');
            self.remote
                .write_file(&target, new_content.as_bytes())
                .await?;
            self.remote.chmod(&target, 0o600).await?;
            Ok(ItemOutcome::Applied)
        }
        .await;

        let outcome = match result {
            Ok(outcome) => outcome,
            Err(err) => ItemOutcome::Failed(Arc::new(err)),
        };
        self.reporter
            .item_finished(Stage::Pubkey, "register_pubkey", &outcome);
        let summary = tally(Stage::Pubkey, std::slice::from_ref(&outcome));
        self.reporter.stage_finished(&summary);
        summary
    }
}

fn tally(stage: Stage, outcomes: &[ItemOutcome]) -> StageSummary {
    let mut applied = 0;
    let mut skipped = 0;
    let mut failed = 0;
    for outcome in outcomes {
        match outcome {
            ItemOutcome::Applied => applied += 1,
            ItemOutcome::Skipped(_) => skipped += 1,
            ItemOutcome::Failed(_) => failed += 1,
        }
    }
    StageSummary {
        stage,
        applied,
        skipped,
        failed,
    }
}

/// Compatibility helper until CLI extraction lands in Task 12.
pub async fn run_sync(config: Config, config_path: &Path) -> Result<SshConfigInfo> {
    config.validate()?;
    let config = resolve_config_paths(config, config_path);
    let ssh_config = resolve_ssh_config(&config)?;
    let info = SshConfigInfo {
        host: ssh_config.host.clone(),
        port: ssh_config.port,
        user: ssh_config.user.clone(),
        key: ssh_config.key_path.clone(),
    };
    let mut client = SshClient::connect(&ssh_config).await?;
    if config.proxy.enabled {
        client
            .start_reverse_forward(config.proxy.local_port, config.proxy.remote_port)
            .await?;
    }
    let reporter = crate::reporter::console::ConsoleReporter::new();
    let asset_root = config.resolve_root(config_path);
    let pipeline = Pipeline {
        config: &config,
        asset_root: &asset_root,
        remote: &client,
        reporter: &reporter,
        opts: PipelineOpts::default(),
    };
    let _ = pipeline.run().await;
    client.close().await?;
    Ok(info)
}

pub struct SshConfigInfo {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub key: Option<String>,
}

fn resolve_ssh_config(config: &Config) -> Result<SshConfig> {
    let host = match &config.host {
        Some(host) if !host.is_empty() => host.clone(),
        _ => Input::new().with_prompt("Host").interact_text()?,
    };
    let port = match config.port {
        Some(port) if port > 0 => port,
        _ => Input::new()
            .with_prompt("Port")
            .default(22u16)
            .interact_text()?,
    };
    let user = match &config.user {
        Some(user) if !user.is_empty() => user.clone(),
        _ => Input::new()
            .with_prompt("User")
            .default("root".to_string())
            .interact_text()?,
    };
    let key_path = config.key.clone();
    let password = match &config.password {
        Some(password) if !password.is_empty() => Some(password.clone()),
        _ => {
            let need_password = key_path.as_ref().is_none_or(|key| {
                let expanded = expand_tilde(key);
                !std::path::Path::new(&expanded).exists()
            });
            if need_password {
                Some(Password::new().with_prompt("Password").interact()?)
            } else {
                None
            }
        }
    };
    Ok(SshConfig {
        host,
        port,
        user,
        key_path,
        password,
    })
}

fn expand_tilde(path: &str) -> String {
    if path.starts_with("~/") {
        if let Some(home) = dirs::home_dir() {
            return path.replacen("~", &home.to_string_lossy(), 1);
        }
    } else if path == "~" {
        if let Some(home) = dirs::home_dir() {
            return home.to_string_lossy().to_string();
        }
    }
    path.to_string()
}

fn resolve_config_paths(mut config: Config, config_path: &Path) -> Config {
    let flux_dir = config.resolve_root(config_path);

    let resolve_local = |path: &str, subdir: &str| -> String {
        if path.starts_with(':') || path.starts_with('/') || path.starts_with('~') {
            return path.to_string();
        }
        if path.contains('/') || path.contains('\\') {
            let full_path = flux_dir.join(path);
            if full_path.exists() {
                return full_path.to_string_lossy().to_string();
            }
            return path.to_string();
        }
        let subdir_path = flux_dir.join(subdir).join(path);
        if subdir_path.exists() {
            return subdir_path.to_string_lossy().to_string();
        }
        let direct_path = flux_dir.join(path);
        if direct_path.exists() {
            return direct_path.to_string_lossy().to_string();
        }
        path.to_string()
    };

    for file in &mut config.file {
        file.src = resolve_local(&file.src, "files");
    }
    for script in &mut config.script {
        script.path = resolve_local(&script.path, "scripts");
    }
    for block in &mut config.block {
        block.path = resolve_local(&block.path, "blocks");
    }

    config
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{FileItem, ProxyConfig, SyncMode};
    use crate::remote::fake::InMemoryRemote;
    use crate::reporter::memory::CapturedReporter;
    use tempfile::TempDir;

    fn minimal_config(items: Vec<FileItem>) -> Config {
        Config {
            version: 1,
            host: Some("127.0.0.1".into()),
            port: Some(22),
            user: Some("u".into()),
            password: None,
            key: None,
            register_key: false,
            interpreter: "/bin/bash".into(),
            flags: vec![],
            comment_template: "# {}".into(),
            proxy: ProxyConfig::default(),
            file: items,
            script: vec![],
            block: vec![],
            flux_home: None,
        }
    }

    #[tokio::test]
    async fn empty_config_yields_empty_summary() {
        let tmp = TempDir::new().unwrap();
        let config = minimal_config(vec![]);
        let remote = InMemoryRemote::new();
        let reporter = CapturedReporter::new();
        let pipe = Pipeline {
            config: &config,
            asset_root: tmp.path(),
            remote: &remote,
            reporter: &reporter,
            opts: PipelineOpts::default(),
        };
        let summary = pipe.run().await;
        assert_eq!(summary.total_failed(), 0);
    }

    #[tokio::test]
    async fn dry_run_does_not_write() {
        let tmp = TempDir::new().unwrap();
        let src = tmp.path().join("a");
        std::fs::write(&src, b"hi").unwrap();
        let config = minimal_config(vec![FileItem {
            name: Some("a".into()),
            src: src.to_string_lossy().into_owned(),
            dst: ":/r/a".into(),
            mode: SyncMode::Cover,
            chmod: None,
        }]);
        let remote = InMemoryRemote::new();
        let reporter = CapturedReporter::new();
        let pipe = Pipeline {
            config: &config,
            asset_root: tmp.path(),
            remote: &remote,
            reporter: &reporter,
            opts: PipelineOpts {
                dry_run: true,
                max_concurrency: 4,
            },
        };
        let _ = pipe.run().await;
        assert_eq!(remote.write_calls().len(), 0);
    }

    #[tokio::test]
    async fn file_failure_does_not_short_circuit() {
        let tmp = TempDir::new().unwrap();
        let good = tmp.path().join("good");
        std::fs::write(&good, b"ok").unwrap();
        let config = minimal_config(vec![
            FileItem {
                name: Some("missing".into()),
                src: "/no/such".into(),
                dst: ":/r/x".into(),
                mode: SyncMode::Cover,
                chmod: None,
            },
            FileItem {
                name: Some("good".into()),
                src: good.to_string_lossy().into_owned(),
                dst: ":/r/y".into(),
                mode: SyncMode::Cover,
                chmod: None,
            },
        ]);
        let remote = InMemoryRemote::new();
        let reporter = CapturedReporter::new();
        let pipe = Pipeline {
            config: &config,
            asset_root: tmp.path(),
            remote: &remote,
            reporter: &reporter,
            opts: PipelineOpts::default(),
        };
        let summary = pipe.run().await;
        assert_eq!(summary.stages[0].failed, 1);
        assert_eq!(summary.stages[0].applied, 1);
    }
}
