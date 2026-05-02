use flux::config::{Config, FileItem, ItemKind, ProxyConfig, ScriptItem, SyncMode};
use flux::remote::fake::InMemoryRemote;
use flux::reporter::memory::CapturedReporter;
use flux::reporter::Stage;
use flux::sync::plan::{FileAction, ScriptAction, SkipReason};
use flux::sync::{Pipeline, PipelineFilter, PipelineOpts};
use std::collections::HashSet;
use tempfile::TempDir;

fn config_with_file_and_script(src: String, script: String) -> Config {
    Config {
        version: 1,
        imports: vec![],
        host: Some("example".into()),
        port: None,
        user: None,
        key: None,
        password: None,
        register_key: false,
        interpreter: "/bin/bash".into(),
        flags: vec!["-i".into()],
        comment_template: "# {}".into(),
        flux_home: None,
        proxy: ProxyConfig::default(),
        file: vec![FileItem {
            name: Some("dotfile".into()),
            src,
            dst: ":/remote/.zshrc".into(),
            kind: ItemKind::File,
            target: None,
            mode: SyncMode::Cover,
            chmod: None,
            tags: vec!["dotfiles".into()],
        }],
        script: vec![ScriptItem {
            path: script,
            interpreter: None,
            flags: None,
            args: vec![],
            tags: vec!["setup".into()],
        }],
        block: vec![],
    }
}

#[tokio::test]
async fn pipeline_filter_only_stage_file_skips_script() {
    let tmp = TempDir::new().unwrap();
    let src = tmp.path().join("zshrc");
    let script = tmp.path().join("setup.sh");
    std::fs::write(&src, "export PATH=/bin\n").unwrap();
    std::fs::write(&script, "#!/bin/sh\necho hi\n").unwrap();
    let config = config_with_file_and_script(
        src.to_string_lossy().into_owned(),
        script.to_string_lossy().into_owned(),
    );
    let remote = InMemoryRemote::new();
    let reporter = CapturedReporter::new();
    let pipe = Pipeline {
        config: &config,
        asset_root: tmp.path(),
        remote: &remote,
        reporter: &reporter,
        opts: PipelineOpts {
            filter: PipelineFilter {
                only_stages: Some(HashSet::from([Stage::File])),
                ..PipelineFilter::default()
            },
            ..PipelineOpts::default()
        },
    };

    let plan = pipe.plan().await;

    assert!(matches!(&plan.file_actions[0], FileAction::Apply { .. }));
    assert!(matches!(
        &plan.script_actions[0],
        ScriptAction::Skip {
            reason: SkipReason::FilteredOut,
            ..
        }
    ));
}

#[tokio::test]
async fn pipeline_filter_tag_keeps_matching_items() {
    let tmp = TempDir::new().unwrap();
    let src = tmp.path().join("zshrc");
    let script = tmp.path().join("setup.sh");
    std::fs::write(&src, "export PATH=/bin\n").unwrap();
    std::fs::write(&script, "#!/bin/sh\necho hi\n").unwrap();
    let config = config_with_file_and_script(
        src.to_string_lossy().into_owned(),
        script.to_string_lossy().into_owned(),
    );
    let remote = InMemoryRemote::new();
    let reporter = CapturedReporter::new();
    let pipe = Pipeline {
        config: &config,
        asset_root: tmp.path(),
        remote: &remote,
        reporter: &reporter,
        opts: PipelineOpts {
            filter: PipelineFilter {
                tags: Some(HashSet::from([String::from("dotfiles")])),
                ..PipelineFilter::default()
            },
            ..PipelineOpts::default()
        },
    };

    let plan = pipe.plan().await;

    assert!(matches!(&plan.file_actions[0], FileAction::Apply { .. }));
    assert!(matches!(
        &plan.script_actions[0],
        ScriptAction::Skip {
            reason: SkipReason::FilteredOut,
            ..
        }
    ));
}
