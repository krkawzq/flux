use flux::config::{Config, FileItem, ItemKind, ProxyConfig, SyncMode};
use flux::remote::fake::InMemoryRemote;
use flux::reporter::memory::CapturedReporter;
use flux::sync::{Pipeline, PipelineOpts};
use futures::StreamExt;
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
async fn pipeline_multi_host_fanout_runs_all_hosts() {
    let tmp = TempDir::new().unwrap();
    let src = tmp.path().join("zshrc");
    std::fs::write(&src, "export PATH=/bin\n").unwrap();
    let config = config_with_file(src.to_string_lossy().into_owned());

    let summaries = futures::stream::iter(["h1", "h2", "h3"])
        .map(|_| {
            let remote = InMemoryRemote::new();
            let reporter = CapturedReporter::new();
            let config = config.clone();
            let asset_root = tmp.path().to_path_buf();
            async move {
                let pipe = Pipeline {
                    config: &config,
                    asset_root: &asset_root,
                    remote: &remote,
                    reporter: &reporter,
                    opts: PipelineOpts::default(),
                };
                pipe.run().await
            }
        })
        .buffer_unordered(3)
        .collect::<Vec<_>>()
        .await;

    assert_eq!(summaries.len(), 3);
    assert!(summaries.iter().all(|summary| summary.total_failed() == 0));
}
