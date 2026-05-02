use flux::config::loader::interpolate;
use proptest::prelude::*;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    #[test]
    fn env_interpolation_honors_dollar_escape(
        prefix in r"[^$\x00]{0,24}",
        suffix in r"[^$\x00]{0,24}",
    ) {
        std::env::remove_var("FLUX_PROP_ESCAPED");
        let rendered = interpolate(&format!("{prefix}$${{FLUX_PROP_ESCAPED:-fallback}}{suffix}")).unwrap();
        prop_assert_eq!(rendered, format!("{prefix}${{FLUX_PROP_ESCAPED:-fallback}}{suffix}"));
    }

    #[test]
    fn env_interpolation_uses_default_for_missing_var(
        default in r"[A-Za-z0-9_./-]{0,24}",
    ) {
        std::env::remove_var("FLUX_PROP_DEFAULT");
        let rendered = interpolate(&format!("${{FLUX_PROP_DEFAULT:-{default}}}")).unwrap();
        prop_assert_eq!(rendered, default);
    }
}
