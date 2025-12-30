//! Sync service - orchestrates all sync operations
//!
//! Main entry point for configuration synchronization

use crate::core::error::{RemoteError, Result};
use crate::core::ssh::{create_client, SshClient, SshClientTrait, SshConfig};
use crate::sync::block_sync::{sync_block_groups, BlockSyncContext, BlockSyncResult};
use crate::sync::file_sync::{sync_files, FileSyncContext, FileSyncResult};
use crate::sync::models::{BlockGroup, FileSync, GlobalEnv, ScriptExec, SyncConfig};
use crate::sync::script_exec::{execute_scripts, ScriptExecContext, ScriptExecResult};
use crate::sync::version::{generate_machine_id, VersionTracker};
use std::path::PathBuf;

/// Sync service result
#[derive(Debug)]
pub struct SyncResult {
    pub files: Vec<FileSyncResult>,
    pub blocks: Vec<BlockSyncResult>,
    pub scripts: Vec<ScriptExecResult>,
    pub is_first_connect: bool,
}

/// Sync callbacks for UI feedback
pub trait SyncCallbacks: Send + Sync {
    fn on_connecting(&self, host: &str) {}
    fn on_connected(&self, host: &str) {}
    fn on_key_generated(&self, key_path: &str) {}
    fn on_first_connect(&self, host: &str) {}
    fn on_file_sync(&self, result: &FileSyncResult) {}
    fn on_block_sync(&self, result: &BlockSyncResult) {}
    fn on_script_start(&self, script: &str) {}
    fn on_script_end(&self, result: &ScriptExecResult) {}
    fn on_complete(&self, result: &SyncResult) {}
    fn on_error(&self, error: &RemoteError) {}
}

/// Default no-op callbacks
pub struct DefaultCallbacks;
impl SyncCallbacks for DefaultCallbacks {}

/// Sync service configuration
pub struct SyncServiceConfig {
    /// Force init mode
    pub force_init: bool,
    /// Dry run mode
    pub dry_run: bool,
    /// Conflict strategy override
    pub conflict_override: Option<String>,
}

impl Default for SyncServiceConfig {
    fn default() -> Self {
        Self {
            force_init: false,
            dry_run: false,
            conflict_override: None,
        }
    }
}

/// Main sync service
pub struct SyncService {
    config: SyncServiceConfig,
}

impl SyncService {
    /// Create a new sync service
    pub fn new(config: SyncServiceConfig) -> Self {
        Self { config }
    }

    /// Run sync with the given configuration
    pub async fn sync(
        &self,
        sync_config: &SyncConfig,
        callbacks: &dyn SyncCallbacks,
    ) -> Result<SyncResult> {
        // Resolve connection parameters
        let (host, user, port, key, password) = self.resolve_connection(sync_config)?;

        callbacks.on_connecting(&host);

        // Create SSH client
        let client = create_client(&host, &user, port, key.as_deref(), password.as_deref());

        // Connect
        client.connect().await?;
        callbacks.on_connected(&host);

        // Generate machine ID
        let machine_id = generate_machine_id(&host, &user);

        // Load version tracker
        let mut version_tracker = VersionTracker::load(&machine_id)?;

        // Check first connect
        let is_first_connect = version_tracker.is_first_sync() || self.config.force_init;

        if is_first_connect {
            callbacks.on_first_connect(&host);
        }

        // Run sync operations
        let result = self
            .run_sync_operations(
                &client,
                sync_config,
                &mut version_tracker,
                is_first_connect,
                callbacks,
            )
            .await;

        // Handle result
        match result {
            Ok(sync_result) => {
                // Update last sync time
                version_tracker.update_last_sync();
                version_tracker.save()?;

                callbacks.on_complete(&sync_result);

                // Close connection
                client.close().await?;

                Ok(sync_result)
            }
            Err(e) => {
                callbacks.on_error(&e);
                client.close().await?;
                Err(e)
            }
        }
    }

    /// Run all sync operations
    async fn run_sync_operations(
        &self,
        client: &SshClient,
        sync_config: &SyncConfig,
        version_tracker: &mut VersionTracker,
        is_first_connect: bool,
        callbacks: &dyn SyncCallbacks,
    ) -> Result<SyncResult> {
        let mut result = SyncResult {
            files: Vec::new(),
            blocks: Vec::new(),
            scripts: Vec::new(),
            is_first_connect,
        };

        // 1. File sync
        if !sync_config.files.is_empty() {
            let mut ctx = FileSyncContext {
                client,
                version_tracker,
                force_init: self.config.force_init,
                dry_run: self.config.dry_run,
            };

            result.files = sync_files(&sync_config.files, &mut ctx).await?;

            for file_result in &result.files {
                callbacks.on_file_sync(file_result);
            }
        }

        // 2. Block sync
        if let Some(ref block_group) = sync_config.block {
            let mut ctx = BlockSyncContext {
                client,
                version_tracker,
                block_home: sync_config.block_home.clone(),
                force_init: self.config.force_init,
                dry_run: self.config.dry_run,
            };

            result.blocks = sync_block_groups(&[block_group.clone()], &mut ctx).await?;

            for block_result in &result.blocks {
                callbacks.on_block_sync(block_result);
            }
        }

        // 3. Script execution
        if !sync_config.scripts.is_empty() {
            let ctx = ScriptExecContext {
                client,
                global_env: &sync_config.env,
                script_home: sync_config.script_home.clone(),
                is_first_connect,
                dry_run: self.config.dry_run,
            };

            for script in &sync_config.scripts {
                callbacks.on_script_start(&script.src);
            }

            result.scripts = execute_scripts(&sync_config.scripts, &ctx).await?;

            for script_result in &result.scripts {
                callbacks.on_script_end(script_result);
            }
        }

        Ok(result)
    }

    /// Resolve connection parameters from config
    fn resolve_connection(
        &self,
        config: &SyncConfig,
    ) -> Result<(String, String, u16, Option<String>, Option<String>)> {
        let host = config
            .host
            .clone()
            .ok_or_else(|| RemoteError::Config("Host is required".to_string()))?;

        let user = config
            .user
            .clone()
            .ok_or_else(|| RemoteError::Config("User is required".to_string()))?;

        let port = config.port.unwrap_or(22);
        let key = config.key.clone();
        let password = config.password.clone();

        Ok((host, user, port, key, password))
    }
}

/// Load sync configuration from TOML file
pub fn load_sync_config(path: &PathBuf) -> Result<SyncConfig> {
    let content = std::fs::read_to_string(path).map_err(|e| RemoteError::InvalidConfig {
        path: path.clone(),
        reason: e.to_string(),
    })?;

    let config: SyncConfig = toml::from_str(&content).map_err(|e| RemoteError::InvalidConfig {
        path: path.clone(),
        reason: e.to_string(),
    })?;

    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = SyncServiceConfig::default();
        assert!(!config.force_init);
        assert!(!config.dry_run);
        assert!(config.conflict_override.is_none());
    }
}
