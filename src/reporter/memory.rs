//! In-memory reporter for tests - captures every event as a structured value.

use super::{ItemOutcome, PipelineSummary, Reporter, Stage, StageSummary};
use crate::sync::plan::Plan;
use std::sync::Mutex;

#[derive(Debug, Clone)]
pub enum CapturedEvent {
    StageStarted {
        stage: Stage,
        items: usize,
    },
    ItemStarted {
        stage: Stage,
        name: String,
    },
    ItemFinished {
        stage: Stage,
        name: String,
        outcome: String,
    },
    StageFinished(StageSummary),
    PrintPlan,
    PipelineSummary(PipelineSummary),
    Warning(String),
    Info(String),
}

#[derive(Default)]
pub struct CapturedReporter {
    pub events: Mutex<Vec<CapturedEvent>>,
}

impl CapturedReporter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn events(&self) -> Vec<CapturedEvent> {
        self.events.lock().unwrap().clone()
    }

    pub fn applied_count(&self, stage: Stage) -> usize {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter(|event| {
                matches!(
                    event,
                    CapturedEvent::ItemFinished { stage: s, outcome, .. }
                        if *s == stage && outcome == "applied"
                )
            })
            .count()
    }

    pub fn failed_items(&self, stage: Stage) -> Vec<String> {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter_map(|event| match event {
                CapturedEvent::ItemFinished {
                    stage: s,
                    name,
                    outcome,
                } if *s == stage && outcome.starts_with("failed:") => Some(name.clone()),
                _ => None,
            })
            .collect()
    }
}

fn outcome_label(outcome: &ItemOutcome) -> String {
    match outcome {
        ItemOutcome::Applied => "applied".into(),
        ItemOutcome::Skipped(_) => "skipped".into(),
        ItemOutcome::Failed(error) => format!("failed:{error}"),
    }
}

impl Reporter for CapturedReporter {
    fn stage_started(&self, stage: Stage, items: usize) {
        self.events
            .lock()
            .unwrap()
            .push(CapturedEvent::StageStarted { stage, items });
    }

    fn item_started(&self, stage: Stage, name: &str) {
        self.events
            .lock()
            .unwrap()
            .push(CapturedEvent::ItemStarted {
                stage,
                name: name.into(),
            });
    }

    fn item_finished(&self, stage: Stage, name: &str, outcome: &ItemOutcome) {
        self.events
            .lock()
            .unwrap()
            .push(CapturedEvent::ItemFinished {
                stage,
                name: name.into(),
                outcome: outcome_label(outcome),
            });
    }

    fn stage_finished(&self, summary: &StageSummary) {
        self.events
            .lock()
            .unwrap()
            .push(CapturedEvent::StageFinished(summary.clone()));
    }

    fn print_plan(&self, _plan: &Plan) {
        self.events.lock().unwrap().push(CapturedEvent::PrintPlan);
    }

    fn pipeline_summary(&self, summary: &PipelineSummary) {
        self.events
            .lock()
            .unwrap()
            .push(CapturedEvent::PipelineSummary(summary.clone()));
    }

    fn warning(&self, msg: &str) {
        self.events
            .lock()
            .unwrap()
            .push(CapturedEvent::Warning(msg.into()));
    }

    fn info(&self, msg: &str) {
        self.events
            .lock()
            .unwrap()
            .push(CapturedEvent::Info(msg.into()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn applied_count_tallies() {
        let reporter = CapturedReporter::new();
        reporter.item_finished(Stage::File, "a", &ItemOutcome::Applied);
        reporter.item_finished(Stage::File, "b", &ItemOutcome::Failed("err".into()));
        reporter.item_finished(Stage::File, "c", &ItemOutcome::Applied);
        assert_eq!(reporter.applied_count(Stage::File), 2);
        assert_eq!(reporter.failed_items(Stage::File), vec!["b"]);
    }
}
