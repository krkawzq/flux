use flux::config::Config;
use flux::remote::fake::InMemoryRemote;
use flux::reporter::memory::CapturedReporter;
use flux::sync::{Pipeline, PipelineOpts};
use tempfile::TempDir;

#[tokio::test]
async fn end_to_end_minimal_config() {
    let yaml = std::fs::read_to_string("tests/fixtures/westlake_minimal.yml").unwrap();
    let cfg: Config = serde_yml::from_str(&yaml).unwrap();
    let tmp = TempDir::new().unwrap();
    let remote = InMemoryRemote::new();
    let reporter = CapturedReporter::new();
    let pipe = Pipeline {
        config: &cfg,
        asset_root: tmp.path(),
        remote: &remote,
        reporter: &reporter,
        opts: PipelineOpts::default(),
    };
    let summary = pipe.run().await;
    assert_eq!(summary.total_failed(), 0);
}
