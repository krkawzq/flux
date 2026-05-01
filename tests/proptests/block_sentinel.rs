use flux::sync::block::{build_markers, find_block};
use proptest::prelude::*;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn round_trip_arbitrary_body(
        name in r"[a-z][a-z0-9_]{0,15}",
        body in r"[^\x00]{0,200}",
    ) {
        let (open, close) = build_markers("# {}", &name, 100).unwrap();
        let body_norm = if body.ends_with('\n') {
            body.clone()
        } else {
            format!("{body}\n")
        };
        let content = format!("preamble\n{open}\n{body_norm}{close}\nepilogue\n");
        let found = find_block("# {}", &name, &content).unwrap().unwrap();
        let captured = &content[found.byte_range.clone()];
        prop_assert!(captured.starts_with(&open));
        prop_assert!(captured.trim_end().ends_with(&close));
    }
}
