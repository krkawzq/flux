use flux::config::{BlockItem, FileItem, ItemKind, ScriptItem, SyncMode};
use flux::sync::{block, file, script};
use proptest::prelude::*;
use tempfile::TempDir;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(32))]

    #[test]
    fn file_cache_key_changes_when_impact_fields_change(
        body in r"[^\x00]{0,32}",
    ) {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("a.txt");
        std::fs::write(&path, body.as_bytes()).unwrap();
        let base = FileItem {
            name: Some("file".into()),
            src: path.to_string_lossy().into_owned(),
            dst: ":/r/a.txt".into(),
            kind: ItemKind::File,
            target: None,
            mode: SyncMode::Cover,
            chmod: Some("600".into()),
            tags: vec![],
        };
        let mut changed = base.clone();
        changed.dst = ":/r/b.txt".into();
        let base_hashes = file::collect_item_hashes(&[base]);
        let changed_hashes = file::collect_item_hashes(&[changed]);
        prop_assert_ne!(
            base_hashes.get("file"),
            changed_hashes.get("file")
        );
    }

    #[test]
    fn block_cache_key_changes_when_target_or_template_changes(
        body in r"[^\x00]{0,32}",
    ) {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("block.sh"), body.as_bytes()).unwrap();
        let base = BlockItem {
            name: "block".into(),
            path: "block.sh".into(),
            file: ":/r/a".into(),
            mode: SyncMode::Sync,
            comment_template: None,
            tags: vec![],
        };
        let mut changed = base.clone();
        changed.file = ":/r/b".into();
        let base_hashes = block::collect_item_hashes(&[base], tmp.path(), "# {}");
        let changed_hashes = block::collect_item_hashes(&[changed], tmp.path(), "# {}");
        prop_assert_ne!(
            base_hashes.get("block"),
            changed_hashes.get("block")
        );
    }

    #[test]
    fn script_cache_key_changes_when_interpreter_flags_or_args_change(
        body in r"[^\x00]{0,32}",
    ) {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("script.sh"), body.as_bytes()).unwrap();
        let base = ScriptItem {
            path: "script.sh".into(),
            interpreter: Some("/bin/bash".into()),
            flags: Some(vec!["-eu".into()]),
            args: vec!["one".into()],
            tags: vec![],
        };
        let mut changed = base.clone();
        changed.args.push("two".into());
        let base_hashes = script::collect_item_hashes(&[base], tmp.path(), "/bin/bash", &[]);
        let changed_hashes = script::collect_item_hashes(&[changed], tmp.path(), "/bin/bash", &[]);
        prop_assert_ne!(
            base_hashes.get("script.sh"),
            changed_hashes.get("script.sh")
        );
    }
}
