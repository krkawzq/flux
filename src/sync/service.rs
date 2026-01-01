//! Sync service - orchestrates all sync operations
//!
//! Main entry point for configuration synchronization

use crate::core::error::{RemoteError, Result};
use crate::core::ssh::{create_client, SshClient, SshClientTrait};
use crate::sync::block_sync::{sync_block_groups, BlockSyncContext, BlockSyncResult};
use crate::sync::file_sync::{sync_files, FileSyncContext, FileSyncResult};
use crate::config::models::ResolvedConfig;
use crate::sync::script_exec::{execute_scripts, ScriptExecContext, ScriptExecResult};
use crate::sync::version::{generate_machine_id, VersionTracker};

/// Connection parameters tuple (host, user, port, key, password)
type ConnectionParams = (String, String, u16, Option<String>, Option<String>);

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
    fn on_connecting(&self, _host: &str) {}
    fn on_connected(&self, _host: &str) {}
    fn on_key_generated(&self, _key_path: &str) {}
    fn on_first_connect(&self, _host: &str) {}
    fn on_file_sync(&self, _result: &FileSyncResult) {}
    fn on_block_sync(&self, _result: &BlockSyncResult) {}
    fn on_script_start(&self, _script: &str) {}
    fn on_script_end(&self, _result: &ScriptExecResult) {}
    fn on_complete(&self, _result: &SyncResult) {}
    fn on_error(&self, _error: &RemoteError) {}
}

/// Default no-op callbacks
pub struct DefaultCallbacks;
impl SyncCallbacks for DefaultCallbacks {}

/// Sync service configuration
#[derive(Default)]
pub struct SyncServiceConfig {
    /// Force init mode
    pub force_init: bool,
    /// Dry run mode
    pub dry_run: bool,
    /// Verbose mode - stream script output in real-time
    pub verbose: bool,
    /// Conflict strategy override
    pub conflict_override: Option<String>,
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
        resolved_config: &ResolvedConfig,
        callbacks: &dyn SyncCallbacks,
    ) -> Result<SyncResult> {
        // Get connection parameters from resolved config
        let host = resolved_config.connection.host.clone();
        let user = resolved_config.connection.user.clone();
        let port = resolved_config.connection.port;
        let key_str = resolved_config.connection.key.as_ref().map(|p| p.to_string_lossy().to_string());
        let password = resolved_config.connection.password.clone();

        callbacks.on_connecting(&host);

        // Create SSH client
        let client = create_client(&host, &user, port, key_str.as_deref(), password.as_deref());

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
                resolved_config,
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
        resolved_config: &ResolvedConfig,
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
        if !resolved_config.files.is_empty() {
            // Determine flux_dir for relative path resolution
            let flux_dir = crate::config::finder::ConfigFinder::new()
                .local_dir()
                .cloned();

            let mut ctx = FileSyncContext {
                client,
                version_tracker,
                force_init: self.config.force_init,
                dry_run: self.config.dry_run,
                flux_dir,
            };

            result.files = sync_files(&resolved_config.files, &mut ctx).await?;

            for file_result in &result.files {
                callbacks.on_file_sync(file_result);
            }
        }

        // 2. Block sync
        if !resolved_config.blocks.is_empty() {
            // Get flux_dir for relative block path resolution
            let flux_dir = crate::config::finder::ConfigFinder::new()
                .local_dir()
                .cloned();

            let mut ctx = BlockSyncContext {
                client,
                version_tracker,
                block_home: resolved_config.block_home.clone(),
                force_init: self.config.force_init,
                dry_run: self.config.dry_run,
                flux_dir,
            };

            result.blocks = sync_block_groups(&resolved_config.blocks, &mut ctx).await?;

            for block_result in &result.blocks {
                callbacks.on_block_sync(block_result);
            }
        }

        // 3. Script execution
        if !resolved_config.scripts.is_empty() {
            // Get flux_dir for relative script path resolution
            let flux_dir = crate::config::finder::ConfigFinder::new()
                .local_dir()
                .cloned();

            let ctx = ScriptExecContext {
                client,
                global_env: &resolved_config.env,
                script_home: resolved_config.script_home.clone(),
                is_first_connect,
                dry_run: self.config.dry_run,
                verbose: self.config.verbose,
                flux_dir,
            };

            for script in &resolved_config.scripts {
                callbacks.on_script_start(&script.src);
            }

            result.scripts = execute_scripts(&resolved_config.scripts, &ctx).await?;

            for script_result in &result.scripts {
                callbacks.on_script_end(script_result);
            }
        }

        Ok(result)
    }

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
