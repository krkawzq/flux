use flux::remote::ssh::shell_escape;
use proptest::prelude::*;

fn decode_single_quoted_shell(input: &str) -> String {
    let inner = &input[1..input.len() - 1];
    inner.replace("'\\''", "'")
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    #[test]
    fn shell_escape_round_trips_arbitrary_strings(input in r"[^\x00]{0,64}") {
        let escaped = shell_escape(&input);
        prop_assert!(escaped.starts_with('\''));
        prop_assert!(escaped.ends_with('\''));
        prop_assert_eq!(decode_single_quoted_shell(&escaped), input);
    }
}
