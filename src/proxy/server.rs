//! Built-in SOCKS5/HTTP proxy server
//!
//! Provides a local proxy server for the built-in proxy mode

use crate::core::error::{RemoteError, Result};
use crate::proxy::models::ProxyMode;
use crate::proxy::tunnel::ForwardTunnel;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// Proxy server statistics
#[derive(Debug, Default)]
pub struct ProxyServerStats {
    pub connections_total: AtomicU64,
    pub connections_active: AtomicU64,
    pub bytes_transferred: AtomicU64,
}

/// Built-in proxy server
pub struct ProxyServer {
    bind_addr: SocketAddr,
    mode: ProxyMode,
    tunnel: Option<Arc<ForwardTunnel>>,
    running: AtomicBool,
    stats: Arc<ProxyServerStats>,
}

impl ProxyServer {
    /// Create a new proxy server
    pub fn new(host: &str, port: u16, mode: ProxyMode) -> Self {
        let bind_addr = format!("{}:{}", host, port)
            .parse()
            .unwrap_or_else(|_| "127.0.0.1:7890".parse().unwrap());

        Self {
            bind_addr,
            mode,
            tunnel: None,
            running: AtomicBool::new(false),
            stats: Arc::new(ProxyServerStats::default()),
        }
    }

    /// Set the forward tunnel for outbound connections
    pub fn set_tunnel(&mut self, tunnel: Arc<ForwardTunnel>) {
        self.tunnel = Some(tunnel);
    }

    /// Start the proxy server
    pub async fn start(&self) -> Result<()> {
        if self.running.load(Ordering::SeqCst) {
            return Err(RemoteError::Proxy("Proxy server already running".into()));
        }

        self.running.store(true, Ordering::SeqCst);

        let listener = TcpListener::bind(&self.bind_addr).await.map_err(|e| {
            RemoteError::Proxy(format!("Failed to bind to {}: {}", self.bind_addr, e))
        })?;

        tracing::info!(
            "Proxy server listening on {} ({:?})",
            self.bind_addr,
            self.mode
        );

        while self.running.load(Ordering::SeqCst) {
            tokio::select! {
                result = listener.accept() => {
                    match result {
                        Ok((stream, addr)) => {
                            let stats = self.stats.clone();
                            let mode = self.mode.clone();
                            let tunnel = self.tunnel.clone();

                            tokio::spawn(async move {
                                if let Err(e) = handle_client(stream, addr, mode, tunnel, stats).await {
                                    tracing::warn!("Client error from {}: {}", addr, e);
                                }
                            });
                        }
                        Err(e) => {
                            tracing::error!("Accept error: {}", e);
                        }
                    }
                }
                _ = tokio::time::sleep(tokio::time::Duration::from_millis(100)) => {
                    // Check if still running
                }
            }
        }

        Ok(())
    }

    /// Stop the proxy server
    pub fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);
        tracing::info!("Stopping proxy server");
    }

    /// Check if server is running
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    /// Get server statistics
    pub fn stats(&self) -> &ProxyServerStats {
        &self.stats
    }
}

/// Handle a client connection
async fn handle_client(
    mut stream: TcpStream,
    addr: SocketAddr,
    mode: ProxyMode,
    tunnel: Option<Arc<ForwardTunnel>>,
    stats: Arc<ProxyServerStats>,
) -> Result<()> {
    stats.connections_total.fetch_add(1, Ordering::SeqCst);
    stats.connections_active.fetch_add(1, Ordering::SeqCst);

    tracing::debug!("New client connection from {}", addr);

    let result = match mode {
        ProxyMode::Socks5 => handle_socks5(&mut stream, tunnel, &stats).await,
        ProxyMode::Http => handle_http(&mut stream, tunnel, &stats).await,
    };

    stats.connections_active.fetch_sub(1, Ordering::SeqCst);

    result
}

/// Handle SOCKS5 connection
async fn handle_socks5(
    stream: &mut TcpStream,
    tunnel: Option<Arc<ForwardTunnel>>,
    stats: &ProxyServerStats,
) -> Result<()> {
    // SOCKS5 greeting
    let mut buf = [0u8; 2];
    stream.read_exact(&mut buf).await?;

    let version = buf[0];
    let nmethods = buf[1];

    if version != 0x05 {
        return Err(RemoteError::Proxy("Invalid SOCKS version".into()));
    }

    // Read auth methods
    let mut methods = vec![0u8; nmethods as usize];
    stream.read_exact(&mut methods).await?;

    // Accept no authentication (0x00)
    stream.write_all(&[0x05, 0x00]).await?;

    // Read connection request
    let mut request = [0u8; 4];
    stream.read_exact(&mut request).await?;

    let version = request[0];
    let cmd = request[1];
    let _rsv = request[2];
    let atyp = request[3];

    if version != 0x05 || cmd != 0x01 {
        // Only support CONNECT command
        stream
            .write_all(&[0x05, 0x07, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
            .await?;
        return Err(RemoteError::Proxy("Unsupported SOCKS command".into()));
    }

    // Parse target address
    let (host, port) = match atyp {
        0x01 => {
            // IPv4
            let mut addr = [0u8; 4];
            stream.read_exact(&mut addr).await?;
            let mut port_buf = [0u8; 2];
            stream.read_exact(&mut port_buf).await?;
            let port = u16::from_be_bytes(port_buf);
            (
                format!("{}.{}.{}.{}", addr[0], addr[1], addr[2], addr[3]),
                port,
            )
        }
        0x03 => {
            // Domain name
            let mut len = [0u8; 1];
            stream.read_exact(&mut len).await?;
            let mut domain = vec![0u8; len[0] as usize];
            stream.read_exact(&mut domain).await?;
            let mut port_buf = [0u8; 2];
            stream.read_exact(&mut port_buf).await?;
            let port = u16::from_be_bytes(port_buf);
            (String::from_utf8_lossy(&domain).to_string(), port)
        }
        0x04 => {
            // IPv6
            let mut addr = [0u8; 16];
            stream.read_exact(&mut addr).await?;
            let mut port_buf = [0u8; 2];
            stream.read_exact(&mut port_buf).await?;
            let port = u16::from_be_bytes(port_buf);
            // Format IPv6 address
            let addr_str = format!(
                "{:x}:{:x}:{:x}:{:x}:{:x}:{:x}:{:x}:{:x}",
                u16::from_be_bytes([addr[0], addr[1]]),
                u16::from_be_bytes([addr[2], addr[3]]),
                u16::from_be_bytes([addr[4], addr[5]]),
                u16::from_be_bytes([addr[6], addr[7]]),
                u16::from_be_bytes([addr[8], addr[9]]),
                u16::from_be_bytes([addr[10], addr[11]]),
                u16::from_be_bytes([addr[12], addr[13]]),
                u16::from_be_bytes([addr[14], addr[15]]),
            );
            (addr_str, port)
        }
        _ => {
            stream
                .write_all(&[0x05, 0x08, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
                .await?;
            return Err(RemoteError::Proxy("Unsupported address type".into()));
        }
    };

    tracing::debug!("SOCKS5 CONNECT to {}:{}", host, port);

    // Connect to target
    let target = connect_target(&host, port, tunnel).await;

    match target {
        Ok(mut target_stream) => {
            // Send success response
            stream
                .write_all(&[0x05, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
                .await?;

            // Bidirectional forwarding
            let (mut client_read, mut client_write) = stream.split();
            let (mut target_read, mut target_write) = target_stream.split();

            tokio::select! {
                _ = copy_with_stats(&mut client_read, &mut target_write, &stats.bytes_transferred) => {}
                _ = copy_with_stats(&mut target_read, &mut client_write, &stats.bytes_transferred) => {}
            }

            Ok(())
        }
        Err(e) => {
            // Send failure response
            stream
                .write_all(&[0x05, 0x05, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
                .await?;
            Err(e)
        }
    }
}

/// Handle HTTP CONNECT request
async fn handle_http(
    stream: &mut TcpStream,
    tunnel: Option<Arc<ForwardTunnel>>,
    stats: &ProxyServerStats,
) -> Result<()> {
    // Read HTTP request
    let mut buf = vec![0u8; 4096];
    let n = stream.read(&mut buf).await?;
    let request = String::from_utf8_lossy(&buf[..n]);

    // Parse CONNECT request
    let lines: Vec<&str> = request.lines().collect();
    if lines.is_empty() {
        return Err(RemoteError::Proxy("Empty request".into()));
    }

    let parts: Vec<&str> = lines[0].split_whitespace().collect();
    if parts.len() < 3 || parts[0] != "CONNECT" {
        stream
            .write_all(b"HTTP/1.1 400 Bad Request\r\n\r\n")
            .await?;
        return Err(RemoteError::Proxy("Invalid HTTP CONNECT request".into()));
    }

    // Parse host:port
    let target = parts[1];
    let (host, port) = if let Some(pos) = target.rfind(':') {
        let host = &target[..pos];
        let port: u16 = target[pos + 1..].parse().unwrap_or(80);
        (host.to_string(), port)
    } else {
        (target.to_string(), 80)
    };

    tracing::debug!("HTTP CONNECT to {}:{}", host, port);

    // Connect to target
    match connect_target(&host, port, tunnel).await {
        Ok(mut target_stream) => {
            // Send success response
            stream
                .write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
                .await?;

            // Bidirectional forwarding
            let (mut client_read, mut client_write) = stream.split();
            let (mut target_read, mut target_write) = target_stream.split();

            tokio::select! {
                _ = copy_with_stats(&mut client_read, &mut target_write, &stats.bytes_transferred) => {}
                _ = copy_with_stats(&mut target_read, &mut client_write, &stats.bytes_transferred) => {}
            }

            Ok(())
        }
        Err(e) => {
            stream
                .write_all(b"HTTP/1.1 502 Bad Gateway\r\n\r\n")
                .await?;
            Err(e)
        }
    }
}

/// Connect to target (directly or through tunnel)
async fn connect_target(
    host: &str,
    port: u16,
    _tunnel: Option<Arc<ForwardTunnel>>,
) -> Result<TcpStream> {
    // TODO: If tunnel is provided, use it for connections
    // For now, connect directly
    let addr = format!("{}:{}", host, port);
    TcpStream::connect(&addr)
        .await
        .map_err(|e| RemoteError::Proxy(format!("Failed to connect to {}: {}", addr, e)))
}

/// Copy data with stats tracking
async fn copy_with_stats<R, W>(
    reader: &mut R,
    writer: &mut W,
    bytes_counter: &AtomicU64,
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
        bytes_counter.fetch_add(n as u64, Ordering::SeqCst);
    }

    Ok(())
}
