//! Reporter abstraction. The pipeline emits structured events; concrete
//! reporters (console, captured-for-tests) decide how to render them.

pub mod console;
pub mod memory;

use crate::sync::plan::{Plan, SkipReason};
use crate::sync::SyncError;
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    File,
    Script,
    Block,
    Pubkey,
}

#[derive(Debug, Clone)]
pub enum ItemOutcome {
    Applied,
    Skipped(SkipReason),
    Failed(Arc<SyncError>),
}

impl ItemOutcome {
    pub fn failed_message(&self) -> Option<String> {
        match self {
            Self::Failed(error) => Some(error.to_string()),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct StageSummary {
    pub stage: Stage,
    pub applied: usize,
    pub skipped: usize,
    pub failed: usize,
}

#[derive(Debug, Clone)]
pub struct PipelineSummary {
    pub stages: Vec<StageSummary>,
    pub interrupted: bool,
    pub dry_run: bool,
}

impl PipelineSummary {
    pub fn total_failed(&self) -> usize {
        self.stages.iter().map(|s| s.failed).sum()
    }

    pub fn exit_code(&self) -> i32 {
        if self.interrupted {
            130
        } else if self.total_failed() > 0 {
            1
        } else {
            0
        }
    }
}

pub trait Reporter: Send + Sync {
    fn stage_started(&self, stage: Stage, item_count: usize);
    fn item_started(&self, stage: Stage, name: &str);
    fn item_finished(&self, stage: Stage, name: &str, outcome: &ItemOutcome);
    fn stage_finished(&self, summary: &StageSummary);
    fn print_plan(&self, plan: &Plan);
    fn pipeline_summary(&self, summary: &PipelineSummary);
    fn warning(&self, msg: &str);
    fn info(&self, msg: &str);
}
