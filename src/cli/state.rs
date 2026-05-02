use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct HostState {
    pub host: String,
    pub last_sync_ts: i64,
    #[serde(default)]
    pub item_hashes: HashMap<String, String>,
    pub last_failed_item: Option<String>,
}

pub fn load(host: &str) -> Option<HostState> {
    let path = state_path(host, default_state_root()?)?;
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

pub fn save(state: &HostState) -> std::io::Result<()> {
    let Some(root) = default_state_root() else {
        return Ok(());
    };
    save_to_root(state, &root)
}

fn save_to_root(state: &HostState, root: &std::path::Path) -> std::io::Result<()> {
    let path = state_path(&state.host, root.to_path_buf()).expect("state path");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let body = serde_json::to_string_pretty(state).map_err(std::io::Error::other)?;
    std::fs::write(path, body)
}

#[cfg(test)]
fn load_from_root(host: &str, root: &std::path::Path) -> Option<HostState> {
    let path = state_path(host, root.to_path_buf())?;
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn default_state_root() -> Option<PathBuf> {
    std::env::var_os("FLUX_STATE_DIR")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".flux").join("state")))
}

fn state_path(host: &str, root: PathBuf) -> Option<PathBuf> {
    Some(root.join(format!("{}.json", sanitize_host(host))))
}

fn sanitize_host(host: &str) -> String {
    host.chars()
        .map(|ch| match ch {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            other => other,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn round_trip_state() {
        let dir = TempDir::new().unwrap();
        let mut state = HostState {
            host: "example".into(),
            last_sync_ts: 123,
            item_hashes: HashMap::new(),
            last_failed_item: Some("x".into()),
        };
        state.item_hashes.insert("a".into(), "b".into());
        save_to_root(&state, dir.path()).unwrap();
        assert_eq!(load_from_root("example", dir.path()), Some(state));
    }

    #[test]
    fn missing_file_returns_none() {
        let dir = TempDir::new().unwrap();
        assert_eq!(load_from_root("missing", dir.path()), None);
    }
}
