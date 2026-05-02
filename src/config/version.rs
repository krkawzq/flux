//! Schema version probing & migration.

use serde::Deserialize;

pub const CURRENT_SCHEMA_VERSION: u32 = 2;

#[derive(Deserialize)]
struct VersionProbe {
    #[serde(default = "default_version")]
    version: u32,
}

fn default_version() -> u32 {
    1
}

#[derive(Debug, thiserror::Error)]
pub enum VersionError {
    #[error("unsupported config schema version {found}; this build supports up to {max}")]
    Unsupported { found: u32, max: u32 },
    #[error("yaml parse: {0}")]
    Yaml(#[from] serde_yml::Error),
}

pub fn probe_version(yaml: &str) -> Result<u32, VersionError> {
    let probe: VersionProbe = serde_yml::from_str(yaml)?;
    if probe.version > CURRENT_SCHEMA_VERSION {
        return Err(VersionError::Unsupported {
            found: probe.version,
            max: CURRENT_SCHEMA_VERSION,
        });
    }
    Ok(probe.version)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_version_defaults_to_1() {
        let version = probe_version("host: x").unwrap();
        assert_eq!(version, 1);
    }

    #[test]
    fn explicit_version_1_ok() {
        let version = probe_version("version: 1\nhost: x").unwrap();
        assert_eq!(version, 1);
    }

    #[test]
    fn future_version_errors() {
        let err = probe_version("version: 999\nhost: x").unwrap_err();
        assert!(matches!(err, VersionError::Unsupported { found: 999, .. }));
    }
}
