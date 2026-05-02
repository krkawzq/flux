use flux::config::{Config, FileItem, ItemKind, ProxyConfig, SyncMode};
use flux::remote::fake::InMemoryRemote;
use flux::reporter::memory::CapturedReporter;
use flux::sync::plan::SkipReason;
use flux::sync::{Pipeline, PipelineOpts};
use tempfile::TempDir;

fn config_with_files(a: String, b: String) -> Config {
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
        file: vec![
            FileItem {
                name: Some("first".into()),
                src: a,
                dst: ":/remote/a".into(),
                kind: ItemKind::File,
                target: None,
                mode: SyncMode::Cover,
                chmod: None,
                tags: vec![],
            },
            FileItem {
                name: Some("second".into()),
                src: b,
                dst: ":/remote/b".into(),
                kind: ItemKind::File,
                target: None,
                mode: SyncMode::Cover,
                chmod: None,
                tags: vec![],
            },
        ],
        script: vec![],
        block: vec![],
    }
}

#[tokio::test]
async fn pipeline_resume_starts_from_failed_item() {
    let tmp = TempDir::new().unwrap();
    let a = tmp.path().join("a");
    let b = tmp.path().join("b");
    std::fs::write(&a, "a").unwrap();
    std::fs::write(&b, "b").unwrap();
    let config = config_with_files(
        a.to_string_lossy().into_owned(),
        b.to_string_lossy().into_owned(),
    );
    let remote = InMemoryRemote::new();
    let reporter = CapturedReporter::new();
    let pipe = Pipeline {
        config: &config,
        asset_root: tmp.path(),
        remote: &remote,
        reporter: &reporter,
        opts: PipelineOpts {
            resume_from: Some("second".into()),
            ..PipelineOpts::default()
        },
    };

    let plan = pipe.plan().await;

    assert!(matches!(
        &plan.file_actions[0],
        flux::sync::plan::FileAction::Skip {
            reason: SkipReason::PreviouslyApplied,
            ..
        }
    ));
    assert!(matches!(
        &plan.file_actions[1],
        flux::sync::plan::FileAction::Apply { .. }
    ));
}
