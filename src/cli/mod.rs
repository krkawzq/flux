//! CLI command implementations.

pub mod ssh_config;

use crate::audit;
use crate::config::Config;
use crate::remote::ssh::{SshClient, SshConfig};
use crate::remote::{RemoteOps, RetryPolicy};
use crate::reporter::console::ConsoleReporter;
use crate::reporter::{Reporter, Stage};
use crate::sync::{Pipeline, PipelineFilter, PipelineOpts};
use anyhow::{Context, Result};
use clap::ValueEnum;
use dialoguer::{Confirm, Password};
use std::collections::HashSet;
use std::path::Path;
use tracing::warn;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
pub enum LogFormat {
    #[default]
    Text,
    Json,
}

#[derive(Debug, Clone, Default)]
pub struct SyncRunOptions {
    pub dry_run: bool,
    pub diff: bool,
    pub log_format: LogFormat,
    pub max_concurrency: Option<usize>,
    pub retries: u8,
    pub script_timeout: Option<u64>,
    pub only_stage: Vec<String>,
    pub skip_stage: Vec<String>,
    pub only_item: Vec<String>,
    pub tag: Vec<String>,
}

pub fn init_tracing(format: LogFormat) {
    use tracing_subscriber::EnvFilter;

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = match format {
        LogFormat::Json => tracing_subscriber::fmt()
            .with_env_filter(filter)
            .json()
            .try_init(),
        LogFormat::Text => tracing_subscriber::fmt().with_env_filter(filter).try_init(),
    };
}

pub async fn run_init() -> Result<()> {
    let cwd = std::env::current_dir()?;
    let root = cwd.join(".flux");
    std::fs::create_dir_all(root.join("files"))?;
    std::fs::create_dir_all(root.join("scripts"))?;
    std::fs::create_dir_all(root.join("blocks"))?;
    println!(
        "initialized .flux/ in {}\n  - put YAML configs in .flux/<name>.yml\n  - assets in files/, scripts/, blocks/",
        cwd.display()
    );
    Ok(())
}

pub async fn run_sync(
    name_or_path: &str,
    save: Option<String>,
    opts: SyncRunOptions,
) -> Result<()> {
    init_tracing(opts.log_format);
    validate_sync_options(&opts)?;

    let (mut config, config_path) =
        Config::find_and_load(name_or_path).context("loading config")?;
    config.validate().context("validating config")?;
    let asset_root = config.resolve_root(&config_path);
    resolve_config_paths(&mut config, &asset_root);
    if let Some(name) = save {
        ssh_config::save_ssh_config(&name, &config).context("saving ssh config")?;
    }

    let ssh_config = resolve_ssh_config(&config)?;
    let mut ssh = SshClient::connect(&ssh_config)
        .await
        .context("ssh connect")?;
    if config.proxy.enabled && !opts.dry_run {
        ssh.start_reverse_forward(config.proxy.local_port, config.proxy.remote_port)
            .await
            .context("starting reverse forward")?;
    }

    let reporter = ConsoleReporter::new();
    let started = std::time::Instant::now();
    let pipeline = Pipeline {
        config: &config,
        asset_root: &asset_root,
        remote: &ssh,
        reporter: &reporter,
        opts: PipelineOpts {
            dry_run: opts.dry_run,
            diff: opts.diff,
            max_concurrency: opts.max_concurrency.unwrap_or(8),
            retry: RetryPolicy {
                max_attempts: opts.retries.max(1),
                base_backoff: std::time::Duration::from_millis(200),
            },
            script_timeout: opts.script_timeout.map(std::time::Duration::from_secs),
            filter: build_pipeline_filter(&opts)?,
        },
    };
    let summary = pipeline.run().await;
    if let Err(err) = audit::append(
        &ssh_config.host,
        name_or_path,
        started.elapsed().as_millis(),
        &summary,
    ) {
        warn!("failed to append audit log: {err}");
    }
    let code = summary.exit_code();
    ssh.close().await.ok();
    if code != 0 {
        std::process::exit(code);
    }
    Ok(())
}

pub async fn run_undo(name_or_path: &str, yes: bool, log_format: LogFormat) -> Result<()> {
    init_tracing(log_format);

    let (config, _) = Config::find_and_load(name_or_path).context("loading config")?;
    config.validate().context("validating config")?;
    let ssh_config = resolve_ssh_config(&config)?;
    let ssh = SshClient::connect(&ssh_config)
        .await
        .context("ssh connect")?;
    let reporter = ConsoleReporter::new();

    let mut targets: Vec<String> = config
        .file
        .iter()
        .map(|item| item.dst.trim_start_matches(':').to_string())
        .collect();
    targets.extend(
        config
            .block
            .iter()
            .map(|item| item.file.trim_start_matches(':').to_string()),
    );
    targets.sort();
    targets.dedup();

    let mut restores = Vec::new();
    for target in targets {
        if let Some(backup) = latest_backup_for_target(&ssh, &target).await? {
            restores.push((target, backup));
        }
    }
    if restores.is_empty() {
        reporter.info("no backup files found");
        ssh.close().await.ok();
        return Ok(());
    }

    reporter.info(&format!("will restore {} file(s):", restores.len()));
    for (target, backup) in &restores {
        reporter.info(&format!("  {backup} -> {target}"));
    }

    if !yes
        && !Confirm::new()
            .with_prompt("Proceed with restore?")
            .default(false)
            .interact()?
    {
        ssh.close().await.ok();
        return Ok(());
    }

    for (target, backup) in restores {
        ssh.rename(&backup, &target).await?;
    }
    ssh.close().await.ok();
    Ok(())
}

pub async fn run_proxy(
    host: String,
    local: u16,
    remote: u16,
    key: Option<String>,
    retry: u64,
) -> Result<()> {
    let reporter = ConsoleReporter::new();
    use std::net::TcpStream as StdTcpStream;

    if StdTcpStream::connect(format!("127.0.0.1:{local}")).is_err() {
        reporter.warning(&format!(
            "Local port {} is not listening (no proxy service?)",
            local
        ));
    }

    let (user, hostname, port, key_from_config) = parse_ssh_host_with_config(&host)?;
    let key_path = key.or(key_from_config).or_else(find_default_key);

    reporter.info(&format!("Remote: {}@{}:{}", user, hostname, port));
    reporter.info(&format!("Tunnel: remote:{} <- local:{}", remote, local));
    if let Some(ref key_path) = key_path {
        reporter.info(&format!("Key: {key_path}"));
    }

    let mut retry_count = 0u32;
    let mut cached_password: Option<String> = None;
    loop {
        retry_count += 1;
        if retry_count > 1 {
            reporter.info(&format!("Reconnecting (attempt {})...", retry_count));
        } else {
            reporter.info("Connecting...");
        }

        let password = if key_path.is_none() && cached_password.is_none() {
            match Password::new().with_prompt("Password").interact() {
                Ok(password) => {
                    cached_password = Some(password.clone());
                    Some(password)
                }
                Err(_) => None,
            }
        } else {
            cached_password.clone()
        };

        let ssh_config = SshConfig {
            host: hostname.clone(),
            port,
            user: user.clone(),
            key_path: key_path.clone(),
            password,
        };

        match SshClient::connect(&ssh_config).await {
            Ok(mut client) => match client.start_reverse_forward(local, remote).await {
                Ok(()) => {
                    reporter.info("Press Ctrl+C to stop");
                    loop {
                        tokio::select! {
                            _ = tokio::signal::ctrl_c() => {
                                reporter.info("Interrupted, closing...");
                                client.close().await.ok();
                                return Ok(());
                            }
                            _ = tokio::time::sleep(std::time::Duration::from_secs(30)) => {}
                        }
                    }
                }
                Err(err) => {
                    eprintln!("failed to setup tunnel: {err}");
                    client.close().await.ok();
                }
            },
            Err(err) => eprintln!("connection failed: {err}"),
        }

        if retry == 0 {
            reporter.info("Retry disabled, exiting");
            return Ok(());
        }
        reporter.info(&format!("Retrying in {} seconds...", retry));
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                reporter.info("Interrupted, exiting");
                return Ok(());
            }
            _ = tokio::time::sleep(std::time::Duration::from_secs(retry)) => {}
        }
    }
}

fn validate_sync_options(opts: &SyncRunOptions) -> Result<()> {
    if !opts.only_stage.is_empty() && !opts.skip_stage.is_empty() {
        anyhow::bail!("--only-stage and --skip-stage cannot be used together");
    }
    if opts.diff && !opts.dry_run {
        anyhow::bail!("--diff requires --dry-run");
    }
    Ok(())
}

fn build_pipeline_filter(opts: &SyncRunOptions) -> Result<PipelineFilter> {
    Ok(PipelineFilter {
        only_stages: if opts.only_stage.is_empty() {
            None
        } else {
            Some(
                opts.only_stage
                    .iter()
                    .map(|value| parse_stage(value))
                    .collect::<Result<HashSet<_>>>()?,
            )
        },
        skip_stages: opts
            .skip_stage
            .iter()
            .map(|value| parse_stage(value))
            .collect::<Result<HashSet<_>>>()?,
        only_items: if opts.only_item.is_empty() {
            None
        } else {
            Some(opts.only_item.iter().cloned().collect())
        },
        tags: if opts.tag.is_empty() {
            None
        } else {
            Some(opts.tag.iter().cloned().collect())
        },
    })
}

fn parse_stage(value: &str) -> Result<Stage> {
    match value {
        "file" => Ok(Stage::File),
        "script" => Ok(Stage::Script),
        "block" => Ok(Stage::Block),
        "pubkey" => Ok(Stage::Pubkey),
        _ => anyhow::bail!("unknown stage '{value}'"),
    }
}

async fn latest_backup_for_target(ssh: &SshClient, target: &str) -> Result<Option<String>> {
    let quoted = shell_quote(target);
    let pattern = format!("\"$(dirname {quoted})/$(basename {quoted})\".flux-*.bak");
    let out = ssh
        .exec(&format!("ls -1 {pattern} 2>/dev/null || true"))
        .await
        .context("listing remote backups")?;
    Ok(out
        .stdout
        .lines()
        .filter_map(|line| backup_timestamp(line.trim()).map(|ts| (ts, line.trim().to_string())))
        .max_by_key(|(ts, _)| *ts)
        .map(|(_, path)| path))
}

fn backup_timestamp(path: &str) -> Option<i64> {
    let marker = ".flux-";
    let start = path.rfind(marker)? + marker.len();
    let end = path[start..].find(".bak")? + start;
    path[start..end].parse().ok()
}

fn shell_quote(input: &str) -> String {
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

fn resolve_ssh_config(config: &Config) -> Result<SshConfig> {
    let host = match &config.host {
        Some(host) if !host.is_empty() => host.clone(),
        _ => dialoguer::Input::new()
            .with_prompt("Host")
            .interact_text()?,
    };
    let port = match config.port {
        Some(port) if port > 0 => port,
        _ => dialoguer::Input::new()
            .with_prompt("Port")
            .default(22u16)
            .interact_text()?,
    };
    let user = match &config.user {
        Some(user) if !user.is_empty() => user.clone(),
        _ => dialoguer::Input::new()
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

fn resolve_config_paths(config: &mut Config, asset_root: &Path) {
    let resolve_local = |path: &str, subdir: &str| -> String {
        if path.starts_with(':') || path.starts_with('/') || path.starts_with('~') {
            return path.to_string();
        }
        if path.contains('/') || path.contains('\\') {
            let full_path = asset_root.join(path);
            if full_path.exists() {
                return full_path.to_string_lossy().to_string();
            }
            return path.to_string();
        }
        let subdir_path = asset_root.join(subdir).join(path);
        if subdir_path.exists() {
            return subdir_path.to_string_lossy().to_string();
        }
        let direct_path = asset_root.join(path);
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
}

fn parse_ssh_host_with_config(input: &str) -> Result<(String, String, u16, Option<String>)> {
    if let Some((hostname, user, port, identity)) = ssh_config::read_ssh_config_entry(input)? {
        return Ok((
            user.unwrap_or_else(|| "root".into()),
            hostname,
            port,
            identity,
        ));
    }
    let (user, host, port) = ssh_config::parse_ssh_host(input)?;
    Ok((user.unwrap_or_else(|| "root".into()), host, port, None))
}

fn find_default_key() -> Option<String> {
    let home = dirs::home_dir()?;
    let ssh_dir = home.join(".ssh");
    for name in ["id_ed25519", "id_rsa", "id_ecdsa"] {
        let key_path = ssh_dir.join(name);
        if key_path.exists() {
            return Some(key_path.to_string_lossy().to_string());
        }
    }
    None
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backup_timestamp_extracts_numeric_suffix() {
        assert_eq!(
            backup_timestamp("/r/a.txt.flux-1700000000.bak"),
            Some(1_700_000_000)
        );
        assert_eq!(backup_timestamp("/r/a.txt"), None);
    }

    #[test]
    fn validate_sync_options_rejects_only_and_skip_stage() {
        let err = validate_sync_options(&SyncRunOptions {
            only_stage: vec!["file".into()],
            skip_stage: vec!["block".into()],
            ..SyncRunOptions::default()
        })
        .unwrap_err();
        assert!(err.to_string().contains("--only-stage and --skip-stage"));
    }
}
