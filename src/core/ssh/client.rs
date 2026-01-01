//! SSH Client wrapper
//!
//! Provides a high-level SSH client interface using russh

use crate::core::error::{RemoteError, Result};
use async_trait::async_trait;
use russh::keys::key::PublicKey;
use russh::*;
use russh_keys::*;
use std::sync::Arc;
use tokio::sync::Mutex;

/// SSH authentication method
#[derive(Debug, Clone)]
pub enum AuthMethod {
    Password(String),
    KeyFile {
        path: String,
        passphrase: Option<String>,
    },
    None,
}

/// SSH client configuration
#[derive(Debug, Clone)]
pub struct SshConfig {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub auth: AuthMethod,
    pub fallback_password: Option<String>,  // Fallback to password if key fails
    pub timeout_secs: u64,
}

impl SshConfig {
    pub fn new(host: impl Into<String>, user: impl Into<String>) -> Self {
        Self {
            host: host.into(),
            port: 22,
            user: user.into(),
            auth: AuthMethod::None,
            fallback_password: None,
            timeout_secs: 30,
        }
    }

    pub fn with_port(mut self, port: u16) -> Self {
        self.port = port;
        self
    }

    pub fn with_password(mut self, password: impl Into<String>) -> Self {
        self.auth = AuthMethod::Password(password.into());
        self
    }

    pub fn with_key(mut self, path: impl Into<String>, passphrase: Option<String>) -> Self {
        self.auth = AuthMethod::KeyFile {
            path: path.into(),
            passphrase,
        };
        self
    }

    pub fn with_timeout(mut self, secs: u64) -> Self {
        self.timeout_secs = secs;
        self
    }
}

/// Command execution result
#[derive(Debug, Clone)]
pub struct ExecResult {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

/// SSH Client interface
///
/// This is a trait to allow for mocking in tests
#[async_trait]
pub trait SshClientTrait: Send + Sync {
    /// Execute a command on the remote server
    async fn exec(&self, cmd: &str) -> Result<ExecResult>;

    /// Execute a command with streaming output
    async fn exec_streaming<F, G>(
        &self,
        cmd: &str,
        on_stdout: F,
        on_stderr: G,
    ) -> Result<ExecResult>
    where
        F: Fn(&str) + Send + Sync,
        G: Fn(&str) + Send + Sync;

    /// Check if the connection is alive
    async fn is_connected(&self) -> bool;

    /// Close the connection
    async fn close(&self) -> Result<()>;

    /// Get the remote home directory
    async fn get_home(&self) -> Result<String>;

    /// Upload file content to remote path
    async fn upload_file(&self, remote_path: &str, content: &[u8]) -> Result<()>;

    /// Download file content from remote path
    async fn download_file(&self, remote_path: &str) -> Result<Vec<u8>>;
}

/// Client handler for russh - wraps errors properly
#[derive(Clone)]
struct ClientHandler;

/// Custom error type that wraps russh errors
#[derive(Debug)]
struct SshError(RemoteError);

impl From<russh::Error> for SshError {
    fn from(e: russh::Error) -> Self {
        SshError(RemoteError::SshConnection(e.to_string()))
    }
}

impl std::fmt::Display for SshError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for SshError {}

#[async_trait]
impl client::Handler for ClientHandler {
    type Error = SshError;

    async fn check_server_key(
        &mut self,
        _server_public_key: &PublicKey,
    ) -> std::result::Result<bool, Self::Error> {
        // Accept all host keys for now
        // TODO: Implement proper host key verification with known_hosts
        Ok(true)
    }
}

/// SSH Client implementation using russh
pub struct SshClient {
    config: SshConfig,
    handle: Arc<Mutex<Option<client::Handle<ClientHandler>>>>,
}

impl SshClient {
    /// Create a new SSH client (does not connect yet)
    pub fn new(config: SshConfig) -> Self {
        Self {
            config,
            handle: Arc::new(Mutex::new(None)),
        }
    }

    /// Connect to the remote server
    pub async fn connect(&self) -> Result<()> {
        tracing::info!(
            "Connecting to {}@{}:{}",
            self.config.user,
            self.config.host,
            self.config.port
        );

        let ssh_config = client::Config {
            inactivity_timeout: Some(std::time::Duration::from_secs(self.config.timeout_secs)),
            ..Default::default()
        };

        let addr = format!("{}:{}", self.config.host, self.config.port);

        // Connect using russh::client::connect which takes an address
        let mut session = client::connect(Arc::new(ssh_config), &addr, ClientHandler)
            .await
            .map_err(|e| RemoteError::SshConnection(format!("SSH connect failed: {}", e)))?;

        // Authenticate
        let auth_result: std::result::Result<bool, RemoteError> = match &self.config.auth {
            AuthMethod::Password(password) => session
                .authenticate_password(&self.config.user, password)
                .await
                .map_err(|e| RemoteError::SshAuth(format!("Password auth failed: {}", e))),
            AuthMethod::KeyFile { path, passphrase } => {
                let key_result = self
                    .authenticate_with_key(&mut session, path, passphrase.as_deref())
                    .await;
                
                match key_result {
                    Ok(true) => Ok(true),
                    Ok(false) | Err(_) => {
                        // Key auth failed, try fallback password if available
                        if let Some(ref fallback_pwd) = self.config.fallback_password {
                            tracing::warn!("Key authentication failed, trying password fallback");
                            session
                                .authenticate_password(&self.config.user, fallback_pwd)
                                .await
                                .map_err(|e| RemoteError::SshAuth(format!("Password fallback auth failed: {}", e)))
                        } else {
                            key_result.map_err(|e| e.0)
                        }
                    }
                }
            }
            AuthMethod::None => session
                .authenticate_none(&self.config.user)
                .await
                .map_err(|e| RemoteError::SshAuth(format!("Auth failed: {}", e))),
        };

        match auth_result {
            Ok(true) => {
                tracing::info!("SSH authentication successful");
                let mut handle = self.handle.lock().await;
                *handle = Some(session);
                Ok(())
            }
            Ok(false) => Err(RemoteError::SshAuth(
                "Authentication failed: rejected by server".into(),
            )),
            Err(e) => Err(e),
        }
    }

    /// Authenticate with SSH key
    async fn authenticate_with_key(
        &self,
        session: &mut client::Handle<ClientHandler>,
        key_path: &str,
        passphrase: Option<&str>,
    ) -> std::result::Result<bool, SshError> {
        // Expand ~ in key path
        let expanded_path = crate::core::platform::expand_tilde(key_path);

        // Load the key
        let key_pair = if let Some(pass) = passphrase {
            load_secret_key(&expanded_path, Some(pass))
        } else {
            load_secret_key(&expanded_path, None)
        }
        .map_err(|e| {
            tracing::error!("Failed to load SSH key {}: {}", expanded_path.display(), e);
            SshError(RemoteError::SshAuth(format!(
                "Failed to load key {}: {}",
                expanded_path.display(),
                e
            )))
        })?;

        // Try authentication with the key
        session
            .authenticate_publickey(&self.config.user, Arc::new(key_pair))
            .await
            .map_err(|e| SshError(RemoteError::SshAuth(format!("Key auth failed: {}", e))))
    }

    /// Get the configuration
    pub fn config(&self) -> &SshConfig {
        &self.config
    }

    /// Execute command and return result
    async fn exec_internal(&self, cmd: &str) -> Result<ExecResult> {
        let handle = self.handle.lock().await;
        let handle = handle
            .as_ref()
            .ok_or_else(|| RemoteError::SshConnection("Not connected".into()))?;

        // Open a channel
        let mut channel = handle
            .channel_open_session()
            .await
            .map_err(|e| RemoteError::SshExec(format!("Failed to open channel: {}", e)))?;

        // Execute command
        channel
            .exec(true, cmd)
            .await
            .map_err(|e| RemoteError::SshExec(format!("Failed to execute command: {}", e)))?;

        // Read output
        let mut stdout = String::new();
        let mut stderr = String::new();
        let mut exit_code = 0i32;

        loop {
            match channel.wait().await {
                Some(ChannelMsg::Data { data }) => {
                    stdout.push_str(&String::from_utf8_lossy(&data));
                }
                Some(ChannelMsg::ExtendedData { data, ext }) => {
                    if ext == 1 {
                        // stderr
                        stderr.push_str(&String::from_utf8_lossy(&data));
                    }
                }
                Some(ChannelMsg::ExitStatus { exit_status }) => {
                    exit_code = exit_status as i32;
                }
                Some(ChannelMsg::Eof) | None => break,
                _ => {}
            }
        }

        Ok(ExecResult {
            stdout,
            stderr,
            exit_code,
        })
    }
}

#[async_trait]
impl SshClientTrait for SshClient {
    async fn exec(&self, cmd: &str) -> Result<ExecResult> {
        tracing::debug!("Executing command: {}", cmd);
        self.exec_internal(cmd).await
    }

    async fn exec_streaming<F, G>(
        &self,
        cmd: &str,
        on_stdout: F,
        on_stderr: G,
    ) -> Result<ExecResult>
    where
        F: Fn(&str) + Send + Sync,
        G: Fn(&str) + Send + Sync,
    {
        let handle = self.handle.lock().await;
        let handle = handle
            .as_ref()
            .ok_or_else(|| RemoteError::SshConnection("Not connected".into()))?;

        // Open a channel
        let mut channel = handle
            .channel_open_session()
            .await
            .map_err(|e| RemoteError::SshExec(format!("Failed to open channel: {}", e)))?;

        // Execute command
        channel
            .exec(true, cmd)
            .await
            .map_err(|e| RemoteError::SshExec(format!("Failed to execute command: {}", e)))?;

        // Read output with streaming callbacks
        let mut stdout = String::new();
        let mut stderr = String::new();
        let mut exit_code = 0i32;

        loop {
            match channel.wait().await {
                Some(ChannelMsg::Data { data }) => {
                    let chunk = String::from_utf8_lossy(&data);
                    on_stdout(&chunk);
                    stdout.push_str(&chunk);
                }
                Some(ChannelMsg::ExtendedData { data, ext }) => {
                    if ext == 1 {
                        let chunk = String::from_utf8_lossy(&data);
                        on_stderr(&chunk);
                        stderr.push_str(&chunk);
                    }
                }
                Some(ChannelMsg::ExitStatus { exit_status }) => {
                    exit_code = exit_status as i32;
                }
                Some(ChannelMsg::Eof) | None => break,
                _ => {}
            }
        }

        Ok(ExecResult {
            stdout,
            stderr,
            exit_code,
        })
    }

    async fn is_connected(&self) -> bool {
        let handle = self.handle.lock().await;
        handle.is_some()
    }

    async fn close(&self) -> Result<()> {
        let mut handle = self.handle.lock().await;
        if let Some(h) = handle.take() {
            let _ = h.disconnect(Disconnect::ByApplication, "", "en").await;
        }
        Ok(())
    }

    async fn get_home(&self) -> Result<String> {
        let result = self.exec("echo $HOME").await?;
        if result.exit_code != 0 {
            return Err(RemoteError::SshExec(format!(
                "Failed to get home directory: {}",
                result.stderr
            )));
        }
        Ok(result.stdout.trim().to_string())
    }

    async fn upload_file(&self, remote_path: &str, content: &[u8]) -> Result<()> {
        // Ensure parent directory exists
        if let Some(parent) = std::path::Path::new(remote_path).parent() {
            if !parent.as_os_str().is_empty() {
                let mkdir_cmd = format!("mkdir -p '{}'", parent.display());
                self.exec(&mkdir_cmd).await?;
            }
        }

        // Use base64 encoding to transfer binary safely
        let b64_content = base64_encode(content);

        // Write using echo and base64 decode
        let cmd = format!("echo '{}' | base64 -d > '{}'", b64_content, remote_path);

        let result = self.exec(&cmd).await?;
        if result.exit_code != 0 {
            return Err(RemoteError::SshExec(format!(
                "Failed to upload file: {}",
                result.stderr
            )));
        }

        Ok(())
    }

    async fn download_file(&self, remote_path: &str) -> Result<Vec<u8>> {
        // Use base64 to safely transfer binary content
        let cmd = format!("base64 '{}'", remote_path);
        let result = self.exec(&cmd).await?;

        if result.exit_code != 0 {
            return Err(RemoteError::SshExec(format!(
                "Failed to download file: {}",
                result.stderr
            )));
        }

        // Decode base64
        base64_decode(result.stdout.trim())
            .map_err(|e| RemoteError::SshExec(format!("Failed to decode file content: {}", e)))
    }
}

/// Base64 encode bytes to string
#[allow(clippy::while_let_loop)]
fn base64_encode(data: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    let mut result = String::new();
    let mut iter = data.iter().copied();

    loop {
        let b0 = match iter.next() {
            Some(b) => b,
            None => break,
        };
        let b1 = iter.next();
        let b2 = iter.next();

        result.push(ALPHABET[(b0 >> 2) as usize] as char);
        result.push(ALPHABET[(((b0 & 0x03) << 4) | (b1.unwrap_or(0) >> 4)) as usize] as char);

        match b1 {
            Some(b1) => {
                result
                    .push(ALPHABET[(((b1 & 0x0f) << 2) | (b2.unwrap_or(0) >> 6)) as usize] as char);
                match b2 {
                    Some(b2) => result.push(ALPHABET[(b2 & 0x3f) as usize] as char),
                    None => result.push('='),
                }
            }
            None => {
                result.push('=');
                result.push('=');
            }
        }
    }

    result
}

/// Base64 decode string to bytes
fn base64_decode(data: &str) -> std::result::Result<Vec<u8>, &'static str> {
    fn decode_char(c: char) -> std::result::Result<u8, &'static str> {
        match c {
            'A'..='Z' => Ok(c as u8 - b'A'),
            'a'..='z' => Ok(c as u8 - b'a' + 26),
            '0'..='9' => Ok(c as u8 - b'0' + 52),
            '+' => Ok(62),
            '/' => Ok(63),
            '=' => Ok(0), // Padding
            _ => Err("Invalid base64 character"),
        }
    }

    let data: String = data.chars().filter(|c| !c.is_whitespace()).collect();

    if !data.len().is_multiple_of(4) {
        return Err("Invalid base64 length");
    }

    let mut result = Vec::new();
    let mut chars = data.chars().peekable();

    while chars.peek().is_some() {
        let c0 = decode_char(chars.next().ok_or("Unexpected end")?)?;
        let c1 = decode_char(chars.next().ok_or("Unexpected end")?)?;
        let c2_char = chars.next().ok_or("Unexpected end")?;
        let c3_char = chars.next().ok_or("Unexpected end")?;

        result.push((c0 << 2) | (c1 >> 4));

        if c2_char != '=' {
            let c2 = decode_char(c2_char)?;
            result.push((c1 << 4) | (c2 >> 2));

            if c3_char != '=' {
                let c3 = decode_char(c3_char)?;
                result.push((c2 << 6) | c3);
            }
        }
    }

    Ok(result)
}

/// Create an SSH client from configuration parameters
/// Priority: key > password (with automatic fallback)
pub fn create_client(
    host: &str,
    user: &str,
    port: u16,
    key: Option<&str>,
    password: Option<&str>,
) -> SshClient {
    let mut config = SshConfig::new(host, user).with_port(port);

    // Prefer key if provided and file exists
    if let Some(key_path) = key {
        let expanded = crate::core::platform::expand_tilde(key_path);
        if expanded.exists() {
            config = config.with_key(key_path, None);
            // Set password as fallback
            if let Some(pwd) = password {
                config.fallback_password = Some(pwd.to_string());
            }
        } else {
            tracing::warn!("SSH key file not found: {}, using password auth", expanded.display());
            if let Some(pwd) = password {
                config = config.with_password(pwd);
            }
        }
    } else if let Some(pwd) = password {
        config = config.with_password(pwd);
    }

    SshClient::new(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_base64_roundtrip() {
        let original = b"Hello, World! This is a test.";
        let encoded = base64_encode(original);
        let decoded = base64_decode(&encoded).unwrap();
        assert_eq!(original.as_slice(), decoded.as_slice());
    }

    #[test]
    fn test_base64_empty() {
        let original = b"";
        let encoded = base64_encode(original);
        let decoded = base64_decode(&encoded).unwrap();
        assert_eq!(original.as_slice(), decoded.as_slice());
    }

    #[test]
    fn test_ssh_config() {
        let config = SshConfig::new("localhost", "root")
            .with_port(2222)
            .with_password("secret");

        assert_eq!(config.host, "localhost");
        assert_eq!(config.port, 2222);
        assert_eq!(config.user, "root");
        match config.auth {
            AuthMethod::Password(p) => assert_eq!(p, "secret"),
            _ => panic!("Expected password auth"),
        }
    }
}
