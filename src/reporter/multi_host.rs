use super::{ItemOutcome, PipelineSummary, Reporter, Stage, StageSummary};
use crate::sync::plan::{BlockAction, FileAction, Plan, ScriptAction, SkipReason};
use console::{style, Term};
use std::sync::Mutex;

pub struct MultiHostConsoleReporter {
    host: String,
    out: Mutex<Term>,
}

impl MultiHostConsoleReporter {
    pub fn new(host: impl Into<String>) -> Self {
        Self {
            host: host.into(),
            out: Mutex::new(Term::stdout()),
        }
    }

    fn prefix(&self) -> String {
        format!("[{}]", self.host)
    }

    fn stage_label(stage: Stage) -> &'static str {
        match stage {
            Stage::File => "file",
            Stage::Script => "script",
            Stage::Block => "block",
            Stage::Pubkey => "pubkey",
        }
    }

    fn skip_reason_label(reason: &SkipReason) -> &'static str {
        match reason {
            SkipReason::AlreadyExists => "already exists",
            SkipReason::RemoteNewer => "remote newer",
            SkipReason::ContentUnchanged => "content unchanged",
            SkipReason::FilteredOut => "filtered out",
            SkipReason::PreviouslyApplied => "previously applied",
        }
    }
}

impl Reporter for MultiHostConsoleReporter {
    fn stage_started(&self, stage: Stage, item_count: usize) {
        let _ = self.out.lock().unwrap().write_line(&format!(
            "{} {} {} {}",
            style(self.prefix()).magenta().bold(),
            style(format!("[{}]", Self::stage_label(stage)))
                .cyan()
                .bold(),
            style("stage").dim(),
            style(format!("({item_count} items)")).dim()
        ));
    }

    fn item_started(&self, _stage: Stage, _name: &str) {}

    fn item_finished(&self, stage: Stage, name: &str, outcome: &ItemOutcome) {
        let mark = match outcome {
            ItemOutcome::Applied => style("✓ apply").green(),
            ItemOutcome::Skipped(_) => style("⊘ skip").yellow(),
            ItemOutcome::Failed(_) => style("✗ fail").red(),
        };
        let detail = match outcome {
            ItemOutcome::Skipped(reason) => format!(" ({})", Self::skip_reason_label(reason)),
            ItemOutcome::Failed(error) => format!(" ({error})"),
            ItemOutcome::Applied => String::new(),
        };
        let _ = self.out.lock().unwrap().write_line(&format!(
            "{} [{}] {mark} {name}{detail}",
            style(self.prefix()).magenta().bold(),
            Self::stage_label(stage)
        ));
    }

    fn stage_finished(&self, summary: &StageSummary) {
        let _ = self.out.lock().unwrap().write_line(&format!(
            "{} {} done: applied={}, skipped={}, failed={}",
            style(self.prefix()).magenta().bold(),
            style(format!("[{}]", Self::stage_label(summary.stage)))
                .cyan()
                .bold(),
            summary.applied,
            summary.skipped,
            summary.failed
        ));
    }

    fn print_plan(&self, plan: &Plan) {
        let out = self.out.lock().unwrap();
        let _ = out.write_line(&format!(
            "{} {}",
            style(self.prefix()).magenta().bold(),
            style("DRY RUN - computed plan:").bold()
        ));
        for action in &plan.file_actions {
            let _ = match action {
                FileAction::Skip { item_name, reason } => out.write_line(&format!(
                    "{} [file] ⊘ skip   {item_name} ({})",
                    self.prefix(),
                    Self::skip_reason_label(reason)
                )),
                FileAction::Apply { item_name, dst, .. } => out.write_line(&format!(
                    "{} [file] ✓ apply  {item_name} -> {dst}",
                    self.prefix()
                )),
                FileAction::ApplyDir {
                    item_name,
                    dst_dir,
                    files,
                    ..
                } => out.write_line(&format!(
                    "{} [file] ✓ apply  {item_name} -> {dst_dir} (dir, {} files)",
                    self.prefix(),
                    files.len()
                )),
                FileAction::ApplyLink {
                    item_name,
                    dst,
                    target,
                } => out.write_line(&format!(
                    "{} [file] ✓ apply  {item_name} -> {dst} (link -> {target})",
                    self.prefix()
                )),
                FileAction::Failed { item_name, error } => out.write_line(&format!(
                    "{} [file] ✗ fail   {item_name} ({error})",
                    self.prefix()
                )),
            };
        }
        for action in &plan.script_actions {
            let _ = match action {
                ScriptAction::Skip { item_name, reason } => out.write_line(&format!(
                    "{} [script] ⊘ skip   {item_name} ({})",
                    self.prefix(),
                    Self::skip_reason_label(reason)
                )),
                ScriptAction::Run { item_name, .. } => {
                    out.write_line(&format!("{} [script] ✓ run    {item_name}", self.prefix()))
                }
                ScriptAction::Failed { item_name, error } => out.write_line(&format!(
                    "{} [script] ✗ fail   {item_name} ({error})",
                    self.prefix()
                )),
            };
        }
        for action in &plan.block_actions {
            let _ = match action {
                BlockAction::Skip { item_name, reason } => out.write_line(&format!(
                    "{} [block] ⊘ skip   {item_name} ({})",
                    self.prefix(),
                    Self::skip_reason_label(reason)
                )),
                BlockAction::Apply {
                    item_name, target, ..
                } => out.write_line(&format!(
                    "{} [block] ✓ apply  {item_name} -> {target}",
                    self.prefix()
                )),
                BlockAction::Failed { item_name, error } => out.write_line(&format!(
                    "{} [block] ✗ fail   {item_name} ({error})",
                    self.prefix()
                )),
            };
        }
    }

    fn pipeline_summary(&self, summary: &PipelineSummary) {
        let out = self.out.lock().unwrap();
        let _ = out.write_line(&format!(
            "{} {}",
            style(self.prefix()).magenta().bold(),
            style("=== summary ===").bold()
        ));
        for stage in &summary.stages {
            let _ = out.write_line(&format!(
                "{} {}: applied={}, skipped={}, failed={}",
                self.prefix(),
                Self::stage_label(stage.stage),
                stage.applied,
                stage.skipped,
                stage.failed
            ));
        }
    }

    fn warning(&self, msg: &str) {
        let _ = self.out.lock().unwrap().write_line(&format!(
            "{} {}",
            style(self.prefix()).magenta().bold(),
            msg
        ));
    }

    fn info(&self, msg: &str) {
        let _ = self.out.lock().unwrap().write_line(&format!(
            "{} {}",
            style(self.prefix()).magenta().bold(),
            msg
        ));
    }
}
