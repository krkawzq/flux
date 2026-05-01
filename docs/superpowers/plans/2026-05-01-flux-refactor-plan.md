# Flux Phase 2 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 把 sync 层与远程副作用解耦，建立可单测的 Plan/Execute 执行模型，并补全测试矩阵；同时引入 dry-run、Reporter 抽象、schema 版本化。

**Architecture:** 低层 `RemoteOps` trait + Plan/Execute 两阶段 + Reporter trait + 领域 enum 错误模型 + stage 内并发（file 全并行 / script 串行 / block 按 target 分组）。

**Tech Stack:** Rust 2021, tokio, async-trait, thiserror, anyhow, futures, proptest（dev）。沿用 Phase 1 已升级的 russh 0.60 / serde_yml / dialoguer 0.12 / dirs 6。

**前置 (Phase 1 已完成)：**
- 依赖整体升级（russh 0.46→0.60.2、serde_yaml→serde_yml、dialoguer 0.12、dirs 6）
- 19 条 bug 修复（脚本 gating、shell escape、block sentinel/CRLF/timestamp、SSH config 解析等）
- `Config::resolve_root` / `Config::validate` 已就位
- `.flux.example/` 脱敏镜像 + `.gitignore` 卫生 + `SECURITY_TODO.md`
- `cargo build --offline` 通过；clippy 还有 5 个 style nit（本计划末尾收口）

---

## File Structure

### 新建
```
src/lib.rs                       # bin+lib, re-export 模块
src/remote/mod.rs                # RemoteOps trait + RemoteOpsError + ExecOutput
src/remote/ssh.rs                # SshClient 移过来 + impl RemoteOps
src/remote/fake.rs               # #[cfg(test)] InMemoryRemote
src/reporter/mod.rs              # Reporter trait + Stage + ItemOutcome
src/reporter/console.rs          # ConsoleReporter (替代 output.rs)
src/reporter/memory.rs           # #[cfg(test)] CapturedReporter
src/config/mod.rs                # 移自 config.rs（保留 Config / ProxyProtocol 等）
src/config/version.rs            # VersionProbe + load_versioned
src/config/validate.rs           # （resolve_root + validate 抽出）
src/cli/mod.rs                   # run_init / run_sync / run_proxy
src/cli/ssh_config.rs            # save_ssh_config / parse_ssh_host / read_entry / Include
src/path.rs                      # 增 AssetLocator（保留 FluxPath）
src/sync/plan.rs                 # Plan struct + Action enums + Sentinel
tests/proptests/block_sentinel.rs
tests/integration/pipeline_file.rs
tests/integration/pipeline_block.rs
tests/integration/pipeline_script.rs
tests/integration/pipeline_dry_run.rs
tests/fixtures/westlake_minimal.yml
tests/fixtures/westlake_with_blocks.yml
docs/schema-migrations.md
.github/workflows/ci.yml
deny.toml
```

### 重写
```
src/main.rs                      # 简化为只 dispatch CLI（其余移 cli/）
src/sync/mod.rs                  # Pipeline + run_pipeline
src/sync/file.rs                 # plan_files + execute_file
src/sync/script.rs               # plan_scripts + execute_script
src/sync/block.rs                # sentinel parser + plan_blocks + execute_block
```

### 删除
```
src/output.rs                    # 内容迁到 reporter/console.rs
src/ssh.rs                       # 内容迁到 remote/ssh.rs
src/config.rs                    # 内容迁到 config/mod.rs
```

---

## Task 1: 转换为 bin+lib，恢复 thiserror，加 proptest dev-dep

**Files:**
- Modify: `Cargo.toml`
- Create: `src/lib.rs`

- [ ] **Step 1.1:** Edit `Cargo.toml`，在 `[dependencies]` 下加回 `thiserror`（Phase 1 删除过）：

```toml
thiserror = "2"
```

- [ ] **Step 1.2:** 在 `Cargo.toml` 加 `[dev-dependencies]` section：

```toml
[dev-dependencies]
proptest = "1"
tempfile = "3"
tokio = { version = "1", features = ["macros", "rt-multi-thread", "test-util"] }
```

- [ ] **Step 1.3:** 在 `Cargo.toml` 改 `[[bin]]` block 上方加 `[lib]`：

```toml
[lib]
name = "flux"
path = "src/lib.rs"
```

- [ ] **Step 1.4:** 创建 `src/lib.rs`：

```rust
//! Flux library — internal crate exposed for tests.
//!
//! Binaries should depend on `flux::cli::run_*` entry points;
//! tests can pull any module via `flux::*`.

pub mod cli;
pub mod config;
pub mod path;
pub mod remote;
pub mod reporter;
pub mod sync;
```

注意：lib.rs 引用的子模块在后续 task 创建。本 task 完成后 `cargo check` 会失败，因为 cli/remote/reporter/sync 还没有 mod.rs 形态——这是预期。**Task 1 不要求 cargo check 通过**。

- [ ] **Step 1.5:** 验证 Cargo.toml 语法：

Run: `cargo metadata --offline --no-deps --format-version 1 > /dev/null`
Expected: 成功（仅校验 toml 合法，不要求依赖解析）。如失败则 toml 写错。

- [ ] **Step 1.6:** Commit：

```bash
git add Cargo.toml src/lib.rs
git commit -m "phase2: convert to bin+lib, restore thiserror, add proptest"
```

---

## Task 2: RemoteOps trait + RemoteOpsError + ExecOutput

**Files:**
- Create: `src/remote/mod.rs`

- [ ] **Step 2.1:** 创建 `src/remote/mod.rs`：

```rust
//! Remote-side primitive operations.
//!
//! The `RemoteOps` trait abstracts everything Flux needs to do on a remote
//! host. The real implementation (`SshClient`) lives in `remote::ssh`; tests
//! use `remote::fake::InMemoryRemote`.

pub mod fake;
pub mod ssh;

use async_trait::async_trait;
use chrono::{DateTime, Utc};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecOutput {
    pub status: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl ExecOutput {
    pub fn success(&self) -> bool {
        self.status == 0
    }
    pub fn stdout_string(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }
    pub fn stderr_string(&self) -> String {
        String::from_utf8_lossy(&self.stderr).into_owned()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RemoteOpsError {
    #[error("remote command failed (status={status}): {stderr}")]
    NonZeroExit { status: i32, stderr: String },
    #[error("remote io: {0}")]
    Io(String),
    #[error("path not found: {0}")]
    NotFound(String),
    #[error("ssh transport: {0}")]
    Transport(String),
    #[error("encoding: {0}")]
    Encoding(String),
}

#[async_trait]
pub trait RemoteOps: Send + Sync {
    /// Run a non-interactive command. Always returns `ExecOutput`; non-zero
    /// status is NOT an error — the caller decides.
    async fn exec(&self, cmd: &str) -> Result<ExecOutput, RemoteOpsError>;

    async fn read_file(&self, path: &str) -> Result<Vec<u8>, RemoteOpsError>;
    async fn write_file(&self, path: &str, data: &[u8]) -> Result<(), RemoteOpsError>;
    async fn exists(&self, path: &str) -> Result<bool, RemoteOpsError>;
    async fn mtime(&self, path: &str) -> Result<DateTime<Utc>, RemoteOpsError>;
    async fn chmod(&self, path: &str, mode: u32) -> Result<(), RemoteOpsError>;
    async fn ensure_dir(&self, path: &str) -> Result<(), RemoteOpsError>;

    /// Like `exec` but streams stdin/stdout/stderr to the local terminal.
    /// Returns the remote process exit status.
    async fn interactive_exec(&self, cmd: &str) -> Result<i32, RemoteOpsError>;
}
```

- [ ] **Step 2.2:** Stub `src/remote/ssh.rs` 与 `src/remote/fake.rs`（Task 3/4 填充）：

```rust
// src/remote/ssh.rs
//! SSH-backed RemoteOps implementation. Real impl lands in Task 3.
```

```rust
// src/remote/fake.rs
//! Fake in-memory RemoteOps for tests. Real impl lands in Task 4.
```

- [ ] **Step 2.3:** Run: `cargo check --offline 2>&1 | grep -E "(error|warning)" | head -20`
Expected: 失败（lib.rs 引用了 cli/reporter/sync——未来 task 创建）。但 remote::mod 自己应该编译通过。可暂时把 lib.rs 改成只 `pub mod remote;` + 其它 module 用 `// pub mod cli;` 注释，然后此处 `cargo check --lib --offline` 应该过。

实际操作：把 `src/lib.rs` 暂改为：

```rust
pub mod remote;
// pub mod cli; pub mod config; pub mod path; pub mod reporter; pub mod sync; (TODO)
```

Run: `cargo check --lib --offline` —— Expected: 通过。

- [ ] **Step 2.4:** Commit：

```bash
git add src/remote/ src/lib.rs
git commit -m "phase2: introduce RemoteOps trait + RemoteOpsError"
```

---

## Task 3: Move src/ssh.rs → src/remote/ssh.rs + impl RemoteOps for SshClient

**Files:**
- Modify (move): `src/ssh.rs` → `src/remote/ssh.rs`
- Modify: `src/remote/ssh.rs`（在原内容上 impl trait）
- Modify: `src/main.rs`、`src/sync/mod.rs`、`src/sync/*.rs`（更新 import）

- [ ] **Step 3.1:** Move file:

```bash
git mv src/ssh.rs src/remote/ssh.rs
```

- [ ] **Step 3.2:** 在 `src/remote/ssh.rs` 文件顶部把现有 `pub use russh::...` 等保持，**并新增** `RemoteOps` 实现：

```rust
use crate::remote::{ExecOutput, RemoteOps, RemoteOpsError};
use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};

#[async_trait]
impl RemoteOps for SshClient {
    async fn exec(&self, cmd: &str) -> Result<ExecOutput, RemoteOpsError> {
        // 复用 SshClient 现有 exec_command；现存方法返回 (status, stdout, stderr)
        let (status, stdout, stderr) = self
            .exec_command(cmd)
            .await
            .map_err(|e| RemoteOpsError::Transport(e.to_string()))?;
        Ok(ExecOutput {
            status,
            stdout: stdout.into_bytes(),
            stderr: stderr.into_bytes(),
        })
    }

    async fn read_file(&self, path: &str) -> Result<Vec<u8>, RemoteOpsError> {
        self.read_remote_file(path)
            .await
            .map(|s| s.into_bytes())
            .map_err(map_anyhow)
    }

    async fn write_file(&self, path: &str, data: &[u8]) -> Result<(), RemoteOpsError> {
        // 现 write_remote_file 接收 &str；新方法适配字节
        let s = std::str::from_utf8(data)
            .map_err(|e| RemoteOpsError::Encoding(format!("write_file utf8: {e}")))?;
        self.write_remote_file(path, s).await.map_err(map_anyhow)
    }

    async fn exists(&self, path: &str) -> Result<bool, RemoteOpsError> {
        self.file_exists(path).await.map_err(map_anyhow)
    }

    async fn mtime(&self, path: &str) -> Result<DateTime<Utc>, RemoteOpsError> {
        let secs = self.get_mtime(path).await.map_err(map_anyhow)?;
        Utc.timestamp_opt(secs, 0)
            .single()
            .ok_or_else(|| RemoteOpsError::Encoding(format!("invalid mtime {secs}")))
    }

    async fn chmod(&self, path: &str, mode: u32) -> Result<(), RemoteOpsError> {
        self.chmod_remote(path, &format!("{mode:o}"))
            .await
            .map_err(map_anyhow)
    }

    async fn ensure_dir(&self, path: &str) -> Result<(), RemoteOpsError> {
        let cmd = format!("mkdir -p {}", shell_escape(path));
        let out = self.exec(&cmd).await?;
        if !out.success() {
            return Err(RemoteOpsError::NonZeroExit {
                status: out.status,
                stderr: out.stderr_string(),
            });
        }
        Ok(())
    }

    async fn interactive_exec(&self, cmd: &str) -> Result<i32, RemoteOpsError> {
        self.exec_interactive(cmd).await.map_err(map_anyhow)
    }
}

fn map_anyhow(e: anyhow::Error) -> RemoteOpsError {
    RemoteOpsError::Io(e.to_string())
}

fn shell_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for ch in s.chars() {
        if ch == '\'' { out.push_str("'\\''"); } else { out.push(ch); }
    }
    out.push('\'');
    out
}
```

- [ ] **Step 3.3:** 全局把 `use crate::ssh::SshClient;` 改成 `use crate::remote::ssh::SshClient;`：

Run: `grep -rn "use crate::ssh" src/`
Expected: 列出几处。逐一改成 `use crate::remote::ssh::SshClient;`

也包括 `mod ssh;` 在 `src/main.rs` 删除（Task 12 重写 main.rs 时一并清理；本 task 至少改成 `// mod ssh; replaced by remote/ssh.rs`）。

- [ ] **Step 3.4:** 把 `src/lib.rs` 临时打开 `pub mod remote`：已经是。

- [ ] **Step 3.5:** Run: `cargo check --offline`
Expected: 失败（其它模块还用旧 path 之类的）。修到通过。如还有错，看错误信息逐一调整。

- [ ] **Step 3.6:** Commit：

```bash
git add -A
git commit -m "phase2: move ssh.rs into remote/, impl RemoteOps for SshClient"
```

---

## Task 4: InMemoryRemote (FakeRemote) + 自身合约测试

**Files:**
- Modify: `src/remote/fake.rs`

- [ ] **Step 4.1:** 写 `src/remote/fake.rs` 完整实现：

```rust
//! In-memory `RemoteOps` for tests.

use crate::remote::{ExecOutput, RemoteOps, RemoteOpsError};
use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use std::collections::HashMap;
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
    dirs: std::collections::HashSet<String>,
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
        let mut g = me.inner.lock().unwrap();
        for (p, b) in files {
            let path = p.into();
            g.files.insert(path.clone(), b.into());
            g.mtimes.insert(path, now);
        }
        drop(g);
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
        let mut g = self.inner.lock().unwrap();
        g.exec_calls.push(cmd.to_string());
        for rule in &g.exec_rules {
            if (rule.matcher)(cmd) {
                return Ok(ExecOutput {
                    status: rule.status,
                    stdout: rule.stdout.clone(),
                    stderr: rule.stderr.clone(),
                });
            }
        }
        // 默认成功
        Ok(ExecOutput { status: 0, stdout: vec![], stderr: vec![] })
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
        let mut g = self.inner.lock().unwrap();
        g.write_calls.push((path.to_string(), data.to_vec()));
        g.files.insert(path.to_string(), data.to_vec());
        // mtime 单调推进（避免和已有 mtime 同秒撞上）
        let prev = g.mtimes.get(path).copied();
        let next = prev.map(|t| t + Duration::seconds(1)).unwrap_or_else(Utc::now);
        g.mtimes.insert(path.to_string(), next);
        Ok(())
    }

    async fn exists(&self, path: &str) -> Result<bool, RemoteOpsError> {
        let g = self.inner.lock().unwrap();
        Ok(g.files.contains_key(path) || g.dirs.contains(path))
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
        let mut g = self.inner.lock().unwrap();
        if !g.files.contains_key(path) && !g.dirs.contains(path) {
            return Err(RemoteOpsError::NotFound(path.to_string()));
        }
        g.modes.insert(path.to_string(), mode);
        Ok(())
    }

    async fn ensure_dir(&self, path: &str) -> Result<(), RemoteOpsError> {
        self.inner.lock().unwrap().dirs.insert(path.to_string());
        Ok(())
    }

    async fn interactive_exec(&self, cmd: &str) -> Result<i32, RemoteOpsError> {
        let mut g = self.inner.lock().unwrap();
        g.interactive_calls.push(cmd.to_string());
        Ok(g.interactive_exit_status)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn write_then_read_round_trips() {
        let r = InMemoryRemote::new();
        r.write_file("/a", b"hello").await.unwrap();
        assert_eq!(r.read_file("/a").await.unwrap(), b"hello".to_vec());
    }

    #[tokio::test]
    async fn read_missing_file_returns_not_found() {
        let r = InMemoryRemote::new();
        let err = r.read_file("/missing").await.unwrap_err();
        assert!(matches!(err, RemoteOpsError::NotFound(_)));
    }

    #[tokio::test]
    async fn mtime_advances_on_rewrite() {
        let r = InMemoryRemote::new();
        r.write_file("/a", b"v1").await.unwrap();
        let t1 = r.mtime("/a").await.unwrap();
        r.write_file("/a", b"v2").await.unwrap();
        let t2 = r.mtime("/a").await.unwrap();
        assert!(t2 > t1);
    }

    #[tokio::test]
    async fn chmod_persists() {
        let r = InMemoryRemote::new();
        r.write_file("/a", b"x").await.unwrap();
        r.chmod("/a", 0o600).await.unwrap();
        assert_eq!(r.file_mode("/a"), Some(0o600));
    }

    #[tokio::test]
    async fn exec_rules_match() {
        let r = InMemoryRemote::new();
        r.add_exec_rule(ExecRule {
            matcher: Box::new(|cmd| cmd.starts_with("echo ")),
            status: 0,
            stdout: b"hi\n".to_vec(),
            stderr: vec![],
        });
        let out = r.exec("echo hi").await.unwrap();
        assert_eq!(out.stdout_string(), "hi\n");
        let out2 = r.exec("ls /").await.unwrap();
        assert_eq!(out2.status, 0);
        assert_eq!(out2.stdout, vec![]);
        assert_eq!(r.exec_calls(), vec!["echo hi".to_string(), "ls /".to_string()]);
    }
}
```

- [ ] **Step 4.2:** Run: `cargo test --offline --lib remote::fake::tests`
Expected: 5 tests pass.

- [ ] **Step 4.3:** Commit：

```bash
git add src/remote/fake.rs
git commit -m "phase2: add InMemoryRemote (FakeRemote) for sync tests"
```

---

## Task 5: Reporter trait + ConsoleReporter + CapturedReporter

**Files:**
- Create: `src/reporter/mod.rs`
- Create: `src/reporter/console.rs`
- Create: `src/reporter/memory.rs`
- Delete (later, Task 12): `src/output.rs`

- [ ] **Step 5.1:** 在 `src/lib.rs` 打开 `pub mod reporter;`。

- [ ] **Step 5.2:** 创建 `src/reporter/mod.rs`：

```rust
//! Reporter abstraction. The pipeline emits structured events; concrete
//! reporters (console, captured-for-tests) decide how to render them.

pub mod console;
pub mod memory;

use crate::sync::plan::{Plan, SkipReason};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage { File, Script, Block, Pubkey }

#[derive(Debug, Clone)]
pub enum ItemOutcome {
    Applied,
    Skipped(SkipReason),
    Failed(String),
}

#[derive(Debug, Clone)]
pub struct StageSummary {
    pub stage: Stage,
    pub applied: usize,
    pub skipped: usize,
    pub failed: usize,
}

#[derive(Debug, Clone)]
pub struct PipelineSummary {
    pub stages: Vec<StageSummary>,
    pub interrupted: bool,
    pub dry_run: bool,
}

impl PipelineSummary {
    pub fn total_failed(&self) -> usize {
        self.stages.iter().map(|s| s.failed).sum()
    }
    pub fn exit_code(&self) -> i32 {
        if self.interrupted { 130 }
        else if self.total_failed() > 0 { 1 }
        else { 0 }
    }
}

pub trait Reporter: Send + Sync {
    fn stage_started(&self, stage: Stage, item_count: usize);
    fn item_started(&self, stage: Stage, name: &str);
    fn item_finished(&self, stage: Stage, name: &str, outcome: &ItemOutcome);
    fn stage_finished(&self, summary: &StageSummary);
    fn print_plan(&self, plan: &Plan);
    fn pipeline_summary(&self, summary: &PipelineSummary);
    fn warning(&self, msg: &str);
    fn info(&self, msg: &str);
}
```

- [ ] **Step 5.3:** 创建 `src/reporter/console.rs`：

```rust
//! Default reporter: write to stdout/stderr with `console` styling.
//! 把 src/output.rs 的逻辑搬过来，包成 trait impl.

use super::{ItemOutcome, PipelineSummary, Reporter, Stage, StageSummary};
use crate::sync::plan::{BlockAction, FileAction, Plan, ScriptAction, SkipReason};
use console::{style, Term};
use std::sync::Mutex;

pub struct ConsoleReporter {
    out: Mutex<Term>,
}

impl ConsoleReporter {
    pub fn new() -> Self {
        Self { out: Mutex::new(Term::stdout()) }
    }

    fn stage_label(stage: Stage) -> &'static str {
        match stage {
            Stage::File => "file",
            Stage::Script => "script",
            Stage::Block => "block",
            Stage::Pubkey => "pubkey",
        }
    }
}

impl Default for ConsoleReporter {
    fn default() -> Self { Self::new() }
}

impl Reporter for ConsoleReporter {
    fn stage_started(&self, stage: Stage, item_count: usize) {
        let label = Self::stage_label(stage);
        let _ = self.out.lock().unwrap().write_line(&format!(
            "{} {} {}",
            style(format!("[{label}]")).cyan().bold(),
            style("stage").dim(),
            style(format!("({item_count} items)")).dim()
        ));
    }

    fn item_started(&self, _stage: Stage, _name: &str) {
        // intentionally quiet — only finish events render
    }

    fn item_finished(&self, stage: Stage, name: &str, outcome: &ItemOutcome) {
        let label = Self::stage_label(stage);
        let mark = match outcome {
            ItemOutcome::Applied => style("✓ apply").green(),
            ItemOutcome::Skipped(_) => style("⊘ skip").yellow(),
            ItemOutcome::Failed(_) => style("✗ fail").red(),
        };
        let detail = match outcome {
            ItemOutcome::Skipped(r) => format!(" ({})", skip_reason_label(r)),
            ItemOutcome::Failed(e) => format!(" ({e})"),
            _ => String::new(),
        };
        let _ = self.out.lock().unwrap().write_line(&format!(
            "  [{label}] {mark} {name}{detail}"
        ));
    }

    fn stage_finished(&self, s: &StageSummary) {
        let label = Self::stage_label(s.stage);
        let _ = self.out.lock().unwrap().write_line(&format!(
            "{} done: applied={}, skipped={}, failed={}",
            style(format!("[{label}]")).cyan().bold(),
            s.applied, s.skipped, s.failed
        ));
    }

    fn print_plan(&self, plan: &Plan) {
        let _ = self.out.lock().unwrap().write_line(
            &style("DRY RUN — computed plan:").bold().to_string()
        );
        for a in &plan.file_actions { print_file_action(self, a); }
        for a in &plan.script_actions { print_script_action(self, a); }
        for a in &plan.block_actions { print_block_action(self, a); }
    }

    fn pipeline_summary(&self, summary: &PipelineSummary) {
        let mut g = self.out.lock().unwrap();
        let _ = g.write_line(&style("=== summary ===").bold().to_string());
        for s in &summary.stages {
            let _ = g.write_line(&format!(
                "  {}: applied={}, skipped={}, failed={}",
                Self::stage_label(s.stage), s.applied, s.skipped, s.failed
            ));
        }
        let total_failed: usize = summary.stages.iter().map(|s| s.failed).sum();
        if total_failed > 0 {
            let _ = g.write_line(&style(format!("{total_failed} item(s) failed")).red().to_string());
        }
    }

    fn warning(&self, msg: &str) {
        let _ = self.out.lock().unwrap().write_line(
            &format!("{} {}", style("[warn]").yellow().bold(), msg)
        );
    }
    fn info(&self, msg: &str) {
        let _ = self.out.lock().unwrap().write_line(
            &format!("{} {}", style("[flux]").cyan().bold(), msg)
        );
    }
}

fn skip_reason_label(r: &SkipReason) -> String {
    match r {
        SkipReason::AlreadyExists => "already exists".into(),
        SkipReason::RemoteNewer => "remote newer".into(),
        SkipReason::ContentUnchanged => "content unchanged".into(),
        SkipReason::DependencyFailed(d) => format!("dep {d} failed"),
    }
}

fn print_file_action(rep: &ConsoleReporter, a: &FileAction) {
    let g = rep.out.lock().unwrap();
    let _ = match a {
        FileAction::Skip { item_name, reason } => g.write_line(
            &format!("  [file] ⊘ skip   {item_name} ({})", skip_reason_label(reason))),
        FileAction::Apply { item_name, dst, chmod, .. } => g.write_line(
            &format!("  [file] ✓ apply  {item_name} -> {dst}{}",
                chmod.map(|m| format!(" chmod={m:o}")).unwrap_or_default())),
        FileAction::Failed { item_name, error } => g.write_line(
            &format!("  [file] ✗ fail   {item_name} ({error})")),
    };
}

fn print_script_action(rep: &ConsoleReporter, a: &ScriptAction) {
    let g = rep.out.lock().unwrap();
    let _ = match a {
        ScriptAction::Skip { item_name, reason } => g.write_line(
            &format!("  [script] ⊘ skip   {item_name} ({})", skip_reason_label(reason))),
        ScriptAction::Run { item_name, .. } => g.write_line(
            &format!("  [script] ✓ run    {item_name}")),
        ScriptAction::Failed { item_name, error } => g.write_line(
            &format!("  [script] ✗ fail   {item_name} ({error})")),
    };
}

fn print_block_action(rep: &ConsoleReporter, a: &BlockAction) {
    let g = rep.out.lock().unwrap();
    let _ = match a {
        BlockAction::Skip { item_name, reason } => g.write_line(
            &format!("  [block] ⊘ skip   {item_name} ({})", skip_reason_label(reason))),
        BlockAction::Apply { item_name, target, .. } => g.write_line(
            &format!("  [block] ✓ apply  {item_name} -> {target}")),
        BlockAction::Failed { item_name, error } => g.write_line(
            &format!("  [block] ✗ fail   {item_name} ({error})")),
    };
}
```

- [ ] **Step 5.4:** 创建 `src/reporter/memory.rs`：

```rust
//! In-memory reporter for tests — captures every event as a structured value.

use super::{ItemOutcome, PipelineSummary, Reporter, Stage, StageSummary};
use crate::sync::plan::Plan;
use std::sync::Mutex;

#[derive(Debug, Clone)]
pub enum CapturedEvent {
    StageStarted { stage: Stage, items: usize },
    ItemStarted { stage: Stage, name: String },
    ItemFinished { stage: Stage, name: String, outcome: String },
    StageFinished(StageSummary),
    PrintPlan,
    PipelineSummary(PipelineSummary),
    Warning(String),
    Info(String),
}

#[derive(Default)]
pub struct CapturedReporter {
    pub events: Mutex<Vec<CapturedEvent>>,
}

impl CapturedReporter {
    pub fn new() -> Self { Self::default() }

    pub fn events(&self) -> Vec<CapturedEvent> {
        self.events.lock().unwrap().clone()
    }

    pub fn applied_count(&self, stage: Stage) -> usize {
        self.events.lock().unwrap().iter().filter(|e| matches!(e,
            CapturedEvent::ItemFinished { stage: s, outcome, .. }
                if *s == stage && outcome == "applied"
        )).count()
    }

    pub fn failed_items(&self, stage: Stage) -> Vec<String> {
        self.events.lock().unwrap().iter().filter_map(|e| match e {
            CapturedEvent::ItemFinished { stage: s, name, outcome }
                if *s == stage && outcome.starts_with("failed:") => Some(name.clone()),
            _ => None,
        }).collect()
    }
}

fn outcome_label(o: &ItemOutcome) -> String {
    match o {
        ItemOutcome::Applied => "applied".into(),
        ItemOutcome::Skipped(_) => "skipped".into(),
        ItemOutcome::Failed(e) => format!("failed:{e}"),
    }
}

impl Reporter for CapturedReporter {
    fn stage_started(&self, stage: Stage, items: usize) {
        self.events.lock().unwrap().push(CapturedEvent::StageStarted { stage, items });
    }
    fn item_started(&self, stage: Stage, name: &str) {
        self.events.lock().unwrap().push(CapturedEvent::ItemStarted { stage, name: name.into() });
    }
    fn item_finished(&self, stage: Stage, name: &str, outcome: &ItemOutcome) {
        self.events.lock().unwrap().push(CapturedEvent::ItemFinished {
            stage, name: name.into(), outcome: outcome_label(outcome),
        });
    }
    fn stage_finished(&self, summary: &StageSummary) {
        self.events.lock().unwrap().push(CapturedEvent::StageFinished(summary.clone()));
    }
    fn print_plan(&self, _plan: &Plan) {
        self.events.lock().unwrap().push(CapturedEvent::PrintPlan);
    }
    fn pipeline_summary(&self, summary: &PipelineSummary) {
        self.events.lock().unwrap().push(CapturedEvent::PipelineSummary(summary.clone()));
    }
    fn warning(&self, msg: &str) {
        self.events.lock().unwrap().push(CapturedEvent::Warning(msg.into()));
    }
    fn info(&self, msg: &str) {
        self.events.lock().unwrap().push(CapturedEvent::Info(msg.into()));
    }
}
```

- [ ] **Step 5.5:** Reporter 引用了 `crate::sync::plan` 但 sync/plan.rs 还不存在。把 `src/lib.rs` 的 `pub mod sync;` 暂保持注释；reporter 引用先用 stub。

实操：reporter 模块内部不引用 sync::plan 的话编不通。**临时做法**：在 reporter/mod.rs 顶部加：

```rust
// SAFETY: temporary forward ref. sync::plan lands in Task 8.
// 暂时 inline 一个最小 Plan 占位，task 8 替换。
```

更稳的做法：**先做 Task 8（sync::plan）再回 Task 5**。**修正任务序**：把 Task 8 提前到 Task 5 之前。

下方按修正后序号继续。**Task 5 暂停，跳到 Task 8（重命名为新 Task 5）**。

---

## Task 5 (renumbered, was Task 8): sync/plan.rs — Plan + Action enums

**Files:**
- Create: `src/sync/plan.rs`
- Modify: `src/lib.rs`（打开 `pub mod sync;`）
- Modify: `src/sync/mod.rs`（添加 `pub mod plan;`）

- [ ] **Step 5.1:** 在 `src/lib.rs` 打开 `pub mod sync;`（其它仍可注释）。

- [ ] **Step 5.2:** 在 `src/sync/mod.rs` 顶部加 `pub mod plan;` 行（保留其余原内容，本 task 不改 mod.rs 的实现）。

- [ ] **Step 5.3:** 创建 `src/sync/plan.rs`：

```rust
//! Pure-data Plan and Action types for the Flux pipeline.
//!
//! `plan_*` functions in `sync::file/script/block` produce these. They never
//! mutate remote state; the `execute_*` companions consume them.

use crate::sync::SyncError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkipReason {
    AlreadyExists,
    RemoteNewer,
    ContentUnchanged,
    DependencyFailed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sentinel {
    pub name: String,
    pub timestamp: i64,
    /// Full open marker line, e.g. `# >>> aliases:1700000000 >>>`
    pub open_marker: String,
    pub close_marker: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum FileAction {
    Skip { item_name: String, reason: SkipReason },
    Apply {
        item_name: String,
        dst: String,
        bytes: Vec<u8>,
        chmod: Option<u32>,
    },
    Failed { item_name: String, error: SyncError },
}

#[derive(Debug, PartialEq, Eq)]
pub enum ScriptAction {
    Skip { item_name: String, reason: SkipReason },
    Run {
        item_name: String,
        upload_to: String,
        local_script_bytes: Vec<u8>,
        command_argv: Vec<String>,
    },
    Failed { item_name: String, error: SyncError },
}

#[derive(Debug, PartialEq, Eq)]
pub enum BlockAction {
    Skip { item_name: String, reason: SkipReason },
    Apply {
        item_name: String,
        target: String,
        body: String,
        sentinel: Sentinel,
    },
    Failed { item_name: String, error: SyncError },
}

#[derive(Debug, PartialEq, Eq)]
pub struct RegisterPubkeyAction {
    pub local_pubkey_path: String,
    pub remote_authorized_keys: String,
}

#[derive(Debug, PartialEq, Eq, Default)]
pub struct Plan {
    pub register_pubkey: Option<RegisterPubkeyAction>,
    pub file_actions: Vec<FileAction>,
    pub script_actions: Vec<ScriptAction>,
    pub block_actions: Vec<BlockAction>,
}

impl Plan {
    pub fn is_empty(&self) -> bool {
        self.register_pubkey.is_none()
            && self.file_actions.is_empty()
            && self.script_actions.is_empty()
            && self.block_actions.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::block::BlockError;

    #[test]
    fn plan_empty() {
        let p = Plan::default();
        assert!(p.is_empty());
    }

    #[test]
    fn block_action_failed_has_error() {
        let a = BlockAction::Failed {
            item_name: "x".into(),
            error: SyncError::Block(BlockError::BadTemplate),
        };
        assert!(matches!(a, BlockAction::Failed { .. }));
    }
}
```

注：`SyncError` 与 `BlockError` 在 sync/mod.rs / sync/block.rs 中定义；本 task **先做 SyncError 占位**，详细实现在 Task 6/7/8/9。

在 `src/sync/mod.rs` 暂加：

```rust
pub mod plan;
pub mod block;  // 占位，下个 task 写
pub mod file;   // 占位
pub mod script; // 占位

#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error("block: {0}")] Block(#[from] block::BlockError),
    #[error("file: {0}")]  File(#[from] file::FileError),
    #[error("script: {0}")]Script(#[from] script::ScriptError),
    #[error("remote: {0}")]Remote(#[from] crate::remote::RemoteOpsError),
}
```

并在 `src/sync/block.rs`、`src/sync/file.rs`、`src/sync/script.rs` 各放一个最小 `*Error` enum 占位（**先 stub**，Task 6/7/8 完整实现）：

```rust
// src/sync/block.rs (开头加，原内容保留在文件下方但不调用)
#[derive(Debug, thiserror::Error)]
pub enum BlockError {
    #[error("bad comment template")] BadTemplate,
}
// 同样在 file.rs / script.rs 各 stub 一个空 enum + 一个 variant
```

- [ ] **Step 5.4:** Run: `cargo test --lib --offline sync::plan::tests`
Expected: 2 tests pass.

- [ ] **Step 5.5:** Commit：

```bash
git add src/sync/plan.rs src/sync/mod.rs src/sync/block.rs src/sync/file.rs src/sync/script.rs src/lib.rs
git commit -m "phase2: introduce Plan + Action types in sync::plan"
```

---

## Task 6: Reporter trait + Console + Memory（原 Task 5 续）

**Files:**
- Create: `src/reporter/mod.rs`
- Create: `src/reporter/console.rs`
- Create: `src/reporter/memory.rs`
- Modify: `src/lib.rs`

按 Task 5（原编号）的 Step 5.2 / 5.3 / 5.4 内容创建三个文件。Plan/Action 类型已就位，`crate::sync::plan` 引用现可解析。

- [ ] **Step 6.1:** Create `src/reporter/mod.rs`、`console.rs`、`memory.rs`（内容见上方原 Task 5 的 Step 5.2-5.4）。

- [ ] **Step 6.2:** 在 `src/lib.rs` 打开 `pub mod reporter;`。

- [ ] **Step 6.3:** Run: `cargo check --lib --offline`
Expected: 通过（reporter 引用 sync::plan 已可解析）。

- [ ] **Step 6.4:** 编写 reporter 测试 `src/reporter/console.rs` 末尾 `#[cfg(test)] mod tests`：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::plan::{FileAction, Plan};

    #[test]
    fn console_does_not_panic_on_empty_plan() {
        let r = ConsoleReporter::new();
        r.print_plan(&Plan::default());
    }

    #[test]
    fn skip_reason_label_covers_all_variants() {
        for r in [
            SkipReason::AlreadyExists,
            SkipReason::RemoteNewer,
            SkipReason::ContentUnchanged,
            SkipReason::DependencyFailed("x".into()),
        ] {
            let s = skip_reason_label(&r);
            assert!(!s.is_empty());
        }
    }
}
```

也给 memory.rs 加测试：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn applied_count_tallies() {
        let r = CapturedReporter::new();
        r.item_finished(Stage::File, "a", &ItemOutcome::Applied);
        r.item_finished(Stage::File, "b", &ItemOutcome::Failed("err".into()));
        r.item_finished(Stage::File, "c", &ItemOutcome::Applied);
        assert_eq!(r.applied_count(Stage::File), 2);
        assert_eq!(r.failed_items(Stage::File), vec!["b"]);
    }
}
```

- [ ] **Step 6.5:** Run: `cargo test --lib --offline reporter`
Expected: 3 tests pass.

- [ ] **Step 6.6:** Commit：

```bash
git add src/reporter src/lib.rs
git commit -m "phase2: add Reporter trait + console & memory impls"
```

---

## Task 7: Move src/output.rs into reporter/console.rs (cleanup)

**Files:**
- Delete: `src/output.rs`
- Modify: `src/main.rs`、`src/sync/mod.rs`、`src/sync/*.rs`（替换 output:: 调用）

- [ ] **Step 7.1:** 全局把 `crate::output::print_*` 调用替换成 `Reporter` 方法或 `ConsoleReporter` 直接实例化。这一步 main.rs 内的 stage 输出会先用 `let reporter = ConsoleReporter::new();` 然后 `reporter.info(...)` / `reporter.warning(...)`。

Run: `grep -rn "use crate::output\|output::print" src/`

逐处修改：
- `output::print_warning(s)` → `reporter.warning(s)`
- `output::print_info(s)` → `reporter.info(s)`
- 其它 `print_file/print_script/print_block/print_*_result` 留给 Task 11/12 在 Pipeline 重构时彻底替换；本 task 仅做 import 调整不立即删。

实操：保持 `src/output.rs` 文件**暂不删**，仅在 main.rs / sync 调用点引入 Reporter。`src/output.rs` 在 Task 12 末尾彻底删除。

- [ ] **Step 7.2:** 在 main.rs 里 `let reporter: Box<dyn Reporter> = Box::new(ConsoleReporter::new());` 作为全局；后续 task 把它传给 Pipeline。

- [ ] **Step 7.3:** Commit:

```bash
git add -A
git commit -m "phase2: introduce Reporter into main entry"
```

（Task 7 是过渡步，不要求 cargo check 通过；但应该过——只是把 import 加上而已。）

---

## Task 8: sync/file.rs — plan_files + execute_file + tests

**Files:**
- Rewrite: `src/sync/file.rs`

- [ ] **Step 8.1:** 完整重写 `src/sync/file.rs`：

```rust
//! File sync stage.

use crate::config::FileItem;
use crate::path::FluxPath;
use crate::remote::{RemoteOps, RemoteOpsError};
use crate::reporter::{ItemOutcome, Reporter, Stage};
use crate::sync::plan::{FileAction, SkipReason};
use sha2::{Digest, Sha256};
use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum FileError {
    #[error("source not found: {0}")]
    SourceNotFound(String),
    #[error("source is a directory, not a file: {0}")]
    SourceIsDirectory(String),
    #[error("local io: {0}")]
    LocalIo(String),
    #[error("invalid path: {0}")]
    InvalidPath(String),
    #[error("only local→remote sync is supported (got src={src} dst={dst})")]
    UnsupportedDirection { src: String, dst: String },
}

/// Compute file actions without touching the remote write surface.
pub async fn plan_files<R: RemoteOps + ?Sized>(
    items: &[FileItem],
    remote: &R,
) -> Vec<FileAction> {
    let mut actions = Vec::with_capacity(items.len());
    for item in items {
        actions.push(plan_one_file(item, remote).await);
    }
    actions
}

async fn plan_one_file<R: RemoteOps + ?Sized>(item: &FileItem, remote: &R) -> FileAction {
    let item_name = item.name.clone().unwrap_or_else(|| item.src.clone());
    // Parse paths
    let src = match FluxPath::parse(&item.src) {
        Ok(p) => p,
        Err(e) => return FileAction::Failed {
            item_name,
            error: FileError::InvalidPath(format!("src: {e}")).into(),
        },
    };
    let dst = match FluxPath::parse(&item.dst) {
        Ok(p) => p,
        Err(e) => return FileAction::Failed {
            item_name,
            error: FileError::InvalidPath(format!("dst: {e}")).into(),
        },
    };
    // Only local→remote supported
    let local_path = match src {
        FluxPath::Local(p) => p,
        FluxPath::Remote(_) => return FileAction::Failed {
            item_name,
            error: FileError::UnsupportedDirection {
                src: item.src.clone(), dst: item.dst.clone(),
            }.into(),
        },
    };
    let remote_path = match dst {
        FluxPath::Remote(p) => p,
        FluxPath::Local(_) => return FileAction::Failed {
            item_name,
            error: FileError::UnsupportedDirection {
                src: item.src.clone(), dst: item.dst.clone(),
            }.into(),
        },
    };
    // Read local
    let bytes = match std::fs::read(&local_path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return FileAction::Failed {
                item_name,
                error: FileError::SourceNotFound(local_path.display().to_string()).into(),
            }
        }
        Err(e) => return FileAction::Failed {
            item_name,
            error: FileError::LocalIo(e.to_string()).into(),
        },
    };
    // Mode-specific decisions
    let mode = item.mode.as_deref().unwrap_or("sync");
    let chmod = item.chmod.as_deref().and_then(|s| u32::from_str_radix(s, 8).ok());
    let exists_remote = match remote.exists(&remote_path).await {
        Ok(b) => b,
        Err(e) => return FileAction::Failed { item_name, error: e.into() },
    };
    match mode {
        "touch" if exists_remote => FileAction::Skip {
            item_name,
            reason: SkipReason::AlreadyExists,
        },
        "sync" if exists_remote => {
            // mtime check first; equal mtime → hash; remote newer → skip
            let local_mtime = match local_mtime(&local_path) {
                Ok(t) => t,
                Err(e) => return FileAction::Failed { item_name, error: e.into() },
            };
            match remote.mtime(&remote_path).await {
                Ok(rt) if rt > local_mtime => FileAction::Skip {
                    item_name, reason: SkipReason::RemoteNewer,
                },
                Ok(rt) if rt == local_mtime => {
                    // hash compare
                    match remote.read_file(&remote_path).await {
                        Ok(rbytes) if hash(&rbytes) == hash(&bytes) => FileAction::Skip {
                            item_name, reason: SkipReason::ContentUnchanged,
                        },
                        Ok(_) => FileAction::Apply { item_name, dst: remote_path, bytes, chmod },
                        Err(e) => FileAction::Failed { item_name, error: e.into() },
                    }
                }
                Ok(_) => FileAction::Apply { item_name, dst: remote_path, bytes, chmod },
                Err(e) => FileAction::Failed { item_name, error: e.into() },
            }
        }
        // "cover" or "sync" + !exists → just apply
        _ => FileAction::Apply { item_name, dst: remote_path, bytes, chmod },
    }
}

fn local_mtime(path: &Path) -> Result<chrono::DateTime<chrono::Utc>, RemoteOpsError> {
    let meta = std::fs::metadata(path).map_err(|e| RemoteOpsError::Io(e.to_string()))?;
    let mt = meta.modified().map_err(|e| RemoteOpsError::Io(e.to_string()))?;
    Ok(chrono::DateTime::<chrono::Utc>::from(mt))
}

fn hash(bytes: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(bytes);
    h.finalize().into()
}

pub async fn execute_file<R: RemoteOps + ?Sized>(
    action: &FileAction,
    remote: &R,
    reporter: &dyn Reporter,
) -> ItemOutcome {
    let name = match action {
        FileAction::Skip { item_name, .. }
        | FileAction::Apply { item_name, .. }
        | FileAction::Failed { item_name, .. } => item_name.clone(),
    };
    reporter.item_started(Stage::File, &name);
    let outcome = match action {
        FileAction::Skip { reason, .. } => ItemOutcome::Skipped(reason.clone()),
        FileAction::Failed { error, .. } => ItemOutcome::Failed(error.to_string()),
        FileAction::Apply { dst, bytes, chmod, .. } => {
            // ensure parent dir
            if let Some(parent) = parent_dir(dst) {
                if let Err(e) = remote.ensure_dir(parent).await {
                    return finish(reporter, &name, ItemOutcome::Failed(e.to_string()));
                }
            }
            if let Err(e) = remote.write_file(dst, bytes).await {
                return finish(reporter, &name, ItemOutcome::Failed(e.to_string()));
            }
            if let Some(m) = chmod {
                if let Err(e) = remote.chmod(dst, *m).await {
                    return finish(reporter, &name, ItemOutcome::Failed(e.to_string()));
                }
            }
            ItemOutcome::Applied
        }
    };
    reporter.item_finished(Stage::File, &name, &outcome);
    outcome
}

fn finish(reporter: &dyn Reporter, name: &str, outcome: ItemOutcome) -> ItemOutcome {
    reporter.item_finished(Stage::File, name, &outcome);
    outcome
}

fn parent_dir(path: &str) -> Option<&str> {
    path.rfind('/').map(|i| &path[..i])
}
```

- [ ] **Step 8.2:** 加单元测试：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::FileItem;
    use crate::remote::fake::InMemoryRemote;
    use crate::reporter::memory::CapturedReporter;
    use tempfile::TempDir;

    fn local_file(dir: &TempDir, name: &str, content: &[u8]) -> String {
        let p = dir.path().join(name);
        std::fs::write(&p, content).unwrap();
        p.to_string_lossy().into_owned()
    }

    fn item(name: &str, src: &str, dst: &str, mode: &str) -> FileItem {
        FileItem {
            name: Some(name.into()),
            src: src.into(),
            dst: dst.into(),
            mode: Some(mode.into()),
            chmod: None,
        }
    }

    #[tokio::test]
    async fn touch_skips_when_remote_exists() {
        let tmp = TempDir::new().unwrap();
        let src = local_file(&tmp, "a.txt", b"x");
        let remote = InMemoryRemote::with_files([("/r/a.txt", b"old".to_vec())]);
        let actions = plan_files(&[item("a", &src, ":/r/a.txt", "touch")], &remote).await;
        assert!(matches!(&actions[0], FileAction::Skip { reason: SkipReason::AlreadyExists, .. }));
    }

    #[tokio::test]
    async fn cover_always_applies() {
        let tmp = TempDir::new().unwrap();
        let src = local_file(&tmp, "a.txt", b"new");
        let remote = InMemoryRemote::with_files([("/r/a.txt", b"old".to_vec())]);
        let actions = plan_files(&[item("a", &src, ":/r/a.txt", "cover")], &remote).await;
        assert!(matches!(&actions[0], FileAction::Apply { .. }));
    }

    #[tokio::test]
    async fn sync_skip_when_remote_newer() {
        use chrono::{Duration, Utc};
        let tmp = TempDir::new().unwrap();
        let src = local_file(&tmp, "a.txt", b"x");
        let remote = InMemoryRemote::with_files([("/r/a.txt", b"old".to_vec())]);
        remote.set_mtime("/r/a.txt", Utc::now() + Duration::seconds(60));
        let actions = plan_files(&[item("a", &src, ":/r/a.txt", "sync")], &remote).await;
        assert!(matches!(&actions[0], FileAction::Skip { reason: SkipReason::RemoteNewer, .. }));
    }

    #[tokio::test]
    async fn sync_skip_when_content_identical_with_equal_mtime() {
        let tmp = TempDir::new().unwrap();
        let src = local_file(&tmp, "a.txt", b"same");
        let local_mtime = std::fs::metadata(&src).unwrap().modified().unwrap();
        let remote = InMemoryRemote::with_files([("/r/a.txt", b"same".to_vec())]);
        remote.set_mtime("/r/a.txt", chrono::DateTime::<chrono::Utc>::from(local_mtime));
        let actions = plan_files(&[item("a", &src, ":/r/a.txt", "sync")], &remote).await;
        assert!(matches!(&actions[0], FileAction::Skip { reason: SkipReason::ContentUnchanged, .. }));
    }

    #[tokio::test]
    async fn missing_source_returns_failed() {
        let remote = InMemoryRemote::new();
        let actions = plan_files(&[item("a", "/no/such/file", ":/r/a.txt", "cover")], &remote).await;
        assert!(matches!(&actions[0], FileAction::Failed { .. }));
    }

    #[tokio::test]
    async fn execute_apply_writes_bytes_and_chmod() {
        let tmp = TempDir::new().unwrap();
        let src = local_file(&tmp, "a.txt", b"hello");
        let remote = InMemoryRemote::new();
        let mut it = item("a", &src, ":/r/a.txt", "cover");
        it.chmod = Some("600".into());
        let actions = plan_files(&[it], &remote).await;
        let reporter = CapturedReporter::new();
        let outcome = execute_file(&actions[0], &remote, &reporter).await;
        assert!(matches!(outcome, ItemOutcome::Applied));
        assert_eq!(remote.file_contents("/r/a.txt"), Some(b"hello".to_vec()));
        assert_eq!(remote.file_mode("/r/a.txt"), Some(0o600));
    }
}
```

- [ ] **Step 8.3:** Run: `cargo test --lib --offline sync::file::tests`
Expected: 6 tests pass。

- [ ] **Step 8.4:** Commit:

```bash
git add src/sync/file.rs
git commit -m "phase2: rewrite sync/file as plan_files + execute_file with tests"
```

---

## Task 9: sync/script.rs — plan_scripts + execute_script + shell_quote

**Files:**
- Rewrite: `src/sync/script.rs`

- [ ] **Step 9.1:** 完整重写 `src/sync/script.rs`：

```rust
//! Script execution stage.

use crate::config::ScriptItem;
use crate::remote::{RemoteOps, RemoteOpsError};
use crate::reporter::{ItemOutcome, Reporter, Stage};
use crate::sync::plan::{ScriptAction, SkipReason};
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum ScriptError {
    #[error("script source not found: {0}")]
    SourceNotFound(String),
    #[error("local io: {0}")]
    LocalIo(String),
    #[error("internal: dependency {0} not validated; this is a bug")]
    UnvalidatedDependency(String),
}

/// Quote a string for safe inclusion in a `/bin/sh` command.
/// Wraps in single quotes; escapes embedded `'` as `'\''`.
pub fn shell_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        if c == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

pub async fn plan_scripts<R: RemoteOps + ?Sized>(
    items: &[ScriptItem],
    file_status: &HashMap<String, bool>,
    asset_root: &Path,
    default_interpreter: &str,
    default_flags: &[String],
) -> Vec<ScriptAction> {
    let mut out = Vec::with_capacity(items.len());
    for it in items {
        out.push(plan_one_script(it, file_status, asset_root, default_interpreter, default_flags));
    }
    out
}

fn plan_one_script(
    it: &ScriptItem,
    file_status: &HashMap<String, bool>,
    asset_root: &Path,
    default_interpreter: &str,
    default_flags: &[String],
) -> ScriptAction {
    let item_name = it.name.clone().unwrap_or_else(|| it.path.clone());
    // dependency check
    for dep in &it.dependencies {
        match file_status.get(dep) {
            Some(true) => continue,
            Some(false) => return ScriptAction::Skip {
                item_name,
                reason: SkipReason::DependencyFailed(dep.clone()),
            },
            None => return ScriptAction::Failed {
                item_name,
                error: ScriptError::UnvalidatedDependency(dep.clone()).into(),
            },
        }
    }
    // local script bytes
    let local_path = asset_root.join(&it.path);
    let bytes = match std::fs::read(&local_path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return ScriptAction::Failed {
                item_name,
                error: ScriptError::SourceNotFound(local_path.display().to_string()).into(),
            }
        }
        Err(e) => return ScriptAction::Failed {
            item_name,
            error: ScriptError::LocalIo(e.to_string()).into(),
        },
    };
    let upload_to = format!(
        "/tmp/flux_script_{}_{}.sh",
        std::process::id(),
        item_name.replace(['/', '.', ' '], "_"),
    );
    let interpreter = it.interpreter.as_deref().unwrap_or(default_interpreter);
    let flags = if it.flags.is_empty() { default_flags.to_vec() } else { it.flags.clone() };
    let mut argv = vec![interpreter.to_string()];
    argv.extend(flags);
    argv.push(upload_to.clone());
    argv.extend(it.args.iter().cloned());
    ScriptAction::Run {
        item_name,
        upload_to,
        local_script_bytes: bytes,
        command_argv: argv,
    }
}

pub async fn execute_script<R: RemoteOps + ?Sized>(
    action: &ScriptAction,
    remote: &R,
    reporter: &dyn Reporter,
) -> ItemOutcome {
    let name = match action {
        ScriptAction::Skip { item_name, .. }
        | ScriptAction::Run { item_name, .. }
        | ScriptAction::Failed { item_name, .. } => item_name.clone(),
    };
    reporter.item_started(Stage::Script, &name);
    let outcome = match action {
        ScriptAction::Skip { reason, .. } => ItemOutcome::Skipped(reason.clone()),
        ScriptAction::Failed { error, .. } => ItemOutcome::Failed(error.to_string()),
        ScriptAction::Run { upload_to, local_script_bytes, command_argv, .. } => {
            // upload script
            if let Err(e) = remote.write_file(upload_to, local_script_bytes).await {
                return finish_script(reporter, &name, ItemOutcome::Failed(e.to_string()));
            }
            if let Err(e) = remote.chmod(upload_to, 0o755).await {
                return finish_script(reporter, &name, ItemOutcome::Failed(e.to_string()));
            }
            // build cmd: shell-quote each argv component
            let cmd = command_argv.iter().map(|a| shell_quote(a)).collect::<Vec<_>>().join(" ");
            match remote.interactive_exec(&cmd).await {
                Ok(0) => ItemOutcome::Applied,
                Ok(code) => ItemOutcome::Failed(format!("exit code {code}")),
                Err(e) => ItemOutcome::Failed(e.to_string()),
            }
        }
    };
    reporter.item_finished(Stage::Script, &name, &outcome);
    outcome
}

fn finish_script(reporter: &dyn Reporter, name: &str, outcome: ItemOutcome) -> ItemOutcome {
    reporter.item_finished(Stage::Script, name, &outcome);
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ScriptItem;
    use crate::remote::fake::InMemoryRemote;
    use crate::reporter::memory::CapturedReporter;
    use tempfile::TempDir;

    fn item(name: &str, path: &str, deps: &[&str]) -> ScriptItem {
        ScriptItem {
            name: Some(name.into()),
            path: path.into(),
            args: vec![],
            interpreter: None,
            flags: vec![],
            dependencies: deps.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn shell_quote_handles_single_quotes() {
        assert_eq!(shell_quote("a'b"), r#"'a'\''b'"#);
        assert_eq!(shell_quote("plain"), "'plain'");
        assert_eq!(shell_quote(""), "''");
    }

    #[tokio::test]
    async fn dependency_failed_skips() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("s.sh"), b"#!/bin/sh\necho hi").unwrap();
        let mut deps = HashMap::new();
        deps.insert("dep1".to_string(), false);
        let acts = plan_scripts(
            &[item("s", "s.sh", &["dep1"])],
            &deps, tmp.path(), "/bin/bash", &[],
        ).await;
        assert!(matches!(&acts[0], ScriptAction::Skip { reason: SkipReason::DependencyFailed(d), .. } if d == "dep1"));
    }

    #[tokio::test]
    async fn unknown_dependency_is_failed_action() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("s.sh"), b"#!/bin/sh").unwrap();
        let acts = plan_scripts(
            &[item("s", "s.sh", &["never-validated"])],
            &HashMap::new(), tmp.path(), "/bin/bash", &[],
        ).await;
        assert!(matches!(&acts[0], ScriptAction::Failed { .. }));
    }

    #[tokio::test]
    async fn run_action_uploads_and_execs() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("s.sh"), b"#!/bin/sh\necho hi").unwrap();
        let acts = plan_scripts(
            &[item("s", "s.sh", &[])], &HashMap::new(), tmp.path(), "/bin/bash", &[],
        ).await;
        let remote = InMemoryRemote::new();
        let reporter = CapturedReporter::new();
        let outcome = execute_script::<InMemoryRemote>(&acts[0], &remote, &reporter).await;
        assert!(matches!(outcome, ItemOutcome::Applied));
        let writes = remote.write_calls();
        assert_eq!(writes.len(), 1);
        assert!(writes[0].0.starts_with("/tmp/flux_script_"));
        let interactive = remote.interactive_calls();
        assert_eq!(interactive.len(), 1);
        assert!(interactive[0].contains("'/bin/bash'"));
    }

    proptest::proptest! {
        #[test]
        fn shell_quote_round_trip(s in r#"[^\x00]{0,40}"#) {
            // Decode shell_quote back: in single-quote mode, only `'\''` is escape;
            // everything else is literal.
            let q = shell_quote(&s);
            assert!(q.starts_with('\''));
            assert!(q.ends_with('\''));
            // strip outer quotes
            let inner = &q[1..q.len()-1];
            // replace `'\''` back to `'`
            let decoded = inner.replace(r#"'\''"#, "'");
            assert_eq!(decoded, s);
        }
    }
}
```

- [ ] **Step 9.2:** Run: `cargo test --lib --offline sync::script::tests`
Expected: 4 tests + 1 proptest pass.

- [ ] **Step 9.3:** Commit:

```bash
git add src/sync/script.rs
git commit -m "phase2: rewrite sync/script as plan_scripts + execute_script with shell_quote"
```

---

## Task 10: sync/block.rs — sentinel parser + plan_blocks + execute_block

**Files:**
- Rewrite: `src/sync/block.rs`
- Create: `tests/proptests/block_sentinel.rs`

- [ ] **Step 10.1:** 完整重写 `src/sync/block.rs`：

```rust
//! Block injection stage.

use crate::config::BlockItem;
use crate::remote::{RemoteOps, RemoteOpsError};
use crate::reporter::{ItemOutcome, Reporter, Stage};
use crate::sync::plan::{BlockAction, Sentinel, SkipReason};
use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};
use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum BlockError {
    #[error("comment template missing {{}} placeholder")]
    BadTemplate,
    #[error("malformed sentinel for block '{name}'")]
    MalformedSentinel { name: String },
    #[error("local block source not found: {0}")]
    SourceNotFound(String),
    #[error("local io: {0}")]
    LocalIo(String),
}

/// Build the open and close marker lines for a block sentinel.
///
/// `template` MUST contain exactly one `{}` placeholder (e.g. `# {}`).
pub fn build_markers(template: &str, name: &str, timestamp: i64) -> Result<(String, String), BlockError> {
    if !template.contains("{}") {
        return Err(BlockError::BadTemplate);
    }
    let open = template.replace("{}", &format!(">>> {name}:{timestamp} >>>"));
    let close = template.replace("{}", &format!("<<< {name}:{timestamp} <<<"));
    Ok((open, close))
}

/// Find an existing block in `content`. Returns `(open_idx, close_idx, sentinel)` or None.
/// Strict matching: the line must equal the template applied to `>>> name:N >>>`.
pub fn find_block(template: &str, name: &str, content: &str) -> Result<Option<FoundBlock>, BlockError> {
    if !template.contains("{}") {
        return Err(BlockError::BadTemplate);
    }
    // Pattern for line scanning. Match any timestamp.
    let prefix_open = template.replace("{}", &format!(">>> {name}:"));
    let prefix_open = prefix_open.trim_end();
    let prefix_close = template.replace("{}", &format!("<<< {name}:"));
    let prefix_close = prefix_close.trim_end();
    let suffix_open = " >>>";
    let suffix_close = " <<<";
    // Walk lines preserving byte offsets.
    let mut byte = 0usize;
    let mut open: Option<(usize, usize, i64)> = None;
    let mut close: Option<(usize, usize)> = None;
    for piece in split_keep_terminators(content) {
        let line = piece.trim_end_matches(['\n', '\r']);
        if open.is_none() {
            if line.starts_with(prefix_open) && line.ends_with(suffix_open) {
                let mid = &line[prefix_open.len()..line.len() - suffix_open.len()];
                if let Ok(ts) = mid.parse::<i64>() {
                    open = Some((byte, byte + piece.len(), ts));
                }
            }
        } else if close.is_none() {
            if line.starts_with(prefix_close) && line.ends_with(suffix_close) {
                let mid = &line[prefix_close.len()..line.len() - suffix_close.len()];
                if mid.parse::<i64>().is_ok() {
                    close = Some((byte, byte + piece.len()));
                    break;
                }
            }
        }
        byte += piece.len();
    }
    match (open, close) {
        (Some((o_start, _, ts)), Some((_, c_end))) => Ok(Some(FoundBlock {
            byte_range: o_start..c_end,
            timestamp: ts,
        })),
        (Some(_), None) => Err(BlockError::MalformedSentinel { name: name.into() }),
        _ => Ok(None),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FoundBlock {
    pub byte_range: std::ops::Range<usize>,
    pub timestamp: i64,
}

fn split_keep_terminators(s: &str) -> Vec<&str> {
    s.split_inclusive('\n').collect()
}

pub async fn plan_blocks<R: RemoteOps + ?Sized>(
    items: &[BlockItem],
    asset_root: &Path,
    template: &str,
    remote: &R,
) -> Vec<BlockAction> {
    let mut out = Vec::with_capacity(items.len());
    for it in items {
        out.push(plan_one_block(it, asset_root, template, remote).await);
    }
    out
}

async fn plan_one_block<R: RemoteOps + ?Sized>(
    it: &BlockItem,
    asset_root: &Path,
    template: &str,
    remote: &R,
) -> BlockAction {
    let item_name = it.name.clone();
    let local_path = asset_root.join(&it.path);
    let local_body = match std::fs::read_to_string(&local_path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return BlockAction::Failed {
                item_name,
                error: BlockError::SourceNotFound(local_path.display().to_string()).into(),
            }
        }
        Err(e) => return BlockAction::Failed {
            item_name,
            error: BlockError::LocalIo(e.to_string()).into(),
        },
    };
    // Parse target as remote path
    let target = match crate::path::FluxPath::parse(&it.file) {
        Ok(crate::path::FluxPath::Remote(p)) => p,
        Ok(_) => return BlockAction::Failed {
            item_name,
            error: BlockError::LocalIo(format!("block target must be remote: {}", it.file)).into(),
        },
        Err(e) => return BlockAction::Failed {
            item_name,
            error: BlockError::LocalIo(e.to_string()).into(),
        },
    };
    // Read remote target if exists
    let remote_exists = match remote.exists(&target).await {
        Ok(b) => b,
        Err(e) => return BlockAction::Failed { item_name, error: e.into() },
    };
    let timestamp = Utc::now().timestamp();
    let (open_marker, close_marker) = match build_markers(template, &item_name, timestamp) {
        Ok(p) => p,
        Err(e) => return BlockAction::Failed { item_name, error: e.into() },
    };
    let sentinel = Sentinel {
        name: item_name.clone(),
        timestamp,
        open_marker: open_marker.clone(),
        close_marker: close_marker.clone(),
    };
    if !remote_exists {
        return BlockAction::Apply {
            item_name,
            target,
            body: local_body,
            sentinel,
        };
    }
    let remote_content = match remote.read_file(&target).await {
        Ok(b) => String::from_utf8_lossy(&b).into_owned(),
        Err(e) => return BlockAction::Failed { item_name, error: e.into() },
    };
    let found = match find_block(template, &item_name, &remote_content) {
        Ok(f) => f,
        Err(e) => return BlockAction::Failed { item_name, error: e.into() },
    };
    let mode = it.mode.as_deref().unwrap_or("sync");
    match (mode, found) {
        ("sync", Some(fb)) => {
            // compare extracted body
            let existing_body = extract_body(&remote_content, &fb);
            if hash(existing_body.as_bytes()) == hash(local_body.as_bytes()) {
                BlockAction::Skip {
                    item_name,
                    reason: SkipReason::ContentUnchanged,
                }
            } else {
                // remote-mtime-vs-local-mtime check
                let local_mtime = std::fs::metadata(&local_path).and_then(|m| m.modified()).ok();
                let remote_mtime = remote.mtime(&target).await.ok();
                if let (Some(rt), Some(lt)) = (remote_mtime, local_mtime) {
                    let lt: DateTime<Utc> = lt.into();
                    if rt > lt {
                        return BlockAction::Skip {
                            item_name,
                            reason: SkipReason::RemoteNewer,
                        };
                    }
                }
                BlockAction::Apply { item_name, target, body: local_body, sentinel }
            }
        }
        _ => BlockAction::Apply { item_name, target, body: local_body, sentinel },
    }
}

fn extract_body(content: &str, found: &FoundBlock) -> String {
    let block = &content[found.byte_range.clone()];
    // Drop first and last line (markers).
    let mut lines: Vec<&str> = block.split_inclusive('\n').collect();
    if !lines.is_empty() { lines.remove(0); }
    if !lines.is_empty() { lines.pop(); }
    lines.concat()
}

fn hash(bytes: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(bytes);
    h.finalize().into()
}

pub async fn execute_block<R: RemoteOps + ?Sized>(
    action: &BlockAction,
    remote: &R,
    template: &str,
    reporter: &dyn Reporter,
) -> ItemOutcome {
    let name = match action {
        BlockAction::Skip { item_name, .. }
        | BlockAction::Apply { item_name, .. }
        | BlockAction::Failed { item_name, .. } => item_name.clone(),
    };
    reporter.item_started(Stage::Block, &name);
    let outcome = match action {
        BlockAction::Skip { reason, .. } => ItemOutcome::Skipped(reason.clone()),
        BlockAction::Failed { error, .. } => ItemOutcome::Failed(error.to_string()),
        BlockAction::Apply { target, body, sentinel, .. } => {
            // Re-read target to get current state (other blocks in same group may have written)
            let cur = match remote.exists(target).await {
                Ok(true) => match remote.read_file(target).await {
                    Ok(b) => String::from_utf8_lossy(&b).into_owned(),
                    Err(e) => return finish_block(reporter, &name, ItemOutcome::Failed(e.to_string())),
                },
                Ok(false) => String::new(),
                Err(e) => return finish_block(reporter, &name, ItemOutcome::Failed(e.to_string())),
            };
            let new_content = compose(&cur, body, sentinel, template, &name);
            match new_content {
                Ok(c) => match remote.write_file(target, c.as_bytes()).await {
                    Ok(()) => ItemOutcome::Applied,
                    Err(e) => ItemOutcome::Failed(e.to_string()),
                },
                Err(e) => ItemOutcome::Failed(e.to_string()),
            }
        }
    };
    reporter.item_finished(Stage::Block, &name, &outcome);
    outcome
}

fn finish_block(reporter: &dyn Reporter, name: &str, outcome: ItemOutcome) -> ItemOutcome {
    reporter.item_finished(Stage::Block, name, &outcome);
    outcome
}

fn compose(
    existing: &str,
    body: &str,
    sentinel: &Sentinel,
    template: &str,
    name: &str,
) -> Result<String, BlockError> {
    let injected = format!(
        "{}\n{}{}{}\n",
        sentinel.open_marker,
        body,
        if body.ends_with('\n') { "" } else { "\n" },
        sentinel.close_marker,
    );
    match find_block(template, name, existing)? {
        Some(fb) => {
            let mut s = String::with_capacity(existing.len());
            s.push_str(&existing[..fb.byte_range.start]);
            s.push_str(&injected);
            s.push_str(&existing[fb.byte_range.end..]);
            Ok(s)
        }
        None => {
            let mut s = String::from(existing);
            if !s.ends_with('\n') && !s.is_empty() {
                s.push('\n');
            }
            s.push_str(&injected);
            Ok(s)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_markers_basic() {
        let (o, c) = build_markers("# {}", "aliases", 1700000000).unwrap();
        assert_eq!(o, "# >>> aliases:1700000000 >>>");
        assert_eq!(c, "# <<< aliases:1700000000 <<<");
    }

    #[test]
    fn build_markers_bad_template() {
        let err = build_markers("no placeholder", "x", 1).unwrap_err();
        assert!(matches!(err, BlockError::BadTemplate));
    }

    #[test]
    fn find_block_missing_returns_none() {
        let r = find_block("# {}", "aliases", "echo hi\n").unwrap();
        assert!(r.is_none());
    }

    #[test]
    fn find_block_round_trip() {
        let s = "before\n# >>> aliases:42 >>>\nalias x='1'\n# <<< aliases:42 <<<\nafter\n";
        let f = find_block("# {}", "aliases", s).unwrap().unwrap();
        assert_eq!(f.timestamp, 42);
        assert_eq!(&s[f.byte_range], "# >>> aliases:42 >>>\nalias x='1'\n# <<< aliases:42 <<<\n");
    }

    #[test]
    fn find_block_crlf() {
        let s = "before\r\n# >>> n:1 >>>\r\nbody\r\n# <<< n:1 <<<\r\nafter\r\n";
        let f = find_block("# {}", "n", s).unwrap().unwrap();
        assert_eq!(f.timestamp, 1);
    }

    #[test]
    fn find_block_orphan_open_is_error() {
        let s = "# >>> n:1 >>>\nbody\n";
        let err = find_block("# {}", "n", s).unwrap_err();
        assert!(matches!(err, BlockError::MalformedSentinel { .. }));
    }

    #[test]
    fn find_block_does_not_match_inside_body() {
        let s = "# >>> a:1 >>>\nthis line says >>> a:2 >>> as text\n# <<< a:1 <<<\n";
        let f = find_block("# {}", "a", s).unwrap().unwrap();
        // Outer sentinels (1,1), inner literal must not be picked.
        assert_eq!(f.timestamp, 1);
    }

    #[test]
    fn compose_inserts_when_missing() {
        let s = compose("alpha\n",
            "beta\n",
            &Sentinel {
                name: "n".into(), timestamp: 1,
                open_marker: "# >>> n:1 >>>".into(),
                close_marker: "# <<< n:1 <<<".into(),
            },
            "# {}",
            "n",
        ).unwrap();
        assert_eq!(s, "alpha\n# >>> n:1 >>>\nbeta\n# <<< n:1 <<<\n");
    }

    #[test]
    fn compose_replaces_existing() {
        let pre = "x\n# >>> n:1 >>>\nold body\n# <<< n:1 <<<\ny\n";
        let out = compose(pre,
            "new body\n",
            &Sentinel {
                name: "n".into(), timestamp: 2,
                open_marker: "# >>> n:2 >>>".into(),
                close_marker: "# <<< n:2 <<<".into(),
            },
            "# {}", "n",
        ).unwrap();
        assert_eq!(out, "x\n# >>> n:2 >>>\nnew body\n# <<< n:2 <<<\ny\n");
    }
}
```

- [ ] **Step 10.2:** 创建 proptest `tests/proptests/block_sentinel.rs`：

```rust
use flux::sync::block::{find_block, build_markers};
use proptest::prelude::*;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn round_trip_arbitrary_body(
        name in r"[a-z][a-z0-9_]{0,15}",
        body in r"[^\x00]{0,200}",
    ) {
        let (open, close) = build_markers("# {}", &name, 100).unwrap();
        let body_norm = if body.ends_with('\n') { body.clone() } else { format!("{body}\n") };
        let content = format!("preamble\n{open}\n{body_norm}{close}\nepilogue\n");
        let found = find_block("# {}", &name, &content).unwrap().unwrap();
        // The byte range should encompass both markers and body.
        let captured = &content[found.byte_range.clone()];
        prop_assert!(captured.starts_with(&open));
        prop_assert!(captured.trim_end().ends_with(&close));
    }
}
```

把 `tests/proptests/` 设为单独的 integration test target——实际操作：在 `Cargo.toml` 新增：

```toml
[[test]]
name = "block_sentinel_proptests"
path = "tests/proptests/block_sentinel.rs"
```

- [ ] **Step 10.3:** Run: `cargo test --offline sync::block::tests block_sentinel_proptests`
Expected: 9 unit tests + proptest 通过。

- [ ] **Step 10.4:** Commit:

```bash
git add src/sync/block.rs tests/proptests/block_sentinel.rs Cargo.toml
git commit -m "phase2: rewrite sync/block as plan_blocks + execute_block + sentinel parser"
```

---

## Task 11: sync/mod.rs — Pipeline + run_pipeline + concurrency

**Files:**
- Rewrite: `src/sync/mod.rs`

- [ ] **Step 11.1:** 完整重写 `src/sync/mod.rs`（以 plan/execute 形态收口三个 stage）。完整内容：

```rust
//! Sync pipeline orchestration.
//!
//! `Pipeline` holds references to `Config`, `RemoteOps`, and `Reporter`,
//! computes a Plan, and executes it stage by stage with stage-level
//! concurrency (file: parallel; script: serial; block: parallel by target).

pub mod block;
pub mod file;
pub mod plan;
pub mod script;

use crate::config::Config;
use crate::remote::{RemoteOps, RemoteOpsError};
use crate::reporter::{ItemOutcome, PipelineSummary, Reporter, Stage, StageSummary};
use crate::sync::plan::{
    BlockAction, FileAction, Plan, RegisterPubkeyAction, ScriptAction, SkipReason,
};
use futures::stream::{FuturesUnordered, StreamExt};
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error("file: {0}")]   File(#[from] file::FileError),
    #[error("script: {0}")] Script(#[from] script::ScriptError),
    #[error("block: {0}")]  Block(#[from] block::BlockError),
    #[error("remote: {0}")] Remote(#[from] RemoteOpsError),
}

#[derive(Debug, Clone)]
pub struct PipelineOpts {
    pub dry_run: bool,
    pub max_concurrency: usize,
}

impl Default for PipelineOpts {
    fn default() -> Self {
        Self { dry_run: false, max_concurrency: 8 }
    }
}

pub struct Pipeline<'a, R: RemoteOps + ?Sized> {
    pub config: &'a Config,
    pub asset_root: &'a Path,
    pub remote: &'a R,
    pub reporter: &'a dyn Reporter,
    pub opts: PipelineOpts,
}

impl<'a, R: RemoteOps + ?Sized> Pipeline<'a, R> {
    pub async fn plan(&self) -> Plan {
        let register_pubkey = self.config.register_key.then(|| RegisterPubkeyAction {
            local_pubkey_path: self
                .config
                .key
                .clone()
                .map(|k| format!("{k}.pub"))
                .unwrap_or_default(),
            remote_authorized_keys: "~/.ssh/authorized_keys".into(),
        });
        let file_actions = file::plan_files(&self.config.file, self.remote).await;
        // file_status for script dependencies: planned status (Apply or Skip-Touch counted as success)
        let file_status: HashMap<String, bool> = file_actions
            .iter()
            .filter_map(|a| match a {
                FileAction::Apply { item_name, .. }
                | FileAction::Skip { item_name, .. } => Some((item_name.clone(), true)),
                FileAction::Failed { item_name, .. } => Some((item_name.clone(), false)),
            })
            .collect();
        let script_actions = script::plan_scripts(
            &self.config.script,
            &file_status,
            self.asset_root,
            self.config.interpreter.as_deref().unwrap_or("/bin/bash"),
            self.config.flags.as_slice(),
        ).await;
        let block_actions = block::plan_blocks(
            &self.config.block,
            self.asset_root,
            self.config.comment_template.as_deref().unwrap_or("# {}"),
            self.remote,
        ).await;
        Plan { register_pubkey, file_actions, script_actions, block_actions }
    }

    pub async fn run(&self) -> PipelineSummary {
        let plan = self.plan().await;
        if self.opts.dry_run {
            self.reporter.print_plan(&plan);
            return PipelineSummary {
                stages: vec![],
                interrupted: false,
                dry_run: true,
            };
        }
        self.execute(&plan).await
    }

    pub async fn execute(&self, plan: &Plan) -> PipelineSummary {
        let mut stages = Vec::new();
        // Pubkey
        if let Some(action) = &plan.register_pubkey {
            stages.push(self.execute_pubkey(action).await);
        }
        // File — parallel
        stages.push(self.execute_file_stage(&plan.file_actions).await);
        // Script — serial
        stages.push(self.execute_script_stage(&plan.script_actions).await);
        // Block — parallel by target
        stages.push(self.execute_block_stage(&plan.block_actions).await);
        let summary = PipelineSummary {
            stages,
            interrupted: false,
            dry_run: false,
        };
        self.reporter.pipeline_summary(&summary);
        summary
    }

    async fn execute_file_stage(&self, actions: &[FileAction]) -> StageSummary {
        self.reporter.stage_started(Stage::File, actions.len());
        let n = self.opts.max_concurrency;
        let outcomes: Vec<ItemOutcome> = futures::stream::iter(actions.iter())
            .map(|a| async move { file::execute_file(a, self.remote, self.reporter).await })
            .buffer_unordered(n)
            .collect()
            .await;
        let summary = tally(Stage::File, &outcomes);
        self.reporter.stage_finished(&summary);
        summary
    }

    async fn execute_script_stage(&self, actions: &[ScriptAction]) -> StageSummary {
        self.reporter.stage_started(Stage::Script, actions.len());
        let mut outcomes = Vec::with_capacity(actions.len());
        for a in actions {
            outcomes.push(script::execute_script::<R>(a, self.remote, self.reporter).await);
        }
        let summary = tally(Stage::Script, &outcomes);
        self.reporter.stage_finished(&summary);
        summary
    }

    async fn execute_block_stage(&self, actions: &[BlockAction]) -> StageSummary {
        self.reporter.stage_started(Stage::Block, actions.len());
        // group by target
        let mut by_target: HashMap<String, Vec<&BlockAction>> = HashMap::new();
        for a in actions {
            let key = match a {
                BlockAction::Skip { .. } | BlockAction::Failed { .. } => "_special".into(),
                BlockAction::Apply { target, .. } => target.clone(),
            };
            by_target.entry(key).or_default().push(a);
        }
        let template = self.config.comment_template.as_deref().unwrap_or("# {}");
        let n = self.opts.max_concurrency;
        let outcomes_groups: Vec<Vec<ItemOutcome>> = futures::stream::iter(by_target.into_values())
            .map(|group| async move {
                let mut out = Vec::with_capacity(group.len());
                for a in group {
                    out.push(block::execute_block(a, self.remote, template, self.reporter).await);
                }
                out
            })
            .buffer_unordered(n)
            .collect()
            .await;
        let outcomes: Vec<ItemOutcome> = outcomes_groups.into_iter().flatten().collect();
        let summary = tally(Stage::Block, &outcomes);
        self.reporter.stage_finished(&summary);
        summary
    }

    async fn execute_pubkey(&self, action: &RegisterPubkeyAction) -> StageSummary {
        self.reporter.stage_started(Stage::Pubkey, 1);
        let result = (|| async {
            let pub_bytes = std::fs::read(&action.local_pubkey_path)
                .map_err(|e| SyncError::Remote(RemoteOpsError::Io(e.to_string())))?;
            let pub_str = String::from_utf8(pub_bytes)
                .map_err(|e| SyncError::Remote(RemoteOpsError::Encoding(e.to_string())))?
                .trim()
                .to_string();
            // append-if-not-present
            let target = action.remote_authorized_keys.clone();
            let existing = match self.remote.read_file(&target).await {
                Ok(b) => String::from_utf8_lossy(&b).into_owned(),
                Err(RemoteOpsError::NotFound(_)) => String::new(),
                Err(e) => return Err(e.into()),
            };
            if existing.lines().any(|l| l.trim() == pub_str) {
                return Ok::<_, SyncError>(ItemOutcome::Skipped(SkipReason::AlreadyExists));
            }
            let mut new = existing;
            if !new.is_empty() && !new.ends_with('\n') {
                new.push('\n');
            }
            new.push_str(&pub_str);
            new.push('\n');
            self.remote.write_file(&target, new.as_bytes()).await?;
            self.remote.chmod(&target, 0o600).await?;
            Ok(ItemOutcome::Applied)
        })().await;
        let outcome = match result {
            Ok(o) => o,
            Err(e) => ItemOutcome::Failed(e.to_string()),
        };
        self.reporter.item_finished(Stage::Pubkey, "register_pubkey", &outcome);
        let summary = tally(Stage::Pubkey, std::slice::from_ref(&outcome));
        self.reporter.stage_finished(&summary);
        summary
    }
}

fn tally(stage: Stage, outcomes: &[ItemOutcome]) -> StageSummary {
    let mut applied = 0;
    let mut skipped = 0;
    let mut failed = 0;
    for o in outcomes {
        match o {
            ItemOutcome::Applied => applied += 1,
            ItemOutcome::Skipped(_) => skipped += 1,
            ItemOutcome::Failed(_) => failed += 1,
        }
    }
    StageSummary { stage, applied, skipped, failed }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, FileItem};
    use crate::remote::fake::InMemoryRemote;
    use crate::reporter::memory::CapturedReporter;
    use tempfile::TempDir;

    fn minimal_config(items: Vec<FileItem>) -> Config {
        Config {
            host: "127.0.0.1".into(),
            user: Some("u".into()),
            password: None,
            key: None,
            register_key: false,
            interpreter: Some("/bin/bash".into()),
            flags: vec![],
            comment_template: Some("# {}".into()),
            proxy: None,
            file: items,
            script: vec![],
            block: vec![],
            flux_home: None,
        }
    }

    #[tokio::test]
    async fn empty_config_yields_empty_summary() {
        let tmp = TempDir::new().unwrap();
        let cfg = minimal_config(vec![]);
        let remote = InMemoryRemote::new();
        let reporter = CapturedReporter::new();
        let pipe = Pipeline { config: &cfg, asset_root: tmp.path(), remote: &remote, reporter: &reporter, opts: PipelineOpts::default() };
        let summary = pipe.run().await;
        assert_eq!(summary.total_failed(), 0);
    }

    #[tokio::test]
    async fn dry_run_does_not_write() {
        let tmp = TempDir::new().unwrap();
        let src = tmp.path().join("a");
        std::fs::write(&src, b"hi").unwrap();
        let cfg = minimal_config(vec![FileItem {
            name: Some("a".into()),
            src: src.to_string_lossy().into_owned(),
            dst: ":/r/a".into(),
            mode: Some("cover".into()),
            chmod: None,
        }]);
        let remote = InMemoryRemote::new();
        let reporter = CapturedReporter::new();
        let pipe = Pipeline {
            config: &cfg, asset_root: tmp.path(), remote: &remote, reporter: &reporter,
            opts: PipelineOpts { dry_run: true, max_concurrency: 4 },
        };
        let _ = pipe.run().await;
        assert_eq!(remote.write_calls().len(), 0);
    }

    #[tokio::test]
    async fn file_failure_does_not_short_circuit() {
        let tmp = TempDir::new().unwrap();
        let good = tmp.path().join("good");
        std::fs::write(&good, b"ok").unwrap();
        let cfg = minimal_config(vec![
            FileItem { name: Some("missing".into()), src: "/no/such".into(), dst: ":/r/x".into(), mode: Some("cover".into()), chmod: None },
            FileItem { name: Some("good".into()), src: good.to_string_lossy().into_owned(), dst: ":/r/y".into(), mode: Some("cover".into()), chmod: None },
        ]);
        let remote = InMemoryRemote::new();
        let reporter = CapturedReporter::new();
        let pipe = Pipeline { config: &cfg, asset_root: tmp.path(), remote: &remote, reporter: &reporter, opts: PipelineOpts::default() };
        let summary = pipe.run().await;
        assert_eq!(summary.stages[0].failed, 1);
        assert_eq!(summary.stages[0].applied, 1);
    }
}
```

- [ ] **Step 11.2:** Run: `cargo test --lib --offline sync::tests`
Expected: 3 tests pass.

- [ ] **Step 11.3:** Commit:

```bash
git add src/sync/mod.rs
git commit -m "phase2: introduce Pipeline + run_pipeline with stage concurrency"
```

---

## Task 12: cli/ extract from main.rs + dry-run + max-concurrency

**Files:**
- Create: `src/cli/mod.rs`
- Create: `src/cli/ssh_config.rs`
- Rewrite: `src/main.rs`
- Delete (final): `src/output.rs`、`src/sync/mod.rs` 中遗留的 SshClient 直引

- [ ] **Step 12.1:** 创建 `src/cli/mod.rs` —— 把 `main.rs` 里的 `run_init / run_sync / run_proxy` 函数搬过来。重命名后引用 Pipeline：

```rust
//! CLI command implementations.

pub mod ssh_config;

use crate::config::Config;
use crate::reporter::{ConsoleReporter, Reporter};
use crate::remote::ssh::SshClient;
use crate::sync::{Pipeline, PipelineOpts};
use anyhow::{Context, Result};
use std::path::PathBuf;

pub async fn run_init() -> Result<()> {
    // 与 main.rs 现有 run_init 等价。简单写好脚手架并退出。
    let cwd = std::env::current_dir()?;
    let root = cwd.join(".flux");
    std::fs::create_dir_all(root.join("files"))?;
    std::fs::create_dir_all(root.join("scripts"))?;
    std::fs::create_dir_all(root.join("blocks"))?;
    println!(
        "initialized .flux/ in {}\n  - put YAML configs in .flux/<name>.yml\n  - assets in files/, scripts/, blocks/",
        cwd.display()
    );
    Ok(())
}

pub async fn run_sync(name_or_path: &str, save: Option<String>, dry_run: bool, max_concurrency: Option<usize>) -> Result<()> {
    let (config, config_path) = Config::find_and_load(name_or_path).context("loading config")?;
    config.validate().context("validating config")?;
    let asset_root = config.resolve_root(&config_path);
    let reporter = ConsoleReporter::new();
    if let Some(name) = save {
        ssh_config::save_ssh_config(&name, &config).context("saving ssh config")?;
    }
    // connect ssh
    let ssh = SshClient::connect(&config).await.context("ssh connect")?;
    if config.register_key && !dry_run {
        // pubkey planning is part of Pipeline now, but we keep this branch idempotent
    }
    if let Some(proxy) = &config.proxy {
        if proxy.enabled && !dry_run {
            ssh.start_reverse_forward(proxy.local_port, proxy.remote_port).await
                .context("starting reverse forward")?;
        }
    }
    let opts = PipelineOpts {
        dry_run,
        max_concurrency: max_concurrency.unwrap_or(8),
    };
    let pipe = Pipeline {
        config: &config,
        asset_root: &asset_root,
        remote: &ssh,
        reporter: &reporter,
        opts,
    };
    let summary = pipe.run().await;
    let code = summary.exit_code();
    if code != 0 {
        std::process::exit(code);
    }
    Ok(())
}

pub async fn run_proxy(host: String, local: u16, remote: u16, key: Option<String>, retry: u64) -> Result<()> {
    SshClient::standalone_proxy(&host, local, remote, key.as_deref(), retry).await
}
```

- [ ] **Step 12.2:** 创建 `src/cli/ssh_config.rs` —— 把 `main.rs` 里的 `save_ssh_config / read_ssh_config_entry / parse_ssh_host*` 函数完整搬过来，并加测试：

```rust
//! Local ~/.ssh/config read & write helpers.

use anyhow::{anyhow, bail, Context, Result};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

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

fn parse_ssh_host_inner(spec: &str, default_port: u16) -> Result<(Option<String>, String, u16), SshConfigError> {
    let (user, hostport) = match spec.find('@') {
        Some(i) => (Some(spec[..i].to_string()), &spec[i+1..]),
        None => (None, spec),
    };
    // IPv6 in brackets
    if let Some(s) = hostport.strip_prefix('[') {
        if let Some(end) = s.find(']') {
            let host = s[..end].to_string();
            let rest = &s[end+1..];
            if rest.is_empty() {
                return Ok((user, host, default_port));
            }
            if let Some(p) = rest.strip_prefix(':') {
                let port = p.parse::<u16>().map_err(|_| SshConfigError::InvalidPort(p.into()))?;
                return Ok((user, host, port));
            }
            return Err(SshConfigError::InvalidHostPort(spec.into()));
        }
        return Err(SshConfigError::InvalidHostPort(spec.into()));
    }
    // host or host:port (last colon is the separator since IPv6 always wraps)
    match hostport.rsplit_once(':') {
        Some((h, p)) if !p.is_empty() => {
            let port = p.parse::<u16>().map_err(|_| SshConfigError::InvalidPort(p.into()))?;
            Ok((user, h.to_string(), port))
        }
        _ => Ok((user, hostport.to_string(), default_port)),
    }
}

pub fn save_ssh_config(name: &str, config: &crate::config::Config) -> Result<()> {
    let home = dirs::home_dir().context("no home dir")?;
    let config_path = home.join(".ssh").join("config");
    std::fs::create_dir_all(home.join(".ssh"))
        .context("ensuring ~/.ssh dir")?;
    let existing = match std::fs::read_to_string(&config_path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => bail!(SshConfigError::Io(e.to_string())),
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
            // else: drop
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
    let mut s = String::new();
    s.push_str(&format!("Host {name}\n"));
    s.push_str(&format!("    HostName {}\n", cfg.host));
    if let Some(u) = &cfg.user {
        s.push_str(&format!("    User {u}\n"));
    }
    if let Some(k) = &cfg.key {
        s.push_str(&format!("    IdentityFile {k}\n"));
    }
    s
}

pub fn read_ssh_config_entry(name: &str) -> Result<Option<(String, Option<String>, u16, Option<String>)>> {
    let home = dirs::home_dir().context("no home dir")?;
    let mut visited = HashSet::new();
    read_entry_recursive(&home.join(".ssh").join("config"), name, &mut visited)
}

fn read_entry_recursive(
    path: &Path,
    name: &str,
    visited: &mut HashSet<PathBuf>,
) -> Result<Option<(String, Option<String>, u16, Option<String>)>> {
    let canon = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    if !visited.insert(canon.clone()) {
        return Ok(None);
    }
    let content = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => bail!(SshConfigError::Io(e.to_string())),
    };
    let mut in_target = false;
    let mut hostname = None;
    let mut user = None;
    let mut port: u16 = 22;
    let mut identity = None;
    for line in content.lines() {
        let l = line.trim();
        if l.is_empty() || l.starts_with('#') { continue; }
        let (kw, rest) = match l.split_once(char::is_whitespace) {
            Some(p) => p,
            None => continue,
        };
        let kw_lower = kw.to_lowercase();
        let rest = rest.trim();
        if kw_lower == "include" {
            // recurse
            let included = expand_include_path(rest, path);
            for ip in included {
                if let Some(found) = read_entry_recursive(&ip, name, visited)? {
                    return Ok(Some(found));
                }
            }
            continue;
        }
        if kw_lower == "host" {
            let patterns: Vec<&str> = rest.split_whitespace().collect();
            in_target = patterns.contains(&name);
            if patterns.iter().any(|p| p.contains('*') || p.contains('?') || p.starts_with('!')) {
                eprintln!("[warn] unsupported wildcard/negation in Host pattern: {rest}");
            }
            continue;
        }
        if kw_lower == "match" {
            eprintln!("[warn] Match block ignored");
            in_target = false;
            continue;
        }
        if !in_target { continue; }
        match kw_lower.as_str() {
            "hostname" => hostname = Some(rest.to_string()),
            "user" => user = Some(rest.to_string()),
            "port" => port = rest.parse::<u16>().map_err(|_| anyhow!("invalid Port {rest}"))?,
            "identityfile" => identity = Some(rest.to_string()),
            _ => {}
        }
    }
    if let Some(h) = hostname {
        Ok(Some((h, user, port, identity)))
    } else {
        Ok(None)
    }
}

fn expand_include_path(spec: &str, base: &Path) -> Vec<PathBuf> {
    let raw = if let Some(rest) = spec.strip_prefix("~/") {
        dirs::home_dir().map(|h| h.join(rest)).unwrap_or_else(|| PathBuf::from(spec))
    } else if Path::new(spec).is_absolute() {
        PathBuf::from(spec)
    } else {
        base.parent().map(|p| p.join(spec)).unwrap_or_else(|| PathBuf::from(spec))
    };
    // We do not glob expand in this minimal implementation; literal path only.
    vec![raw]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_plain_host() {
        let (u, h, p) = parse_ssh_host("example.com").unwrap();
        assert_eq!(u, None);
        assert_eq!(h, "example.com");
        assert_eq!(p, 22);
    }

    #[test]
    fn parse_host_with_port() {
        let (_, h, p) = parse_ssh_host("example.com:2222").unwrap();
        assert_eq!(h, "example.com");
        assert_eq!(p, 2222);
    }

    #[test]
    fn parse_user_at_host_port() {
        let (u, h, p) = parse_ssh_host("alice@example.com:22").unwrap();
        assert_eq!(u, Some("alice".into()));
        assert_eq!(h, "example.com");
        assert_eq!(p, 22);
    }

    #[test]
    fn parse_ipv6_with_port() {
        let (_, h, p) = parse_ssh_host("[::1]:2222").unwrap();
        assert_eq!(h, "::1");
        assert_eq!(p, 2222);
    }

    #[test]
    fn parse_ipv6_no_port() {
        let (_, h, p) = parse_ssh_host("[::1]").unwrap();
        assert_eq!(h, "::1");
        assert_eq!(p, 22);
    }

    #[test]
    fn parse_user_at_ipv6_port() {
        let (u, h, p) = parse_ssh_host("alice@[fe80::1]:443").unwrap();
        assert_eq!(u, Some("alice".into()));
        assert_eq!(h, "fe80::1");
        assert_eq!(p, 443);
    }

    #[test]
    fn parse_invalid_port_errors() {
        let err = parse_ssh_host("example.com:notaport").unwrap_err();
        assert!(matches!(err, SshConfigError::InvalidPort(_)));
    }

    #[test]
    fn replace_or_append_keeps_other_hosts() {
        let pre = "Host foo\n    HostName 1.1.1.1\n\nHost bar\n    HostName 2.2.2.2\n";
        let cfg = crate::config::Config {
            host: "9.9.9.9".into(), user: None, password: None, key: None,
            register_key: false, interpreter: None, flags: vec![], comment_template: None,
            proxy: None, file: vec![], script: vec![], block: vec![], flux_home: None,
        };
        let out = replace_or_append_host(pre, "foo", &cfg);
        assert!(out.contains("Host bar"));
        assert!(out.contains("HostName 9.9.9.9"));
        assert!(!out.contains("HostName 1.1.1.1"));
    }

    #[test]
    fn replace_skips_through_comments_to_next_host() {
        let pre = "Host foo\n    HostName 1.1.1.1\n    # a comment\n    User old\n\nHost bar\n    HostName 2.2.2.2\n";
        let cfg = crate::config::Config {
            host: "9.9.9.9".into(), user: Some("new".into()), password: None, key: None,
            register_key: false, interpreter: None, flags: vec![], comment_template: None,
            proxy: None, file: vec![], script: vec![], block: vec![], flux_home: None,
        };
        let out = replace_or_append_host(pre, "foo", &cfg);
        assert!(out.contains("Host bar"));
        assert!(out.contains("HostName 9.9.9.9"));
        assert!(out.contains("User new"));
        assert!(!out.contains("User old"));
        assert!(!out.contains("# a comment"));
    }
}
```

- [ ] **Step 12.3:** Rewrite `src/main.rs` 极简：

```rust
//! Flux CLI binary entry — thin dispatcher over `flux::cli::*`.

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "flux", version, about = "SSH remote configuration sync tool")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Init,
    Sync {
        config: String,
        #[arg(long)]
        save: Option<String>,
        #[arg(long, help = "Compute the plan and print it without applying changes")]
        dry_run: bool,
        #[arg(long, value_name = "N")]
        max_concurrency: Option<usize>,
    },
    Proxy {
        host: String,
        #[arg(short, long, default_value = "7899")]
        local: u16,
        #[arg(short, long, default_value = "7890")]
        remote: u16,
        #[arg(short, long)]
        key: Option<String>,
        #[arg(long, default_value = "5")]
        retry: u64,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Init => flux::cli::run_init().await,
        Commands::Sync { config, save, dry_run, max_concurrency } =>
            flux::cli::run_sync(&config, save, dry_run, max_concurrency).await,
        Commands::Proxy { host, local, remote, key, retry } =>
            flux::cli::run_proxy(host, local, remote, key, retry).await,
    }
}
```

- [ ] **Step 12.4:** 删除 `src/output.rs`：

```bash
git rm src/output.rs
```

- [ ] **Step 12.5:** Run: `cargo build --offline` 与 `cargo test --offline`
Expected: 全过。

- [ ] **Step 12.6:** Commit:

```bash
git add -A
git commit -m "phase2: extract cli module, add --dry-run / --max-concurrency, drop output.rs"
```

---

## Task 13: config/ split + version probe

**Files:**
- Move: `src/config.rs` → `src/config/mod.rs`
- Create: `src/config/version.rs`
- Modify: `src/lib.rs`（`pub mod config;`）

- [ ] **Step 13.1:** `git mv src/config.rs src/config/mod.rs`，在 `src/config/mod.rs` 顶部加 `pub mod version;`。

- [ ] **Step 13.2:** 创建 `src/config/version.rs`：

```rust
//! Schema version probing & migration.

use serde::Deserialize;

pub const CURRENT_SCHEMA_VERSION: u32 = 1;

#[derive(Deserialize)]
struct VersionProbe {
    #[serde(default = "default_version")]
    version: u32,
}

fn default_version() -> u32 { 1 }

#[derive(Debug, thiserror::Error)]
pub enum VersionError {
    #[error("unsupported config schema version {found}; this build supports up to {max}")]
    Unsupported { found: u32, max: u32 },
    #[error("yaml parse: {0}")]
    Yaml(#[from] serde_yml::Error),
}

pub fn probe_version(yaml: &str) -> Result<u32, VersionError> {
    let p: VersionProbe = serde_yml::from_str(yaml)?;
    if p.version > CURRENT_SCHEMA_VERSION {
        return Err(VersionError::Unsupported {
            found: p.version,
            max: CURRENT_SCHEMA_VERSION,
        });
    }
    Ok(p.version)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_version_defaults_to_1() {
        let v = probe_version("host: x").unwrap();
        assert_eq!(v, 1);
    }
    #[test]
    fn explicit_version_1_ok() {
        let v = probe_version("version: 1\nhost: x").unwrap();
        assert_eq!(v, 1);
    }
    #[test]
    fn future_version_errors() {
        let err = probe_version("version: 999\nhost: x").unwrap_err();
        assert!(matches!(err, VersionError::Unsupported { found: 999, .. }));
    }
}
```

- [ ] **Step 13.3:** 在 `src/config/mod.rs` 的 `Config::load_from_string`（或同名加载函数）调用前加 `version::probe_version(&yaml)?`。如该函数不存在，找到 `find_and_load` 内部读 yaml 字符串处加调用。

具体：
```rust
let yaml = std::fs::read_to_string(&path).context(...)?;
super::version::probe_version(&yaml).map_err(|e| anyhow::anyhow!("{e}"))?;
let config: Config = serde_yml::from_str(&yaml).context(...)?;
```

- [ ] **Step 13.4:** 在 `Config` struct 上加 `#[serde(default)] pub version: u32`，配合默认值：

```rust
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default = "default_version")]
    pub version: u32,
    // ...
}

fn default_version() -> u32 { 1 }
```

- [ ] **Step 13.5:** 创建 `docs/schema-migrations.md`：

```markdown
# Flux Schema Migrations

Each entry: trigger, motivation, impact, automatic vs manual.

## v1 (current — 2026-05-01)

Initial declared schema version. Older configs without a `version:` field
default to 1. No automatic migration; everything is identity.
```

- [ ] **Step 13.6:** Run: `cargo test --lib --offline config::version::tests`
Expected: 3 tests pass.

- [ ] **Step 13.7:** Commit:

```bash
git add -A
git commit -m "phase2: split config/, add schema version probe"
```

---

## Task 14: path::AssetLocator + tests

**Files:**
- Modify: `src/path.rs`（保留 FluxPath，新增 AssetLocator）

- [ ] **Step 14.1:** 在 `src/path.rs` 末尾追加：

```rust
/// Resolve config-relative asset paths under `<flux_home>/{files,scripts,blocks}/`.
pub struct AssetLocator {
    root: std::path::PathBuf,
}

impl AssetLocator {
    pub fn new(root: impl Into<std::path::PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn file(&self, name: &str) -> std::path::PathBuf { self.root.join("files").join(name) }
    pub fn script(&self, name: &str) -> std::path::PathBuf { self.root.join("scripts").join(name) }
    pub fn block(&self, name: &str) -> std::path::PathBuf { self.root.join("blocks").join(name) }

    pub fn root(&self) -> &std::path::Path { &self.root }
}

#[cfg(test)]
mod asset_tests {
    use super::*;

    #[test]
    fn locator_paths() {
        let l = AssetLocator::new("/x/.flux");
        assert_eq!(l.file("a.txt").as_path(), std::path::Path::new("/x/.flux/files/a.txt"));
        assert_eq!(l.script("s.sh").as_path(), std::path::Path::new("/x/.flux/scripts/s.sh"));
        assert_eq!(l.block("b.sh").as_path(), std::path::Path::new("/x/.flux/blocks/b.sh"));
    }
}
```

- [ ] **Step 14.2:** Run: `cargo test --lib --offline path::asset_tests`
Expected: 1 test passes.

- [ ] **Step 14.3:** Commit:

```bash
git add src/path.rs
git commit -m "phase2: add AssetLocator for .flux/{files,scripts,blocks}"
```

---

## Task 15: Integration tests + CI

**Files:**
- Create: `tests/integration/pipeline_file.rs`（其它 stage 类似省略——本任务先做 file，剩两个仿制）
- Create: `tests/fixtures/westlake_minimal.yml`
- Create: `.github/workflows/ci.yml`
- Create: `deny.toml`

- [ ] **Step 15.1:** Create `tests/fixtures/westlake_minimal.yml`：

```yaml
version: 1
host: "127.0.0.1"
user: "test"
key: ~/.ssh/test_key
interpreter: /bin/bash
comment_template: "# {}"
file: []
script: []
block: []
```

- [ ] **Step 15.2:** Create `tests/integration/pipeline_file.rs`：

```rust
use flux::config::Config;
use flux::remote::fake::InMemoryRemote;
use flux::reporter::memory::CapturedReporter;
use flux::sync::{Pipeline, PipelineOpts};
use tempfile::TempDir;

#[tokio::test]
async fn end_to_end_minimal_config() {
    let yaml = std::fs::read_to_string("tests/fixtures/westlake_minimal.yml").unwrap();
    let cfg: Config = serde_yml::from_str(&yaml).unwrap();
    let tmp = TempDir::new().unwrap();
    let remote = InMemoryRemote::new();
    let reporter = CapturedReporter::new();
    let pipe = Pipeline {
        config: &cfg,
        asset_root: tmp.path(),
        remote: &remote,
        reporter: &reporter,
        opts: PipelineOpts::default(),
    };
    let summary = pipe.run().await;
    assert_eq!(summary.total_failed(), 0);
}
```

- [ ] **Step 15.3:** Create `.github/workflows/ci.yml`：

```yaml
name: ci
on: [push, pull_request]
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with:
          components: rustfmt, clippy
      - run: cargo fmt --all -- --check
      - run: cargo clippy --all-targets -- -D warnings
      - run: cargo test --all-targets
      - uses: EmbarkStudios/cargo-deny-action@v1
        with:
          arguments: --all-features check
```

- [ ] **Step 15.4:** Create `deny.toml`：

```toml
[graph]
all-features = false

[advisories]
yanked = "deny"
ignore = []

[bans]
multiple-versions = "warn"
wildcards = "deny"

[licenses]
allow = ["MIT", "Apache-2.0", "Apache-2.0 WITH LLVM-exception", "BSD-2-Clause", "BSD-3-Clause", "ISC", "Unicode-DFS-2016", "Unicode-3.0", "OpenSSL", "MPL-2.0", "Zlib", "0BSD"]

[sources]
unknown-registry = "warn"
unknown-git = "warn"
```

- [ ] **Step 15.5:** Run: `cargo test --offline --test pipeline_file`
Expected: 1 test passes.

- [ ] **Step 15.6:** Commit:

```bash
git add -A
git commit -m "phase2: add integration test scaffold + CI workflow + deny.toml"
```

---

## Task 16: Final clippy/fmt cleanup + clear remaining warnings

**Files:**
- Various (touch wherever clippy yells)

- [ ] **Step 16.1:** Run: `cargo fmt --all`

- [ ] **Step 16.2:** Run: `cargo clippy --all-targets --offline -- -D warnings 2>&1 | tee /tmp/clippy-final.txt`
Expected: 0 errors. If errors:
- `manual_contains` → use `Vec::contains`
- `unnecessary_map_or` → use `Option::is_none_or` if ≥ 1.82
- `type_complexity` → introduce a `type Alias = ...;` at module top
- `manual_strip` → use `.strip_prefix(...)`

Apply fixes until clean.

- [ ] **Step 16.3:** Run: `cargo test --all-targets --offline`
Expected: all tests pass.

- [ ] **Step 16.4:** Commit:

```bash
git add -A
git commit -m "phase2: clippy/fmt cleanup"
```

---

## Self-Review (executed by author)

**Spec coverage:**
- [x] Module layout — Tasks 1, 2, 5(now 5), 6, 12, 13, 14
- [x] RemoteOps trait — Tasks 2, 3, 4
- [x] Plan/Execute model — Tasks 5(plan.rs), 8, 9, 10, 11
- [x] Error model (domain enums + anyhow boundary) — Each stage task defines its own enum; Task 11 SyncError
- [x] Reporter — Task 6
- [x] Concurrency model — Task 11 (file parallel, script serial, block per-target parallel)
- [x] dry-run + max-concurrency — Task 12
- [x] Schema version + deny_unknown_fields — Task 13 + Phase 1 (deny_unknown_fields already done)
- [x] Test matrix — Task 4 (FakeRemote), 5(plan), 6(reporter), 8(file), 9(script), 10(block + proptest), 11(pipeline), 12(ssh_config), 14(path), 15(integration)
- [x] CI + cargo deny — Task 15
- [x] Final cleanup — Task 16

**Placeholders:** none ("TBD"/"TODO" not used as content).

**Type consistency:**
- `Sentinel`, `SkipReason`, `Plan`, `FileAction`, `ScriptAction`, `BlockAction` — consistent across Tasks 5/8/9/10/11
- `RemoteOps` methods used in Tasks 3/4/8/9/10/11 with same signatures
- `Reporter` methods used same in Tasks 6/8/9/10/11/12

**Notes for codex executor:**
- Tasks 5–14 may need to occasionally jump back to earlier stub files (e.g., `BlockError` enum in Task 5 is a stub; Task 10 fully populates it). This is expected — prioritize keeping the dependency graph consistent over strict task-order purity.
- If `cargo check --offline` fails mid-task with a "missing item" error from a future task, that item lands in a later task — do not invent placeholders. Mark the task as blocked, finish the dependent task first, then come back.

---

## Acceptance criteria (Phase 2 complete when all true)

1. `cargo fmt --all -- --check` passes
2. `cargo clippy --all-targets -- -D warnings` passes (0 errors)
3. `cargo test --all-targets` passes; total ≥ 30 tests
4. `cargo build --release` succeeds
5. `flux sync <name> --dry-run` exits 0 on a configured target with no changes pending
6. `tests/proptests/block_sentinel.rs` runs ≥ 64 cases without falsifying
7. `.github/workflows/ci.yml` is green on push (verified locally by mimicking each step)
8. No `TODO`/`unimplemented!()`/`unreachable!()` in production code outside well-justified host-key TODO
