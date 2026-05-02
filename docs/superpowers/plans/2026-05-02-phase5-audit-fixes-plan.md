# Flux Phase 5 — Audit Fixes

> Codex 全面审计 + 我手动测试 共发现 16 项问题；本 plan 合并修复。
>
> 4 项是**发布级阻塞**，必须修。其余按优先级。

**Goal:** 把 Phase 4 后的真 bug、安全洞、cache 污染问题全部修干净；补 schema 文档；扩 integration / proptest 覆盖。

**Acceptance:** cargo fmt + clippy + test 全过；新增 ≥10 个测试；4 个阻塞级 bug 都有回归测试。

---

## 阻塞级修复（4 项 — 必须先做）

### F1: shell_escape 改 single-quote（命令注入）

**位置：** `src/remote/ssh.rs:658` 附近

**问题：** 当前 shell_escape 用双引号，`$` `` ` `` `$()` 仍会展开。被 `write_remote_file`、`rename`、`remove_file`、`ensure_dir` 调用 → 命令注入面。

**修：** 全部改单引号 escaping。算法：

```rust
fn shell_escape(s: &str) -> String {
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
```

**回归测试：**
```rust
#[test]
fn shell_escape_neutralizes_dollar_sign() {
    assert_eq!(shell_escape("$HOME"), "'$HOME'");
}
#[test]
fn shell_escape_neutralizes_command_substitution() {
    assert_eq!(shell_escape("$(rm -rf /)"), "'$(rm -rf /)'");
}
#[test]
fn shell_escape_handles_embedded_single_quote() {
    assert_eq!(shell_escape("a'b"), r"'a'\''b'");
}
```

### F2: save_host_state guard

**位置：** `src/cli/mod.rs:386` 附近的 `save_host_state` 调用

**问题：** dry_run / interrupted / failed 也写 state；下次实跑命中 cache 跳执行，sync 实际没做。

**修：** 仅在 `!summary.dry_run && !summary.interrupted && summary.total_failed() == 0` 时落盘。

**回归测试：**
```rust
#[tokio::test]
async fn dry_run_does_not_write_state() {
    // ... run pipeline with dry_run=true and state empty
    // assert state file does not exist after run
}
#[tokio::test]
async fn failed_run_does_not_write_state() {
    // construct pipeline with one Failed file; run; assert state unchanged
}
```

### F3: cache key 重做

**位置：** 
- `src/sync/file.rs:105` (plan hash)
- `src/sync/file.rs:379` (collect_item_hashes)
- `src/sync/block.rs:178`
- `src/sync/script.rs:87` 和 `:141`（plan vs save 不一致）

**问题：** cache key 只 hash 内容；改 dst/mode/chmod/template/target 不影响 hash → 错误命中 cache 跳过。script 一边用真实 `/tmp/flux_script_<pid>_...` 一边用 `/tmp/placeholder.sh`，永不命中。

**修：** 三个 stage 各自重新设计 cache key，**纳入所有影响执行结果的字段**：

- file: `sha256(content || dst || mode_string || chmod_octal)`
- block: `sha256(body || target || mode_string || comment_template || item_name)`
- script: `sha256(local_script_bytes || interpreter || flags || args || item_name)` —— **不包含 upload_to/pid**；plan 和 save 两侧统一用此函数

把 hash 计算抽成 `cache_key(...)` helper，plan 和 save 共用。

**回归测试：**
```rust
#[tokio::test]
async fn changing_dst_invalidates_file_cache() {
    // first sync writes state with dst=/r/a; second sync changes dst to /r/b same content
    // assert plan returns Apply not Skip(ContentUnchanged)
}
#[tokio::test]
async fn changing_chmod_invalidates_file_cache() { ... }
#[tokio::test]
async fn changing_block_target_invalidates_block_cache() { ... }
#[tokio::test]
async fn script_cache_hits_across_pids() {
    // 模拟两次不同 pid，cache 应该命中（不再 by-pid）
}
```

### F4: Ctrl-C 语义统一

**位置：** `src/sync/mod.rs:157` (Pipeline::run select on ctrl_c) 与 `src/remote/ssh.rs:388` (interactive_exec 内部 5s SIGINT 双击)

**问题：** Pipeline 顶层 `tokio::select!` 监听 ctrl_c；interactive_exec 也注册 ctrl_c 监听。第一次 Ctrl-C 可能被外层抢走 → 整个 pipeline 立刻 interrupted，没机会转发 SIGINT 给远端 script。

**修：** 共享 cancellation。提议方案：
- Pipeline 持 `Arc<tokio_util::sync::CancellationToken>` 或自定义 `Notify`
- 监听 ctrl_c 时**只设置 token，不立即返回**
- interactive_exec 看到 token 改为先发 SIGINT 给远端，再短暂等待远端退出
- 第二次 Ctrl-C（5s 内）才硬 kill

不引入新 crate：用 `Arc<AtomicUsize>` 计 ctrl_c 次数 + `Notify`。

**回归测试：** 难直接测信号。改测：构造一个假 ctrl_c trigger（FakeRemote 暴露 inject_ctrl_c），断言第一次只转发不中断，第二次才 close。

### F5: plan/execute TOCTOU revalidate

**位置：** `src/sync/file.rs:486` / `src/sync/file.rs:623` / `src/sync/block.rs:275` / `src/sync/block.rs:356`

**问题：** plan 阶段判断"远端 mtime > 本地"则 Skip(RemoteNewer)；execute 直接覆盖。两阶段间远端被改的话，合法更新会被覆盖。

**修：** execute_file / execute_block 在 Apply 分支前**重查一次 remote mtime**：
- 如远端 mtime > 我们 plan 时记下的 remote mtime → 改为 outcome=Skipped(RemoteNewer)，记 reporter.warning
- 否则正常 Apply

需要 Action 携带 plan 时观察到的 mtime（新增字段 `observed_remote_mtime: Option<DateTime<Utc>>`）。

**回归测试：**
```rust
#[tokio::test]
async fn execute_skips_if_remote_changed_between_plan_and_execute() {
    let remote = InMemoryRemote::with_files([("/r/a", b"old".to_vec())]);
    let actions = file::plan_files(...).await;
    // simulate remote being updated externally
    remote.write_file("/r/a", b"external").await.unwrap();
    let outcome = execute_file(&actions[0], &remote, &reporter).await;
    assert!(matches!(outcome, ItemOutcome::Skipped(SkipReason::RemoteNewer)));
}
```

---

## 中优先级修复（11 项）

### F6: 唯一 tmp / backup 名

**位置：** `src/sync/file.rs:627`、`src/sync/file.rs:636`

**修：**
- tmp: `<dst>.flux.tmp.<pid>.<nanos>.<rand6>`
- backup: `<dst>.flux-<unix_nanos>.bak`（替代秒级）

可加 `rand` crate（轻量）或用 `std::time::SystemTime::now().duration_since(UNIX_EPOCH).as_nanos()` + thread_rng-like 简易计数器。**优先后者免新依赖**。

### F7: rotate_backups 排序按 ts 字段

**位置：** `src/sync/file.rs:765`

**问题：** `sort -t- -k2,2nr` 按整路径第二个 `-` 排序；目录或 basename 含 `-` 时错。

**修：** 在 Rust 端读列表，用 `backup_timestamp(name)` parser 显式提取 ts 排序。

### F8: env interpolation 跳过 yaml 注释

**位置：** `src/config/loader.rs` `interpolate` 函数

**问题：** `${VAR}` 在 `# ... ${VAR} ...` 注释里也被插值。

**修：** 两选一（推荐方案 a）：
- a. 支持 `$$` 转义为字面量 `$`，文档化"注释里如要写 ${} 用 $${"
- b. 跳过 `#` 开头到行尾的内容（YAML 注释规则；但字符串内 `#` 不算注释，需简单 yaml 词法识别）

选 **a**：实现简单，可控。

```rust
// in interpolate(): if char == '$' && peek == '$' → emit single '$', advance
```

**回归测试：**
```rust
#[test]
fn interpolate_supports_dollar_escape() {
    assert_eq!(interpolate("a $$VAR b").unwrap(), "a $VAR b");
}
```

### F9: 非 TTY 时 port prompt 自动用 default

**位置：** `src/sync/mod.rs:649-654` 的 `Input::new().with_prompt("Port").default(22).interact_text()`

**问题：** 非 TTY 直接 fail "not a terminal"，即使有 default。

**修：** 检测 TTY：`if console::Term::stdout().is_term() { interact_text } else { default value }`。同样适用于 Host / User prompt 时如果有 default 就直接用。

**回归测试：** 难自动化（涉及 stdin）。至少加 unit test 验证 helper "if !is_term && has_default → return default"。

### F10: Config::validate 补完

**位置：** `src/config/mod.rs:294`（当前空）

**修：** 加：
- `register_key=true` 但无 `key` → ConfigError::RegisterKeyRequiresKey
- file/script/block items 中 `chmod` 字段非合法八进制 → ConfigError::InvalidChmod
- proxy.local_port == 0 → ConfigError::InvalidPort

### F11: 非法 chmod 改返 Failed

**位置：** `src/sync/file.rs:113`

**修：** `chmod = item.chmod.as_deref().and_then(|s| u32::from_str_radix(s, 8).ok())` → 改成 `.map(|s| u32::from_str_radix(s, 8))`，若 Err 直接 `FileAction::Failed`。

实际上 F10 在配置加载时已拦下，F11 是防御。

### F12: compose 保留远端 EOL

**位置：** `src/sync/block.rs:404`

**修：** compose 时检测 existing 用 `\r\n` 还是 `\n`，保留同风格。

### F13: retry 排除非幂等 op

**位置：** `src/remote/retry.rs:30`

**问题：** retry 包了 rename/remove_file；rename 第一次成功响应丢失，重试报 NotFound。

**修：** 提供 `with_retry_idempotent(policy, op)` vs `with_retry_unsafe`(...)；调用点显式选择。简单做法：**rename 和 remove_file 调用时不走 with_retry**（即把 retry 包装从这两个 op 上摘掉）。

### F14: schema-migrations 更新 v2

**位置：** `docs/schema-migrations.md`

**修：** 加 v2 章节，列出新增字段：tags, kind, target, imports, password (SecretValue), version。声明全部向后兼容。

### F15: README 反映 Phase 4

**位置：** `README.md`

**修：** 加段落讲：dry-run + diff、partial sync (--only-stage/--tag)、undo、imports、env interpolation、keychain、多 host fan-out。

### F16: register_pubkey 空 key path 提前报错

**位置：** `src/sync/mod.rs:89` 的 plan_pubkey

**修：** 在 plan 阶段，若 register_key=true 但 key 字段空 → action.local_pubkey_path 不应是空字符串；改为返回 RegisterPubkeyAction::Failed（或 None+warning）。**实际 F10 在 Config::validate 已经拦掉，这里只是防御**。

---

## 测试增量（必做）

### T1: integration 扩展

补：
- `tests/integration/pipeline_block.rs`
- `tests/integration/pipeline_dry_run.rs`（验证 dry-run 不写 state）
- `tests/integration/pipeline_resume.rs`
- `tests/integration/pipeline_multi_host.rs`
- `tests/integration/pipeline_filter.rs`（--only-stage/--tag 行为）

每个用 InMemoryRemote 模拟，断言核心契约。

### T2: proptest 扩展

加：
- `tests/proptests/shell_escape.rs`：随机 string → shell_escape → echo 'X' 反推（用 `sh -c` 真跑或 lexer 模拟）
- `tests/proptests/env_interpolation.rs`：随机包含 `${...}` 的字符串，断言无 var 报错、有 default 回退
- `tests/proptests/cache_key.rs`：变更任一影响字段（dst/mode/chmod/target/template）→ hash 变

---

## 不做（本 phase）

- host key 严格校验（F16 在 audit 中标 `ssh.rs:47`） — 设计较大，留 Phase 6
- expand_tilde 去重 — 重构性
- 长文件拆分 — 重构性
- 多 host SSH 池化 — 性能优化
- file stage 批量 stat — 性能优化

---

## 工作模式

- 不做 git
- offline cargo
- 一次 turn 跑多远跑多远；自然边界停
- 末尾 Progress / Verification / Handoff
- F1-F5 是阻塞，一定要先做掉

## Acceptance

1. `cargo fmt --all -- --check` 干净
2. `cargo clippy --all-targets -- -D warnings` 0 warnings
3. `cargo test --all-targets` 全过；测试数 ≥ 105（当前 92 + 阻塞回归 ≥4 + cache 4 + 中优先级 ≥3 + integration ≥5 + proptest ≥3）
4. 4 个阻塞 bug 都有命名清晰的回归测试
5. cargo build --release 通过
6. README + schema-migrations.md 反映 v2
