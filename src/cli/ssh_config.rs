//! Local ~/.ssh/config read & write helpers.

use anyhow::{anyhow, bail, Context, Result};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

type SshEntry = (String, Option<String>, u16, Option<String>);

#[derive(Debug, thiserror::Error)]
pub enum SshConfigError {
    #[error("invalid host:port spec: {0}")]
    InvalidHostPort(String),
    #[error("invalid port: {0}")]
    InvalidPort(String),
    #[error("io: {0}")]
    Io(String),
}

pub fn parse_ssh_host(spec: &str) -> Result<(Option<String>, String, u16), SshConfigError> {
    parse_ssh_host_inner(spec, 22)
}

fn parse_ssh_host_inner(
    spec: &str,
    default_port: u16,
) -> Result<(Option<String>, String, u16), SshConfigError> {
    let (user, hostport) = match spec.find('@') {
        Some(index) => (Some(spec[..index].to_string()), &spec[index + 1..]),
        None => (None, spec),
    };

    if let Some(rest) = hostport.strip_prefix('[') {
        if let Some(end) = rest.find(']') {
            let host = rest[..end].to_string();
            let remainder = &rest[end + 1..];
            if remainder.is_empty() {
                return Ok((user, host, default_port));
            }
            if let Some(port) = remainder.strip_prefix(':') {
                let port = port
                    .parse::<u16>()
                    .map_err(|_| SshConfigError::InvalidPort(port.into()))?;
                return Ok((user, host, port));
            }
            return Err(SshConfigError::InvalidHostPort(spec.into()));
        }
        return Err(SshConfigError::InvalidHostPort(spec.into()));
    }

    match hostport.rsplit_once(':') {
        Some((host, port)) if !port.is_empty() => {
            let port = port
                .parse::<u16>()
                .map_err(|_| SshConfigError::InvalidPort(port.into()))?;
            Ok((user, host.to_string(), port))
        }
        _ => Ok((user, hostport.to_string(), default_port)),
    }
}

pub fn save_ssh_config(name: &str, config: &crate::config::Config) -> Result<()> {
    let home = dirs::home_dir().context("no home dir")?;
    let config_path = home.join(".ssh").join("config");
    std::fs::create_dir_all(home.join(".ssh")).context("ensuring ~/.ssh dir")?;
    let existing = match std::fs::read_to_string(&config_path) {
        Ok(content) => content,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(err) => bail!(SshConfigError::Io(err.to_string())),
    };
    let updated = replace_or_append_host(&existing, name, config);
    std::fs::write(&config_path, updated).context("writing ~/.ssh/config")
}

pub fn replace_or_append_host(existing: &str, name: &str, cfg: &crate::config::Config) -> String {
    let mut out = String::new();
    let mut skipping = false;
    let host_line = format!("Host {name}");
    for line in existing.split_inclusive('\n') {
        let trimmed = line.trim_end_matches(['\n', '\r']);
        let is_host_line = trimmed.starts_with("Host ");
        if skipping {
            if is_host_line {
                skipping = false;
                out.push_str(line);
            }
            continue;
        }
        if trimmed == host_line {
            skipping = true;
            continue;
        }
        out.push_str(line);
    }
    if !out.ends_with('\n') && !out.is_empty() {
        out.push('\n');
    }
    out.push_str(&render_host_block(name, cfg));
    out
}

fn render_host_block(name: &str, cfg: &crate::config::Config) -> String {
    let mut output = String::new();
    output.push_str(&format!("Host {name}\n"));
    output.push_str(&format!(
        "    HostName {}\n",
        cfg.host.clone().unwrap_or_default()
    ));
    if let Some(user) = &cfg.user {
        output.push_str(&format!("    User {user}\n"));
    }
    if let Some(port) = cfg.port {
        output.push_str(&format!("    Port {port}\n"));
    }
    if let Some(key) = &cfg.key {
        output.push_str(&format!("    IdentityFile {key}\n"));
    }
    output
}

pub fn read_ssh_config_entry(name: &str) -> Result<Option<SshEntry>> {
    let home = dirs::home_dir().context("no home dir")?;
    let mut visited = HashSet::new();
    read_entry_recursive(&home.join(".ssh").join("config"), name, &mut visited)
}

fn read_entry_recursive(
    path: &Path,
    name: &str,
    visited: &mut HashSet<PathBuf>,
) -> Result<Option<SshEntry>> {
    let canon = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    if !visited.insert(canon) {
        return Ok(None);
    }
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => bail!(SshConfigError::Io(err.to_string())),
    };

    let mut in_target = false;
    let mut hostname = None;
    let mut user = None;
    let mut port = 22;
    let mut identity = None;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let (keyword, rest) = match trimmed.split_once(char::is_whitespace) {
            Some(parts) => parts,
            None => continue,
        };
        let keyword = keyword.to_lowercase();
        let rest = rest.trim();
        if keyword == "include" {
            for included in expand_include_path(rest, path) {
                if let Some(found) = read_entry_recursive(&included, name, visited)? {
                    return Ok(Some(found));
                }
            }
            continue;
        }
        if keyword == "host" {
            let patterns: Vec<&str> = rest.split_whitespace().collect();
            in_target = patterns.contains(&name);
            if patterns.iter().any(|pattern| {
                pattern.contains('*') || pattern.contains('?') || pattern.starts_with('!')
            }) {
                eprintln!("[warn] unsupported wildcard/negation in Host pattern: {rest}");
            }
            continue;
        }
        if keyword == "match" {
            eprintln!("[warn] Match block ignored");
            in_target = false;
            continue;
        }
        if !in_target {
            continue;
        }
        match keyword.as_str() {
            "hostname" => hostname = Some(rest.to_string()),
            "user" => user = Some(rest.to_string()),
            "port" => {
                port = rest
                    .parse::<u16>()
                    .map_err(|_| anyhow!("invalid Port {rest}"))?
            }
            "identityfile" => identity = Some(rest.to_string()),
            _ => {}
        }
    }

    if let Some(hostname) = hostname {
        Ok(Some((hostname, user, port, identity)))
    } else {
        Ok(None)
    }
}

fn expand_include_path(spec: &str, base: &Path) -> Vec<PathBuf> {
    let raw = if let Some(rest) = spec.strip_prefix("~/") {
        dirs::home_dir()
            .map(|home| home.join(rest))
            .unwrap_or_else(|| PathBuf::from(spec))
    } else if Path::new(spec).is_absolute() {
        PathBuf::from(spec)
    } else {
        base.parent()
            .map(|parent| parent.join(spec))
            .unwrap_or_else(|| PathBuf::from(spec))
    };
    vec![raw]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, ProxyConfig};

    fn config(host: &str, user: Option<&str>) -> Config {
        Config {
            version: 1,
            imports: vec![],
            host: Some(host.into()),
            port: Some(22),
            user: user.map(ToOwned::to_owned),
            key: None,
            password: None,
            register_key: false,
            interpreter: "/bin/bash".into(),
            flags: vec![],
            comment_template: "# {}".into(),
            flux_home: None,
            proxy: ProxyConfig::default(),
            file: vec![],
            script: vec![],
            block: vec![],
        }
    }

    #[test]
    fn parse_plain_host() {
        let (user, host, port) = parse_ssh_host("example.com").unwrap();
        assert_eq!(user, None);
        assert_eq!(host, "example.com");
        assert_eq!(port, 22);
    }

    #[test]
    fn parse_host_with_port() {
        let (_, host, port) = parse_ssh_host("example.com:2222").unwrap();
        assert_eq!(host, "example.com");
        assert_eq!(port, 2222);
    }

    #[test]
    fn parse_user_at_host_port() {
        let (user, host, port) = parse_ssh_host("alice@example.com:22").unwrap();
        assert_eq!(user, Some("alice".into()));
        assert_eq!(host, "example.com");
        assert_eq!(port, 22);
    }

    #[test]
    fn parse_ipv6_with_port() {
        let (_, host, port) = parse_ssh_host("[::1]:2222").unwrap();
        assert_eq!(host, "::1");
        assert_eq!(port, 2222);
    }

    #[test]
    fn parse_ipv6_no_port() {
        let (_, host, port) = parse_ssh_host("[::1]").unwrap();
        assert_eq!(host, "::1");
        assert_eq!(port, 22);
    }

    #[test]
    fn parse_user_at_ipv6_port() {
        let (user, host, port) = parse_ssh_host("alice@[fe80::1]:443").unwrap();
        assert_eq!(user, Some("alice".into()));
        assert_eq!(host, "fe80::1");
        assert_eq!(port, 443);
    }

    #[test]
    fn parse_invalid_port_errors() {
        let err = parse_ssh_host("example.com:notaport").unwrap_err();
        assert!(matches!(err, SshConfigError::InvalidPort(_)));
    }

    #[test]
    fn replace_or_append_keeps_other_hosts() {
        let pre = "Host foo\n    HostName 1.1.1.1\n\nHost bar\n    HostName 2.2.2.2\n";
        let out = replace_or_append_host(pre, "foo", &config("9.9.9.9", None));
        assert!(out.contains("Host bar"));
        assert!(out.contains("HostName 9.9.9.9"));
        assert!(!out.contains("HostName 1.1.1.1"));
    }

    #[test]
    fn replace_skips_through_comments_to_next_host() {
        let pre =
            "Host foo\n    HostName 1.1.1.1\n    # a comment\n    User old\n\nHost bar\n    HostName 2.2.2.2\n";
        let out = replace_or_append_host(pre, "foo", &config("9.9.9.9", Some("new")));
        assert!(out.contains("Host bar"));
        assert!(out.contains("HostName 9.9.9.9"));
        assert!(out.contains("User new"));
        assert!(!out.contains("User old"));
        assert!(!out.contains("# a comment"));
    }
}
