# Flux 重构设计文档（Phase 2）

**日期**：2026-05-01
**状态**：approved（用户在 brainstorm 中已批准全部 6 段）
**前置**：Phase 1 已落地（依赖升级、bug 修复、安全卫生）

## 背景

Phase 1 完成了机械层修复：依赖整体升级（russh 0.46→0.60.2 等）、19 条 bug 修复、secret 模板与 `.gitignore` 卫生。Phase 2 是架构重构，目标是把 sync 层与远程副作用解耦，建立可单测的执行计划层，并补全测试矩阵。

代码体量当前约 2400 行 Rust，仅 path.rs 有 3 个测试。Phase 2 完成后预期单测覆盖率显著提升，测试不依赖真实 SSH。

## 设计目标

1. **可测性**：sync 层每个 stage 都能用 in-memory fake 完整跑过，不需 SSH server
2. **dry-run 一等公民**：`flux sync --dry-run` 输出人类可读 plan，不动远端
3. **错误可观察**：domain enum 让测试断言具体 variant，CLI 边界统一 anyhow
4. **并发**：file/block 阶段在 stage 内并行（block 按 target file 分组）
5. **零向后破坏**：Phase 2 不改变 `westlake.yml` 的合法 schema；老配置仍能跑

## 架构

### 模块布局

```
src/
├── main.rs              # CLI dispatch（clap），仅做参数转发
├── lib.rs               # 把内部模块 re-export 给 tests/ 使用
├── cli/
│   ├── mod.rs           # run_init / run_sync / run_proxy（从 main.rs 抽出）
│   └── ssh_config.rs    # save / read / parse SSH config（IPv6, Include）
├── config/
│   ├── mod.rs           # Config struct, ProxyProtocol enum
│   ├── version.rs       # schema version probe + migration
│   └── validate.rs      # resolve_root, validate (refint), deny_unknown_fields
├── path.rs              # FluxPath + AssetLocator（聚合 .flux/{files,scripts,blocks} 约定）
├── remote/
│   ├── mod.rs           # RemoteOps trait + RemoteOpsError
│   ├── ssh.rs           # SshClient impl RemoteOps
│   └── fake.rs          # #[cfg(test)] InMemoryRemote
├── reporter/
│   ├── mod.rs           # Reporter trait + Stage / ItemOutcome
│   ├── console.rs       # ConsoleReporter（替代 src/output.rs）
│   └── memory.rs        # #[cfg(test)] CapturedReporter
└── sync/
    ├── mod.rs           # Pipeline { config, remote, reporter, opts } + run_pipeline
    ├── plan.rs          # Plan struct + Action enums
    ├── file.rs          # plan_files, execute_file
    ├── script.rs        # plan_scripts, execute_script
    └── block.rs         # sentinel parser + plan_blocks + execute_block
```

`Cargo.toml` 转 `bin + lib` 双 crate：bin 仍是 `main.rs`，lib 暴露给 `tests/` 使用，避免在测试里 `mod` 引入 src 内部文件。

### RemoteOps trait

低层原子操作，不带 Flux 业务概念：

```rust
// src/remote/mod.rs
#[async_trait::async_trait]
pub trait RemoteOps: Send + Sync {
    async fn exec(&self, cmd: &str) -> Result<ExecOutput, RemoteOpsError>;
    async fn read_file(&self, path: &str) -> Result<Vec<u8>, RemoteOpsError>;
    async fn write_file(&self, path: &str, data: &[u8]) -> Result<(), RemoteOpsError>;
    async fn exists(&self, path: &str) -> Result<bool, RemoteOpsError>;
    async fn mtime(&self, path: &str) -> Result<chrono::DateTime<chrono::Utc>, RemoteOpsError>;
    async fn chmod(&self, path: &str, mode: u32) -> Result<(), RemoteOpsError>;
    async fn ensure_dir(&self, path: &str) -> Result<(), RemoteOpsError>;
    async fn interactive_exec(&self, cmd: &str) -> Result<i32, RemoteOpsError>;
}

pub struct ExecOutput {
    pub status: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}
```

不进 trait 的（`SshClient` 独有方法）：
- `connect`、`register_pubkey`、`start_reverse_forward`

`sync` 层用泛型 `R: RemoteOps + ?Sized` 接受 `&R`——比 `&dyn` 静态分发，并且测试时直接传 `&FakeRemote` 不需 box。

### Plan / Execute 数据模型

每个 stage 一对 plan/execute：

```rust
// src/sync/plan.rs
pub struct Plan {
    pub register_pubkey: Option<RegisterPubkeyAction>,
    pub file_actions: Vec<FileAction>,
    pub script_actions: Vec<ScriptAction>,
    pub block_actions: Vec<BlockAction>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum FileAction {
    Skip   { item_name: String, reason: SkipReason },
    Apply  { item_name: String, dst: String, bytes: Vec<u8>, chmod: Option<u32> },
    Failed { item_name: String, error: SyncError },
}

#[derive(Debug, PartialEq, Eq)]
pub enum ScriptAction {
    Skip   { item_name: String, reason: SkipReason },
    Run    { item_name: String, upload_to: String, command_argv: Vec<String> },
    Failed { item_name: String, error: SyncError },
}

#[derive(Debug, PartialEq, Eq)]
pub enum BlockAction {
    Skip   { item_name: String, reason: SkipReason },
    Apply  { item_name: String, target: String, body: String, sentinel: Sentinel },
    Failed { item_name: String, error: SyncError },
}

pub struct Sentinel {
    pub name: String,
    pub timestamp: i64,
    pub open_marker: String,
    pub close_marker: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum SkipReason {
    AlreadyExists,
    RemoteNewer,
    ContentUnchanged,
    DependencyFailed(String),
}
```

**关键约束**：
- `plan_*` 函数只读 RemoteOps（exists / mtime / read_file），不写、不 exec、不 chmod
- `execute_*` 根据 action 调写接口，不再决定逻辑分支
- `BlockAction::Apply` 不存预计算的 byte_range——execute 阶段根据 sentinel 重新定位（同 target 多 block 串行时上一次写入会改变 offset）

### Pipeline 入口

```rust
// src/sync/mod.rs
pub struct PipelineOpts {
    pub dry_run: bool,
    pub max_concurrency: Option<usize>,  // default 8
}

pub struct Pipeline<'a, R: RemoteOps + ?Sized> {
    pub config: &'a Config,
    pub remote: &'a R,
    pub reporter: &'a dyn Reporter,
    pub opts: PipelineOpts,
}

impl<'a, R: RemoteOps + ?Sized> Pipeline<'a, R> {
    pub async fn plan(&self) -> Plan { ... }
    pub async fn execute(&self, plan: &Plan) -> PipelineSummary { ... }
    pub async fn run(&self) -> PipelineSummary {
        let plan = self.plan().await;
        if self.opts.dry_run {
            self.reporter.print_plan(&plan);
            return PipelineSummary::dry_run(plan);
        }
        self.execute(&plan).await
    }
}
```

### 错误模型

每个领域模块用 `thiserror` 定义自己的 enum；`run_sync` 边界包成 `anyhow::Result`。

```rust
// src/remote/mod.rs
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
}

// src/sync/mod.rs
#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error("file: {0}")]   File(#[from] FileError),
    #[error("script: {0}")] Script(#[from] ScriptError),
    #[error("block: {0}")]  Block(#[from] BlockError),
    #[error("remote: {0}")] Remote(#[from] RemoteOpsError),
}

// 各 stage 的具体错误：FileError / ScriptError / BlockError 在对应 sync/*.rs
// ConfigError 在 config/mod.rs
// SshConfigError 在 cli/ssh_config.rs
```

`thiserror` 在 Phase 1 被 Worker A 删除——Phase 2 重新加入 `thiserror = "2"`。

### Reporter trait

```rust
// src/reporter/mod.rs
pub trait Reporter: Send + Sync {
    fn stage_started(&self, stage: Stage, item_count: usize);
    fn item_started(&self, stage: Stage, name: &str);
    fn item_finished(&self, stage: Stage, name: &str, outcome: &ItemOutcome);
    fn stage_finished(&self, stage: Stage, summary: &StageSummary);
    fn print_plan(&self, plan: &Plan);
    fn pipeline_summary(&self, summary: &PipelineSummary);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage { File, Script, Block, Pubkey }

#[derive(Debug)]
pub enum ItemOutcome {
    Applied,
    Skipped(SkipReason),
    Failed(String),  // 已 stringify，避免 reporter 依赖 sync error 类型
}
```

`ConsoleReporter` 是默认的实现，包装当前 `src/output.rs` 里的 console/dialoguer 调用；`CapturedReporter` 测试用，把所有事件 push 到 `Mutex<Vec<Event>>` 供断言。

### 并发模型

| Stage | 策略 |
|---|---|
| **file** | `FuturesUnordered` + `buffer_unordered(N)` 全并行；N 默认 8，可经 `--max-concurrency` 覆盖 |
| **script** | 串行（保留现有"yaml 顺序即依赖顺序"语义） |
| **block** | 按 target file 分组：组间并行，组内串行 |
| **pubkey** | 单项 |

block 组内串行的强依赖：执行第二个 block 前必须重新 `read_file` 远端最新内容（第一个 block 已改了）。`BlockAction::Apply` 不缓存 byte_range，靠 sentinel 在 execute 时重定位。

错误传播：单 item 失败仍记 summary，不取消 stage 内其它并发任务。

取消：`run_sync` 顶层 `tokio::select!` 监听 `tokio::signal::ctrl_c`，命中时 drop 当前 FuturesUnordered，输出 "interrupted" summary。

### dry-run

```
flux sync westlake --dry-run
flux sync westlake --max-concurrency 4
```

行为：
- 仍建 SSH 连接（plan 阶段需要 read-only RemoteOps）
- **不**做 register_pubkey、**不**开 reverse forward
- 跑 `Pipeline::plan()` → `reporter.print_plan(&plan)` → 退出
- **退出码语义**：
  - 0：plan 全部 Skip / Apply
  - 2：plan 含 Failed action（事还没做但已经知道会失败）

### Schema 版本化

```rust
// src/config/version.rs
pub const CURRENT_SCHEMA_VERSION: u32 = 1;

#[derive(Deserialize)]
struct VersionProbe {
    #[serde(default = "default_version")]
    version: u32,
}

fn default_version() -> u32 { 1 }

pub fn load_versioned(yaml: &str) -> Result<Config, ConfigError> {
    let probe: VersionProbe = serde_yml::from_str(yaml)?;
    match probe.version {
        0 | 1 => parse_v1(yaml),
        v if v > CURRENT_SCHEMA_VERSION => Err(ConfigError::FutureVersion { found: v, max: CURRENT_SCHEMA_VERSION }),
        _ => unreachable!(),
    }
}
```

约定：
- `version` 字段缺省即 v1（不破坏老 yaml）
- v2 出现时新增 `parse_v2 + migrate_v1_to_v2`
- v1 内 schema 演进必须向后兼容或加 `#[serde(alias = "...")]`
- 突破性变更必须升 version

新建 `docs/schema-migrations.md`，每次 schema 变更记一段。

## 测试矩阵

```
tests/
├── proptests/
│   └── block_sentinel.rs
├── integration/
│   ├── pipeline_file.rs
│   ├── pipeline_script.rs
│   ├── pipeline_block.rs
│   └── pipeline_dry_run.rs
└── fixtures/
    ├── westlake_minimal.yml
    └── westlake_with_blocks.yml
```

src 内 `#[cfg(test)] mod tests` 覆盖纯函数级。

### 必须覆盖清单

**path.rs**：保留 3 个旧测试 + 增 `:` 前缀异常、嵌套 `~`、Windows 反斜杠当字面量。

**config/**：
- minimal yaml / 完整 westlake.yml 都加载成功
- unknown_fields → ConfigError
- 缺省值正确（version, interpreter, flags, comment_template, register_key）
- `Config::validate`：依赖未知 → `UnknownDependency` variant
- `ProxyProtocol`：未识别值 → 解析错
- `Config::resolve_root`：有/无 flux_home，相对/绝对路径
- `version: 999` → `FutureVersion`

**sync/block.rs**（重点投资）：
- sentinel parse 正反双向
- proptest：`extract(inject(s)) == s` 对随机 body（含 CRLF / sentinel-like 字符 / emoji）
- comment_template 缺 `{}` → `BadTemplate`
- 远程文件无 sentinel → Apply (insert)
- 远程文件有 sentinel + 内容相同 → `Skip(ContentUnchanged)`
- 远程文件有 sentinel + 内容不同 + sync mode → Apply（FakeRemote 验证 read 后 write 而非 append）
- CRLF 远程文件下的字节定位
- 同 target 多 block 串行：第二个 block 看到第一个写完的状态

**sync/file.rs**：
- 三种 mode (touch/sync/cover) 各自的 plan
- mtime 漂移
- chmod 字段进 Action
- sha256 fallback（mtime 相等时）

**sync/script.rs**：
- script gating bug 回归（无 file 配置时 script 仍执行）
- dependency 失败 → `Skip(DependencyFailed("name"))`
- proptest：随机 args 含 `"`、`'`、`$`、`\`、空格、`;` → 远端 shell 解析回原 args（`shell_quote` round-trip）

**remote/fake.rs**（FakeRemote 自身）：
- 行为合约：write→read 一致、mtime 单调、chmod 持久
- exec 录制：返回预设输出 + 调用历史

**reporter/console.rs**：
- ItemOutcome 着色映射不 panic
- print_plan 不依赖 colors（ANSI strip 后断言文本）

**cli/ssh_config.rs**（新覆盖）：
- save_ssh_config 在含注释的旧 Host 块前/中/后插入
- save_ssh_config 删旧块时跳过到下一个 `Host ` 而非首个 `# `
- parse_ssh_host：`[::1]:22`、`[::1]`、`user@[::1]:2222`、`host`、`host:22`、`user@host`
- read_ssh_config_entry：`Include` 递归 + 多 host pattern + Match/通配 emit warning

**Pipeline 端到端**（`tests/integration/`）：
- minimal westlake → 全部 Apply、退出码 0
- file 阶段半数失败 → script/block 仍跑、退出码 1
- proxy.enabled + register_key → FakeRemote 收到正确 forward 请求
- dry-run：相同输入下 Plan 与正常 run 的 Plan 一致；FakeRemote.write_calls() == 0

### CI

```yaml
# .github/workflows/ci.yml
- cargo fmt --check
- cargo clippy -- -D warnings
- cargo test
- cargo audit
- cargo deny check
```

`cargo audit` + `cargo deny` 是 Phase 2 默认开起来——历史 secret 教训驱动。

### 不在 Phase 2 的测试

- 真实 SSH 集成（用户选了"纯 unit + FakeRemote"）
- 性能 benchmark
- Mutation testing
- 模糊测试 SSH 协议层（russh 自身的事）

## 风险

1. **Reporter trait 引入是 breaking change**：`output.rs` 的全部公开函数会被 `ConsoleReporter` 内部接管。不会影响 `westlake.yml` 用户，但任何外部 import flux 库的代码会受影响——目前没有这种用户。
2. **block sentinel 严格匹配**可能让"已有但格式略偏"的旧 block 被识别失败 → 触发新 insert，导致同名 block 重复。Worker C 在 Phase 1 已经收紧了匹配，Phase 2 沿用其格式约定。在 migration doc 里记录"如旧 .zshrc 内有手工写的伪 sentinel，需手动清理"。
3. **测试基础设施投入**：FakeRemote + CapturedReporter 是新增的 ~300 行 testing infra；初期成本大，但解锁后续所有测试。
4. **Stage 内并发**有竞态可能（同 target 写入）：block 已按 target 分组缓解；file 假设 dst 唯一（由 `Config::validate` 在加载阶段保证）。

## 不在 Phase 2 的事

- host key 严格校验（Worker A 留了 TODO）
- SIGINT/SIGTERM 转发到远端（Worker A 留了 TODO）
- 真 Windows remote 支持（schema 已在文档里说明仅 Unix-like）
- 性能优化（先求正确，再求快）
