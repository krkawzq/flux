//! Health check module for proxy tunnels
//!
//! Provides periodic health monitoring and status reporting

use crate::core::ssh::{SshClient, SshClientTrait};
use crate::proxy::models::{HealthCheckConfig, ProxyStatus};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;
use tokio::time::interval;

/// Health check result
#[derive(Debug, Clone)]
pub enum HealthCheckResult {
    Healthy,
    Degraded { reason: String },
    Unhealthy { reason: String },
}

/// Health checker for a proxy tunnel
pub struct HealthChecker {
    config: HealthCheckConfig,
    client: Arc<SshClient>,
    running: AtomicBool,
    status_tx: watch::Sender<HealthCheckResult>,
    status_rx: watch::Receiver<HealthCheckResult>,
}

impl HealthChecker {
    /// Create a new health checker
    pub fn new(config: HealthCheckConfig, client: Arc<SshClient>) -> Self {
        let (status_tx, status_rx) = watch::channel(HealthCheckResult::Healthy);

        Self {
            config,
            client,
            running: AtomicBool::new(false),
            status_tx,
            status_rx,
        }
    }

    /// Start health checking
    pub async fn start(&self) {
        if !self.config.enabled {
            tracing::debug!("Health checks disabled");
            return;
        }

        self.running.store(true, Ordering::SeqCst);

        let mut check_interval = interval(Duration::from_secs(self.config.interval_secs));
        let timeout = Duration::from_secs(self.config.timeout_secs);

        tracing::info!(
            "Starting health checks every {}s (timeout: {}s)",
            self.config.interval_secs,
            self.config.timeout_secs
        );

        while self.running.load(Ordering::SeqCst) {
            check_interval.tick().await;

            let result = self.perform_check(timeout).await;
            let _ = self.status_tx.send(result);
        }
    }

    /// Stop health checking
    pub fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);
    }

    /// Get current health status
    pub fn current_status(&self) -> HealthCheckResult {
        self.status_rx.borrow().clone()
    }

    /// Subscribe to health status updates
    pub fn subscribe(&self) -> watch::Receiver<HealthCheckResult> {
        self.status_rx.clone()
    }

    /// Perform a single health check
    async fn perform_check(&self, timeout: Duration) -> HealthCheckResult {
        // Simple SSH command to check connection
        let check_result = tokio::time::timeout(timeout, self.client.exec("echo 1")).await;

        match check_result {
            Ok(Ok(result)) if result.exit_code == 0 => {
                tracing::trace!("Health check passed");
                HealthCheckResult::Healthy
            }
            Ok(Ok(result)) => {
                let reason = format!("Command exited with code {}", result.exit_code);
                tracing::warn!("Health check degraded: {}", reason);
                HealthCheckResult::Degraded { reason }
            }
            Ok(Err(e)) => {
                let reason = format!("Command failed: {}", e);
                tracing::warn!("Health check unhealthy: {}", reason);
                HealthCheckResult::Unhealthy { reason }
            }
            Err(_) => {
                let reason = "Health check timed out".to_string();
                tracing::warn!("Health check unhealthy: {}", reason);
                HealthCheckResult::Unhealthy { reason }
            }
        }
    }
}

/// Convert health check result to proxy status
impl From<HealthCheckResult> for ProxyStatus {
    fn from(result: HealthCheckResult) -> Self {
        match result {
            HealthCheckResult::Healthy => ProxyStatus::Running,
            HealthCheckResult::Degraded { reason } => ProxyStatus::Degraded { reason },
            HealthCheckResult::Unhealthy { reason } => ProxyStatus::Degraded { reason },
        }
    }
}

/// Reconnection manager with exponential backoff
pub struct ReconnectionManager {
    max_retries: u32,
    initial_delay_ms: u64,
    max_delay_ms: u64,
    backoff_multiplier: f64,
    current_attempt: u32,
}

impl ReconnectionManager {
    pub fn new(
        max_retries: u32,
        initial_delay_ms: u64,
        max_delay_ms: u64,
        backoff_multiplier: f64,
    ) -> Self {
        Self {
            max_retries,
            initial_delay_ms,
            max_delay_ms,
            backoff_multiplier,
            current_attempt: 0,
        }
    }

    /// Reset the manager after successful connection
    pub fn reset(&mut self) {
        self.current_attempt = 0;
    }

    /// Check if we should retry
    pub fn should_retry(&self) -> bool {
        self.current_attempt < self.max_retries
    }

    /// Get next retry delay and increment attempt counter
    pub fn next_delay(&mut self) -> Duration {
        let delay = self.calculate_delay();
        self.current_attempt += 1;
        Duration::from_millis(delay)
    }

    /// Current attempt number
    pub fn attempt(&self) -> u32 {
        self.current_attempt
    }

    fn calculate_delay(&self) -> u64 {
        let delay = self.initial_delay_ms as f64
            * self.backoff_multiplier.powi(self.current_attempt as i32);
        (delay as u64).min(self.max_delay_ms)
    }
}

impl Default for ReconnectionManager {
    fn default() -> Self {
        Self::new(10, 1000, 60000, 2.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_reconnection_manager() {
        let mut mgr = ReconnectionManager::new(3, 1000, 10000, 2.0);

        assert!(mgr.should_retry());
        assert_eq!(mgr.attempt(), 0);

        let d1 = mgr.next_delay();
        assert_eq!(d1, Duration::from_millis(1000));
        assert_eq!(mgr.attempt(), 1);

        let d2 = mgr.next_delay();
        assert_eq!(d2, Duration::from_millis(2000));

        let d3 = mgr.next_delay();
        assert_eq!(d3, Duration::from_millis(4000));

        assert!(!mgr.should_retry());

        mgr.reset();
        assert!(mgr.should_retry());
        assert_eq!(mgr.attempt(), 0);
    }
}
