use flux::config::{BlockItem, Config, ProxyConfig, SyncMode};
use flux::remote::fake::InMemoryRemote;
use flux::reporter::memory::CapturedReporter;
use flux::sync::{Pipeline, PipelineOpts};
use tempfile::TempDir;

fn config_with_block() -> Config {
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
        file: vec![],
        script: vec![],
        block: vec![BlockItem {
            name: "aliases".into(),
            path: "aliases.sh".into(),
            file: ":/remote/.bashrc".into(),
            mode: SyncMode::Sync,
            comment_template: None,
            tags: vec![],
        }],
    }
}

#[tokio::test]
async fn pipeline_block_applies_block_stage_end_to_end() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join("aliases.sh"), "alias ll='ls -la'\n").unwrap();
    let remote = InMemoryRemote::with_files([("/remote/.bashrc", b"export PATH=/bin\n".to_vec())]);
    let reporter = CapturedReporter::new();
    let pipe = Pipeline {
        config: &config_with_block(),
        asset_root: tmp.path(),
        remote: &remote,
        reporter: &reporter,
        opts: PipelineOpts::default(),
    };

    let summary = pipe.run().await;

    assert_eq!(summary.total_failed(), 0);
    let content = String::from_utf8(remote.file_contents("/remote/.bashrc").unwrap()).unwrap();
    assert!(content.contains("# >>> aliases:"));
    assert!(content.contains("alias ll='ls -la'"));
    assert!(content.contains("# <<< aliases:"));
}
