//! Configuration models for flux.

pub mod loader;
pub mod version;

use anyhow::{Context, Result};
use serde::de::{Error as DeError, Unexpected};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::path::{Path, PathBuf};

/// Root configuration structure.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub imports: Vec<String>,
    pub host: Option<String>,
    pub port: Option<u16>,
    pub user: Option<String>,
    pub key: Option<String>,
    pub password: Option<SecretValue>,
    #[serde(default)]
    pub register_key: bool,
    #[serde(default = "default_interpreter")]
    pub interpreter: String,
    #[serde(default = "default_flags")]
    pub flags: Vec<String>,
    #[serde(default = "default_comment_template")]
    pub comment_template: String,
    pub flux_home: Option<PathBuf>,
    #[serde(default)]
    pub proxy: ProxyConfig,
    #[serde(default)]
    pub file: Vec<FileItem>,
    #[serde(default)]
    pub script: Vec<ScriptItem>,
    #[serde(default)]
    pub block: Vec<BlockItem>,
}

fn default_version() -> u32 {
    1
}

fn default_interpreter() -> String {
    if cfg!(windows) {
        "cmd".to_string()
    } else {
        "/bin/bash".to_string()
    }
}

fn default_flags() -> Vec<String> {
    vec!["-i".to_string()]
}

fn default_comment_template() -> String {
    "# {}".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct ProxyConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_local_port")]
    pub local_port: u16,
    #[serde(default = "default_remote_port")]
    pub remote_port: u16,
    #[serde(default = "default_protocol")]
    pub protocol: ProxyProtocol,
}

fn default_local_port() -> u16 {
    7899
}

fn default_remote_port() -> u16 {
    7890
}

fn default_protocol() -> ProxyProtocol {
    ProxyProtocol::default()
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ProxyProtocol {
    #[default]
    Http,
    Socks5,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ItemKind {
    #[default]
    Auto,
    File,
    Dir,
    Glob,
    Link,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileItem {
    pub name: Option<String>,
    pub src: String,
    pub dst: String,
    #[serde(default)]
    pub kind: ItemKind,
    pub target: Option<String>,
    #[serde(default)]
    pub mode: SyncMode,
    pub chmod: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScriptItem {
    pub path: String,
    pub interpreter: Option<String>,
    pub flags: Option<Vec<String>>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BlockItem {
    pub name: String,
    pub path: String,
    pub file: String,
    #[serde(default)]
    pub mode: SyncMode,
    pub comment_template: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum SyncMode {
    Cover,
    #[default]
    Sync,
    Touch,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SecretValue {
    Inline(String),
    FromKeychain(String),
}

impl Serialize for SecretValue {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Self::Inline(value) => serializer.serialize_str(value),
            Self::FromKeychain(spec) => serializer.serialize_str(&format!("keychain:{spec}")),
        }
    }
}

impl<'de> Deserialize<'de> for SecretValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = match serde_yml::Value::deserialize(deserializer)? {
            serde_yml::Value::String(value) => value,
            _ => {
                return Err(D::Error::invalid_type(
                    Unexpected::Other("non-string"),
                    &"a string secret or keychain:<service>.<account>",
                ))
            }
        };
        if let Some(spec) = raw.strip_prefix("keychain:") {
            validate_keychain_spec(spec).map_err(D::Error::custom)?;
            Ok(Self::FromKeychain(spec.to_string()))
        } else {
            Ok(Self::Inline(raw))
        }
    }
}

#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum SecretError {
    #[error("invalid keychain spec '{0}'; expected service.account")]
    InvalidKeychainSpec(String),
    #[error("keychain secret not found for {0}")]
    NotFound(String),
    #[error("keychain command failed for {0}: {1}")]
    CommandFailed(String, String),
    #[error("keychain io for {0}: {1}")]
    Io(String, String),
    #[error("keychain lookup is not supported on this platform")]
    UnsupportedPlatform,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct KeychainRef<'a> {
    service: &'a str,
    account: &'a str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CommandOutput {
    success: bool,
    stdout: String,
    stderr: String,
}

impl SecretValue {
    pub fn resolve(&self) -> Result<String, SecretError> {
        match self {
            Self::Inline(value) => Ok(value.clone()),
            Self::FromKeychain(spec) => resolve_keychain_value(spec),
        }
    }
}

impl Config {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let content = loader::load_with_imports(path)
            .map_err(|err| anyhow::anyhow!("{err}"))
            .with_context(|| format!("failed to load config file {}", path.display()))?;
        version::probe_version(&content).map_err(|err| anyhow::anyhow!("{err}"))?;
        let config: Config = serde_yml::from_str(&content)
            .with_context(|| format!("failed to parse config file {}", path.display()))?;
        Ok(config)
    }

    pub fn find_and_load(name_or_path: &str) -> anyhow::Result<(Self, PathBuf)> {
        let path = Self::find_config(name_or_path)?;
        let config = Self::load(&path)?;
        Ok((config, path))
    }

    pub fn find_config(name_or_path: &str) -> anyhow::Result<PathBuf> {
        let path = PathBuf::from(name_or_path);
        if path.exists() {
            return Ok(path);
        }

        let search_dirs = vec![
            std::env::current_dir()?.join(".flux"),
            dirs::home_dir()
                .ok_or_else(|| anyhow::anyhow!("Cannot find home directory"))?
                .join(".flux"),
        ];

        for dir in search_dirs {
            for ext in ["yml", "yaml"] {
                let file_path = dir.join(format!("{}.{}", name_or_path, ext));
                if file_path.exists() {
                    return Ok(file_path);
                }
            }
        }

        anyhow::bail!(
            "Configuration '{}' not found. Searched:\n  - {}\n  - ./.flux/{}.yml\n  - ~/.flux/{}.yml",
            name_or_path,
            name_or_path,
            name_or_path,
            name_or_path
        )
    }

    pub fn resolve_root(&self, config_path: &Path) -> PathBuf {
        let config_dir = config_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from(".flux"));

        match &self.flux_home {
            Some(flux_home) => resolve_path_root(flux_home, &config_dir),
            None => config_dir,
        }
    }

    pub fn validate(&self) -> Result<()> {
        if self.register_key && self.key.as_deref().is_none_or(str::is_empty) {
            anyhow::bail!("register_key=true requires non-empty key");
        }
        if self.proxy.local_port == 0 {
            anyhow::bail!("proxy.local_port must be > 0");
        }
        for file in &self.file {
            validate_chmod(file.chmod.as_deref(), &file.src)?;
        }
        for block in &self.block {
            if block.file.is_empty() {
                anyhow::bail!("block '{}' has empty target file", block.name);
            }
        }
        Ok(())
    }
}

fn validate_chmod(raw: Option<&str>, context: &str) -> Result<()> {
    if let Some(value) = raw {
        u32::from_str_radix(value, 8)
            .map_err(|_| anyhow::anyhow!("invalid chmod '{value}' for '{context}'"))?;
    }
    Ok(())
}

fn resolve_keychain_value(spec: &str) -> Result<String, SecretError> {
    let keychain_ref = parse_keychain_ref(spec)?;
    resolve_keychain_with_runner(spec, &keychain_ref, run_keychain_command)
}

fn parse_keychain_ref(spec: &str) -> Result<KeychainRef<'_>, SecretError> {
    validate_keychain_spec(spec)?;
    let (service, account) = spec
        .split_once('.')
        .ok_or_else(|| SecretError::InvalidKeychainSpec(spec.to_string()))?;
    Ok(KeychainRef { service, account })
}

fn validate_keychain_spec(spec: &str) -> Result<(), SecretError> {
    let Some((service, account)) = spec.split_once('.') else {
        return Err(SecretError::InvalidKeychainSpec(spec.to_string()));
    };
    if service.is_empty() || account.is_empty() {
        return Err(SecretError::InvalidKeychainSpec(spec.to_string()));
    }
    Ok(())
}

fn resolve_keychain_with_runner<F>(
    spec: &str,
    keychain_ref: &KeychainRef<'_>,
    runner: F,
) -> Result<String, SecretError>
where
    F: Fn(&KeychainRef<'_>) -> Result<CommandOutput, SecretError>,
{
    let output = runner(keychain_ref)?;
    if !output.success {
        let stderr = output.stderr.trim();
        if stderr.contains("could not be found")
            || stderr.contains("The specified item could not be found")
            || stderr.contains("No such secret collection")
            || stderr.contains("No matching")
        {
            return Err(SecretError::NotFound(spec.to_string()));
        }
        return Err(SecretError::CommandFailed(
            spec.to_string(),
            stderr.to_string(),
        ));
    }
    let value = output.stdout.trim_end_matches(['\r', '\n']).to_string();
    if value.is_empty() {
        return Err(SecretError::NotFound(spec.to_string()));
    }
    Ok(value)
}

fn run_keychain_command(keychain_ref: &KeychainRef<'_>) -> Result<CommandOutput, SecretError> {
    #[cfg(target_os = "macos")]
    {
        use std::process::Command;

        let output = Command::new("security")
            .args([
                "find-generic-password",
                "-s",
                keychain_ref.service,
                "-a",
                keychain_ref.account,
                "-w",
            ])
            .output()
            .map_err(|err| {
                SecretError::Io(
                    format!("{}.{}", keychain_ref.service, keychain_ref.account),
                    err.to_string(),
                )
            })?;
        Ok(CommandOutput {
            success: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }

    #[cfg(target_os = "linux")]
    {
        use std::process::Command;

        let output = Command::new("secret-tool")
            .args([
                "lookup",
                "service",
                keychain_ref.service,
                "account",
                keychain_ref.account,
            ])
            .output()
            .map_err(|err| {
                SecretError::Io(
                    format!("{}.{}", keychain_ref.service, keychain_ref.account),
                    err.to_string(),
                )
            })?;
        return Ok(CommandOutput {
            success: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = keychain_ref;
        Err(SecretError::UnsupportedPlatform)
    }
}

fn resolve_path_root(path: &Path, base_dir: &Path) -> PathBuf {
    let path_str = path.to_string_lossy();
    let expanded = if path_str == "~" || path_str.starts_with("~/") {
        if let Some(home) = dirs::home_dir() {
            let suffix = path_str.strip_prefix('~').unwrap_or("");
            home.join(suffix.trim_start_matches('/'))
        } else {
            path.to_path_buf()
        }
    } else {
        path.to_path_buf()
    };

    if expanded.is_absolute() {
        expanded
    } else {
        base_dir.join(expanded)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_value_round_trips_inline_and_keychain() {
        let inline: SecretValue = serde_yml::from_str("\"plain\"").unwrap();
        assert_eq!(inline, SecretValue::Inline("plain".into()));
        assert_eq!(serde_yml::to_string(&inline).unwrap().trim(), "plain");

        let keychain: SecretValue = serde_yml::from_str("\"keychain:svc.acc\"").unwrap();
        assert_eq!(keychain, SecretValue::FromKeychain("svc.acc".into()));
        assert_eq!(
            serde_yml::to_string(&keychain).unwrap().trim(),
            "keychain:svc.acc"
        );
    }

    #[test]
    fn invalid_keychain_spec_is_clear() {
        let err = serde_yml::from_str::<SecretValue>("\"keychain:missingdot\"").unwrap_err();
        assert!(err.to_string().contains("expected service.account"));
    }

    #[test]
    fn non_string_secret_value_is_rejected() {
        let err = serde_yml::from_str::<SecretValue>("123").unwrap_err();
        assert!(err.to_string().contains("a string secret"));
    }

    #[test]
    fn keychain_not_found_error_is_clear() {
        let parsed = parse_keychain_ref("svc.acc").unwrap();
        let err = resolve_keychain_with_runner("svc.acc", &parsed, |_| {
            Ok(CommandOutput {
                success: false,
                stdout: String::new(),
                stderr: "The specified item could not be found in the keychain.".into(),
            })
        })
        .unwrap_err();
        assert_eq!(err, SecretError::NotFound("svc.acc".into()));
    }

    #[test]
    fn validate_rejects_register_key_without_key() {
        let config: Config = serde_yml::from_str("register_key: true\nhost: h\n").unwrap();
        assert!(config
            .validate()
            .unwrap_err()
            .to_string()
            .contains("register_key=true requires non-empty key"));
    }

    #[test]
    fn validate_rejects_invalid_chmod() {
        let config = Config {
            version: 1,
            imports: vec![],
            host: Some("h".into()),
            port: None,
            user: None,
            key: None,
            password: None,
            register_key: false,
            interpreter: default_interpreter(),
            flags: default_flags(),
            comment_template: default_comment_template(),
            flux_home: None,
            proxy: ProxyConfig::default(),
            file: vec![FileItem {
                name: None,
                src: "a".into(),
                dst: ":/r/a".into(),
                kind: ItemKind::File,
                target: None,
                mode: SyncMode::Sync,
                chmod: Some("abc".into()),
                tags: vec![],
            }],
            script: vec![],
            block: vec![],
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn validate_rejects_zero_proxy_port() {
        let config: Config =
            serde_yml::from_str("host: h\nproxy:\n  enabled: true\n  local_port: 0\n").unwrap();
        assert!(config
            .validate()
            .unwrap_err()
            .to_string()
            .contains("proxy.local_port must be > 0"));
    }
}
