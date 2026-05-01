//! Default reporter: write to stdout/stderr with `console` styling.

use super::{ItemOutcome, PipelineSummary, Reporter, Stage, StageSummary};
use crate::sync::plan::{BlockAction, FileAction, Plan, ScriptAction, SkipReason};
use console::{style, Term};
use std::sync::Mutex;

pub struct ConsoleReporter {
    out: Mutex<Term>,
}

impl ConsoleReporter {
    pub fn new() -> Self {
        Self {
            out: Mutex::new(Term::stdout()),
        }
    }

    fn stage_label(stage: Stage) -> &'static str {
        match stage {
            Stage::File => "file",
            Stage::Script => "script",
            Stage::Block => "block",
            Stage::Pubkey => "pubkey",
        }
    }
}

impl Default for ConsoleReporter {
    fn default() -> Self {
        Self::new()
    }
}

impl Reporter for ConsoleReporter {
    fn stage_started(&self, stage: Stage, item_count: usize) {
        let label = Self::stage_label(stage);
        let _ = self.out.lock().unwrap().write_line(&format!(
            "{} {} {}",
            style(format!("[{label}]")).cyan().bold(),
            style("stage").dim(),
            style(format!("({item_count} items)")).dim()
        ));
    }

    fn item_started(&self, _stage: Stage, _name: &str) {}

    fn item_finished(&self, stage: Stage, name: &str, outcome: &ItemOutcome) {
        let label = Self::stage_label(stage);
        let mark = match outcome {
            ItemOutcome::Applied => style("✓ apply").green(),
            ItemOutcome::Skipped(_) => style("⊘ skip").yellow(),
            ItemOutcome::Failed(_) => style("✗ fail").red(),
        };
        let detail = match outcome {
            ItemOutcome::Skipped(reason) => format!(" ({})", skip_reason_label(reason)),
            ItemOutcome::Failed(error) => format!(" ({error})"),
            ItemOutcome::Applied => String::new(),
        };
        let _ = self
            .out
            .lock()
            .unwrap()
            .write_line(&format!("  [{label}] {mark} {name}{detail}"));
    }

    fn stage_finished(&self, summary: &StageSummary) {
        let label = Self::stage_label(summary.stage);
        let _ = self.out.lock().unwrap().write_line(&format!(
            "{} done: applied={}, skipped={}, failed={}",
            style(format!("[{label}]")).cyan().bold(),
            summary.applied,
            summary.skipped,
            summary.failed
        ));
    }

    fn print_plan(&self, plan: &Plan) {
        let _ = self
            .out
            .lock()
            .unwrap()
            .write_line(&style("DRY RUN - computed plan:").bold().to_string());
        for action in &plan.file_actions {
            print_file_action(self, action);
        }
        for action in &plan.script_actions {
            print_script_action(self, action);
        }
        for action in &plan.block_actions {
            print_block_action(self, action);
        }
    }

    fn pipeline_summary(&self, summary: &PipelineSummary) {
        let out = self.out.lock().unwrap();
        let _ = out.write_line(&style("=== summary ===").bold().to_string());
        for stage in &summary.stages {
            let _ = out.write_line(&format!(
                "  {}: applied={}, skipped={}, failed={}",
                Self::stage_label(stage.stage),
                stage.applied,
                stage.skipped,
                stage.failed
            ));
        }
        let total_failed: usize = summary.stages.iter().map(|s| s.failed).sum();
        if total_failed > 0 {
            let _ = out.write_line(
                &style(format!("{total_failed} item(s) failed"))
                    .red()
                    .to_string(),
            );
        }
    }

    fn warning(&self, msg: &str) {
        let _ = self.out.lock().unwrap().write_line(&format!(
            "{} {}",
            style("[warn]").yellow().bold(),
            msg
        ));
    }

    fn info(&self, msg: &str) {
        let _ = self.out.lock().unwrap().write_line(&format!(
            "{} {}",
            style("[flux]").cyan().bold(),
            msg
        ));
    }
}

fn skip_reason_label(reason: &SkipReason) -> String {
    match reason {
        SkipReason::AlreadyExists => "already exists".into(),
        SkipReason::RemoteNewer => "remote newer".into(),
        SkipReason::ContentUnchanged => "content unchanged".into(),
        SkipReason::DependencyFailed(dep) => format!("dep {dep} failed"),
    }
}

fn print_file_action(reporter: &ConsoleReporter, action: &FileAction) {
    let out = reporter.out.lock().unwrap();
    let _ = match action {
        FileAction::Skip { item_name, reason } => out.write_line(&format!(
            "  [file] ⊘ skip   {item_name} ({})",
            skip_reason_label(reason)
        )),
        FileAction::Apply {
            item_name,
            dst,
            chmod,
            ..
        } => out.write_line(&format!(
            "  [file] ✓ apply  {item_name} -> {dst}{}",
            chmod
                .map(|mode| format!(" chmod={mode:o}"))
                .unwrap_or_default()
        )),
        FileAction::Failed { item_name, error } => {
            out.write_line(&format!("  [file] ✗ fail   {item_name} ({error})"))
        }
    };
}

fn print_script_action(reporter: &ConsoleReporter, action: &ScriptAction) {
    let out = reporter.out.lock().unwrap();
    let _ = match action {
        ScriptAction::Skip { item_name, reason } => out.write_line(&format!(
            "  [script] ⊘ skip   {item_name} ({})",
            skip_reason_label(reason)
        )),
        ScriptAction::Run { item_name, .. } => {
            out.write_line(&format!("  [script] ✓ run    {item_name}"))
        }
        ScriptAction::Failed { item_name, error } => {
            out.write_line(&format!("  [script] ✗ fail   {item_name} ({error})"))
        }
    };
}

fn print_block_action(reporter: &ConsoleReporter, action: &BlockAction) {
    let out = reporter.out.lock().unwrap();
    let _ = match action {
        BlockAction::Skip { item_name, reason } => out.write_line(&format!(
            "  [block] ⊘ skip   {item_name} ({})",
            skip_reason_label(reason)
        )),
        BlockAction::Apply {
            item_name, target, ..
        } => out.write_line(&format!("  [block] ✓ apply  {item_name} -> {target}")),
        BlockAction::Failed { item_name, error } => {
            out.write_line(&format!("  [block] ✗ fail   {item_name} ({error})"))
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::plan::Plan;

    #[test]
    fn console_does_not_panic_on_empty_plan() {
        let reporter = ConsoleReporter::new();
        reporter.print_plan(&Plan::default());
    }

    #[test]
    fn skip_reason_label_covers_all_variants() {
        for reason in [
            SkipReason::AlreadyExists,
            SkipReason::RemoteNewer,
            SkipReason::ContentUnchanged,
            SkipReason::DependencyFailed("x".into()),
        ] {
            let rendered = skip_reason_label(&reason);
            assert!(!rendered.is_empty());
        }
    }
}
