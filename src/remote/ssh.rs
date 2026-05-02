//! SSH client module
//!
//! Handles SSH connections, authentication, command execution, file transfer,
//! and reverse port forwarding.

use crate::remote::{ExecOutput, RemoteOps, RemoteOpsError, SharedCancellation};
use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};
use console::Term;
use futures::future::pending;
use russh::keys::{load_secret_key, PrivateKeyWithHashAlg, PublicKey};
use russh::{client, Channel, ChannelMsg, Disconnect, Sig};
use std::io::Cursor;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

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

struct AbortOnDrop {
    handle: Option<JoinHandle<()>>,
}

impl AbortOnDrop {
    fn new(handle: JoinHandle<()>) -> Self {
        Self {
            handle: Some(handle),
        }
    }
}

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            handle.abort();
        }
    }
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
    pub async fn exec_interactive(
        &self,
        command: &str,
        timeout: Option<Duration>,
        cancellation: Option<&SharedCancellation>,
    ) -> Result<i32> {
        let (rows, cols) = Term::stdout().size_checked().unwrap_or((24, 80));
        let mut channel = self
            .session
            .channel_open_session()
            .await
            .context("Failed to open SSH channel")?;

        channel
            .request_pty(false, "xterm-256color", cols as u32, rows as u32, 0, 0, &[])
            .await
            .context("Failed to request PTY")?;

        channel
            .exec(true, command)
            .await
            .context("Failed to execute command")?;

        let mut stdin = tokio::io::stdin();
        let mut stdout = tokio::io::stdout();
        let mut stderr = tokio::io::stderr();
        let mut stdin_buf = [0u8; 8192];
        let mut stdin_closed = false;
        let mut exit_code = None;
        let mut first_ctrl_c = None::<Instant>;
        let active_cancellation;
        let _signal_task = if let Some(cancellation) = cancellation {
            active_cancellation = cancellation.clone();
            None
        } else {
            active_cancellation = SharedCancellation::new();
            let state = active_cancellation.clone();
            Some(AbortOnDrop::new(tokio::spawn(async move {
                while tokio::signal::ctrl_c().await.is_ok() {
                    state.press();
                }
            })))
        };
        let mut seen_presses = active_cancellation.presses();
        let timeout_label = timeout;
        let mut timeout_fut = Box::pin(async move {
            if let Some(duration) = timeout {
                tokio::time::sleep(duration).await;
            } else {
                pending::<()>().await;
            }
        });
        #[cfg(unix)]
        {
            let mut winch =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::window_change()).ok();
            loop {
                tokio::select! {
                    read = stdin.read(&mut stdin_buf), if !stdin_closed => {
                        match read {
                            Ok(0) => {
                                stdin_closed = true;
                                channel.eof().await.ok();
                            }
                            Ok(n) => {
                                channel
                                    .data(&stdin_buf[..n])
                                    .await
                                    .context("Failed to forward stdin to remote PTY")?;
                            }
                            Err(err) => return Err(err).context("Failed to read local stdin"),
                        }
                    }
                    msg = channel.wait() => {
                        match msg {
                            Some(ChannelMsg::Data { data }) => {
                                stdout.write_all(&data).await?;
                                stdout.flush().await?;
                            }
                            Some(ChannelMsg::ExtendedData { data, ext: 1 }) => {
                                stderr.write_all(&data).await?;
                                stderr.flush().await?;
                            }
                            Some(ChannelMsg::ExitStatus { exit_status }) => {
                                exit_code = Some(exit_status as i32);
                                break;
                            }
                            Some(ChannelMsg::Close) | Some(ChannelMsg::Eof) | None => break,
                            _ => {}
                        }
                    }
                    presses = active_cancellation.wait_for_change(seen_presses) => {
                        seen_presses = presses;
                        let now = Instant::now();
                        let second_press = first_ctrl_c
                            .map(|first| now.duration_since(first) <= Duration::from_secs(5))
                            .unwrap_or(false);
                        if second_press {
                            channel.close().await.ok();
                        } else {
                            channel.signal(Sig::INT).await.ok();
                            first_ctrl_c = Some(now);
                        }
                    }
                    _ = &mut timeout_fut => {
                        channel.close().await.ok();
                        let duration = timeout_label.expect("timeout future only resolves when timeout is set");
                        return Err(anyhow::anyhow!("interactive_exec timed out after {duration:?}"));
                    }
                    _ = async {
                        match &mut winch {
                            Some(signal) => {
                                signal.recv().await;
                            }
                            None => pending::<()>().await,
                        }
                    } => {
                        let (rows, cols) = Term::stdout().size_checked().unwrap_or((24, 80));
                        channel.window_change(cols as u32, rows as u32, 0, 0).await.ok();
                    }
                }
            }
        }

        #[cfg(not(unix))]
        loop {
            tokio::select! {
                read = stdin.read(&mut stdin_buf), if !stdin_closed => {
                    match read {
                        Ok(0) => {
                            stdin_closed = true;
                            channel.eof().await.ok();
                        }
                        Ok(n) => {
                            channel
                                .data(&stdin_buf[..n])
                                .await
                                .context("Failed to forward stdin to remote PTY")?;
                        }
                        Err(err) => return Err(err).context("Failed to read local stdin"),
                    }
                }
                msg = channel.wait() => {
                    match msg {
                        Some(ChannelMsg::Data { data }) => {
                            stdout.write_all(&data).await?;
                            stdout.flush().await?;
                        }
                        Some(ChannelMsg::ExtendedData { data, ext: 1 }) => {
                            stderr.write_all(&data).await?;
                            stderr.flush().await?;
                        }
                        Some(ChannelMsg::ExitStatus { exit_status }) => {
                            exit_code = Some(exit_status as i32);
                            break;
                        }
                        Some(ChannelMsg::Close) | Some(ChannelMsg::Eof) | None => break,
                        _ => {}
                    }
                }
                presses = active_cancellation.wait_for_change(seen_presses) => {
                    seen_presses = presses;
                    let now = Instant::now();
                    let second_press = first_ctrl_c
                        .map(|first| now.duration_since(first) <= Duration::from_secs(5))
                        .unwrap_or(false);
                    if second_press {
                        channel.close().await.ok();
                    } else {
                        channel.signal(Sig::INT).await.ok();
                        first_ctrl_c = Some(now);
                    }
                }
                _ = &mut timeout_fut => {
                    channel.close().await.ok();
                    let duration = timeout_label.expect("timeout future only resolves when timeout is set");
                    return Err(anyhow::anyhow!("interactive_exec timed out after {duration:?}"));
                }
            }
        }

        channel.close().await.ok();

        Ok(exit_code.unwrap_or(0))
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
        let result = self
            .exec_command(&format!("test -f {}", escaped_path))
            .await?;
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
pub fn shell_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        if c == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
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
        self.read_remote_file(path).await.map_err(map_anyhow)
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

    async fn stat_mode(&self, path: &str) -> Result<u32, RemoteOpsError> {
        let cmd = format!(
            "stat -c %a {0} 2>/dev/null || stat -f %Lp {0}",
            shell_escape(path)
        );
        let out = self.exec_command(&cmd).await.map_err(map_anyhow)?;
        if out.exit_code != 0 {
            return Err(RemoteOpsError::NonZeroExit {
                status: out.exit_code as i32,
                stderr: out.stderr,
            });
        }
        let trimmed = out.stdout.trim();
        u32::from_str_radix(trimmed, 8)
            .map_err(|e| RemoteOpsError::Encoding(format!("stat_mode parse '{trimmed}': {e}")))
    }

    async fn chmod(&self, path: &str, mode: u32) -> Result<(), RemoteOpsError> {
        self.chmod_remote(path, &format!("{mode:o}"))
            .await
            .map_err(map_anyhow)
    }

    async fn rename(&self, from: &str, to: &str) -> Result<(), RemoteOpsError> {
        let out = self
            .exec_command(&format!(
                "mv -f {} {}",
                shell_escape(from),
                shell_escape(to)
            ))
            .await
            .map_err(map_anyhow)?;
        if out.exit_code != 0 {
            return Err(RemoteOpsError::NonZeroExit {
                status: out.exit_code as i32,
                stderr: out.stderr,
            });
        }
        Ok(())
    }

    async fn remove_file(&self, path: &str) -> Result<(), RemoteOpsError> {
        let out = self
            .exec_command(&format!("rm -f {}", shell_escape(path)))
            .await
            .map_err(map_anyhow)?;
        if out.exit_code != 0 {
            return Err(RemoteOpsError::NonZeroExit {
                status: out.exit_code as i32,
                stderr: out.stderr,
            });
        }
        Ok(())
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

    async fn interactive_exec(
        &self,
        cmd: &str,
        timeout: Option<Duration>,
        cancellation: Option<&SharedCancellation>,
    ) -> Result<i32, RemoteOpsError> {
        self.exec_interactive(cmd, timeout, cancellation)
            .await
            .map_err(map_anyhow)
    }
}

fn map_anyhow(error: anyhow::Error) -> RemoteOpsError {
    RemoteOpsError::Io(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::{shell_escape, AbortOnDrop};
    use futures::future::pending;
    use std::time::Duration;
    use tokio::sync::oneshot;

    struct NotifyOnDrop(Option<oneshot::Sender<()>>);

    impl Drop for NotifyOnDrop {
        fn drop(&mut self) {
            if let Some(sender) = self.0.take() {
                let _ = sender.send(());
            }
        }
    }

    #[test]
    fn shell_escape_neutralizes_dollar_sign() {
        assert_eq!(shell_escape("$HOME"), "'$HOME'");
    }

    #[test]
    fn shell_escape_neutralizes_command_substitution() {
        assert_eq!(shell_escape("$(rm -rf /)"), "'$(rm -rf /)'");
    }

    #[test]
    fn shell_escape_handles_embedded_single_quote() {
        assert_eq!(shell_escape("a'b"), r"'a'\''b'");
    }

    #[tokio::test]
    async fn abort_on_drop_cancels_background_task() {
        let (tx, rx) = oneshot::channel();
        let guard = AbortOnDrop::new(tokio::spawn(async move {
            let _notify = NotifyOnDrop(Some(tx));
            pending::<()>().await;
        }));
        drop(guard);
        assert!(tokio::time::timeout(Duration::from_millis(100), rx)
            .await
            .is_ok());
    }
}
