//! Sync pipeline orchestration.
//!
//! `Pipeline` holds references to `Config`, `RemoteOps`, and `Reporter`,
//! computes a Plan, and executes it stage by stage with stage-level
//! concurrency (file: parallel; script: serial; block: parallel by target).

pub mod block;
pub mod file;
pub mod plan;
pub mod script;

use crate::cli::state::HostState;
use crate::config::Config;
use crate::remote::ssh::{SshClient, SshConfig};
use crate::remote::{with_retry, RemoteOps, RemoteOpsError};
use crate::remote::{RetryPolicy, SharedCancellation};
use crate::reporter::console::print_plan_with_diff;
use crate::reporter::{ItemOutcome, PipelineSummary, Reporter, Stage, StageSummary};
use crate::sync::plan::{
    BlockAction, FileAction, Plan, RegisterPubkeyAction, ScriptAction, SkipReason,
};
use anyhow::Result;
use dialoguer::{Input, Password};
use futures::stream::StreamExt;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

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
    pub diff: bool,
    pub max_concurrency: usize,
    pub retry: RetryPolicy,
    pub script_timeout: Option<Duration>,
    pub filter: PipelineFilter,
    pub state: Option<HostState>,
    pub use_cache: bool,
    pub resume_from: Option<String>,
    pub cancellation: Option<SharedCancellation>,
}

impl Default for PipelineOpts {
    fn default() -> Self {
        Self {
            dry_run: false,
            diff: false,
            max_concurrency: 8,
            retry: RetryPolicy::default(),
            script_timeout: None,
            filter: PipelineFilter::default(),
            state: None,
            use_cache: true,
            resume_from: None,
            cancellation: None,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct PipelineFilter {
    pub only_stages: Option<HashSet<Stage>>,
    pub skip_stages: HashSet<Stage>,
    pub only_items: Option<HashSet<String>>,
    pub tags: Option<HashSet<String>>,
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
        let register_pubkey = if self.config.register_key {
            self.config
                .key
                .as_ref()
                .filter(|key| !key.is_empty())
                .map(|key| RegisterPubkeyAction {
                    local_pubkey_path: format!("{key}.pub"),
                    remote_authorized_keys: "~/.ssh/authorized_keys".into(),
                })
        } else {
            None
        };
        let file_actions = file::plan_files_with_concurrency(
            &self.config.file,
            self.remote,
            self.opts.max_concurrency,
            self.opts.retry,
            self.opts.state.as_ref(),
            self.opts.use_cache,
        )
        .await;
        let script_actions = script::plan_scripts(
            &self.config.script,
            self.asset_root,
            &self.config.interpreter,
            self.config.flags.as_slice(),
            self.opts.state.as_ref(),
            self.opts.use_cache,
        )
        .await;
        let block_actions = block::plan_blocks_with_concurrency(
            &self.config.block,
            self.asset_root,
            &self.config.comment_template,
            self.remote,
            self.opts.max_concurrency,
            self.opts.retry,
            self.opts.state.as_ref(),
            self.opts.use_cache,
        )
        .await;
        let mut plan = Plan {
            register_pubkey,
            file_actions,
            script_actions,
            block_actions,
        };
        apply_resume(&mut plan, self.opts.resume_from.as_deref());
        apply_filter(&mut plan, &self.opts.filter, &self.item_tags());
        plan
    }

    pub async fn run_with_plan(&self) -> (Plan, PipelineSummary) {
        let plan = self.plan().await;
        let initial_failed = first_failed_in_plan(&plan);
        if self.opts.dry_run {
            if self.opts.diff {
                print_plan_with_diff(&plan, self.remote, self.reporter).await;
            } else {
                self.reporter.print_plan(&plan);
            }
            return (
                plan,
                PipelineSummary {
                    stages: vec![],
                    interrupted: false,
                    dry_run: true,
                    first_failed_item: initial_failed,
                },
            );
        }
        let cancellation = self.opts.cancellation.clone().unwrap_or_default();
        let listener_state = cancellation.clone();
        let signal_task = tokio::spawn(async move {
            while tokio::signal::ctrl_c().await.is_ok() {
                listener_state.press();
            }
        });
        let mut summary = self.execute(&plan, &cancellation).await;
        signal_task.abort();
        if cancellation.presses() > 0 {
            self.reporter.warning("interrupted by user (Ctrl-C)");
            summary.interrupted = true;
        }
        if summary.first_failed_item.is_none() {
            summary.first_failed_item = initial_failed;
        }
        (plan, summary)
    }

    pub async fn run(&self) -> PipelineSummary {
        self.run_with_plan().await.1
    }

    pub async fn execute(&self, plan: &Plan, cancellation: &SharedCancellation) -> PipelineSummary {
        let mut stages = Vec::new();
        let mut first_failed_item = None;
        if let Some(action) = &plan.register_pubkey {
            let (summary, failed) = self.execute_pubkey(action).await;
            first_failed_item = first_failed_item.or(failed);
            stages.push(summary);
            if cancellation.presses() > 0 {
                return self.finish_interrupted(stages, first_failed_item);
            }
        }
        let (file_summary, file_failed) = self.execute_file_stage(&plan.file_actions).await;
        first_failed_item = first_failed_item.or(file_failed);
        stages.push(file_summary);
        if cancellation.presses() > 0 {
            return self.finish_interrupted(stages, first_failed_item);
        }
        let (script_summary, script_failed) = self
            .execute_script_stage(&plan.script_actions, cancellation)
            .await;
        first_failed_item = first_failed_item.or(script_failed);
        stages.push(script_summary);
        if cancellation.presses() > 0 {
            return self.finish_interrupted(stages, first_failed_item);
        }
        let (block_summary, block_failed) = self.execute_block_stage(&plan.block_actions).await;
        first_failed_item = first_failed_item.or(block_failed);
        stages.push(block_summary);
        let summary = PipelineSummary {
            stages,
            interrupted: false,
            dry_run: false,
            first_failed_item,
        };
        self.reporter.pipeline_summary(&summary);
        summary
    }

    async fn execute_file_stage(&self, actions: &[FileAction]) -> (StageSummary, Option<String>) {
        self.reporter.stage_started(Stage::File, actions.len());
        let outcomes: Vec<(usize, String, ItemOutcome)> =
            futures::stream::iter(actions.iter().enumerate())
                .map(|(idx, action)| async move {
                    (
                        idx,
                        file_item_name(action).to_string(),
                        file::execute_file(action, self.remote, self.reporter, self.opts.retry)
                            .await,
                    )
                })
                .buffer_unordered(self.opts.max_concurrency.max(1))
                .collect()
                .await;
        let mut ordered = outcomes;
        ordered.sort_by_key(|(idx, _, _)| *idx);
        let first_failed = ordered.iter().find_map(|(_, name, outcome)| {
            matches!(outcome, ItemOutcome::Failed(_)).then(|| name.clone())
        });
        let summary = tally(
            Stage::File,
            &ordered
                .iter()
                .map(|(_, _, outcome)| outcome.clone())
                .collect::<Vec<_>>(),
        );
        self.reporter.stage_finished(&summary);
        (summary, first_failed)
    }

    async fn execute_script_stage(
        &self,
        actions: &[ScriptAction],
        cancellation: &SharedCancellation,
    ) -> (StageSummary, Option<String>) {
        self.reporter.stage_started(Stage::Script, actions.len());
        let mut outcomes = Vec::with_capacity(actions.len());
        let mut first_failed = None;
        for action in actions {
            if cancellation.presses() > 0 {
                break;
            }
            let outcome = script::execute_script(
                action,
                self.remote,
                self.reporter,
                self.opts.retry,
                self.opts.script_timeout,
                Some(cancellation),
            )
            .await;
            if first_failed.is_none() && matches!(outcome, ItemOutcome::Failed(_)) {
                first_failed = Some(script_item_name(action).to_string());
            }
            outcomes.push(outcome);
        }
        let summary = tally(Stage::Script, &outcomes);
        self.reporter.stage_finished(&summary);
        (summary, first_failed)
    }

    fn finish_interrupted(
        &self,
        stages: Vec<StageSummary>,
        first_failed_item: Option<String>,
    ) -> PipelineSummary {
        let summary = PipelineSummary {
            stages,
            interrupted: true,
            dry_run: false,
            first_failed_item,
        };
        self.reporter.pipeline_summary(&summary);
        summary
    }

    async fn execute_block_stage(&self, actions: &[BlockAction]) -> (StageSummary, Option<String>) {
        self.reporter.stage_started(Stage::Block, actions.len());
        let mut by_target: HashMap<String, Vec<(usize, &BlockAction)>> = HashMap::new();
        for (idx, action) in actions.iter().enumerate() {
            let key = match action {
                BlockAction::Apply { target, .. } => target.clone(),
                BlockAction::Skip { item_name, .. } | BlockAction::Failed { item_name, .. } => {
                    format!("_special:{item_name}")
                }
            };
            by_target.entry(key).or_default().push((idx, action));
        }

        let template = self.config.comment_template.clone();
        let outcomes_groups: Vec<Vec<(usize, String, ItemOutcome)>> =
            futures::stream::iter(by_target.into_values())
                .map(|group| async {
                    let mut outcomes = Vec::with_capacity(group.len());
                    for (idx, action) in group {
                        outcomes.push((
                            idx,
                            block_item_name(action).to_string(),
                            block::execute_block(
                                action,
                                self.remote,
                                &template,
                                self.reporter,
                                self.opts.retry,
                            )
                            .await,
                        ));
                    }
                    outcomes
                })
                .buffer_unordered(self.opts.max_concurrency.max(1))
                .collect()
                .await;
        let mut ordered: Vec<(usize, String, ItemOutcome)> =
            outcomes_groups.into_iter().flatten().collect();
        ordered.sort_by_key(|(idx, _, _)| *idx);
        let first_failed = ordered.iter().find_map(|(_, name, outcome)| {
            matches!(outcome, ItemOutcome::Failed(_)).then(|| name.clone())
        });
        let summary = tally(
            Stage::Block,
            &ordered
                .iter()
                .map(|(_, _, outcome)| outcome.clone())
                .collect::<Vec<_>>(),
        );
        self.reporter.stage_finished(&summary);
        (summary, first_failed)
    }

    async fn execute_pubkey(
        &self,
        action: &RegisterPubkeyAction,
    ) -> (StageSummary, Option<String>) {
        self.reporter.stage_started(Stage::Pubkey, 1);
        let result = async {
            let pub_bytes = std::fs::read(&action.local_pubkey_path)
                .map_err(|err| SyncError::Remote(RemoteOpsError::Io(err.to_string())))?;
            let pub_str = String::from_utf8(pub_bytes)
                .map_err(|err| SyncError::Remote(RemoteOpsError::Encoding(err.to_string())))?
                .trim()
                .to_string();
            let Some(new_key) = parse_pubkey_body(&pub_str) else {
                return Err(SyncError::Remote(RemoteOpsError::Encoding(
                    "invalid public key format".into(),
                )));
            };
            let target = action.remote_authorized_keys.clone();
            let existing =
                match with_retry(self.opts.retry, || self.remote.read_file(&target)).await {
                    Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
                    Err(RemoteOpsError::NotFound(_)) => String::new(),
                    Err(err) => return Err(SyncError::Remote(err)),
                };
            if existing
                .lines()
                .filter_map(parse_pubkey_body)
                .any(|existing_key| existing_key == new_key)
            {
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
            with_retry(self.opts.retry, || {
                self.remote.write_file(&target, new_content.as_bytes())
            })
            .await?;
            with_retry(self.opts.retry, || self.remote.chmod(&target, 0o600)).await?;
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
        let failed =
            matches!(outcome, ItemOutcome::Failed(_)).then(|| "register_pubkey".to_string());
        (summary, failed)
    }

    fn item_tags(&self) -> HashMap<String, Vec<String>> {
        let mut tags = HashMap::new();
        for item in &self.config.file {
            let name = item.name.clone().unwrap_or_else(|| item.src.clone());
            tags.insert(name, item.tags.clone());
        }
        for item in &self.config.script {
            tags.insert(item.path.clone(), item.tags.clone());
        }
        for item in &self.config.block {
            tags.insert(item.name.clone(), item.tags.clone());
        }
        tags
    }
}

fn parse_pubkey_body(line: &str) -> Option<(String, String)> {
    let mut parts = line.split_whitespace();
    let key_type = parts.next()?;
    if !(key_type.starts_with("ssh-")
        || key_type.starts_with("ecdsa-")
        || key_type.starts_with("sk-"))
    {
        return None;
    }
    let key_body = parts.next()?;
    Some((key_type.to_string(), key_body.to_string()))
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

fn apply_filter(
    plan: &mut Plan,
    filter: &PipelineFilter,
    item_tags: &HashMap<String, Vec<String>>,
) {
    if !stage_selected(filter, Stage::Pubkey) {
        plan.register_pubkey = None;
    }
    for action in &mut plan.file_actions {
        let name = file_item_name(action).to_string();
        if !action_selected(filter, Stage::File, &name, item_tags) {
            *action = FileAction::Skip {
                item_name: name,
                reason: SkipReason::FilteredOut,
            };
        }
    }
    for action in &mut plan.script_actions {
        let name = script_item_name(action).to_string();
        if !action_selected(filter, Stage::Script, &name, item_tags) {
            *action = ScriptAction::Skip {
                item_name: name,
                reason: SkipReason::FilteredOut,
            };
        }
    }
    for action in &mut plan.block_actions {
        let name = block_item_name(action).to_string();
        if !action_selected(filter, Stage::Block, &name, item_tags) {
            *action = BlockAction::Skip {
                item_name: name,
                reason: SkipReason::FilteredOut,
            };
        }
    }
}

fn apply_resume(plan: &mut Plan, resume_from: Option<&str>) {
    let Some(target) = resume_from else {
        return;
    };
    let mut reached = false;
    if let Some(action) = &plan.register_pubkey {
        if target == "register_pubkey" {
            reached = true;
        } else if !reached {
            let _ = action;
            plan.register_pubkey = None;
        }
    }
    for action in &mut plan.file_actions {
        resume_action_file(action, target, &mut reached);
    }
    for action in &mut plan.script_actions {
        resume_action_script(action, target, &mut reached);
    }
    for action in &mut plan.block_actions {
        resume_action_block(action, target, &mut reached);
    }
}

fn resume_action_file(action: &mut FileAction, target: &str, reached: &mut bool) {
    let name = file_item_name(action).to_string();
    if !*reached && name != target {
        *action = FileAction::Skip {
            item_name: name,
            reason: SkipReason::PreviouslyApplied,
        };
    } else if name == target {
        *reached = true;
    }
}

fn resume_action_script(action: &mut ScriptAction, target: &str, reached: &mut bool) {
    let name = script_item_name(action).to_string();
    if !*reached && name != target {
        *action = ScriptAction::Skip {
            item_name: name,
            reason: SkipReason::PreviouslyApplied,
        };
    } else if name == target {
        *reached = true;
    }
}

fn resume_action_block(action: &mut BlockAction, target: &str, reached: &mut bool) {
    let name = block_item_name(action).to_string();
    if !*reached && name != target {
        *action = BlockAction::Skip {
            item_name: name,
            reason: SkipReason::PreviouslyApplied,
        };
    } else if name == target {
        *reached = true;
    }
}

fn first_failed_in_plan(plan: &Plan) -> Option<String> {
    for action in &plan.file_actions {
        if matches!(action, FileAction::Failed { .. }) {
            return Some(file_item_name(action).to_string());
        }
    }
    for action in &plan.script_actions {
        if matches!(action, ScriptAction::Failed { .. }) {
            return Some(script_item_name(action).to_string());
        }
    }
    for action in &plan.block_actions {
        if matches!(action, BlockAction::Failed { .. }) {
            return Some(block_item_name(action).to_string());
        }
    }
    None
}

fn action_selected(
    filter: &PipelineFilter,
    stage: Stage,
    item_name: &str,
    item_tags: &HashMap<String, Vec<String>>,
) -> bool {
    if !stage_selected(filter, stage) {
        return false;
    }
    if let Some(only_items) = &filter.only_items {
        if !only_items.contains(item_name) {
            return false;
        }
    }
    if let Some(tags) = &filter.tags {
        let matched = item_tags
            .get(item_name)
            .into_iter()
            .flatten()
            .any(|tag| tags.contains(tag));
        if !matched {
            return false;
        }
    }
    true
}

fn stage_selected(filter: &PipelineFilter, stage: Stage) -> bool {
    if filter.skip_stages.contains(&stage) {
        return false;
    }
    filter
        .only_stages
        .as_ref()
        .is_none_or(|stages| stages.contains(&stage))
}

fn file_item_name(action: &FileAction) -> &str {
    match action {
        FileAction::Skip { item_name, .. }
        | FileAction::Apply { item_name, .. }
        | FileAction::ApplyDir { item_name, .. }
        | FileAction::ApplyLink { item_name, .. }
        | FileAction::Failed { item_name, .. } => item_name,
    }
}

fn script_item_name(action: &ScriptAction) -> &str {
    match action {
        ScriptAction::Skip { item_name, .. }
        | ScriptAction::Run { item_name, .. }
        | ScriptAction::Failed { item_name, .. } => item_name,
    }
}

fn block_item_name(action: &BlockAction) -> &str {
    match action {
        BlockAction::Skip { item_name, .. }
        | BlockAction::Apply { item_name, .. }
        | BlockAction::Failed { item_name, .. } => item_name,
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
        _ => prompt_required("Host")?,
    };
    let port = match config.port {
        Some(port) if port > 0 => port,
        _ => prompt_with_default("Port", 22u16)?,
    };
    let user = match &config.user {
        Some(user) if !user.is_empty() => user.clone(),
        _ => prompt_with_default("User", "root".to_string())?,
    };
    let key_path = config.key.clone();
    let password = match &config.password {
        Some(secret) => {
            let resolved = secret.resolve()?;
            if resolved.is_empty() {
                None
            } else {
                Some(resolved)
            }
        }
        None => {
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

fn prompt_required(prompt: &str) -> Result<String> {
    if console::Term::stdout().is_term() {
        Ok(Input::new().with_prompt(prompt).interact_text()?)
    } else {
        anyhow::bail!("{prompt} prompt requires a terminal")
    }
}

fn prompt_with_default<T>(prompt: &str, default: T) -> Result<T>
where
    T: Clone + std::fmt::Display + std::str::FromStr + Send + Sync + 'static,
    <T as std::str::FromStr>::Err: std::fmt::Display + Send + Sync + 'static,
{
    if console::Term::stdout().is_term() {
        Ok(Input::new()
            .with_prompt(prompt)
            .default(default)
            .interact_text()?)
    } else {
        Ok(default)
    }
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
    use crate::cli::state::HostState;
    use crate::config::{FileItem, ItemKind, ProxyConfig, SyncMode};
    use crate::remote::fake::InMemoryRemote;
    use crate::reporter::memory::CapturedReporter;
    use crate::reporter::multi_host::MultiHostConsoleReporter;
    use futures::StreamExt;
    use tempfile::TempDir;

    fn minimal_config(items: Vec<FileItem>) -> Config {
        Config {
            version: 1,
            imports: vec![],
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
    async fn zero_max_concurrency_still_runs_pipeline() {
        let tmp = TempDir::new().unwrap();
        let src = tmp.path().join("a");
        std::fs::write(&src, b"hi").unwrap();
        let config = minimal_config(vec![FileItem {
            name: Some("a".into()),
            src: src.to_string_lossy().into_owned(),
            dst: ":/r/a".into(),
            kind: ItemKind::Auto,
            target: None,
            mode: SyncMode::Cover,
            chmod: None,
            tags: vec![],
        }]);
        let remote = InMemoryRemote::new();
        let reporter = CapturedReporter::new();
        let pipe = Pipeline {
            config: &config,
            asset_root: tmp.path(),
            remote: &remote,
            reporter: &reporter,
            opts: PipelineOpts {
                max_concurrency: 0,
                ..PipelineOpts::default()
            },
        };
        let summary = pipe.run().await;
        assert_eq!(summary.total_failed(), 0);
        assert_eq!(remote.file_contents("/r/a"), Some(b"hi".to_vec()));
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
            kind: ItemKind::Auto,
            target: None,
            mode: SyncMode::Cover,
            chmod: None,
            tags: vec![],
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
                diff: false,
                max_concurrency: 4,
                retry: RetryPolicy::default(),
                script_timeout: None,
                filter: PipelineFilter::default(),
                state: None,
                use_cache: true,
                resume_from: None,
                cancellation: None,
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
                kind: ItemKind::Auto,
                target: None,
                mode: SyncMode::Cover,
                chmod: None,
                tags: vec![],
            },
            FileItem {
                name: Some("good".into()),
                src: good.to_string_lossy().into_owned(),
                dst: ":/r/y".into(),
                kind: ItemKind::Auto,
                target: None,
                mode: SyncMode::Cover,
                chmod: None,
                tags: vec![],
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

    #[test]
    fn parse_pubkey_body_accepts_comment() {
        let parsed = parse_pubkey_body("ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAI comment").unwrap();
        assert_eq!(
            parsed,
            (
                "ssh-ed25519".to_string(),
                "AAAAC3NzaC1lZDI1NTE5AAAAI".to_string()
            )
        );
    }

    #[test]
    fn parse_pubkey_body_accepts_no_comment() {
        let parsed = parse_pubkey_body("ecdsa-sha2-nistp256 AAAAE2VjZHNhLXNoYTI=").unwrap();
        assert_eq!(
            parsed,
            (
                "ecdsa-sha2-nistp256".to_string(),
                "AAAAE2VjZHNhLXNoYTI=".to_string()
            )
        );
    }

    #[test]
    fn parse_pubkey_body_rejects_option_prefixed_line() {
        assert!(parse_pubkey_body(
            "command=\"echo hi\" ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAI comment"
        )
        .is_none());
    }

    #[test]
    fn interrupted_summary_uses_sigint_exit_code() {
        let summary = PipelineSummary {
            stages: vec![],
            interrupted: true,
            dry_run: false,
            first_failed_item: None,
        };
        assert_eq!(summary.exit_code(), 130);
    }

    #[tokio::test]
    async fn skip_stage_marks_all_as_filtered_out() {
        let tmp = TempDir::new().unwrap();
        let src = tmp.path().join("a");
        std::fs::write(&src, b"hi").unwrap();
        let config = minimal_config(vec![FileItem {
            name: Some("a".into()),
            src: src.to_string_lossy().into_owned(),
            dst: ":/r/a".into(),
            kind: ItemKind::Auto,
            target: None,
            mode: SyncMode::Cover,
            chmod: None,
            tags: vec!["dotfiles".into()],
        }]);
        let remote = InMemoryRemote::new();
        let reporter = CapturedReporter::new();
        let pipe = Pipeline {
            config: &config,
            asset_root: tmp.path(),
            remote: &remote,
            reporter: &reporter,
            opts: PipelineOpts {
                filter: PipelineFilter {
                    only_stages: None,
                    skip_stages: HashSet::from([Stage::File]),
                    only_items: None,
                    tags: None,
                },
                ..PipelineOpts::default()
            },
        };
        let plan = pipe.plan().await;
        assert!(matches!(
            &plan.file_actions[0],
            FileAction::Skip {
                reason: SkipReason::FilteredOut,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn only_item_keeps_only_named() {
        let tmp = TempDir::new().unwrap();
        let src1 = tmp.path().join("a");
        let src2 = tmp.path().join("b");
        std::fs::write(&src1, b"a").unwrap();
        std::fs::write(&src2, b"b").unwrap();
        let config = minimal_config(vec![
            FileItem {
                name: Some("keep".into()),
                src: src1.to_string_lossy().into_owned(),
                dst: ":/r/a".into(),
                kind: ItemKind::Auto,
                target: None,
                mode: SyncMode::Cover,
                chmod: None,
                tags: vec![],
            },
            FileItem {
                name: Some("drop".into()),
                src: src2.to_string_lossy().into_owned(),
                dst: ":/r/b".into(),
                kind: ItemKind::Auto,
                target: None,
                mode: SyncMode::Cover,
                chmod: None,
                tags: vec![],
            },
        ]);
        let remote = InMemoryRemote::new();
        let reporter = CapturedReporter::new();
        let pipe = Pipeline {
            config: &config,
            asset_root: tmp.path(),
            remote: &remote,
            reporter: &reporter,
            opts: PipelineOpts {
                filter: PipelineFilter {
                    only_stages: None,
                    skip_stages: HashSet::new(),
                    only_items: Some(HashSet::from([String::from("keep")])),
                    tags: None,
                },
                ..PipelineOpts::default()
            },
        };
        let plan = pipe.plan().await;
        assert!(matches!(&plan.file_actions[0], FileAction::Apply { .. }));
        assert!(matches!(
            &plan.file_actions[1],
            FileAction::Skip {
                reason: SkipReason::FilteredOut,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn tag_filter_intersects() {
        let tmp = TempDir::new().unwrap();
        let src = tmp.path().join("a");
        std::fs::write(&src, b"hi").unwrap();
        let config = minimal_config(vec![FileItem {
            name: Some("a".into()),
            src: src.to_string_lossy().into_owned(),
            dst: ":/r/a".into(),
            kind: ItemKind::Auto,
            target: None,
            mode: SyncMode::Cover,
            chmod: None,
            tags: vec!["dotfiles".into()],
        }]);
        let remote = InMemoryRemote::new();
        let reporter = CapturedReporter::new();
        let pipe = Pipeline {
            config: &config,
            asset_root: tmp.path(),
            remote: &remote,
            reporter: &reporter,
            opts: PipelineOpts {
                filter: PipelineFilter {
                    only_stages: None,
                    skip_stages: HashSet::new(),
                    only_items: None,
                    tags: Some(HashSet::from([String::from("dotfiles")])),
                },
                ..PipelineOpts::default()
            },
        };
        let plan = pipe.plan().await;
        assert!(matches!(&plan.file_actions[0], FileAction::Apply { .. }));
    }

    #[tokio::test]
    async fn cache_skip_avoids_remote_exists_calls() {
        let tmp = TempDir::new().unwrap();
        let src = tmp.path().join("a");
        std::fs::write(&src, b"hi").unwrap();
        let config = minimal_config(vec![FileItem {
            name: Some("a".into()),
            src: src.to_string_lossy().into_owned(),
            dst: ":/r/a".into(),
            kind: ItemKind::Auto,
            target: None,
            mode: SyncMode::Cover,
            chmod: None,
            tags: vec![],
        }]);
        let remote = InMemoryRemote::new();
        let reporter = CapturedReporter::new();
        let pipe = Pipeline {
            config: &config,
            asset_root: tmp.path(),
            remote: &remote,
            reporter: &reporter,
            opts: PipelineOpts {
                state: Some(HostState {
                    host: "h".into(),
                    last_sync_ts: 0,
                    item_hashes: crate::sync::file::collect_item_hashes(&config.file),
                    last_failed_item: None,
                }),
                use_cache: true,
                ..PipelineOpts::default()
            },
        };
        let plan = pipe.plan().await;
        assert!(matches!(
            &plan.file_actions[0],
            FileAction::Skip {
                reason: SkipReason::ContentUnchanged,
                ..
            }
        ));
        assert_eq!(remote.exists_calls().len(), 0);
        assert_eq!(remote.exec_calls().len(), 0);
    }

    #[tokio::test]
    async fn resume_only_runs_target_and_after() {
        let tmp = TempDir::new().unwrap();
        let src1 = tmp.path().join("a");
        let src2 = tmp.path().join("b");
        std::fs::write(&src1, b"a").unwrap();
        std::fs::write(&src2, b"b").unwrap();
        let config = minimal_config(vec![
            FileItem {
                name: Some("first".into()),
                src: src1.to_string_lossy().into_owned(),
                dst: ":/r/a".into(),
                kind: ItemKind::Auto,
                target: None,
                mode: SyncMode::Cover,
                chmod: None,
                tags: vec![],
            },
            FileItem {
                name: Some("second".into()),
                src: src2.to_string_lossy().into_owned(),
                dst: ":/r/b".into(),
                kind: ItemKind::Auto,
                target: None,
                mode: SyncMode::Cover,
                chmod: None,
                tags: vec![],
            },
        ]);
        let remote = InMemoryRemote::new();
        let reporter = CapturedReporter::new();
        let pipe = Pipeline {
            config: &config,
            asset_root: tmp.path(),
            remote: &remote,
            reporter: &reporter,
            opts: PipelineOpts {
                resume_from: Some("second".into()),
                ..PipelineOpts::default()
            },
        };
        let plan = pipe.plan().await;
        assert!(matches!(
            &plan.file_actions[0],
            FileAction::Skip {
                reason: SkipReason::PreviouslyApplied,
                ..
            }
        ));
        assert!(matches!(&plan.file_actions[1], FileAction::Apply { .. }));
    }

    #[tokio::test]
    async fn multi_host_fanout_counts_summaries() {
        let tmp = TempDir::new().unwrap();
        let src = tmp.path().join("a");
        std::fs::write(&src, b"hi").unwrap();
        let config = minimal_config(vec![FileItem {
            name: Some("a".into()),
            src: src.to_string_lossy().into_owned(),
            dst: ":/r/a".into(),
            kind: ItemKind::Auto,
            target: None,
            mode: SyncMode::Cover,
            chmod: None,
            tags: vec![],
        }]);
        let results = futures::stream::iter(["h1", "h2", "h3"].into_iter().map(|host| {
            let remote = InMemoryRemote::new();
            let reporter = MultiHostConsoleReporter::new(host);
            let config = config.clone();
            let asset_root = tmp.path().to_path_buf();
            async move {
                let pipe = Pipeline {
                    config: &config,
                    asset_root: &asset_root,
                    remote: &remote,
                    reporter: &reporter,
                    opts: PipelineOpts::default(),
                };
                pipe.run().await
            }
        }))
        .buffer_unordered(3)
        .collect::<Vec<_>>()
        .await;
        assert_eq!(results.len(), 3);
        assert_eq!(
            results
                .iter()
                .map(|summary| summary
                    .stages
                    .iter()
                    .map(|stage| stage.applied)
                    .sum::<usize>())
                .sum::<usize>(),
            3
        );
    }

    #[tokio::test]
    async fn first_ctrl_c_does_not_interrupt_pipeline_until_second_press() {
        let tmp = Box::leak(Box::new(TempDir::new().unwrap()));
        std::fs::write(tmp.path().join("s.sh"), b"#!/bin/sh\nsleep 30").unwrap();
        let mut config = minimal_config(vec![]);
        config.script.push(crate::config::ScriptItem {
            path: "s.sh".into(),
            interpreter: None,
            flags: None,
            args: vec![],
            tags: vec![],
        });
        let config = Box::leak(Box::new(config));
        let remote: &'static InMemoryRemote = Box::leak(Box::new(InMemoryRemote::new()));
        remote.set_interactive_wait_for_cancellation(true);
        let reporter: &'static CapturedReporter = Box::leak(Box::new(CapturedReporter::new()));
        let cancellation = SharedCancellation::new();
        let pipe = Pipeline {
            config,
            asset_root: tmp.path(),
            remote,
            reporter,
            opts: PipelineOpts {
                cancellation: Some(cancellation.clone()),
                ..PipelineOpts::default()
            },
        };

        let signal_cancellation = cancellation.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            signal_cancellation.press();
            tokio::time::sleep(Duration::from_millis(100)).await;
            signal_cancellation.press();
        });
        let task = pipe.run();
        tokio::pin!(task);
        assert!(tokio::time::timeout(Duration::from_millis(50), &mut task)
            .await
            .is_err());
        assert_eq!(remote.interactive_cancel_log(), vec![1]);

        let summary = task.await;
        assert!(summary.interrupted);
        assert_eq!(remote.interactive_cancel_log(), vec![1, 2]);
    }
}
