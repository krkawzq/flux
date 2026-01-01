//! SSH Reverse Tunnel implementation
//!
//! Creates reverse port forwarding through SSH connection

use crate::core::error::{RemoteError, Result};
use crate::core::ssh::{SshClient, SshClientTrait};
use crate::proxy::models::TunnelConfig;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// Tunnel statistics
#[derive(Debug, Default)]
pub struct TunnelStats {
    pub connections_total: AtomicU64,
    pub connections_active: AtomicU64,
    pub bytes_sent: AtomicU64,
    pub bytes_received: AtomicU64,
}

/// SSH Reverse Tunnel
///
/// Forwards connections from remote_port on the server to local_host:local_port
pub struct ReverseTunnel {
    config: TunnelConfig,
    client: Arc<SshClient>,
    running: AtomicBool,
    stats: Arc<TunnelStats>,
}

impl ReverseTunnel {
    /// Create a new reverse tunnel
    pub fn new(config: TunnelConfig, client: Arc<SshClient>) -> Self {
        Self {
            config,
            client,
            running: AtomicBool::new(false),
            stats: Arc::new(TunnelStats::default()),
        }
    }

    /// Start the tunnel
    pub async fn start(&self) -> Result<()> {
        if self.running.load(Ordering::SeqCst) {
            return Err(RemoteError::Tunnel("Tunnel already running".into()));
        }

        self.running.store(true, Ordering::SeqCst);

        tracing::info!(
            "Starting reverse tunnel: remote:{} -> {}:{}",
            self.config.remote_port,
            self.config.local_host,
            self.config.local_port
        );

        // Request remote port forward via SSH
        // Note: Actual implementation requires russh channel handling
        // This is a placeholder for the structure
        self.request_port_forward().await?;

        Ok(())
    }

    /// Stop the tunnel
    pub async fn stop(&self) -> Result<()> {
        self.running.store(false, Ordering::SeqCst);
        tracing::info!(
            "Stopping reverse tunnel on port {}",
            self.config.remote_port
        );
        Ok(())
    }

    /// Check if tunnel is running
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    /// Get tunnel statistics
    pub fn stats(&self) -> &TunnelStats {
        &self.stats
    }

    /// Request port forward on remote server
    async fn request_port_forward(&self) -> Result<()> {
        // TODO: Implement actual russh port forwarding
        // This requires:
        // 1. channel.request_port_forward() on the SSH session
        // 2. Accept incoming connections on the forwarded port
        // 3. For each connection, create a local TCP connection
        // 4. Bidirectional data forwarding

        let cmd = format!(
            "echo 'Tunnel established: remote:{} -> {}:{}'",
            self.config.remote_port, self.config.local_host, self.config.local_port
        );

        self.client.exec(&cmd).await?;

        Ok(())
    }

    /// Handle a single tunneled connection
    async fn handle_connection(
        local_host: String,
        local_port: u16,
        stats: Arc<TunnelStats>,
    ) -> Result<()> {
        stats.connections_total.fetch_add(1, Ordering::SeqCst);
        stats.connections_active.fetch_add(1, Ordering::SeqCst);

        // Connect to local target
        let local_addr = format!("{}:{}", local_host, local_port);
        let _local_stream = TcpStream::connect(&local_addr).await.map_err(|e| {
            RemoteError::Tunnel(format!("Failed to connect to {}: {}", local_addr, e))
        })?;

        tracing::debug!("Connected to local target: {}", local_addr);

        // TODO: Bidirectional data forwarding with SSH channel
        // This requires the actual SSH channel from russh

        stats.connections_active.fetch_sub(1, Ordering::SeqCst);

        Ok(())
    }
}

/// Forward tunnel (for built-in proxy mode)
///
/// Creates outbound connections through SSH channel
pub struct ForwardTunnel {
    client: Arc<SshClient>,
}

impl ForwardTunnel {
    pub fn new(client: Arc<SshClient>) -> Self {
        Self { client }
    }

    /// Connect to a remote address through the SSH tunnel
    pub async fn connect(&self, host: &str, port: u16) -> Result<TunnelConnection> {
        // TODO: Implement actual direct-tcpip channel
        // This opens a channel to the target through SSH

        tracing::debug!("Opening tunnel connection to {}:{}", host, port);

        Ok(TunnelConnection {
            host: host.to_string(),
            port,
        })
    }
}

/// A connection through the tunnel
pub struct TunnelConnection {
    pub host: String,
    pub port: u16,
    // TODO: Add actual russh channel handle
}

impl TunnelConnection {
    /// Read data from the tunnel
    pub async fn read(&mut self, _buf: &mut [u8]) -> Result<usize> {
        // TODO: Read from SSH channel
        Ok(0)
    }

    /// Write data to the tunnel
    pub async fn write(&mut self, buf: &[u8]) -> Result<usize> {
        // TODO: Write to SSH channel
        Ok(buf.len())
    }

    /// Close the connection
    pub async fn close(&mut self) -> Result<()> {
        Ok(())
    }
}

/// Bidirectional data forwarder
pub async fn forward_data<R, W>(
    mut reader: R,
    mut writer: W,
    stats: Arc<TunnelStats>,
    is_send: bool,
) -> Result<()>
where
    R: AsyncReadExt + Unpin,
    W: AsyncWriteExt + Unpin,
{
    let mut buf = vec![0u8; 8192];

    loop {
        let n = reader.read(&mut buf).await?;
        if n == 0 {
            break;
        }

        writer.write_all(&buf[..n]).await?;

        if is_send {
            stats.bytes_sent.fetch_add(n as u64, Ordering::SeqCst);
        } else {
            stats.bytes_received.fetch_add(n as u64, Ordering::SeqCst);
        }
    }

    Ok(())
}
