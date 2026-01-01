//! Proxy service - orchestrates proxy lifecycle
//!
//! Main entry point for proxy tunnel management

use crate::core::error::{RemoteError, Result};
use crate::core::platform::{get_background_service, BackgroundService};
use crate::core::ssh::{create_client, SshClient, SshConfig};
use crate::proxy::health::{HealthChecker, ReconnectionManager};
use crate::proxy::models::{ProxyConfig, ProxyState, ProxyStats, ProxyStatus};
use crate::proxy::server::ProxyServer;
use crate::proxy::tunnel::{ForwardTunnel, ReverseTunnel};
use crate::state::FileStateStore;
use std::sync::Arc;

/// Proxy service callbacks for UI feedback
pub trait ProxyCallbacks: Send + Sync {
    fn on_starting(&self, _name: &str) {}
    fn on_connected(&self, _name: &str) {}
    fn on_tunnel_established(&self, _name: &str, _remote_port: u16) {}
    fn on_reconnecting(&self, _name: &str, _attempt: u32) {}
    fn on_stopped(&self, _name: &str) {}
    fn on_error(&self, _name: &str, _error: &RemoteError) {}
}

/// Default no-op callbacks
pub struct DefaultProxyCallbacks;
impl ProxyCallbacks for DefaultProxyCallbacks {}

/// Proxy service for managing SSH tunnels
pub struct ProxyService {
    state_store: FileStateStore,
    background_service: Box<dyn BackgroundService>,
}

impl ProxyService {
    /// Create a new proxy service
    pub fn new() -> Self {
        Self {
            state_store: FileStateStore::new(),
            background_service: get_background_service(),
        }
    }

    /// Start a proxy tunnel
    pub async fn start(
        &self,
        name: &str,
        ssh_host: &str,
        config: ProxyConfig,
        ssh_config: SshConfig,
        foreground: bool,
        callbacks: &dyn ProxyCallbacks,
    ) -> Result<ProxyState> {
        // Check if already running
        if let Some(state) = self.get_state(name)? {
            if self.background_service.is_running(state.pid) {
                return Err(RemoteError::ProxyAlreadyRunning {
                    name: name.to_string(),
                    pid: state.pid,
                });
            }
            // Clean up stale state
            self.state_store.delete(name)?;
        }

        callbacks.on_starting(name);

        if foreground {
            // Run in foreground
            self.run_proxy(name, ssh_host, config, ssh_config, callbacks)
                .await
        } else {
            // Spawn background process
            self.spawn_background(name, ssh_host, &config).await
        }
    }

    /// Stop a proxy tunnel
    pub async fn stop(&self, name: &str, callbacks: &dyn ProxyCallbacks) -> Result<()> {
        let state = self
            .get_state(name)?
            .ok_or_else(|| RemoteError::ProxyNotRunning {
                name: name.to_string(),
            })?;

        if self.background_service.is_running(state.pid) {
            self.background_service.stop_background(state.pid)?;
        }

        self.state_store.delete(name)?;
        callbacks.on_stopped(name);

        tracing::info!("Stopped proxy: {}", name);

        Ok(())
    }

    /// Stop all proxy tunnels
    pub async fn stop_all(&self, callbacks: &dyn ProxyCallbacks) -> Result<()> {
        let names = self.list()?;
        for name in names {
            if let Err(e) = self.stop(&name, callbacks).await {
                tracing::warn!("Failed to stop {}: {}", name, e);
            }
        }
        Ok(())
    }

    /// Get proxy state by name
    pub fn get_state(&self, name: &str) -> Result<Option<ProxyState>> {
        self.state_store.load(name)
    }

    /// Get proxy status
    pub fn get_status(&self, name: &str) -> Result<Option<ProxyStatus>> {
        if let Some(state) = self.get_state(name)? {
            if self.background_service.is_running(state.pid) {
                Ok(Some(state.status))
            } else {
                Ok(Some(ProxyStatus::Stopped))
            }
        } else {
            Ok(None)
        }
    }

    /// List all proxy instances
    pub fn list(&self) -> Result<Vec<String>> {
        self.state_store.list()
    }

    /// Get all proxy states
    pub fn get_all_states(&self) -> Result<Vec<ProxyState>> {
        let names = self.list()?;
        let mut states = Vec::new();
        for name in names {
            if let Some(state) = self.get_state(&name)? {
                states.push(state);
            }
        }
        Ok(states)
    }

    /// Run proxy in foreground
    async fn run_proxy(
        &self,
        name: &str,
        ssh_host: &str,
        config: ProxyConfig,
        ssh_config: SshConfig,
        callbacks: &dyn ProxyCallbacks,
    ) -> Result<ProxyState> {
        // Create SSH client
        let client = create_client(
            &ssh_config.host,
            &ssh_config.user,
            ssh_config.port,
            None, // TODO: key support
            None, // TODO: password support
        );

        // Connect
        client.connect().await?;
        callbacks.on_connected(name);

        let client = Arc::new(client);

        // Create initial state
        let state = ProxyState {
            name: name.to_string(),
            pid: std::process::id(),
            ssh_host: ssh_host.to_string(),
            config: config.clone(),
            started_at: chrono::Utc::now().timestamp(),
            status: ProxyStatus::Running,
            stats: ProxyStats::default(),
        };

        // Save state
        self.state_store.save(name, &state)?;

        // Setup reconnection manager
        let _reconnect = ReconnectionManager::new(
            config.reconnect.max_retries,
            config.reconnect.initial_delay_ms,
            config.reconnect.max_delay_ms,
            config.reconnect.backoff_multiplier,
        );

        // Start health checker
        let _health_checker = Arc::new(HealthChecker::new(
            config.health_check.clone(),
            client.clone(),
        ));

        // Run main loop
        if config.use_builtin {
            // Built-in proxy mode
            self.run_builtin_proxy(name, client, config, callbacks)
                .await?;
        } else {
            // Reverse tunnel mode
            self.run_reverse_tunnel(name, client, config, callbacks)
                .await?;
        }

        callbacks.on_tunnel_established(name, state.config.remote_port);

        Ok(state)
    }

    /// Run reverse tunnel mode
    async fn run_reverse_tunnel(
        &self,
        _name: &str,
        client: Arc<SshClient>,
        config: ProxyConfig,
        _callbacks: &dyn ProxyCallbacks,
    ) -> Result<()> {
        let tunnel_config = crate::proxy::models::TunnelConfig {
            remote_port: config.remote_port,
            local_host: config.local_host.clone(),
            local_port: config.local_port.unwrap_or(7890),
        };

        let tunnel = ReverseTunnel::new(tunnel_config, client);
        tunnel.start().await?;

        tracing::info!(
            "Reverse tunnel established: remote:{} -> {}:{}",
            config.remote_port,
            config.local_host,
            config.local_port.unwrap_or(7890)
        );

        // Keep running until stopped
        // TODO: Add signal handling
        loop {
            tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
            if !tunnel.is_running() {
                break;
            }
        }

        Ok(())
    }

    /// Run built-in proxy mode
    async fn run_builtin_proxy(
        &self,
        _name: &str,
        client: Arc<SshClient>,
        config: ProxyConfig,
        _callbacks: &dyn ProxyCallbacks,
    ) -> Result<()> {
        let local_port = config.local_port.unwrap_or(7890);

        // Create forward tunnel for outbound connections
        let forward_tunnel = Arc::new(ForwardTunnel::new(client.clone()));

        // Create proxy server
        let mut server = ProxyServer::new(&config.local_host, local_port, config.mode.clone());
        server.set_tunnel(forward_tunnel);

        // Start reverse tunnel for remote access
        let tunnel_config = crate::proxy::models::TunnelConfig {
            remote_port: config.remote_port,
            local_host: config.local_host.clone(),
            local_port,
        };

        let reverse_tunnel = ReverseTunnel::new(tunnel_config, client);
        reverse_tunnel.start().await?;

        tracing::info!(
            "Built-in proxy started: remote:{} -> local proxy:{}",
            config.remote_port,
            local_port
        );

        // Start proxy server
        server.start().await?;

        Ok(())
    }

    /// Spawn proxy as background process
    async fn spawn_background(
        &self,
        name: &str,
        ssh_host: &str,
        config: &ProxyConfig,
    ) -> Result<ProxyState> {
        // Build arguments for background process
        let args = vec![
            "proxy".to_string(),
            "start".to_string(),
            name.to_string(),
            "--foreground".to_string(),
            "-r".to_string(),
            config.remote_port.to_string(),
            "-l".to_string(),
            config.local_port.unwrap_or(7890).to_string(),
            "-m".to_string(),
            config.mode.to_string(),
        ];

        let pid = self.background_service.spawn_background(name, args)?;

        let state = ProxyState {
            name: name.to_string(),
            pid,
            ssh_host: ssh_host.to_string(),
            config: config.clone(),
            started_at: chrono::Utc::now().timestamp(),
            status: ProxyStatus::Starting,
            stats: ProxyStats::default(),
        };

        self.state_store.save(name, &state)?;

        tracing::info!("Started proxy {} in background (PID: {})", name, pid);

        Ok(state)
    }
}

impl Default for ProxyService {
    fn default() -> Self {
        Self::new()
    }
}
