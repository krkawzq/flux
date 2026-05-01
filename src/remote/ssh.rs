//! SSH client module
//!
//! Handles SSH connections, authentication, command execution, file transfer,
//! and reverse port forwarding.

use crate::remote::{ExecOutput, RemoteOps, RemoteOpsError};
use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};
use russh::keys::{load_secret_key, PrivateKeyWithHashAlg, PublicKey};
use russh::{client, Channel, ChannelMsg, Disconnect};
use std::io::Cursor;
use std::path::Path;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::Mutex;

/// SSH client wrapper
pub struct SshClient {
    session: client::Handle<ClientHandler>,
    host: String,
    user: String,
    handler_state: Arc<Mutex<HandlerState>>,
}

/// Shared state for the client handler
struct HandlerState {
    local_proxy_port: Option<u16>,
}

/// Client handler for russh
struct ClientHandler {
    state: Arc<Mutex<HandlerState>>,
}

impl client::Handler for ClientHandler {
    type Error = anyhow::Error;

    async fn check_server_key(
        &mut self,
        _server_public_key: &PublicKey,
    ) -> Result<bool, Self::Error> {
        // TODO: implement strict host key verification once host key policy is defined.
        // Accept all server keys (like ssh -o StrictHostKeyChecking=no)
        Ok(true)
    }

    /// Handle forwarded TCP/IP connection (reverse port forwarding)
    async fn server_channel_open_forwarded_tcpip(
        &mut self,
        channel: Channel<client::Msg>,
        _connected_address: &str,
        _connected_port: u32,
        _originator_address: &str,
        _originator_port: u32,
        _session: &mut client::Session,
    ) -> Result<(), Self::Error> {
        let state = self.state.lock().await;
        let local_port = state.local_proxy_port.unwrap_or(7899);
        drop(state);

        // Spawn a task to handle this forwarded connection
        tokio::spawn(async move {
            if let Err(e) = handle_forwarded_connection(channel, local_port).await {
                eprintln!("Forwarded connection error: {}", e);
            }
        });

        Ok(())
    }
}

/// Handle a single forwarded connection with full bidirectional data transfer
async fn handle_forwarded_connection(
    mut channel: Channel<client::Msg>,
    local_port: u16,
) -> Result<()> {
    use tokio::sync::mpsc;

    // Connect to local proxy
    let local_stream = TcpStream::connect(format!("127.0.0.1:{}", local_port))
        .await
        .context(format!(
            "Failed to connect to local proxy on port {}",
            local_port
        ))?;

    let (mut local_read, mut local_write) = local_stream.into_split();

    // Channel for sending data from local to remote
    let (tx, mut rx) = mpsc::channel::<Vec<u8>>(32);

    // Task: local -> mpsc (read from local, queue for remote)
    tokio::spawn(async move {
        let mut buf = [0u8; 8192];
        loop {
            match local_read.read(&mut buf).await {
                Ok(0) => break, // EOF
                Ok(n) => {
                    if tx.send(buf[..n].to_vec()).await.is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    // Main loop: handle both directions
    loop {
        tokio::select! {
            // Data from local (via mpsc) -> send to remote
            Some(data) = rx.recv() => {
                if channel.data(Cursor::new(data)).await.is_err() {
                    break;
                }
            }
            // Data from remote -> send to local
            msg = channel.wait() => {
                match msg {
                    Some(ChannelMsg::Data { data }) => {
                        if local_write.write_all(&data).await.is_err() {
                            break;
                        }
                    }
                    Some(ChannelMsg::Eof) | None => break,
                    _ => {}
                }
            }
        }
    }

    channel.close().await.ok();
    Ok(())
}

/// SSH connection configuration
pub struct SshConfig {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub key_path: Option<String>,
    pub password: Option<String>,
}

impl SshClient {
    /// Connect to SSH server
    pub async fn connect(config: &SshConfig) -> Result<Self> {
        let ssh_config = client::Config::default();
        let handler_state = Arc::new(Mutex::new(HandlerState {
            local_proxy_port: None,
        }));
        let handler = ClientHandler {
            state: handler_state.clone(),
        };

        let mut session = client::connect(
            Arc::new(ssh_config),
            (config.host.as_str(), config.port),
            handler,
        )
        .await
        .context("Failed to connect to SSH server")?;

        // Try key authentication first
        let authenticated = if let Some(key_path) = &config.key_path {
            let key_path = expand_tilde(key_path);
            if Path::new(&key_path).exists() {
                match Self::auth_with_key(&mut session, &config.user, &key_path).await {
                    Ok(true) => true,
                    Ok(false) | Err(_) => {
                        // Fall back to password
                        if let Some(password) = &config.password {
                            Self::auth_with_password(&mut session, &config.user, password).await?
                        } else {
                            false
                        }
                    }
                }
            } else if let Some(password) = &config.password {
                Self::auth_with_password(&mut session, &config.user, password).await?
            } else {
                false
            }
        } else if let Some(password) = &config.password {
            Self::auth_with_password(&mut session, &config.user, password).await?
        } else {
            false
        };

        if !authenticated {
            anyhow::bail!("SSH authentication failed");
        }

        Ok(Self {
            session,
            host: config.host.clone(),
            user: config.user.clone(),
            handler_state,
        })
    }

    /// Authenticate with private key
    async fn auth_with_key(
        session: &mut client::Handle<ClientHandler>,
        user: &str,
        key_path: &str,
    ) -> Result<bool> {
        let key_pair = load_secret_key(key_path, None)
            .context(format!("Failed to load private key: {}", key_path))?;
        let key_pair = PrivateKeyWithHashAlg::new(Arc::new(key_pair), None);

        let result = session
            .authenticate_publickey(user, key_pair)
            .await
            .context("Key authentication failed")?;

        Ok(result.success())
    }

    /// Authenticate with password
    async fn auth_with_password(
        session: &mut client::Handle<ClientHandler>,
        user: &str,
        password: &str,
    ) -> Result<bool> {
        let result = session
            .authenticate_password(user, password)
            .await
            .context("Password authentication failed")?;

        Ok(result.success())
    }

    /// Start reverse port forwarding
    ///
    /// Remote server will listen on `remote_port` and forward to local `local_port`.
    pub async fn start_reverse_forward(&mut self, local_port: u16, remote_port: u16) -> Result<()> {
        // Store local port in handler state
        {
            let mut state = self.handler_state.lock().await;
            state.local_proxy_port = Some(local_port);
        }

        // Request loopback-only port forwarding to match the user-facing safety expectation.
        // russh returns Ok(_) on success, Err on failure
        // The returned port number may be 0 when using a fixed port (not dynamic)
        self.session
            .tcpip_forward("127.0.0.1", remote_port as u32)
            .await
            .context(format!(
                "Port forwarding failed for port {}. \
                 Check server's sshd_config: GatewayPorts yes, AllowTcpForwarding yes",
                remote_port
            ))?;

        Ok(())
    }

    /// Execute a command on the remote server
    pub async fn exec_command(&self, command: &str) -> Result<ExecResult> {
        let mut channel = self
            .session
            .channel_open_session()
            .await
            .context("Failed to open SSH channel")?;

        channel
            .exec(true, command)
            .await
            .context("Failed to execute command")?;

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut exit_code = 0;

        loop {
            let msg = channel.wait().await;
            match msg {
                Some(ChannelMsg::Data { data }) => {
                    stdout.extend_from_slice(&data);
                }
                Some(ChannelMsg::ExtendedData { data, ext }) => {
                    if ext == 1 {
                        stderr.extend_from_slice(&data);
                    }
                }
                Some(ChannelMsg::ExitStatus { exit_status }) => {
                    exit_code = exit_status;
                }
                Some(ChannelMsg::Eof) | None => break,
                _ => {}
            }
        }

        channel.close().await.ok();

        Ok(ExecResult {
            exit_code,
            stdout: String::from_utf8_lossy(&stdout).to_string(),
            stderr: String::from_utf8_lossy(&stderr).to_string(),
        })
    }

    /// Compatibility wrapper while the rest of the codebase is still on the
    /// pre-Phase-2 API surface.
    pub async fn exec(&self, command: &str) -> Result<ExecResult> {
        self.exec_command(command).await
    }

    /// Execute command with stdin/stdout streaming (with PTY)
    pub async fn exec_interactive(&self, command: &str) -> Result<i32> {
        let mut channel = self
            .session
            .channel_open_session()
            .await
            .context("Failed to open SSH channel")?;

        // Request PTY for interactive commands
        channel
            .request_pty(false, "xterm", 80, 24, 0, 0, &[])
            .await
            .context("Failed to request PTY")?;

        channel
            .exec(true, command)
            .await
            .context("Failed to execute command")?;

        let mut stdin_writer = channel.make_writer();
        let stdin_task = tokio::spawn(async move {
            let mut stdin = tokio::io::stdin();
            tokio::io::copy(&mut stdin, &mut stdin_writer).await?;
            stdin_writer.shutdown().await
        });

        let mut exit_code = 0;

        loop {
            let msg = channel.wait().await;
            match msg {
                Some(ChannelMsg::Data { data }) => {
                    tokio::io::stdout().write_all(&data).await?;
                    tokio::io::stdout().flush().await?;
                }
                Some(ChannelMsg::ExtendedData { data, ext }) => {
                    if ext == 1 {
                        tokio::io::stderr().write_all(&data).await?;
                        tokio::io::stderr().flush().await?;
                    }
                }
                Some(ChannelMsg::ExitStatus { exit_status }) => {
                    exit_code = exit_status as i32;
                }
                Some(ChannelMsg::Eof) | None => break,
                _ => {}
            }
        }

        if !stdin_task.is_finished() {
            stdin_task.abort();
        }

        match stdin_task.await {
            Ok(Ok(())) => {}
            Err(e) if e.is_cancelled() => {}
            Ok(Err(e)) => return Err(e.into()),
            Err(e) => return Err(anyhow::Error::new(e).context("stdin forwarding task failed")),
        }

        channel.close().await.ok();

        Ok(exit_code)
    }

    /// Upload a file to the remote server
    pub async fn upload_file(&self, local_path: &Path, remote_path: &str) -> Result<()> {
        let content = tokio::fs::read(local_path).await.context(format!(
            "Failed to read local file: {}",
            local_path.display()
        ))?;

        self.write_remote_file(remote_path, &content).await
    }

    /// Write content to a remote file
    pub async fn write_remote_file(&self, remote_path: &str, content: &[u8]) -> Result<()> {
        // Use cat to write file content
        let escaped_path = shell_escape(remote_path);
        let command = format!(
            "mkdir -p \"$(dirname {})\" && cat > {}",
            escaped_path, escaped_path
        );

        let mut channel = self
            .session
            .channel_open_session()
            .await
            .context("Failed to open SSH channel")?;

        channel
            .exec(true, command.as_str())
            .await
            .context("Failed to start file upload")?;

        channel
            .data(Cursor::new(content))
            .await
            .context("Failed to send file data")?;

        channel.eof().await.context("Failed to send EOF")?;

        let mut exit_code = 0;
        loop {
            let msg = channel.wait().await;
            match msg {
                Some(ChannelMsg::ExitStatus { exit_status }) => {
                    exit_code = exit_status;
                }
                Some(ChannelMsg::Eof) | None => break,
                _ => {}
            }
        }

        channel.close().await.ok();

        if exit_code != 0 {
            anyhow::bail!("Failed to write remote file: {}", remote_path);
        }

        Ok(())
    }

    /// Read a remote file
    pub async fn read_remote_file(&self, remote_path: &str) -> Result<Vec<u8>> {
        let escaped_path = shell_escape(remote_path);
        let result = self.exec_command(&format!("cat {}", escaped_path)).await?;

        if result.exit_code != 0 {
            anyhow::bail!("Failed to read remote file: {}", result.stderr);
        }

        Ok(result.stdout.into_bytes())
    }

    /// Check if a remote file exists
    pub async fn file_exists(&self, remote_path: &str) -> Result<bool> {
        let escaped_path = shell_escape(remote_path);
        let result = self.exec_command(&format!("test -f {}", escaped_path)).await?;
        Ok(result.exit_code == 0)
    }

    /// Get remote file modification time (unix timestamp)
    pub async fn get_mtime(&self, remote_path: &str) -> Result<Option<i64>> {
        let escaped_path = shell_escape(remote_path);
        let result = self
            .exec_command(&format!(
                "stat -c %Y {} 2>/dev/null || stat -f %m {} 2>/dev/null",
                escaped_path, escaped_path
            ))
            .await?;

        if result.exit_code != 0 {
            return Ok(None);
        }

        let mtime = result.stdout.trim().parse::<i64>().ok();
        Ok(mtime)
    }

    /// Set file permissions on remote
    pub async fn chmod_remote(&self, remote_path: &str, mode: &str) -> Result<()> {
        let escaped_path = shell_escape(remote_path);
        let result = self
            .exec_command(&format!("chmod {} {}", mode, escaped_path))
            .await?;

        if result.exit_code != 0 {
            anyhow::bail!("Failed to chmod: {}", result.stderr);
        }

        Ok(())
    }

    /// Compatibility wrapper while callers migrate to `RemoteOps::chmod`.
    pub async fn chmod(&self, remote_path: &str, mode: &str) -> Result<()> {
        self.chmod_remote(remote_path, mode).await
    }

    /// Get the remote home directory
    pub async fn home_dir(&self) -> Result<String> {
        let result = self.exec_command("echo $HOME").await?;
        Ok(result.stdout.trim().to_string())
    }

    /// Expand ~ in remote path
    pub async fn expand_remote_path(&self, path: &str) -> Result<String> {
        if path.starts_with("~/") {
            let home = self.home_dir().await?;
            Ok(path.replacen("~", &home, 1))
        } else if path == "~" {
            self.home_dir().await
        } else {
            Ok(path.to_string())
        }
    }

    /// Close the SSH connection
    pub async fn close(self) -> Result<()> {
        self.session
            .disconnect(Disconnect::ByApplication, "", "en")
            .await?;
        Ok(())
    }

    /// Get host
    #[allow(dead_code)]
    pub fn host(&self) -> &str {
        &self.host
    }

    /// Get user
    #[allow(dead_code)]
    pub fn user(&self) -> &str {
        &self.user
    }
}

/// Result of command execution
#[derive(Debug)]
pub struct ExecResult {
    pub exit_code: u32,
    pub stdout: String,
    pub stderr: String,
}

/// Expand ~ to home directory in path
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

/// Escape shell special characters
fn shell_escape(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

#[async_trait]
impl RemoteOps for SshClient {
    async fn exec(&self, cmd: &str) -> Result<ExecOutput, RemoteOpsError> {
        let out = self
            .exec_command(cmd)
            .await
            .map_err(|e| RemoteOpsError::Transport(e.to_string()))?;
        Ok(ExecOutput {
            status: out.exit_code as i32,
            stdout: out.stdout.into_bytes(),
            stderr: out.stderr.into_bytes(),
        })
    }

    async fn read_file(&self, path: &str) -> Result<Vec<u8>, RemoteOpsError> {
        self.read_remote_file(path)
            .await
            .map_err(map_anyhow)
    }

    async fn write_file(&self, path: &str, data: &[u8]) -> Result<(), RemoteOpsError> {
        self.write_remote_file(path, data).await.map_err(map_anyhow)
    }

    async fn exists(&self, path: &str) -> Result<bool, RemoteOpsError> {
        self.file_exists(path).await.map_err(map_anyhow)
    }

    async fn mtime(&self, path: &str) -> Result<DateTime<Utc>, RemoteOpsError> {
        let secs = self
            .get_mtime(path)
            .await
            .map_err(map_anyhow)?
            .ok_or_else(|| RemoteOpsError::NotFound(path.to_string()))?;
        Utc.timestamp_opt(secs, 0)
            .single()
            .ok_or_else(|| RemoteOpsError::Encoding(format!("invalid mtime {secs}")))
    }

    async fn chmod(&self, path: &str, mode: u32) -> Result<(), RemoteOpsError> {
        self.chmod_remote(path, &format!("{mode:o}"))
            .await
            .map_err(map_anyhow)
    }

    async fn ensure_dir(&self, path: &str) -> Result<(), RemoteOpsError> {
        let out = self
            .exec_command(&format!("mkdir -p {}", shell_escape(path)))
            .await
            .map_err(map_anyhow)?;
        if out.exit_code == 0 {
            Ok(())
        } else {
            Err(RemoteOpsError::NonZeroExit {
                status: out.exit_code as i32,
                stderr: out.stderr,
            })
        }
    }

    async fn interactive_exec(&self, cmd: &str) -> Result<i32, RemoteOpsError> {
        self.exec_interactive(cmd).await.map_err(map_anyhow)
    }
}

fn map_anyhow(error: anyhow::Error) -> RemoteOpsError {
    RemoteOpsError::Io(error.to_string())
}
