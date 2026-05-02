//! Default reporter: write to stdout/stderr with `console` styling.

use super::{ItemOutcome, PipelineSummary, Reporter, Stage, StageSummary};
use crate::remote::{RemoteOps, RemoteOpsError};
use crate::sync::plan::{BlockAction, FileAction, Plan, ScriptAction, SkipReason};
use console::{style, Term};
use similar::TextDiff;
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

pub async fn print_plan_with_diff<R: RemoteOps + ?Sized>(
    plan: &Plan,
    remote: &R,
    reporter: &dyn Reporter,
) {
    let term = Term::stdout();
    let _ = term.write_line(
        &style("DRY RUN - computed plan with diff:")
            .bold()
            .to_string(),
    );
    for action in &plan.file_actions {
        print_file_action_line(&term, action);
        if let FileAction::Apply { src, dst, .. } = action {
            let local = match std::fs::read_to_string(src) {
                Ok(text) => text,
                Err(err) => {
                    reporter.warning(&format!("failed to read {} for diff: {err}", src.display()));
                    continue;
                }
            };
            let remote_text = match remote.read_file(dst).await {
                Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
                Err(RemoteOpsError::NotFound(_)) => String::new(),
                Err(err) => {
                    reporter.warning(&format!("failed to read remote {dst} for diff: {err}"));
                    continue;
                }
            };
            print_unified_diff(&term, dst, &remote_text, &local);
        } else if let FileAction::ApplyDir { files, dst_dir, .. } = action {
            for (src, relative_dst) in files {
                let local = match std::fs::read_to_string(src) {
                    Ok(text) => text,
                    Err(err) => {
                        reporter
                            .warning(&format!("failed to read {} for diff: {err}", src.display()));
                        continue;
                    }
                };
                let dst = join_remote_path(dst_dir, relative_dst);
                let remote_text = match remote.read_file(&dst).await {
                    Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
                    Err(RemoteOpsError::NotFound(_)) => String::new(),
                    Err(err) => {
                        reporter.warning(&format!("failed to read remote {dst} for diff: {err}"));
                        continue;
                    }
                };
                print_unified_diff(&term, &dst, &remote_text, &local);
            }
        }
    }
    for action in &plan.script_actions {
        print_script_action_line(&term, action);
    }
    for action in &plan.block_actions {
        print_block_action_line(&term, action);
        if let BlockAction::Apply {
            target,
            body,
            sentinel,
            ..
        } = action
        {
            let remote_text = match remote.read_file(target).await {
                Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
                Err(RemoteOpsError::NotFound(_)) => String::new(),
                Err(err) => {
                    reporter.warning(&format!("failed to read remote {target} for diff: {err}"));
                    continue;
                }
            };
            let existing_body =
                extract_existing_block_body(&remote_text, &sentinel.name).unwrap_or_default();
            print_unified_diff(&term, target, &existing_body, body);
        }
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
            ItemOutcome::Failed(error) => format!(" ({})", error),
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
        let out = self.out.lock().unwrap();
        let _ = out.write_line(&style("DRY RUN - computed plan:").bold().to_string());
        for action in &plan.file_actions {
            print_file_action_line(&out, action);
        }
        for action in &plan.script_actions {
            print_script_action_line(&out, action);
        }
        for action in &plan.block_actions {
            print_block_action_line(&out, action);
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
        SkipReason::FilteredOut => "filtered out".into(),
    }
}

fn print_file_action_line(out: &Term, action: &FileAction) {
    let _ = match action {
        FileAction::Skip { item_name, reason } => out.write_line(&format!(
            "  [file] ⊘ skip   {item_name} ({})",
            skip_reason_label(reason)
        )),
        FileAction::Apply {
            item_name,
            len,
            dst,
            chmod,
            ..
        } => out.write_line(&format!(
            "  [file] ✓ apply  {item_name} -> {dst} ({len} bytes){}",
            chmod
                .map(|mode| format!(" chmod={mode:o}"))
                .unwrap_or_default()
        )),
        FileAction::ApplyDir {
            item_name,
            dst_dir,
            files,
            chmod,
            ..
        } => out.write_line(&format!(
            "  [file] ✓ apply  {item_name} -> {dst_dir} (dir, {} files){}",
            files.len(),
            chmod
                .map(|mode| format!(" chmod={mode:o}"))
                .unwrap_or_default()
        )),
        FileAction::ApplyLink {
            item_name,
            dst,
            target,
        } => out.write_line(&format!(
            "  [file] ✓ apply  {item_name} -> {dst} (link -> {target})"
        )),
        FileAction::Failed { item_name, error } => {
            out.write_line(&format!("  [file] ✗ fail   {item_name} ({error})"))
        }
    };
}

fn join_remote_path(base: &str, relative: &str) -> String {
    if base == "/" {
        format!("/{relative}")
    } else {
        format!("{}/{}", base.trim_end_matches('/'), relative)
    }
}

fn print_script_action_line(out: &Term, action: &ScriptAction) {
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

fn print_block_action_line(out: &Term, action: &BlockAction) {
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

fn print_unified_diff(out: &Term, label: &str, old: &str, new: &str) {
    let diff = TextDiff::from_lines(old, new);
    for line in diff
        .unified_diff()
        .context_radius(3)
        .header(label, label)
        .to_string()
        .lines()
    {
        let rendered = if line.starts_with('+') && !line.starts_with("+++") {
            style(line).green().to_string()
        } else if line.starts_with('-') && !line.starts_with("---") {
            style(line).red().to_string()
        } else if line.starts_with("@@") {
            style(line).cyan().to_string()
        } else {
            line.to_string()
        };
        let _ = out.write_line(&format!("    {rendered}"));
    }
}

fn extract_existing_block_body(content: &str, name: &str) -> Option<String> {
    let mut in_block = false;
    let mut body = String::new();
    for line in content.split_inclusive('\n') {
        let trimmed = line.trim_end_matches(['\n', '\r']);
        if !in_block {
            if trimmed.contains(&format!(">>> {name}:")) && trimmed.ends_with(" >>>") {
                in_block = true;
            }
            continue;
        }
        if trimmed.contains(&format!("<<< {name}:")) && trimmed.ends_with(" <<<") {
            return Some(body);
        }
        body.push_str(line);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote::fake::InMemoryRemote;
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
            SkipReason::FilteredOut,
        ] {
            let rendered = skip_reason_label(&reason);
            assert!(!rendered.is_empty());
        }
    }

    #[tokio::test]
    async fn diff_prints_changed_file_lines() {
        let remote = InMemoryRemote::with_files([("/r/a.txt", b"old\n".to_vec())]);
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("a.txt");
        std::fs::write(&path, "new\n").unwrap();
        let plan = Plan {
            register_pubkey: None,
            file_actions: vec![FileAction::Apply {
                item_name: "a".into(),
                src: path,
                dst: "/r/a.txt".into(),
                len: 4,
                chmod: None,
            }],
            script_actions: vec![],
            block_actions: vec![],
        };
        let reporter = ConsoleReporter::new();
        print_plan_with_diff(&plan, &remote, &reporter).await;
    }
}
