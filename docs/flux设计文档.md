# flux 设计文档

flux 是一个 SSH 工具，用于管理远程临时环境的个性化配置同步。

## 核心概念

### 路径约定

- `:` 前缀表示**远程路径**
- 无前缀表示**本地路径**

示例：
```
:~/.zshrc       → 远程家目录下的 .zshrc
:/etc/hosts     → 远程 /etc/hosts
~/.bashrc       → 本地家目录下的 .bashrc
/etc/hosts      → 本地 /etc/hosts
```

### 相对路径解析

- **本地相对路径**: 相对于 flux 命令执行时的工作目录
- **远程相对路径**: 相对于远程用户家目录 `~`

> **建议**: 尽量使用绝对路径或 `~` 开头的路径，避免相对路径带来的歧义。

### 三种同步类型

1. **file** - 文件同步
2. **script** - 脚本执行（远程）
3. **block** - 配置块注入

### 执行顺序

```
file → script → block
```

### Pipeline 设计

flux 是一个 **Pipeline**，按顺序执行所有配置项：

- **不会因为某个配置失败而停止**（除了 proxy 失败）
- 每个操作独立执行，失败只影响依赖它的后续操作
- 状态无关设计，总是执行完所有配置

---

## file 同步

将文件从 src 复制到 dst，支持任意本地/远程组合。

**注意**: 不支持目录同步，只能逐个文件同步。

### 字段

| 字段 | 必需 | 说明 |
|------|------|------|
| name | 否 | 标识符，供 script 依赖 |
| src | 是 | 源路径 |
| dst | 是 | 目标路径 |
| mode | 否 | 同步模式，默认 `sync` |
| chmod | 否 | 同步后设置权限（如 `"755"`） |

### mode 值

- `cover` - 总是覆盖
- `sync` - 根据时间戳判断（dst 更新则跳过）
- `touch` - 仅当 dst 不存在时复制

### chmod 字段

设置同步后的文件权限，使用 Unix 权限格式（如 `"644"`、`"755"`）。

- **Linux/macOS**: 直接应用
- **Windows**: 映射为只读/可写属性
  - `x` 位无效果
  - `w` 位控制只读属性

### 失败处理

file 操作允许失败并标记状态（如权限不足）。依赖该 file 的 script 将跳过执行。

---

## script 执行

在远程服务器执行脚本。每次 sync 都会执行（除非依赖的 file 失败）。

### 字段

| 字段 | 必需 | 说明 |
|------|------|------|
| path | 是 | 脚本路径（本地或 `:` 前缀的远程） |
| interpreter | 否 | 解释器，默认使用全局配置 |
| flags | 否 | 解释器标志，默认使用全局配置 |
| args | 否 | 脚本参数 |
| dependencies | 否 | 依赖的 file name 列表 |

### 执行逻辑

- **本地路径** → 上传到远程 `/tmp` 后执行
- **远程路径** (`:` 前缀) → 直接在远程执行，找不到则失败

### I/O 转发

自动转发 stdin/stdout/stderr，支持交互式行为（但建议不要写成交互式）。

### 失败判定

脚本返回非零退出码时视为 **FAILED**。

---

## block 同步

将配置块注入到远程文件中，使用哨兵注释标记 block 位置。

### 字段

| 字段 | 必需 | 说明 |
|------|------|------|
| name | 是 | block 名称，用于生成哨兵标记 |
| path | 是 | block 内容来源（本地文件） |
| file | 是 | 目标文件（远程，`:` 前缀） |
| mode | 否 | 同步模式，默认 `sync` |
| comment_template | 否 | 注释模板，默认使用全局配置 |

### mode 值

- `cover` - 总是替换
- `sync` - 根据时间戳判断（时间戳解析失败则视为需要更新）
- `touch` - 仅当 block 不存在时插入

### 哨兵格式

用户通过 `comment_template` 指定注释格式，如 `"# {}"`。

`{}` 会被替换为 `blockname:timestamp`，生成头尾哨兵：

```bash
# >>> myblock:1736789012 >>>
export PATH="/usr/local/bin:$PATH"
# <<< myblock:1736789012 <<<
```

sync 时通过头尾哨兵定位 block 范围，计算 hash 判断内容是否被修改。

### 失败情况

- 目标文件不存在 → 跳过

---

## 全局配置

| 字段 | 说明 | 默认值 |
|------|------|--------|
| interpreter | 默认脚本解释器 | `/bin/bash` (Linux) / `cmd` (Windows) |
| flags | 默认解释器标志 | `["-i"]` (交互式) |
| comment_template | 默认注释模板 | `"# {}"` |
| flux_home | .flux 目录位置 | 自动查找 |

---

## Proxy 配置

proxy 用于在 sync 过程中建立反向代理，供远程脚本使用网络。

### 字段

| 字段 | 说明 | 默认值 |
|------|------|--------|
| proxy.enabled | 是否启用代理 | `false` |
| proxy.local_port | 本地代理端口（clash/v2ray 等） | `7890` |
| proxy.remote_port | 远程监听端口 | `1081` |
| proxy.protocol | 代理协议 | `socks` |

### protocol 值

- `socks` - SOCKS5 代理
- `http` - HTTP 代理

### 配置示例

```yaml
proxy:
  enabled: true
  local_port: 7890
  remote_port: 1081
  protocol: socks
```

### 两阶段执行

当启用 proxy 时，`flux sync` 分为两个阶段：

**阶段 1：建立连接和代理**
1. 建立 SSH 连接
2. 验证密码/密钥
3. 注册公钥（如果配置了 register_key）
4. 建立反向代理隧道
5. **确认代理成功**

**阶段 2：执行 sync**
1. 执行 file 同步
2. 执行 script
3. 执行 block 同步

### Proxy 失败处理

**Proxy 是唯一会导致 flux 立即退出的失败情况。**

原因：用户配置 proxy 说明后续 script 需要网络代理，proxy 失败意味着 script 无法正常工作，应该及时停止而不是继续执行。

### 环境变量

flux **不会**自动设置 `HTTP_PROXY` 等环境变量。

用户需要在自己的脚本中手动设置：

```bash
export HTTP_PROXY=socks5://127.0.0.1:1081
export HTTPS_PROXY=socks5://127.0.0.1:1081
export ALL_PROXY=socks5://127.0.0.1:1081
```

---

## SSH 配置

| 字段 | 说明 | 缺省行为 |
|------|------|----------|
| host | SSH 主机地址 | 交互式输入 |
| port | SSH 端口 | 交互式输入，默认 22 |
| user | SSH 用户名 | 交互式输入，默认 root |
| key | 私钥文件路径 | 不交互，可选 |
| password | 密码 | 交互式输入 |
| register_key | 是否注册公钥到 authorized_keys | `false` |

### 验证顺序

1. 尝试 key 验证（如果提供了 key）
2. key 失败或未提供 → 尝试 password
3. password 未提供 → 交互式输入

### 交互式输入

缺省时按顺序提示：`host → port → user → password`

注：key 不会交互式输入。

---

## 配置文件

### 格式

使用 **YAML** 格式，扩展名 `.yml` 或 `.yaml` 均可。

### 位置与查找

配置文件可以是：
- `.flux/` 目录下的 YAML 文件
- 任意路径的 YAML 文件

```bash
flux sync westlake           # 查找 .flux/westlake.yml 或 ~/.flux/westlake.yml
flux sync ./config/dev.yaml  # 直接使用指定路径
```

查找顺序：
1. 判断参数是否为文件路径（存在则直接使用）
2. 查找 `./.flux/<name>.yml` 或 `./.flux/<name>.yaml`
3. 查找 `~/.flux/<name>.yml` 或 `~/.flux/<name>.yaml`

### 示例配置

```yaml
# westlake.yml

# SSH 连接配置
host: 192.168.1.100
port: 22
user: root
key: ~/.ssh/id_rsa
register_key: true

# 全局设置
interpreter: /bin/bash
flags: ["-i"]
comment_template: "# {}"

# 代理配置
proxy:
  enabled: true
  local_port: 7890
  remote_port: 1081
  protocol: socks

# 文件同步
file:
  - name: ssh_key
    src: ~/.ssh/id_rsa
    dst: :~/.ssh/id_rsa
    mode: touch
    chmod: "600"

  - src: ~/.tmux.conf
    dst: :~/.tmux.conf
    mode: sync

  - src: ~/scripts/deploy.sh
    dst: :~/bin/deploy.sh
    mode: cover
    chmod: "755"

# 脚本执行
script:
  - path: scripts/init.sh
    dependencies: [ssh_key]

  - path: :/usr/local/bin/setup.sh
    args: ["--force"]

# 块同步
block:
  - name: zsh_env
    path: blocks/zsh_env.sh
    file: :~/.zshrc
    mode: sync

  - name: vimrc_plugins
    path: blocks/vimrc.vim
    file: :~/.vimrc
    comment_template: "\" {}"
```

---

## CLI 命令

### flux init

创建 `.flux/` 目录结构：

```
.flux/
├── scripts/
├── blocks/
└── files/
```

不创建任何配置文件。

### flux sync \<config\>

执行同步操作：

```bash
flux sync westlake           # 使用 .flux/westlake.yml
flux sync ./custom/dev.yml   # 使用指定路径
```

### flux proxy

独立的代理功能，用于创建 SSH 反向代理。

与 sync 配置中的 `proxy` 选项是不同的功能：
- `flux proxy` - 独立命令，仅建立代理隧道
- sync 的 `proxy.enabled: true` - 在 sync 过程中启用代理

（两者可复用底层代理实现代码，但业务逻辑独立）

---

## 输出显示

flux 不使用传统日志格式，而是使用**彩色终端输出**显示执行状态。

### 状态标记

每个操作都会显示执行结果：

| 状态 | 颜色 | 说明 |
|------|------|------|
| ✓ SUCCESS | 绿色 | 操作成功完成 |
| ✗ FAILED | 红色 | 操作失败 |
| ○ SKIP | 黄色 | 操作跳过 |

### Skip 触发条件

- **file**: `touch` 模式下目标文件已存在
- **file**: `sync` 模式下目标文件更新
- **script**: 依赖的 file 失败
- **block**: `touch` 模式下 block 已存在
- **block**: 目标文件不存在

### 输出示例

```
[flux] Connecting to 192.168.1.100...
[flux] ✓ SSH connection established
[flux] ✓ Proxy tunnel established (local:7890 → remote:1081)

[file] ~/.ssh/id_rsa → :~/.ssh/id_rsa
       ○ SKIP (file exists, mode: touch)

[file] .tmux.conf → :~/.tmux.conf
       ✓ SUCCESS

[script] scripts/init.sh
         ✓ SUCCESS

[script] :/usr/local/bin/setup.sh
         ✗ FAILED (file not found)

[block] blocks/zsh_env.sh → :~/.zshrc
        ✓ SUCCESS

[flux] Sync completed: 3 success, 1 failed, 1 skipped
```

### 实现建议

使用 Rust 的成熟终端库：
- `colored` - 简单的彩色输出
- `console` - 终端样式和交互
- `indicatif` - 进度条和 spinner

---

## 依赖关系

```
file (允许失败，标记状态)
  ↓
script (检查依赖的 file 是否成功，失败则跳过)
  ↓
block (无依赖，独立执行)
```

---

## 设计原则

1. **Pipeline 模式** - 按顺序执行所有配置，不因单个失败而中断
2. **状态无关** - 不依赖任何状态文件，只使用时间戳和依赖管理
3. **幂等性** - 多次运行 sync 结果等效，无需 dry-run 模式
4. **允许失败** - file/script/block 的失败不会中断 sync 流程
5. **Proxy 优先** - proxy 失败是唯一会导致立即退出的情况
6. **最小交互** - 仅在必要时（缺少关键配置）才要求交互式输入
7. **清晰反馈** - 使用彩色输出清晰显示每个操作的执行状态
