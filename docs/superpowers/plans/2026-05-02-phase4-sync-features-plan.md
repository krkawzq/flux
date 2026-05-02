# Flux Phase 4 — Sync Feature Expansion

> Continuation of Phase 3. Same conventions: codex doesn't touch git; uses `mv`/`rm`; offline cargo only.

**Goal:** 把 sync 功能从"对 SSH 远端执行 plan"扩成"日常生产级工具"——加 partial sync、diff/原子写/备份、PTY 完美 + 观测、env 与 imports、glob/dir/symlink、多机 fan-out 与 state cache。

**Strategy:** 3 batch sequential。每 batch 在自己 commit；失败可独立回滚不影响后续。

**Tech additions:**
- `similar = "2"` (text diff)
- `tracing = "0.1" + tracing-subscriber = "0.3"`（structured logging）
- `serde_json = "1"`（audit.jsonl + state cache）
- `globset = "0.4"`（glob 匹配）
- `walkdir = "2"`（dir 递归）

**Defaults (user-approved):**
- A.kind: auto-detect (`*` → glob; local dir → dir; else file); `link` 必须显式
- B: `--only-*` 和 `--skip-*` 互斥
- C.backup: `<dst>.flux-<ts>.bak`，保留 3 份；`flux undo` 只回最后一次
- D.tracing: 默认 text；`--log-format json` 启用 JSON
- E: `${VAR}` / `${VAR:-default}` 内联；不引入新 yaml tag；`from_keychain: "service.account"`；imports 后覆盖前；递归 + 循环检测
- F: `--hosts` CLI 优先于 yaml 单 host；state cache 默认开启；`--no-cache` 跳过；`max_hosts = min(8, len(hosts))`

---

## Batch 1: D + B + C — observability + UX + safety

### Files

```
Cargo.toml                                 # +similar, tracing, tracing-subscriber, serde_json
src/remote/mod.rs                          # RemoteOps::rename
src/remote/ssh.rs                          # rename impl + write_file uses tmp+rename
src/remote/fake.rs                         # rename + retention model
src/sync/plan.rs                           # SkipReason::FilteredOut + ItemTags type
src/sync/mod.rs                            # PipelineOpts.filters; apply_filters() post-plan;
                                           # backup/atomic in execute_file
src/config/mod.rs                          # ItemTags on FileItem/ScriptItem/BlockItem
src/cli/mod.rs                             # filters args, --diff, --log-format, undo subcmd
src/main.rs                                # clap: filters + --diff + --log-format + Undo subcmd
src/reporter/mod.rs                        # print_plan diff variant; tracing setup helper
src/reporter/console.rs                    # diff rendering via similar
src/audit.rs                               # NEW: append_audit() to ~/.flux/audit.jsonl
src/lib.rs                                 # pub mod audit
docs/schema-migrations.md                  # note: tags field added
```

### Task 1: Add deps + tracing setup

- [ ] **1.1** `Cargo.toml [dependencies]`：

```toml
similar = "2"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter", "json"] }
serde_json = "1"
```

- [ ] **1.2** Create `src/audit.rs`:

```rust
//! Append-only structured audit log at ~/.flux/audit.jsonl.

use crate::reporter::PipelineSummary;
use chrono::Utc;
use serde::Serialize;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;

#[derive(Debug, Serialize)]
pub struct AuditEntry<'a> {
    pub ts: String,
    pub host: &'a str,
    pub config_name: &'a str,
    pub duration_ms: u128,
    pub interrupted: bool,
    pub dry_run: bool,
    pub stages: Vec<StageRecord>,
}

#[derive(Debug, Serialize)]
pub struct StageRecord {
    pub stage: String,
    pub applied: usize,
    pub skipped: usize,
    pub failed: usize,
}

pub fn audit_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".flux").join("audit.jsonl"))
}

pub fn append(host: &str, config_name: &str, duration_ms: u128, summary: &PipelineSummary) -> std::io::Result<()> {
    let Some(path) = audit_path() else { return Ok(()); };
    if let Some(parent) = path.parent() { std::fs::create_dir_all(parent)?; }
    let entry = AuditEntry {
        ts: Utc::now().to_rfc3339(),
        host,
        config_name,
        duration_ms,
        interrupted: summary.interrupted,
        dry_run: summary.dry_run,
        stages: summary.stages.iter().map(|s| StageRecord {
            stage: format!("{:?}", s.stage),
            applied: s.applied,
            skipped: s.skipped,
            failed: s.failed,
        }).collect(),
    };
    let line = serde_json::to_string(&entry).map_err(std::io::Error::other)?;
    let mut f = OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(f, "{line}")
}
```

- [ ] **1.3** Wire tracing in `cli::run_sync` early:

```rust
fn init_tracing(format: LogFormat) {
    use tracing_subscriber::{fmt, EnvFilter};
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    match format {
        LogFormat::Json => tracing_subscriber::fmt().with_env_filter(filter).json().init(),
        LogFormat::Text => tracing_subscriber::fmt().with_env_filter(filter).init(),
    }
}
```

`#[derive(Clone, Copy)] pub enum LogFormat { Text, Json }` in cli/mod.rs.

- [ ] **1.4** `cargo build --offline` should succeed (tracing-subscriber may pull deps). If offline cache lacks tracing/similar/globset/walkdir/serde_json, **report which**; we'll retry online.

### Task 2: PTY size + SIGWINCH

- [ ] **2.1** `src/remote/ssh.rs` interactive_exec:
  - Replace fixed 80x24 with `console::Term::stdout().size_checked().unwrap_or((24, 80))`
  - Add SIGWINCH listener:
    ```rust
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut winch = signal(SignalKind::window_change()).ok();
        // in main select loop, on winch: reread console::Term::size and call channel.window_change(cols, rows, 0, 0)
    }
    ```
  - Windows: skip SIGWINCH (cfg-gate)

- [ ] **2.2** Test with FakeRemote: cannot test signals; assert that `interactive_exec` still signature-compatible.

### Task 3: RemoteOps::rename + atomic write

- [ ] **3.1** `src/remote/mod.rs` trait:
```rust
async fn rename(&self, from: &str, to: &str) -> Result<(), RemoteOpsError>;
```

- [ ] **3.2** `src/remote/ssh.rs` impl:
```rust
async fn rename(&self, from: &str, to: &str) -> Result<(), RemoteOpsError> {
    let cmd = format!("mv -f {} {}", shell_escape(from), shell_escape(to));
    let out = self.exec(&cmd).await?;
    if !out.success() {
        return Err(RemoteOpsError::NonZeroExit { status: out.status, stderr: out.stderr_string() });
    }
    Ok(())
}
```

- [ ] **3.3** `src/remote/fake.rs` rename: move entry in files/mtimes/modes maps.

- [ ] **3.4** Don't change `write_file`'s public signature. Atomic-write happens at sync layer (execute_file), where we can also handle backup. Specifically: in `execute_file`'s Apply path, after `ensure_dir`:
```rust
let tmp = format!("{dst}.flux.tmp.{}", std::process::id());
remote.write_file(&tmp, &bytes).await?;
if let Some(mode) = chmod {
    if remote.stat_mode(&tmp).await.ok() != Some(*mode) {
        remote.chmod(&tmp, *mode).await?;
    }
}
// backup if dst exists
if remote.exists(dst).await? {
    let backup = format!("{dst}.flux-{}.bak", chrono::Utc::now().timestamp());
    remote.rename(dst, &backup).await?;
    rotate_backups(remote, dst, 3).await; // keep 3 most recent .flux-*.bak
}
remote.rename(&tmp, dst).await?;
```

- [ ] **3.5** `rotate_backups` impl: `ls <dst>.flux-*.bak | sort -r | tail -n +4 | xargs rm` via exec.

- [ ] **3.6** Tests:
```rust
#[tokio::test]
async fn execute_apply_creates_backup_when_dst_existed() {
    let remote = InMemoryRemote::with_files([("/r/a.txt", b"old".to_vec())]);
    // run plan + execute on a Cover apply with new bytes
    // assert backup file exists
}
#[tokio::test]
async fn rotate_keeps_latest_3_backups() { ... }
```

### Task 4: --diff in dry-run

- [ ] **4.1** Reporter trait: `print_plan(&self, plan: &Plan, diff: bool)`. Or add second method `print_plan_with_diff(...)` to avoid breaking. Pick: **add `print_plan_with_diff` separate method, call site decides which**.

- [ ] **4.2** ConsoleReporter impl uses `similar::TextDiff::from_lines` for file content:
  - For `FileAction::Apply { src, dst, .. }`: read local bytes, attempt remote.read_file(dst), produce unified diff
  - For `BlockAction::Apply { body, target, sentinel, .. }`: extract existing block body if present, produce unified diff `existing_body` vs `body`
  - Print colored hunks (`+` green, `-` red, `@@` cyan)

- [ ] **4.3** Diff requires the reporter to access RemoteOps. So `print_plan_with_diff` signature accepts `&dyn RemoteOps`:
```rust
fn print_plan_with_diff<R: RemoteOps + ?Sized>(&self, plan: &Plan, remote: &R) -> ...
```
Wait, trait can't be generic over R. Solution: take `Box<dyn RemoteOps>` won't work either. Cleanest: `print_plan_with_diff` is a free function, not a Reporter method, in reporter/console.rs.

```rust
pub async fn print_plan_with_diff<R: RemoteOps + ?Sized>(plan: &Plan, remote: &R) {
    // for each action, fetch current and produce diff via similar
}
```

CLI: `--diff` triggers this; `--dry-run` alone uses Reporter::print_plan (no remote calls).

- [ ] **4.4** Test: prints diff with at least one `+`/`-` line for changed file.

### Task 5: SkipReason::FilteredOut + filter args

- [ ] **5.1** `src/sync/plan.rs`:
```rust
pub enum SkipReason {
    AlreadyExists,
    RemoteNewer,
    ContentUnchanged,
    FilteredOut,                  // NEW
}
```

- [ ] **5.2** `src/config/mod.rs`: `FileItem` / `ScriptItem` / `BlockItem` 都加：
```rust
#[serde(default)]
pub tags: Vec<String>,
```

- [ ] **5.3** `PipelineOpts` 加：
```rust
pub filter: PipelineFilter,
```

```rust
#[derive(Debug, Clone, Default)]
pub struct PipelineFilter {
    pub only_stages: Option<HashSet<Stage>>,
    pub skip_stages: HashSet<Stage>,
    pub only_items: Option<HashSet<String>>,
    pub tags: Option<HashSet<String>>,
}
```

- [ ] **5.4** `Pipeline::plan()` 末尾把 plan 过滤：
```rust
fn apply_filter(plan: &mut Plan, filter: &PipelineFilter, item_tags: &HashMap<String, Vec<String>>) {
    for action in &mut plan.file_actions {
        if let FileAction::Apply { item_name, .. } = action {
            if !passes(filter, Stage::File, item_name, item_tags) {
                let name = item_name.clone();
                *action = FileAction::Skip { item_name: name, reason: SkipReason::FilteredOut };
            }
        }
        // also handle Skip/Failed pass-through (still subject to stage filter for visibility)
    }
    // same for script_actions / block_actions / register_pubkey
}
```

实现 `passes`：stage 在 only_stages 内/不在 skip_stages 内；item_name 在 only_items 内（如果 set）；tags 与 filter.tags 有交集（如果 set）。

- [ ] **5.5** CLI `--only-stage file,block` `--skip-stage script` `--only-item zsh_config,ssh_key` `--tag dotfiles` 。校验互斥 (`--only-stage` 与 `--skip-stage` 同时给 → 报错)。

- [ ] **5.6** Tests:
```rust
#[tokio::test]
async fn skip_stage_marks_all_as_filtered_out() { ... }
#[tokio::test]
async fn only_item_keeps_only_named() { ... }
#[tokio::test]
async fn tag_filter_intersects() { ... }
#[test]
fn cli_rejects_only_and_skip_stage_together() { ... }
```

### Task 6: `flux undo` subcommand

- [ ] **6.1** `main.rs` clap：
```rust
Undo {
    config: String,
    #[arg(long)] yes: bool,  // skip confirm
}
```

- [ ] **6.2** `cli::run_undo`：
  - 加载 config（验证 host/port）
  - 连 SSH
  - 对每个 dst（file + block 的 target），列出 `<dst>.flux-*.bak`
  - 取每个 dst 的最新一份 .bak（按 ts 数字）
  - 提示 "will restore N files from these backups: ..."；非 `--yes` 时 dialoguer 确认
  - rename(.bak, dst)（覆盖当前）；删除其它较老的 .bak？**保留**——给"再 undo 一次"留余地（虽然实际上不会有 chain，因为 backup 只在 sync 时新建）
  - 报告 summary

- [ ] **6.3** 不写复杂 cli 测试（涉及 SSH stub）；至少加一个解析 backup ts 的纯函数 + 测试。

### Task 7: audit append after each run

- [ ] **7.1** `cli::run_sync`：
```rust
let start = std::time::Instant::now();
let summary = pipeline.run().await;
let _ = crate::audit::append(&config.host, name_or_path, start.elapsed().as_millis(), &summary);
```

- [ ] **7.2** 在 `--dry-run` 下也写 audit（标 dry_run=true）——方便 debug "我刚做的 dry-run plan 是什么"。

### Task 8: Batch 1 收尾

- cargo fmt --all
- cargo clippy --all-targets -- -D warnings 全过
- cargo test --all-targets 全过（预期 +10 测试）
- cargo build --release

---

## Batch 2: E + A — env / imports / file model expansion

### Files

```
Cargo.toml                                 # +globset, walkdir, dotenvy
src/config/mod.rs                          # FileItem -> ItemKind enum + tags 已加
src/config/loader.rs                       # NEW: env interpolation + dotenv + imports + cycle check
src/config/secrets.rs                      # NEW: from_keychain resolver (macOS / Linux)
src/sync/plan.rs                           # FileAction extended for Dir / Glob / Link
src/sync/file.rs                           # plan_one_file dispatches by ItemKind
src/path.rs                                # AssetLocator + glob expand helper
docs/schema-migrations.md                  # bump v1 -> v2; how to migrate
```

### Task 9: env interpolation + dotenv

- [ ] **9.1** Add `dotenvy = "0.15"` to deps.

- [ ] **9.2** `src/config/loader.rs`:

```rust
use std::collections::HashMap;

#[derive(Debug, thiserror::Error)]
pub enum LoaderError {
    #[error("undefined variable: {0}")]
    UndefinedVar(String),
    #[error("io: {0}")]
    Io(String),
    #[error("import cycle detected: {0}")]
    Cycle(String),
}

pub fn load_yaml_with_env(path: &Path) -> Result<String, LoaderError> {
    // 1. read .flux/.env if present (relative to config file dir), populate process env via dotenvy
    if let Some(parent) = path.parent() {
        let _ = dotenvy::from_filename(parent.join(".env"));
    }
    let raw = std::fs::read_to_string(path).map_err(|e| LoaderError::Io(e.to_string()))?;
    interpolate(&raw)
}

pub fn interpolate(raw: &str) -> Result<String, LoaderError> {
    // implement ${VAR} and ${VAR:-default}; error on undefined-without-default
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '$' && chars.peek() == Some(&'{') {
            chars.next(); // consume {
            let mut name = String::new();
            let mut default = None;
            while let Some(&nc) = chars.peek() {
                if nc == '}' { chars.next(); break; }
                if nc == ':' {
                    chars.next();
                    if chars.peek() == Some(&'-') {
                        chars.next();
                        let mut d = String::new();
                        while let Some(&dc) = chars.peek() {
                            if dc == '}' { chars.next(); break; }
                            d.push(dc); chars.next();
                        }
                        default = Some(d);
                        break;
                    }
                }
                name.push(nc); chars.next();
            }
            match std::env::var(&name) {
                Ok(v) => out.push_str(&v),
                Err(_) => match default {
                    Some(d) => out.push_str(&d),
                    None => return Err(LoaderError::UndefinedVar(name)),
                },
            }
        } else {
            out.push(c);
        }
    }
    Ok(out)
}
```

- [ ] **9.3** `Config::find_and_load` 改用 `load_yaml_with_env`。

- [ ] **9.4** Tests for `interpolate`:
- 缺 var 无 default → UndefinedVar
- `${HOME:-/tmp}` 在没设 HOME 时回退 /tmp
- 嵌套 yaml 语义：插值不破坏 YAML 引号（注意：`password: "${PWD}"` 仍是字符串）

### Task 10: imports

- [ ] **10.1** Add to Config struct: `#[serde(default)] pub imports: Vec<String>`.

- [ ] **10.2** `loader.rs` 加：

```rust
pub fn load_with_imports(path: &Path) -> Result<String, LoaderError> {
    let mut visited = HashSet::new();
    load_recursive(path, &mut visited)
}

fn load_recursive(path: &Path, visited: &mut HashSet<PathBuf>) -> Result<String, LoaderError> {
    let canonical = path.canonicalize().map_err(|e| LoaderError::Io(e.to_string()))?;
    if !visited.insert(canonical.clone()) {
        return Err(LoaderError::Cycle(path.display().to_string()));
    }
    let yaml = load_yaml_with_env(path)?;
    // Probe imports list
    #[derive(Deserialize)]
    struct ImportsProbe { #[serde(default)] imports: Vec<String> }
    let probe: ImportsProbe = serde_yml::from_str(&yaml).map_err(|e| LoaderError::Io(e.to_string()))?;
    if probe.imports.is_empty() { return Ok(yaml); }
    // Merge: each import yaml loaded recursively, then current overlays
    let mut merged = serde_yml::Value::Mapping(Default::default());
    for imp in &probe.imports {
        let imp_path = if Path::new(imp).is_absolute() {
            PathBuf::from(imp)
        } else {
            path.parent().map(|p| p.join(imp)).unwrap_or_else(|| PathBuf::from(imp))
        };
        let imp_yaml = load_recursive(&imp_path, visited)?;
        let imp_val: serde_yml::Value = serde_yml::from_str(&imp_yaml).map_err(|e| LoaderError::Io(e.to_string()))?;
        deep_merge(&mut merged, imp_val);
    }
    let cur_val: serde_yml::Value = serde_yml::from_str(&yaml).map_err(|e| LoaderError::Io(e.to_string()))?;
    deep_merge(&mut merged, cur_val);
    serde_yml::to_string(&merged).map_err(|e| LoaderError::Io(e.to_string()))
}

fn deep_merge(target: &mut serde_yml::Value, source: serde_yml::Value) {
    match (target, source) {
        (serde_yml::Value::Mapping(t), serde_yml::Value::Mapping(s)) => {
            for (k, v) in s {
                if let Some(tv) = t.get_mut(&k) {
                    deep_merge(tv, v);
                } else {
                    t.insert(k, v);
                }
            }
        }
        (serde_yml::Value::Sequence(t), serde_yml::Value::Sequence(s)) => {
            t.extend(s);  // arrays concat
        }
        (slot, src) => *slot = src,  // scalars override
    }
}
```

- [ ] **10.3** Tests:
- `base.yml` defines `host: ...`; `override.yml` imports base, overrides `password`
- Cycle detection: `a.yml` imports `b.yml` imports `a.yml` → Cycle error
- Array concatenation (file: in base + override)

### Task 11: from_keychain resolver

- [ ] **11.1** Add field: `password: Option<SecretValue>` instead of `Option<String>`. Or keep String, parse at use:

```rust
pub enum SecretValue {
    Inline(String),
    FromKeychain(String),  // "service.account"
}

impl<'de> Deserialize<'de> for SecretValue {
    // tag the string: starts with "keychain:" -> FromKeychain
    // else Inline
}
```

User-facing yaml: `password: "keychain:flux-westlake.root"` or `password: "literal"`.

- [ ] **11.2** Resolver:

```rust
pub fn resolve_secret(value: &SecretValue) -> Result<String, ConfigError> {
    match value {
        SecretValue::Inline(s) => Ok(s.clone()),
        SecretValue::FromKeychain(spec) => {
            let (service, account) = spec.split_once('.').ok_or(ConfigError::BadKeychainSpec(spec.clone()))?;
            #[cfg(target_os = "macos")]
            return run_security_cli(service, account);
            #[cfg(not(target_os = "macos"))]
            return run_secret_tool(service, account);
        }
    }
}

#[cfg(target_os = "macos")]
fn run_security_cli(service: &str, account: &str) -> Result<String, ConfigError> {
    let out = std::process::Command::new("security")
        .args(["find-generic-password", "-s", service, "-a", account, "-w"])
        .output()
        .map_err(|e| ConfigError::KeychainCmdFailed(e.to_string()))?;
    if !out.status.success() {
        return Err(ConfigError::KeychainNotFound(format!("{service}.{account}")));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}
```

- [ ] **11.3** Tests: SecretValue::Inline round-trip; FromKeychain detection; spec parsing.

### Task 12: FileItem -> ItemKind enum + plan dispatch

- [ ] **12.1** `src/config/mod.rs` 改：

```rust
#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct FileItem {
    pub name: Option<String>,
    pub src: String,
    pub dst: String,
    #[serde(default)]
    pub mode: SyncMode,
    #[serde(default)]
    pub kind: ItemKind,
    pub chmod: Option<String>,
    pub target: Option<String>,    // only for Link
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Debug, Deserialize, Clone, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ItemKind {
    #[default]
    Auto,
    File,
    Dir,
    Glob,
    Link,
}
```

- [ ] **12.2** `auto_detect_kind(item: &FileItem) -> ItemKind`：
- `Link` 必显式
- 含 `*` / `?` / `[` → Glob
- 本地路径是目录 → Dir
- 否则 File

- [ ] **12.3** `src/sync/plan.rs` 加 actions：
```rust
pub enum FileAction {
    Skip { ... },
    Apply { item_name, src, dst, len, chmod },         // existing — single file
    ApplyDir { item_name, src_dir, dst_dir, files: Vec<(PathBuf, String)>, chmod },
    ApplyLink { item_name, dst, target },
    Failed { ... },
}
```

- [ ] **12.4** `plan_one_file` dispatch by kind:
- Auto → resolve to one of {File, Dir, Glob, Link}, recurse
- File → existing logic
- Glob → expand via `globset` walking; produce one FileAction::Apply per matched file (or one ApplyDir aggregating; pick **multi Apply per match** for visibility)
- Dir → walk via `walkdir`; produce one ApplyDir with `files: Vec<(local_path, dst_subpath)>`
- Link → ApplyLink

实际：把 Glob 也展开成多个 plan_one_file(file_kind=File) 调用（递归一层即可——用 helper）。这样 reporter 看到 N 个 file action。

- [ ] **12.5** `execute_file` 加分支：
- ApplyDir: ensure_dir target；逐个 file 写入（按文件大小串行还是并行？默认串行避免复杂；用一个内层 loop）
- ApplyLink: `remote.exec(format!("ln -sfn {} {}", target_quoted, dst_quoted))`

- [ ] **12.6** Schema migration: bump `CURRENT_SCHEMA_VERSION = 2`；老 yaml 无 `kind` 字段也合法（默认 Auto）。所以仍可读 v1 yaml，不需要 migrate 函数；只在 docs/schema-migrations.md 文档化。

但考虑到 `tags` 在 Batch 1 也加，可以把 v2 bump 推到 Batch 2 末尾。

- [ ] **12.7** Tests:
- glob `*.zsh` 在 fixture 目录里 → 产生 N 个 Apply
- dir → 产生 ApplyDir，files 数对
- link auto-detect 不行（必显式）；显式 link → ApplyLink
- execute ApplyLink 调用了 ln -sfn

### Task 13: Batch 2 收尾

- cargo fmt + clippy + test 全绿
- 确认现有 westlake.yml 不用改任何字段仍能加载（向后兼容）

---

## Batch 3: F — multi-host fan-out + state cache + resume

### Files

```
src/main.rs                                # --hosts, --no-cache, --resume, --max-hosts
src/cli/mod.rs                             # spawn_per_host orchestrator
src/cli/state.rs                           # NEW: load/save ~/.flux/state/<host>.json
src/sync/mod.rs                            # consume PipelineOpts.state_cache
src/reporter/console.rs                    # MultiHostConsoleReporter wraps ConsoleReporter
                                           # with prefix
```

### Task 14: state cache load/save

- [ ] **14.1** `src/cli/state.rs`:

```rust
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct HostState {
    pub host: String,
    pub last_sync_ts: i64,
    pub item_hashes: HashMap<String, String>,
    pub last_failed_item: Option<String>,
}

pub fn state_path(host: &str) -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".flux").join("state").join(format!("{host}.json")))
}

pub fn load(host: &str) -> Option<HostState> {
    let path = state_path(host)?;
    let raw = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&raw).ok()
}

pub fn save(state: &HostState) -> std::io::Result<()> {
    let Some(path) = state_path(&state.host) else { return Ok(()); };
    if let Some(parent) = path.parent() { std::fs::create_dir_all(parent)?; }
    let raw = serde_json::to_string_pretty(state).map_err(std::io::Error::other)?;
    std::fs::write(path, raw)
}
```

- [ ] **14.2** Tests: round-trip; missing file → None.

### Task 15: skip-by-cache in plan

- [ ] **15.1** `PipelineOpts` 加：
```rust
pub state: Option<HostState>,
pub use_cache: bool,        // default true; --no-cache flips to false
pub resume_from: Option<String>,  // --resume sets to last_failed_item
```

- [ ] **15.2** Pipeline plan post-process：
- 对每个 Apply action：算 hash(plan content)；与 state.item_hashes 中同名条目比较；相同 → 改为 Skip(ContentUnchanged)
- 这其实用 RemoteOps mtime 已经做了，但 state cache 是**无 RTT 优化**——直接 plan 时 skip，省 exists/mtime/read
- 实际更稳：state cache 仅作为 plan 的**辅助提示**，不替代 mtime 检查；但当 use_cache 且 cache 命中时直接进 Skip 分支，不再调 RemoteOps

- [ ] **15.3** execute 末尾：保存 state（item_hashes 覆盖；last_failed_item = 首个失败的 item name）

- [ ] **15.4** Tests with FakeRemote: 第一次 sync 写入 state；第二次相同输入直接 Skipped(ContentUnchanged)，远端 RemoteOps 不应被调用。

### Task 16: --resume

- [ ] **16.1** `state.last_failed_item` 在执行失败时记录。
- [ ] **16.2** `--resume` flag：从 state.last_failed_item 起执行；之前的 item 全标 Skipped(已成功过)。
- [ ] **16.3** Test: state 已有 last_failed_item，--resume 后只 plan/execute 该 item 之后的。

### Task 17: 多机 fan-out

- [ ] **17.1** `main.rs`：
```rust
Sync {
    config: String,
    #[arg(long)] save: Option<String>,
    #[arg(long)] dry_run: bool,
    #[arg(long, value_name = "N")] max_concurrency: Option<usize>,
    #[arg(long, value_name = "N")] retries: Option<u8>,
    #[arg(long, value_name = "SECS")] script_timeout: Option<u64>,
    #[arg(long)] diff: bool,
    #[arg(long, value_enum, default_value_t = LogFormat::Text)] log_format: LogFormat,
    #[arg(long, value_delimiter = ',')] only_stage: Vec<Stage>,
    #[arg(long, value_delimiter = ',')] skip_stage: Vec<Stage>,
    #[arg(long, value_delimiter = ',')] only_item: Vec<String>,
    #[arg(long, value_delimiter = ',')] tag: Vec<String>,
    #[arg(long, value_delimiter = ',')] hosts: Vec<String>,
    #[arg(long)] no_cache: bool,
    #[arg(long)] resume: bool,
    #[arg(long, default_value = "8")] max_hosts: usize,
}
```

- [ ] **17.2** `cli::run_sync`：
- 如果 `--hosts` 非空：override config.host；为每个 host 起一个 Pipeline async future；用 `futures::stream::iter().buffer_unordered(max_hosts).collect()`
- 每个 host 用 `MultiHostConsoleReporter::new(prefix=host)`
- 收集 PipelineSummary per host，最后合并打印 grand total
- audit 每个 host 各 append 一行

- [ ] **17.3** `MultiHostConsoleReporter`: wraps `ConsoleReporter`；每条 message 前缀 `[host] `。

- [ ] **17.4** Tests：用 FakeRemote 多实例模拟 3 机；assert summary count 正确。

### Task 18: Batch 3 收尾

- cargo fmt + clippy + test 全绿
- cargo build --release
- 手动 sanity: `flux sync --help` 显示所有新 flag

---

## Final acceptance

1. `cargo fmt --all -- --check` 干净
2. `cargo clippy --all-targets -- -D warnings` 0 warnings
3. `cargo test --all-targets` ≥ 80 tests pass
4. `flux sync` 帮助显示所有 Phase 4 flag
5. `westlake.yml` 不改任何字段仍能加载（schema 向后兼容）
6. `~/.flux/audit.jsonl` 在 release sanity 中至少 append 一行
7. `flux undo --help` / `flux sync --diff --dry-run` 都展示且不 panic
