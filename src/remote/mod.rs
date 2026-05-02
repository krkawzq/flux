//! Remote-side primitive operations.
//!
//! The `RemoteOps` trait abstracts everything Flux needs to do on a remote
//! host. The real implementation (`SshClient`) lives in `remote::ssh`; tests
//! use `remote::fake::InMemoryRemote`.

pub mod fake;
pub mod retry;
pub mod ssh;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
pub use retry::{with_retry, RetryPolicy};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Notify;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecOutput {
    pub status: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl ExecOutput {
    pub fn success(&self) -> bool {
        self.status == 0
    }

    pub fn stdout_string(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }

    pub fn stderr_string(&self) -> String {
        String::from_utf8_lossy(&self.stderr).into_owned()
    }
}

#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum RemoteOpsError {
    #[error("remote command failed (status={status}): {stderr}")]
    NonZeroExit { status: i32, stderr: String },
    #[error("remote io: {0}")]
    Io(String),
    #[error("path not found: {0}")]
    NotFound(String),
    #[error("ssh transport: {0}")]
    Transport(String),
    #[error("encoding: {0}")]
    Encoding(String),
}

#[derive(Debug, Clone, Default)]
pub struct SharedCancellation {
    presses: Arc<AtomicUsize>,
    notify: Arc<Notify>,
}

impl SharedCancellation {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn press(&self) -> usize {
        let next = self.presses.fetch_add(1, Ordering::SeqCst) + 1;
        self.notify.notify_waiters();
        next
    }

    pub fn presses(&self) -> usize {
        self.presses.load(Ordering::SeqCst)
    }

    pub async fn wait_for_change(&self, last_seen: usize) -> usize {
        loop {
            let current = self.presses();
            if current != last_seen {
                return current;
            }
            self.notify.notified().await;
        }
    }
}

#[async_trait]
pub trait RemoteOps: Send + Sync {
    /// Run a non-interactive command. Always returns `ExecOutput`; non-zero
    /// status is not an error because the caller decides how to interpret it.
    async fn exec(&self, cmd: &str) -> Result<ExecOutput, RemoteOpsError>;

    async fn read_file(&self, path: &str) -> Result<Vec<u8>, RemoteOpsError>;
    async fn write_file(&self, path: &str, data: &[u8]) -> Result<(), RemoteOpsError>;
    async fn exists(&self, path: &str) -> Result<bool, RemoteOpsError>;
    async fn mtime(&self, path: &str) -> Result<DateTime<Utc>, RemoteOpsError>;
    async fn stat_mode(&self, path: &str) -> Result<u32, RemoteOpsError>;
    async fn chmod(&self, path: &str, mode: u32) -> Result<(), RemoteOpsError>;
    async fn rename(&self, from: &str, to: &str) -> Result<(), RemoteOpsError>;
    async fn remove_file(&self, path: &str) -> Result<(), RemoteOpsError>;
    async fn ensure_dir(&self, path: &str) -> Result<(), RemoteOpsError>;

    /// Like `exec` but streams stdin/stdout/stderr to the local terminal.
    /// Returns the remote process exit status.
    async fn interactive_exec(
        &self,
        cmd: &str,
        timeout: Option<Duration>,
        cancellation: Option<&SharedCancellation>,
    ) -> Result<i32, RemoteOpsError>;
}
