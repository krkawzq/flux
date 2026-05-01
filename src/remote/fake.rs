//! In-memory `RemoteOps` for tests.

use crate::remote::{ExecOutput, RemoteOps, RemoteOpsError};
use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

/// Predefined response for an exec command match.
pub struct ExecRule {
    pub matcher: Box<dyn Fn(&str) -> bool + Send + Sync>,
    pub status: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

#[derive(Default)]
struct Inner {
    files: HashMap<String, Vec<u8>>,
    mtimes: HashMap<String, DateTime<Utc>>,
    modes: HashMap<String, u32>,
    dirs: HashSet<String>,
    exec_rules: Vec<ExecRule>,
    exec_calls: Vec<String>,
    interactive_calls: Vec<String>,
    interactive_exit_status: i32,
    write_calls: Vec<(String, Vec<u8>)>,
}

#[derive(Default)]
pub struct InMemoryRemote {
    inner: Mutex<Inner>,
}

impl InMemoryRemote {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_files<S: Into<String>, B: Into<Vec<u8>>>(
        files: impl IntoIterator<Item = (S, B)>,
    ) -> Self {
        let me = Self::new();
        let now = Utc::now();
        let mut guard = me.inner.lock().unwrap();
        for (path, bytes) in files {
            let path = path.into();
            guard.files.insert(path.clone(), bytes.into());
            guard.mtimes.insert(path, now);
        }
        drop(guard);
        me
    }

    pub fn add_exec_rule(&self, rule: ExecRule) {
        self.inner.lock().unwrap().exec_rules.push(rule);
    }

    pub fn set_mtime<S: Into<String>>(&self, path: S, when: DateTime<Utc>) {
        self.inner.lock().unwrap().mtimes.insert(path.into(), when);
    }

    pub fn set_interactive_exit(&self, status: i32) {
        self.inner.lock().unwrap().interactive_exit_status = status;
    }

    pub fn exec_calls(&self) -> Vec<String> {
        self.inner.lock().unwrap().exec_calls.clone()
    }

    pub fn write_calls(&self) -> Vec<(String, Vec<u8>)> {
        self.inner.lock().unwrap().write_calls.clone()
    }

    pub fn interactive_calls(&self) -> Vec<String> {
        self.inner.lock().unwrap().interactive_calls.clone()
    }

    pub fn file_contents(&self, path: &str) -> Option<Vec<u8>> {
        self.inner.lock().unwrap().files.get(path).cloned()
    }

    pub fn file_mode(&self, path: &str) -> Option<u32> {
        self.inner.lock().unwrap().modes.get(path).copied()
    }
}

#[async_trait]
impl RemoteOps for InMemoryRemote {
    async fn exec(&self, cmd: &str) -> Result<ExecOutput, RemoteOpsError> {
        let mut guard = self.inner.lock().unwrap();
        guard.exec_calls.push(cmd.to_string());
        for rule in &guard.exec_rules {
            if (rule.matcher)(cmd) {
                return Ok(ExecOutput {
                    status: rule.status,
                    stdout: rule.stdout.clone(),
                    stderr: rule.stderr.clone(),
                });
            }
        }
        Ok(ExecOutput {
            status: 0,
            stdout: vec![],
            stderr: vec![],
        })
    }

    async fn read_file(&self, path: &str) -> Result<Vec<u8>, RemoteOpsError> {
        self.inner
            .lock()
            .unwrap()
            .files
            .get(path)
            .cloned()
            .ok_or_else(|| RemoteOpsError::NotFound(path.to_string()))
    }

    async fn write_file(&self, path: &str, data: &[u8]) -> Result<(), RemoteOpsError> {
        let mut guard = self.inner.lock().unwrap();
        guard.write_calls.push((path.to_string(), data.to_vec()));
        guard.files.insert(path.to_string(), data.to_vec());
        let prev = guard.mtimes.get(path).copied();
        let next = prev
            .map(|t| t + Duration::seconds(1))
            .unwrap_or_else(Utc::now);
        guard.mtimes.insert(path.to_string(), next);
        Ok(())
    }

    async fn exists(&self, path: &str) -> Result<bool, RemoteOpsError> {
        let guard = self.inner.lock().unwrap();
        Ok(guard.files.contains_key(path) || guard.dirs.contains(path))
    }

    async fn mtime(&self, path: &str) -> Result<DateTime<Utc>, RemoteOpsError> {
        self.inner
            .lock()
            .unwrap()
            .mtimes
            .get(path)
            .copied()
            .ok_or_else(|| RemoteOpsError::NotFound(path.to_string()))
    }

    async fn chmod(&self, path: &str, mode: u32) -> Result<(), RemoteOpsError> {
        let mut guard = self.inner.lock().unwrap();
        if !guard.files.contains_key(path) && !guard.dirs.contains(path) {
            return Err(RemoteOpsError::NotFound(path.to_string()));
        }
        guard.modes.insert(path.to_string(), mode);
        Ok(())
    }

    async fn ensure_dir(&self, path: &str) -> Result<(), RemoteOpsError> {
        self.inner.lock().unwrap().dirs.insert(path.to_string());
        Ok(())
    }

    async fn interactive_exec(&self, cmd: &str) -> Result<i32, RemoteOpsError> {
        let mut guard = self.inner.lock().unwrap();
        guard.interactive_calls.push(cmd.to_string());
        Ok(guard.interactive_exit_status)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn write_then_read_round_trips() {
        let remote = InMemoryRemote::new();
        remote.write_file("/a", b"hello").await.unwrap();
        assert_eq!(remote.read_file("/a").await.unwrap(), b"hello".to_vec());
    }

    #[tokio::test]
    async fn read_missing_file_returns_not_found() {
        let remote = InMemoryRemote::new();
        let err = remote.read_file("/missing").await.unwrap_err();
        assert!(matches!(err, RemoteOpsError::NotFound(_)));
    }

    #[tokio::test]
    async fn mtime_advances_on_rewrite() {
        let remote = InMemoryRemote::new();
        remote.write_file("/a", b"v1").await.unwrap();
        let t1 = remote.mtime("/a").await.unwrap();
        remote.write_file("/a", b"v2").await.unwrap();
        let t2 = remote.mtime("/a").await.unwrap();
        assert!(t2 > t1);
    }

    #[tokio::test]
    async fn chmod_persists() {
        let remote = InMemoryRemote::new();
        remote.write_file("/a", b"x").await.unwrap();
        remote.chmod("/a", 0o600).await.unwrap();
        assert_eq!(remote.file_mode("/a"), Some(0o600));
    }

    #[tokio::test]
    async fn exec_rules_match() {
        let remote = InMemoryRemote::new();
        remote.add_exec_rule(ExecRule {
            matcher: Box::new(|cmd| cmd.starts_with("echo ")),
            status: 0,
            stdout: b"hi\n".to_vec(),
            stderr: vec![],
        });
        let out = remote.exec("echo hi").await.unwrap();
        assert_eq!(out.stdout_string(), "hi\n");
        let out2 = remote.exec("ls /").await.unwrap();
        assert_eq!(out2.status, 0);
        assert_eq!(out2.stdout, vec![]);
        assert_eq!(
            remote.exec_calls(),
            vec!["echo hi".to_string(), "ls /".to_string()]
        );
    }
}
