//! Proxy domain models

use serde::{Deserialize, Serialize};

/// Proxy protocol mode
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ProxyMode {
    #[default]
    Socks5,
    Http,
}

impl std::fmt::Display for ProxyMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProxyMode::Socks5 => write!(f, "socks5"),
            ProxyMode::Http => write!(f, "http"),
        }
    }
}

/// Reconnection configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReconnectConfig {
    /// Enable automatic reconnection
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Maximum retry attempts
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,
    /// Initial delay in milliseconds
    #[serde(default = "default_initial_delay")]
    pub initial_delay_ms: u64,
    /// Maximum delay in milliseconds
    #[serde(default = "default_max_delay")]
    pub max_delay_ms: u64,
    /// Backoff multiplier
    #[serde(default = "default_backoff")]
    pub backoff_multiplier: f64,
}

fn default_true() -> bool {
    true
}
fn default_max_retries() -> u32 {
    10
}
fn default_initial_delay() -> u64 {
    1000
}
fn default_max_delay() -> u64 {
    60000
}
fn default_backoff() -> f64 {
    2.0
}

impl Default for ReconnectConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_retries: 10,
            initial_delay_ms: 1000,
            max_delay_ms: 60000,
            backoff_multiplier: 2.0,
        }
    }
}

/// Health check configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthCheckConfig {
    /// Enable health checks
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Check interval in seconds
    #[serde(default = "default_interval")]
    pub interval_secs: u64,
    /// Check timeout in seconds
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
}

fn default_interval() -> u64 {
    30
}
fn default_timeout() -> u64 {
    5
}

impl Default for HealthCheckConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            interval_secs: 30,
            timeout_secs: 5,
        }
    }
}

/// Proxy configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyConfig {
    /// Remote port to bind on the server
    pub remote_port: u16,
    /// Local port to forward to
    pub local_port: Option<u16>,
    /// Local host to forward to
    #[serde(default = "default_localhost")]
    pub local_host: String,
    /// Proxy protocol mode
    #[serde(default)]
    pub mode: ProxyMode,
    /// Use built-in proxy server instead of forwarding
    #[serde(default)]
    pub use_builtin: bool,
    /// Reconnection settings
    #[serde(default)]
    pub reconnect: ReconnectConfig,
    /// Health check settings
    #[serde(default)]
    pub health_check: HealthCheckConfig,
}

fn default_localhost() -> String {
    "127.0.0.1".to_string()
}

impl Default for ProxyConfig {
    fn default() -> Self {
        Self {
            remote_port: 1081,
            local_port: Some(7890),
            local_host: default_localhost(),
            mode: ProxyMode::default(),
            use_builtin: false,
            reconnect: ReconnectConfig::default(),
            health_check: HealthCheckConfig::default(),
        }
    }
}

/// Tunnel configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TunnelConfig {
    pub remote_port: u16,
    pub local_host: String,
    pub local_port: u16,
}

/// Proxy runtime status
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ProxyStatus {
    Starting,
    Running,
    Reconnecting { attempt: u32 },
    Degraded { reason: String },
    Stopped,
}

impl std::fmt::Display for ProxyStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProxyStatus::Starting => write!(f, "Starting"),
            ProxyStatus::Running => write!(f, "Running"),
            ProxyStatus::Reconnecting { attempt } => {
                write!(f, "Reconnecting (attempt {})", attempt)
            }
            ProxyStatus::Degraded { reason } => write!(f, "Degraded: {}", reason),
            ProxyStatus::Stopped => write!(f, "Stopped"),
        }
    }
}

/// Proxy statistics
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProxyStats {
    /// Total connections handled
    pub connections_total: u64,
    /// Currently active connections
    pub connections_active: u32,
    /// Total bytes transferred
    pub bytes_transferred: u64,
    /// Last connection timestamp
    pub last_connection_at: Option<i64>,
}

/// Proxy instance state (persisted)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyState {
    /// Instance name
    pub name: String,
    /// Process ID
    pub pid: u32,
    /// SSH host name
    pub ssh_host: String,
    /// Configuration
    pub config: ProxyConfig,
    /// Started timestamp
    pub started_at: i64,
    /// Current status
    pub status: ProxyStatus,
    /// Statistics
    #[serde(default)]
    pub stats: ProxyStats,
}
