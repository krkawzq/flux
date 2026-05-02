# Flux Phase 3: Sync Optimization Plan

> **Continuation of Phase 2.** Same TDD pattern, same "no git ops in codex" rule.

**Goal:** 完成 Phase 2 后剩下的"用户能直接感觉到"的 sync 优化，并补全 PTY/signal/IO 透传。

**Architecture additions:**
- `RemoteOps` 加 2 个方法：`stat_mode`、`remove_file`
- `RetryPolicy` 包装 RemoteOps 调用（仅 Transport/Io）
- `ItemOutcome::Failed` 类型从 `String` 升级到 `Arc<SyncError>`
- `interactive_exec` 改成 `(cmd, timeout)`，支持 stdin/stderr 分流 + SIGINT 转发
- Pipeline 顶层 `tokio::select!` 配合 `ctrl_c` 做 interrupted summary

**Tech changes:**
- `ScriptItem.dependencies` 字段**删除**（DAG 不再需要）
- 大文件不在 plan 阶段读 bytes（用 PathBuf + len）
- block sentinel timestamp = local block file mtime（idempotent）

**Decisions taken (no more brainstorming):**
- retry: 3 次 / 200ms·400ms·800ms 指数退避 / 只 Transport+Io
- streaming: 不加 write_stream API；plan 阶段只持 path，execute 阶段读 bytes
- SIGINT: 转发到远端，**不**立即 drop channel；连按 2 次才硬 kill
- tmp script cleanup 失败只 warn

---

## File Structure changes

```
src/remote/mod.rs                    # add stat_mode, remove_file to trait + RetryPolicy
src/remote/ssh.rs                    # impl stat_mode/remove_file + new interactive_exec
src/remote/fake.rs                   # impl stat_mode/remove_file + interactive_exec timeout
src/remote/retry.rs                  # NEW — RetryPolicy + with_retry helper
src/sync/plan.rs                     # FileAction::Apply { src: PathBuf, len: u64, ... }
src/sync/file.rs                     # rewrite plan: parallel; remove bytes from Apply
src/sync/script.rs                   # remove file_status param; tmp cleanup; remove dependencies
src/sync/block.rs                    # parallel plan; sentinel ts = local mtime
src/sync/mod.rs                      # Pipeline ctrl_c select; PipelineOpts::retries
src/config/mod.rs                    # delete ScriptItem.dependencies + Config::validate refint
src/reporter/mod.rs                  # ItemOutcome::Failed(Arc<SyncError>) + Failed display helper
src/reporter/console.rs              # adapt to new ItemOutcome
src/reporter/memory.rs               # adapt
```

---

## Task 1: RemoteOps additions — stat_mode + remove_file

**Files:** `src/remote/mod.rs`, `src/remote/ssh.rs`, `src/remote/fake.rs`

- [ ] **1.1** `src/remote/mod.rs` 在 trait 加：

```rust
async fn stat_mode(&self, path: &str) -> Result<u32, RemoteOpsError>;
async fn remove_file(&self, path: &str) -> Result<(), RemoteOpsError>;
```

- [ ] **1.2** `src/remote/ssh.rs` impl：

```rust
async fn stat_mode(&self, path: &str) -> Result<u32, RemoteOpsError> {
    // GNU stat -c %a; BSD/macOS stat -f %Lp; try GNU first, fallback BSD
    let cmd = format!(
        "stat -c %a {0} 2>/dev/null || stat -f %Lp {0}",
        shell_escape(path)
    );
    let out = self.exec(&cmd).await?;
    if !out.success() {
        return Err(RemoteOpsError::NonZeroExit {
            status: out.status,
            stderr: out.stderr_string(),
        });
    }
    let s = out.stdout_string();
    let trimmed = s.trim();
    u32::from_str_radix(trimmed, 8)
        .map_err(|e| RemoteOpsError::Encoding(format!("stat_mode parse '{trimmed}': {e}")))
}

async fn remove_file(&self, path: &str) -> Result<(), RemoteOpsError> {
    let cmd = format!("rm -f {}", shell_escape(path));
    let out = self.exec(&cmd).await?;
    if !out.success() {
        return Err(RemoteOpsError::NonZeroExit {
            status: out.status,
            stderr: out.stderr_string(),
        });
    }
    Ok(())
}
```

- [ ] **1.3** `src/remote/fake.rs` impl：维护现有 `modes: HashMap<String, u32>`，stat_mode 查表（缺省返回 0o644 if file exists, NotFound otherwise）。remove_file 从 files / mtimes / modes 删。

- [ ] **1.4** Tests: `cargo test --lib --offline remote::fake::tests`：
  ```rust
  #[tokio::test]
  async fn stat_mode_returns_chmod_value() {
      let r = InMemoryRemote::new();
      r.write_file("/a", b"x").await.unwrap();
      r.chmod("/a", 0o600).await.unwrap();
      assert_eq!(r.stat_mode("/a").await.unwrap(), 0o600);
  }
  #[tokio::test]
  async fn stat_mode_default_for_unchmoded() {
      let r = InMemoryRemote::new();
      r.write_file("/a", b"x").await.unwrap();
      assert_eq!(r.stat_mode("/a").await.unwrap(), 0o644);
  }
  #[tokio::test]
  async fn remove_file_drops_entry() {
      let r = InMemoryRemote::new();
      r.write_file("/a", b"x").await.unwrap();
      r.remove_file("/a").await.unwrap();
      assert!(matches!(r.read_file("/a").await.unwrap_err(), RemoteOpsError::NotFound(_)));
  }
  ```

---

## Task 2: RetryPolicy + with_retry wrapper

**Files:** `src/remote/retry.rs` (new), `src/remote/mod.rs`

- [ ] **2.1** Create `src/remote/retry.rs`:

```rust
//! Retry policy for transient RemoteOps errors.
use crate::remote::RemoteOpsError;
use std::time::Duration;

#[derive(Debug, Clone, Copy)]
pub struct RetryPolicy {
    pub max_attempts: u8,        // total attempts (1 = no retry)
    pub base_backoff: Duration,  // first retry wait; subsequent doubled
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self { max_attempts: 3, base_backoff: Duration::from_millis(200) }
    }
}

impl RetryPolicy {
    pub fn no_retry() -> Self {
        Self { max_attempts: 1, base_backoff: Duration::ZERO }
    }
}

pub fn is_retryable(err: &RemoteOpsError) -> bool {
    matches!(err, RemoteOpsError::Transport(_) | RemoteOpsError::Io(_))
}

/// Run `op` and retry on transient errors per policy.
pub async fn with_retry<T, F, Fut>(policy: RetryPolicy, mut op: F) -> Result<T, RemoteOpsError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, RemoteOpsError>>,
{
    let mut wait = policy.base_backoff;
    let mut last_err = None;
    for attempt in 0..policy.max_attempts {
        match op().await {
            Ok(t) => return Ok(t),
            Err(e) => {
                if !is_retryable(&e) || attempt + 1 == policy.max_attempts {
                    return Err(e);
                }
                last_err = Some(e);
                tokio::time::sleep(wait).await;
                wait = wait.saturating_mul(2);
            }
        }
    }
    Err(last_err.unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    #[tokio::test]
    async fn retries_on_transport_then_succeeds() {
        let calls = Arc::new(AtomicUsize::new(0));
        let calls2 = calls.clone();
        let policy = RetryPolicy { max_attempts: 3, base_backoff: Duration::from_millis(1) };
        let result = with_retry(policy, || {
            let calls = calls2.clone();
            async move {
                let n = calls.fetch_add(1, Ordering::SeqCst);
                if n < 2 { Err(RemoteOpsError::Transport("flake".into())) } else { Ok(()) }
            }
        }).await;
        assert!(result.is_ok());
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn does_not_retry_not_found() {
        let calls = Arc::new(AtomicUsize::new(0));
        let calls2 = calls.clone();
        let policy = RetryPolicy::default();
        let _ = with_retry::<(), _, _>(policy, || {
            let calls = calls2.clone();
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Err(RemoteOpsError::NotFound("/x".into()))
            }
        }).await;
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}
```

- [ ] **2.2** `src/remote/mod.rs` 加 `pub mod retry;` + re-export `pub use retry::{RetryPolicy, with_retry};`

- [ ] **2.3** Test: `cargo test --lib --offline remote::retry::tests` → 2 pass.

**注意**：retry 不直接进 RemoteOps trait 实现，而是在 sync 层调用 RemoteOps 时由 `with_retry(policy, || remote.exec(cmd))` 包一层。这样 FakeRemote 不需要 retry 行为污染。

---

## Task 3: ItemOutcome::Failed → Arc<SyncError>

**Files:** `src/reporter/mod.rs`, `src/reporter/console.rs`, `src/reporter/memory.rs`, `src/sync/file.rs`, `src/sync/script.rs`, `src/sync/block.rs`, `src/sync/mod.rs`

- [ ] **3.1** `src/reporter/mod.rs` 改：

```rust
use crate::sync::SyncError;
use std::sync::Arc;

#[derive(Debug, Clone)]
pub enum ItemOutcome {
    Applied,
    Skipped(SkipReason),
    Failed(Arc<SyncError>),
}

impl ItemOutcome {
    pub fn failed_message(&self) -> Option<String> {
        match self {
            ItemOutcome::Failed(e) => Some(e.to_string()),
            _ => None,
        }
    }
}
```

- [ ] **3.2** `console.rs` / `memory.rs` 调用点改：`ItemOutcome::Failed(e)` 用 `e.to_string()` 打印；CapturedReporter 把 outcome label 改为 `format!("failed:{e}")`。`failed_items` 仍按字符串前缀匹配。

- [ ] **3.3** sync/*.rs 各处 `ItemOutcome::Failed(err.to_string())` 改成 `ItemOutcome::Failed(Arc::new(err.into()))`。

- [ ] **3.4** Run: `cargo test --all-targets --offline` 仍全绿。

---

## Task 4: 删除 ScriptItem.dependencies (DAG removal)

**Files:** `src/config/mod.rs`, `src/sync/script.rs`, `src/sync/mod.rs`, tests

- [ ] **4.1** `src/config/mod.rs` 中 `ScriptItem` 删除 `pub dependencies: Vec<String>` 字段。删除 `Config::validate` 里依赖引用完整性那段（**保留 validate 函数本身**，可能后续要补别的校验）。

- [ ] **4.2** `src/sync/script.rs` 的 `plan_scripts` 签名从：
```rust
pub async fn plan_scripts<R>(items, file_status, asset_root, default_interpreter, default_flags) -> ...
```
改为：
```rust
pub async fn plan_scripts(items, asset_root, default_interpreter, default_flags) -> ...
```

`plan_one_script` 内的 dependency 检查代码完全删除。`SkipReason::DependencyFailed` enum variant 也删除（**注意会影响 `SkipReason::DependencyFailed` 在 reporter 输出处的 match arm**——一并删掉）。

`ScriptError::UnvalidatedDependency` variant 删除。

- [ ] **4.3** `src/sync/mod.rs` 的 `Pipeline::plan` 删除 `file_status` 计算块，调用 plan_scripts 时不再传它。

- [ ] **4.4** 删除受影响测试：
  - `sync::script::tests::dependency_failed_skips`
  - `sync::script::tests::unknown_dependency_is_failed_action`

- [ ] **4.5** Run: `cargo test --all-targets --offline` 全绿。

---

## Task 5: plan_files / plan_blocks 并行化

**Files:** `src/sync/file.rs`, `src/sync/block.rs`

- [ ] **5.1** `src/sync/file.rs` 的 `plan_files` 改：

```rust
pub async fn plan_files<R: RemoteOps + ?Sized>(
    items: &[FileItem],
    remote: &R,
    max_concurrency: usize,
) -> Vec<FileAction> {
    use futures::stream::{self, StreamExt};
    let indexed: Vec<(usize, &FileItem)> = items.iter().enumerate().collect();
    let mut results: Vec<Option<FileAction>> = (0..items.len()).map(|_| None).collect();
    let mut stream = stream::iter(indexed)
        .map(|(idx, item)| async move { (idx, plan_one_file(item, remote).await) })
        .buffer_unordered(max_concurrency.max(1));
    while let Some((idx, action)) = stream.next().await {
        results[idx] = Some(action);
    }
    results.into_iter().map(|o| o.unwrap()).collect()
}
```

- [ ] **5.2** `src/sync/block.rs` 的 `plan_blocks` 同样模式 + 加 `max_concurrency` 参数。

- [ ] **5.3** `src/sync/mod.rs` 的 `Pipeline::plan` 调用点传 `self.opts.max_concurrency`。

- [ ] **5.4** Run all tests still pass。

---

## Task 6: block sentinel timestamp = local file mtime

**Files:** `src/sync/block.rs`

- [ ] **6.1** `plan_one_block` 中：

```rust
// 替代 let timestamp = Utc::now().timestamp();
let timestamp = std::fs::metadata(&local_path)
    .and_then(|m| m.modified())
    .map(|t| chrono::DateTime::<chrono::Utc>::from(t).timestamp())
    .unwrap_or_else(|_| chrono::Utc::now().timestamp());
```

- [ ] **6.2** 加测试：相同输入两次 `plan_one_block` 得到相同 sentinel.timestamp（idempotent）。

```rust
#[tokio::test]
async fn block_plan_is_idempotent_across_runs() {
    let tmp = TempDir::new().unwrap();
    let block_path = tmp.path().join("aliases.sh");
    std::fs::write(&block_path, b"alias x='1'\n").unwrap();
    let remote = InMemoryRemote::new();
    let item = BlockItem { /* ... */ };
    let actions1 = plan_blocks(&[item.clone()], tmp.path(), "# {}", &remote, 1).await;
    tokio::time::sleep(Duration::from_millis(20)).await;
    let actions2 = plan_blocks(&[item], tmp.path(), "# {}", &remote, 1).await;
    let ts1 = match &actions1[0] { BlockAction::Apply { sentinel, .. } => sentinel.timestamp, _ => panic!() };
    let ts2 = match &actions2[0] { BlockAction::Apply { sentinel, .. } => sentinel.timestamp, _ => panic!() };
    assert_eq!(ts1, ts2);
}
```

---

## Task 7: 大文件 Action 不持 bytes（streaming-by-deferred-read）

**Files:** `src/sync/plan.rs`, `src/sync/file.rs`

- [ ] **7.1** `src/sync/plan.rs` 改 `FileAction::Apply`：

```rust
FileAction::Apply {
    item_name: String,
    src: std::path::PathBuf,  // 替换 bytes: Vec<u8>
    dst: String,
    len: u64,                  // 用于 reporter / progress
    chmod: Option<u32>,
}
```

- [ ] **7.2** `plan_one_file` 不再 `fs::read`（仍 `fs::metadata` 拿 size 和 mtime）：

```rust
let len = metadata.len();
// 删除 let bytes = fs::read(...)
// Sync mode 下需要 hash 比较，仍读：在 mode 分支内仅 sync 模式读一次
```

实际：sync mode + remote-mtime-equal 时仍需 hash → 那时候 `fs::read`。其它情况不读。

- [ ] **7.3** `execute_file` 在 Apply 分支：`let bytes = std::fs::read(src).map_err(...)?;` 然后 write_file。

- [ ] **7.4** 测试更新：`assert_eq!(remote.file_contents("/r/a.txt"), Some(b"hello".to_vec()))` 仍能用，但 `FileAction::Apply { bytes }` 改为 `{ src }`，单测断言相应改。

---

## Task 8: chmod idempotent + tmp script cleanup

**Files:** `src/sync/file.rs`, `src/sync/script.rs`

- [ ] **8.1** `src/sync/file.rs` 的 `execute_file` 中 chmod 调用前先 stat：

```rust
if let Some(mode) = chmod {
    let cur = remote.stat_mode(dst).await.ok();
    if cur != Some(*mode) {
        if let Err(err) = remote.chmod(dst, *mode).await { ... }
    }
}
```

- [ ] **8.2** `src/sync/script.rs` 的 `execute_script` 在 Run 分支末尾、不论 exit code 多少都尝试清理：

```rust
let exec_outcome = match remote.interactive_exec(&command, None).await { ... };  // 暂用 None timeout
let _ = remote.remove_file(upload_to).await;  // best-effort cleanup
exec_outcome
```

如果清理失败，不改变 exec_outcome；可以 `reporter.warning` 一行。

- [ ] **8.3** Run all tests pass。

---

## Task 9: register_pubkey 解析 key 后比较

**Files:** `src/sync/mod.rs`

- [ ] **9.1** 改 `Pipeline::execute_pubkey`：

```rust
fn parse_pubkey_body(line: &str) -> Option<(String, String)> {
    // skip leading SSH options: comma-separated key=val tokens with no space → not implementing
    // Simple form: "type base64 [comment]"
    let mut parts = line.split_whitespace();
    let ty = parts.next()?;
    let key = parts.next()?;
    if !ty.starts_with("ssh-") && !ty.starts_with("ecdsa-") && !ty.starts_with("sk-") {
        return None;
    }
    Some((ty.to_string(), key.to_string()))
}

// 在 execute_pubkey 中：
let new_body = parse_pubkey_body(&pub_str).ok_or_else(|| ...)?;
let already = existing.lines().any(|l| {
    parse_pubkey_body(l).map(|b| b == new_body).unwrap_or(false)
});
if already { return Ok(ItemOutcome::Skipped(SkipReason::AlreadyExists)); }
```

- [ ] **9.2** 加 `parse_pubkey_body` 单元测试 ≥3：含 comment / 不含 / 含 options（应返回 None 或在 type 位被吞，确保不误判）。

---

## Task 10: PipelineOpts.retries + 在 file/block plan 调用 RemoteOps 时包 retry

**Files:** `src/sync/mod.rs`, `src/sync/file.rs`, `src/sync/block.rs`

- [ ] **10.1** `PipelineOpts` 加：

```rust
pub retry: crate::remote::retry::RetryPolicy,
```

Default 用 `RetryPolicy::default()`。

- [ ] **10.2** `plan_one_file` / `plan_one_block` / `execute_file` / `execute_block` / `execute_script` / `execute_pubkey` 在调用 `remote.exists/mtime/read_file/write_file/chmod/ensure_dir/exec/stat_mode/remove_file` 时，用 `with_retry(policy, || remote.xxx(...))` 包。**`interactive_exec` 不 retry**（脚本可能有副作用）。

- [ ] **10.3** Pipeline 入口把 `self.opts.retry` 透传到 `plan_files / plan_blocks` 等。需要稍调签名加 retry 参数（或者直接传 PipelineOpts 引用）。

实际更简洁：在 sync stage 函数签名上加 `policy: RetryPolicy`，从 Pipeline.opts 取。

- [ ] **10.4** Test 用 FakeRemote 注入 transient transport error 验证 retry 后成功。

---

## Task 11: interactive_exec 完整 PTY + IO + signal + timeout

**Files:** `src/remote/mod.rs`, `src/remote/ssh.rs`, `src/remote/fake.rs`, `src/sync/script.rs`

- [ ] **11.1** trait 改：

```rust
async fn interactive_exec(&self, cmd: &str, timeout: Option<std::time::Duration>) -> Result<i32, RemoteOpsError>;
```

- [ ] **11.2** `src/remote/ssh.rs` 的 `interactive_exec` 实现要求：
  1. PTY 申请：`channel.request_pty("xterm-256color", cols, rows, ...)`（已有）
  2. exec 命令
  3. 启动 4 个 task：
     - `stdin_task`: tokio::io::stdin → `channel.data(buf)` 直到 EOF / cancel
     - `stdout_task`: 收 `ChannelMsg::Data` → tokio::io::stdout
     - `stderr_task`: 收 `ChannelMsg::ExtendedData(stderr_id)` → tokio::io::stderr
     - `exit_task`: 收 `ChannelMsg::ExitStatus(code)` → 返回
  4. SIGINT 转发：`tokio::signal::ctrl_c()` 触发 `channel.signal(Sig::INT)`；本地不退出。
     - **二次 Ctrl-C** 在 5 秒内：`channel.close()` 强 drop。
  5. timeout：`tokio::time::timeout(timeout, exit_task)`，超时返回 `RemoteOpsError::Io("interactive_exec timed out after {dur:?}")`。
  6. 资源清理：所有 task abort/cancel；channel close。

russh API 提示：
- Sig signal 经 `channel.signal(Sig::INT)`（具体名字看 russh 0.60 文档）
- ExtendedData 对应 stderr_id = 1

实现细节可能要查 russh changelog；如果某 API 不可用，留 `// TODO: russh-0.60 API 不直接提供 X，暂以 Y 替代` 但要让程序能跑。

- [ ] **11.3** `src/remote/fake.rs` 的 `interactive_exec` 也加 timeout 参数（行为简单：还是查 interactive_exit_status 立即返回）。

- [ ] **11.4** `execute_script` 调用 `remote.interactive_exec(&command, opts.script_timeout)`，opts.script_timeout 默认 `None`（无超时）。`PipelineOpts.script_timeout: Option<Duration>`。

- [ ] **11.5** 单元测试：FakeRemote 验证 timeout 参数透传到记录里（FakeRemote 加 `interactive_calls: Vec<(String, Option<Duration>)>`）。

---

## Task 12: Pipeline 顶层 Ctrl-C 透传 + interrupted summary

**Files:** `src/sync/mod.rs`

- [ ] **12.1** `Pipeline::run` 改：

```rust
pub async fn run(&self) -> PipelineSummary {
    use tokio::signal;
    let plan = self.plan().await;
    if self.opts.dry_run {
        self.reporter.print_plan(&plan);
        return PipelineSummary { stages: vec![], interrupted: false, dry_run: true };
    }
    let exec_fut = self.execute(&plan);
    tokio::select! {
        s = exec_fut => s,
        _ = signal::ctrl_c() => {
            self.reporter.warning("interrupted by user (Ctrl-C)");
            PipelineSummary { stages: vec![], interrupted: true, dry_run: false }
        }
    }
}
```

实际更稳：让 `execute` 在 stage 间检查 cancellation token 而不是粗暴 drop。简化版：select drop 即可，FuturesUnordered 会被 drop 释放。

- [ ] **12.2** `PipelineSummary::exit_code` 已经 handle interrupted（130）——确认。

- [ ] **12.3** 单测：很难测 ctrl_c；改测 `interrupted: true → exit_code == 130`。

---

## Task 13: 配置层透出新 CLI flag

**Files:** `src/main.rs`, `src/cli/mod.rs`

- [ ] **13.1** `main.rs` 的 `Sync` 子命令加 `--retries N` (default 3)、`--script-timeout SECS`（可选）。

- [ ] **13.2** `cli/run_sync` 把这些塞进 `PipelineOpts`。

---

## Task 14: 最终 fmt + clippy + test 全过

- [ ] **14.1** `cargo fmt --all` 干净
- [ ] **14.2** `cargo clippy --all-targets -- -D warnings` 零 warning
- [ ] **14.3** `cargo test --all-targets` 全绿
- [ ] **14.4** Build release sanity: `cargo build --release --offline`

---

## Self-Review

**Spec coverage:**
- [x] DAG removal — Task 4
- [x] plan parallelism — Task 5
- [x] timestamp = mtime — Task 6
- [x] streaming files — Task 7
- [x] stat_mode + remove_file — Task 1
- [x] chmod idempotent — Task 8
- [x] tmp cleanup — Task 8
- [x] retry — Tasks 2 + 10
- [x] ItemOutcome rich error — Task 3
- [x] register_pubkey better — Task 9
- [x] interactive_exec PTY/IO/signal/timeout — Task 11
- [x] Pipeline ctrl-c — Task 12
- [x] CLI flags — Task 13
- [x] Final cleanup — Task 14

**Type consistency check:** `RetryPolicy` declared in retry.rs, used in PipelineOpts (Task 10). `ItemOutcome::Failed(Arc<SyncError>)` used consistently in reporter+sync (Task 3). `FileAction::Apply::src` (Task 7) used in execute_file (Task 7) and chmod check (Task 8).

**No placeholders.**

---

## Acceptance

1. `cargo fmt --all -- --check` 干净
2. `cargo clippy --all-targets -- -D warnings` 0 warnings
3. `cargo test --all-targets` 全绿且**测试数 ≥ 60**（Phase 2 是 51；新加 retry 2 + idempotency 1 + parse_pubkey 3+ + stat_mode/remove_file 3 ≈ +9）
4. `flux sync <name> --dry-run` 退出码语义保持
5. Ctrl-C 干净退出，summary 显示 interrupted
6. 没有 `dependencies` 字段任何残留
