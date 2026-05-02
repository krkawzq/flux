//! In-memory `RemoteOps` for tests.

use crate::remote::{ExecOutput, RemoteOps, RemoteOpsError};
use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::Duration as StdDuration;

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
    interactive_calls: Vec<(String, Option<StdDuration>)>,
    interactive_exit_status: i32,
    write_calls: Vec<(String, Vec<u8>)>,
    transient_failures: HashMap<&'static str, Vec<RemoteOpsError>>,
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

    pub fn fail_next(&self, op: &'static str, err: RemoteOpsError) {
        self.inner
            .lock()
            .unwrap()
            .transient_failures
            .entry(op)
            .or_default()
            .push(err);
    }

    pub fn exec_calls(&self) -> Vec<String> {
        self.inner.lock().unwrap().exec_calls.clone()
    }

    pub fn write_calls(&self) -> Vec<(String, Vec<u8>)> {
        self.inner.lock().unwrap().write_calls.clone()
    }

    pub fn interactive_calls(&self) -> Vec<(String, Option<StdDuration>)> {
        self.inner.lock().unwrap().interactive_calls.clone()
    }

    pub fn file_contents(&self, path: &str) -> Option<Vec<u8>> {
        self.inner.lock().unwrap().files.get(path).cloned()
    }

    pub fn file_mode(&self, path: &str) -> Option<u32> {
        self.inner.lock().unwrap().modes.get(path).copied()
    }

    fn take_failure(guard: &mut Inner, op: &'static str) -> Option<RemoteOpsError> {
        let failures = guard.transient_failures.get_mut(op)?;
        if failures.is_empty() {
            return None;
        }
        let err = failures.remove(0);
        if failures.is_empty() {
            guard.transient_failures.remove(op);
        }
        Some(err)
    }
}

#[async_trait]
impl RemoteOps for InMemoryRemote {
    async fn exec(&self, cmd: &str) -> Result<ExecOutput, RemoteOpsError> {
        let mut guard = self.inner.lock().unwrap();
        if let Some(err) = Self::take_failure(&mut guard, "exec") {
            return Err(err);
        }
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
        let mut guard = self.inner.lock().unwrap();
        if let Some(err) = Self::take_failure(&mut guard, "read_file") {
            return Err(err);
        }
        guard
            .files
            .get(path)
            .cloned()
            .ok_or_else(|| RemoteOpsError::NotFound(path.to_string()))
    }

    async fn write_file(&self, path: &str, data: &[u8]) -> Result<(), RemoteOpsError> {
        let mut guard = self.inner.lock().unwrap();
        if let Some(err) = Self::take_failure(&mut guard, "write_file") {
            return Err(err);
        }
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
        let mut guard = self.inner.lock().unwrap();
        if let Some(err) = Self::take_failure(&mut guard, "exists") {
            return Err(err);
        }
        Ok(guard.files.contains_key(path) || guard.dirs.contains(path))
    }

    async fn mtime(&self, path: &str) -> Result<DateTime<Utc>, RemoteOpsError> {
        let mut guard = self.inner.lock().unwrap();
        if let Some(err) = Self::take_failure(&mut guard, "mtime") {
            return Err(err);
        }
        guard
            .mtimes
            .get(path)
            .copied()
            .ok_or_else(|| RemoteOpsError::NotFound(path.to_string()))
    }

    async fn stat_mode(&self, path: &str) -> Result<u32, RemoteOpsError> {
        let mut guard = self.inner.lock().unwrap();
        if let Some(err) = Self::take_failure(&mut guard, "stat_mode") {
            return Err(err);
        }
        if let Some(mode) = guard.modes.get(path).copied() {
            return Ok(mode);
        }
        if guard.files.contains_key(path) {
            return Ok(0o644);
        }
        Err(RemoteOpsError::NotFound(path.to_string()))
    }

    async fn chmod(&self, path: &str, mode: u32) -> Result<(), RemoteOpsError> {
        let mut guard = self.inner.lock().unwrap();
        if let Some(err) = Self::take_failure(&mut guard, "chmod") {
            return Err(err);
        }
        if !guard.files.contains_key(path) && !guard.dirs.contains(path) {
            return Err(RemoteOpsError::NotFound(path.to_string()));
        }
        guard.modes.insert(path.to_string(), mode);
        Ok(())
    }

    async fn remove_file(&self, path: &str) -> Result<(), RemoteOpsError> {
        let mut guard = self.inner.lock().unwrap();
        if let Some(err) = Self::take_failure(&mut guard, "remove_file") {
            return Err(err);
        }
        guard.files.remove(path);
        guard.mtimes.remove(path);
        guard.modes.remove(path);
        Ok(())
    }

    async fn ensure_dir(&self, path: &str) -> Result<(), RemoteOpsError> {
        let mut guard = self.inner.lock().unwrap();
        if let Some(err) = Self::take_failure(&mut guard, "ensure_dir") {
            return Err(err);
        }
        guard.dirs.insert(path.to_string());
        Ok(())
    }

    async fn interactive_exec(
        &self,
        cmd: &str,
        timeout: Option<StdDuration>,
    ) -> Result<i32, RemoteOpsError> {
        let mut guard = self.inner.lock().unwrap();
        if let Some(err) = Self::take_failure(&mut guard, "interactive_exec") {
            return Err(err);
        }
        guard.interactive_calls.push((cmd.to_string(), timeout));
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
    async fn stat_mode_returns_chmod_value() {
        let remote = InMemoryRemote::new();
        remote.write_file("/a", b"x").await.unwrap();
        remote.chmod("/a", 0o600).await.unwrap();
        assert_eq!(remote.stat_mode("/a").await.unwrap(), 0o600);
    }

    #[tokio::test]
    async fn stat_mode_default_for_unchmoded() {
        let remote = InMemoryRemote::new();
        remote.write_file("/a", b"x").await.unwrap();
        assert_eq!(remote.stat_mode("/a").await.unwrap(), 0o644);
    }

    #[tokio::test]
    async fn remove_file_drops_entry() {
        let remote = InMemoryRemote::new();
        remote.write_file("/a", b"x").await.unwrap();
        remote.remove_file("/a").await.unwrap();
        assert!(matches!(
            remote.read_file("/a").await.unwrap_err(),
            RemoteOpsError::NotFound(_)
        ));
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

    #[tokio::test]
    async fn injected_transient_error_fires_once() {
        let remote = InMemoryRemote::with_files([("/a", b"x".to_vec())]);
        remote.fail_next("exists", RemoteOpsError::Transport("flake".into()));
        assert!(matches!(
            remote.exists("/a").await.unwrap_err(),
            RemoteOpsError::Transport(_)
        ));
        assert!(remote.exists("/a").await.unwrap());
    }
}
