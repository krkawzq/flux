use flux::config::{Config, FileItem, ItemKind, ProxyConfig, SyncMode};
use flux::remote::fake::InMemoryRemote;
use flux::reporter::memory::CapturedReporter;
use flux::sync::{Pipeline, PipelineOpts};
use tempfile::TempDir;

fn config_with_file(src: String) -> Config {
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
            tags: vec![],
        }],
        script: vec![],
        block: vec![],
    }
}

#[tokio::test]
async fn pipeline_dry_run_does_not_write_remote_or_state() {
    let tmp = TempDir::new().unwrap();
    let state_dir = TempDir::new().unwrap();
    std::env::set_var("FLUX_STATE_DIR", state_dir.path());
    let src = tmp.path().join("zshrc");
    std::fs::write(&src, "export PATH=/bin\n").unwrap();
    let config = config_with_file(src.to_string_lossy().into_owned());
    let remote = InMemoryRemote::new();
    let reporter = CapturedReporter::new();
    let pipe = Pipeline {
        config: &config,
        asset_root: tmp.path(),
        remote: &remote,
        reporter: &reporter,
        opts: PipelineOpts {
            dry_run: true,
            ..PipelineOpts::default()
        },
    };

    let summary = pipe.run().await;

    assert!(summary.dry_run);
    assert!(remote.write_calls().is_empty());
    assert!(std::fs::read_dir(state_dir.path())
        .unwrap()
        .next()
        .is_none());
}
