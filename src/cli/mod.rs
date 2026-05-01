//! CLI command implementations.

pub mod ssh_config;

use crate::config::Config;
use crate::remote::ssh::{SshClient, SshConfig};
use crate::reporter::console::ConsoleReporter;
use crate::reporter::Reporter;
use crate::sync::{Pipeline, PipelineOpts};
use anyhow::{Context, Result};
use dialoguer::Password;
use std::path::Path;

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
    dry_run: bool,
    max_concurrency: Option<usize>,
) -> Result<()> {
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
    if config.proxy.enabled && !dry_run {
        ssh.start_reverse_forward(config.proxy.local_port, config.proxy.remote_port)
            .await
            .context("starting reverse forward")?;
    }
    let reporter = ConsoleReporter::new();
    let pipeline = Pipeline {
        config: &config,
        asset_root: &asset_root,
        remote: &ssh,
        reporter: &reporter,
        opts: PipelineOpts {
            dry_run,
            max_concurrency: max_concurrency.unwrap_or(8),
        },
    };
    let summary = pipeline.run().await;
    let code = summary.exit_code();
    if code != 0 {
        std::process::exit(code);
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
