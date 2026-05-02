//! Pure-data Plan and Action types for the Flux pipeline.
//!
//! `plan_*` functions in `sync::file/script/block` produce these. They never
//! mutate remote state; the `execute_*` companions consume them.

use crate::sync::SyncError;
use chrono::{DateTime, Utc};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkipReason {
    AlreadyExists,
    RemoteNewer,
    ContentUnchanged,
    FilteredOut,
    PreviouslyApplied,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sentinel {
    pub name: String,
    pub timestamp: i64,
    pub open_marker: String,
    pub close_marker: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum FileAction {
    Skip {
        item_name: String,
        reason: SkipReason,
    },
    Apply {
        item_name: String,
        src: PathBuf,
        dst: String,
        len: u64,
        chmod: Option<u32>,
        observed_remote_mtime: Option<DateTime<Utc>>,
    },
    ApplyDir {
        item_name: String,
        src_dir: PathBuf,
        dst_dir: String,
        files: Vec<(PathBuf, String)>,
        chmod: Option<u32>,
    },
    ApplyLink {
        item_name: String,
        dst: String,
        target: String,
    },
    Failed {
        item_name: String,
        error: SyncError,
    },
}

#[derive(Debug, PartialEq, Eq)]
pub enum ScriptAction {
    Skip {
        item_name: String,
        reason: SkipReason,
    },
    Run {
        item_name: String,
        upload_to: String,
        local_script_bytes: Vec<u8>,
        command_argv: Vec<String>,
    },
    Failed {
        item_name: String,
        error: SyncError,
    },
}

#[derive(Debug, PartialEq, Eq)]
pub enum BlockAction {
    Skip {
        item_name: String,
        reason: SkipReason,
    },
    Apply {
        item_name: String,
        target: String,
        body: String,
        sentinel: Sentinel,
        observed_remote_mtime: Option<DateTime<Utc>>,
    },
    Failed {
        item_name: String,
        error: SyncError,
    },
}

#[derive(Debug, PartialEq, Eq)]
pub struct RegisterPubkeyAction {
    pub local_pubkey_path: String,
    pub remote_authorized_keys: String,
}

#[derive(Debug, PartialEq, Eq, Default)]
pub struct Plan {
    pub register_pubkey: Option<RegisterPubkeyAction>,
    pub file_actions: Vec<FileAction>,
    pub script_actions: Vec<ScriptAction>,
    pub block_actions: Vec<BlockAction>,
}

impl Plan {
    pub fn is_empty(&self) -> bool {
        self.register_pubkey.is_none()
            && self.file_actions.is_empty()
            && self.script_actions.is_empty()
            && self.block_actions.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::block::BlockError;

    #[test]
    fn plan_empty() {
        let plan = Plan::default();
        assert!(plan.is_empty());
    }

    #[test]
    fn block_action_failed_has_error() {
        let action = BlockAction::Failed {
            item_name: "x".into(),
            error: SyncError::Block(BlockError::BadTemplate),
        };
        assert!(matches!(action, BlockAction::Failed { .. }));
    }
}
