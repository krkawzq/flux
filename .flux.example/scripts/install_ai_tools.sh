#!/usr/bin/env bash

# 安装 AI 编程工具：Claude Code, OpenCode, Codex CLI 及 Claude 插件
# 配置文件由 westlake.yml file sync 管理，此脚本只负责安装
#
# 要求：
# 1. 不安装 Kimi
# 2. 全部使用 npm 安装，不使用 curl 官方安装脚本
# 3. 使用 HTTP 7890 端口代理
# 4. 优先使用 npm 官方 registry，失败后回退到淘宝镜像
# 5. 不使用 function，线性平铺

set -e

# 容器/root 环境下常见：直接禁用 sudo
alias sudo=""

# ========== 基础检查 ==========

echo "=== 开始安装 AI 编程工具 ==="

if ! command -v npm &>/dev/null; then
  echo "错误：未检测到 npm，无法安装 npm 包"
  exit 1
fi

# ========== 代理配置 ==========

echo "=== 配置 npm 代理：HTTP 7890 ==="

export HTTP_PROXY="http://127.0.0.1:7890"
export HTTPS_PROXY="http://127.0.0.1:7890"
export http_proxy="http://127.0.0.1:7890"
export https_proxy="http://127.0.0.1:7890"

npm config set proxy "http://127.0.0.1:7890"
npm config set https-proxy "http://127.0.0.1:7890"

# 优先官方 registry
npm config set registry "https://registry.npmjs.org/"

# ========== npm 安装辅助逻辑：官方失败后回退淘宝镜像 ==========
# 由于不使用 function，这里每个包单独展开。

# ---------- Claude Code ----------

if command -v claude &>/dev/null; then
  echo "=== Claude Code 已安装，跳过 ==="
else
  echo "=== 安装 Claude Code：@anthropic-ai/claude-code@latest ==="

  if npm install -g @anthropic-ai/claude-code@latest; then
    echo "=== Claude Code 安装完成：官方 registry ==="
  else
    echo "=== Claude Code 官方 registry 安装失败，回退淘宝镜像 ==="
    npm config set registry "https://registry.npmmirror.com"
    npm install -g @anthropic-ai/claude-code@latest
    npm config set registry "https://registry.npmjs.org/"
    echo "=== Claude Code 安装完成：淘宝镜像 ==="
  fi
fi

# ---------- OpenCode ----------

if command -v opencode &>/dev/null; then
  echo "=== OpenCode 已安装，跳过 ==="
else
  echo "=== 安装 OpenCode：opencode-ai@latest ==="

  if npm install -g opencode-ai@latest; then
    echo "=== OpenCode 安装完成：官方 registry ==="
  else
    echo "=== OpenCode 官方 registry 安装失败，回退淘宝镜像 ==="
    npm config set registry "https://registry.npmmirror.com"
    npm install -g opencode-ai@latest
    npm config set registry "https://registry.npmjs.org/"
    echo "=== OpenCode 安装完成：淘宝镜像 ==="
  fi

  mkdir -p ~/.config/opencode
  mkdir -p ~/.local/share/opencode
fi

# ---------- Codex CLI ----------

if command -v codex &>/dev/null; then
  echo "=== Codex CLI 已安装，跳过 ==="
else
  echo "=== 安装 Codex CLI：@openai/codex@latest ==="

  if npm install -g @openai/codex@latest; then
    echo "=== Codex CLI 安装完成：官方 registry ==="
  else
    echo "=== Codex CLI 官方 registry 安装失败，回退淘宝镜像 ==="
    npm config set registry "https://registry.npmmirror.com"
    npm install -g @openai/codex@latest
    npm config set registry "https://registry.npmjs.org/"
    echo "=== Codex CLI 安装完成：淘宝镜像 ==="
  fi

  mkdir -p ~/.codex
fi

# ========== 其他 npm CLI 工具 ==========

echo "=== 安装 openspec：@fission-ai/openspec@latest ==="

if npm install -g @fission-ai/openspec@latest; then
  echo "=== openspec 安装完成：官方 registry ==="
else
  echo "=== openspec 官方 registry 安装失败，回退淘宝镜像 ==="
  npm config set registry "https://registry.npmmirror.com"
  npm install -g @fission-ai/openspec@latest
  npm config set registry "https://registry.npmjs.org/"
  echo "=== openspec 安装完成：淘宝镜像 ==="
fi

echo "=== 安装 UIPro CLI：uipro-cli@latest ==="

if npm install -g uipro-cli@latest; then
  echo "=== UIPro CLI 安装完成：官方 registry ==="
else
  echo "=== UIPro CLI 官方 registry 安装失败，回退淘宝镜像 ==="
  npm config set registry "https://registry.npmmirror.com"
  npm install -g uipro-cli@latest
  npm config set registry "https://registry.npmjs.org/"
  echo "=== UIPro CLI 安装完成：淘宝镜像 ==="
fi

echo "=== 安装 ccusage：ccusage@latest ==="

if npm install -g ccusage@latest; then
  echo "=== ccusage 安装完成：官方 registry ==="
else
  echo "=== ccusage 官方 registry 安装失败，回退淘宝镜像 ==="
  npm config set registry "https://registry.npmmirror.com"
  npm install -g ccusage@latest
  npm config set registry "https://registry.npmjs.org/"
  echo "=== ccusage 安装完成：淘宝镜像 ==="
fi

# ========== Codex 二进制冲突处理 ==========

# 你原脚本里有 rm -f /usr/local/bin/codex。
# 保留这个逻辑，但放在 Codex 安装之后。
# 注意：如果 npm 全局 bin 正好就是 /usr/local/bin/codex，这一步会删除刚装好的 codex。
# 如果你确认需要清理旧 symlink，可以保留；否则建议注释掉。

rm -f /usr/local/bin/codex

# ========== Claude 插件安装 ==========

if ! command -v claude &>/dev/null; then
  echo "警告：未检测到 claude 命令，跳过 Claude 插件安装"
else
  echo "=== 安装 Claude 插件 ==="

  claude plugin marketplace add jarrodwatts/claude-hud || true

  claude plugin install claude-hud || true
  claude plugin install pyright-lsp || true
  claude plugin install clangd-lsp || true
  claude plugin install rust-analyzer-lsp || true
  claude plugin install code-review || true
  claude plugin install github || true
  claude plugin install context7 || true
  claude plugin install frontend-design || true
  claude plugin install skill-creator || true
  claude plugin install huggingface-skills || true
  claude plugin install superpowers || true
  claude plugin install code-simplifier || true
  claude plugin install chrome-devtools-mcp || true
  claude plugin install feature-dev || true
  claude plugin install typescript-lsp || true

  claude plugin marketplace add openai/codex-plugin-cc || true
  claude plugin install codex@openai-codex || true

  claude plugin marketplace add krkawzq/cc-codex-team || true
  claude plugin install codex-team || true

  echo "=== Claude 插件安装完成 ==="
f嗯i

# ========== 恢复 npm registry 到官方源 ==========

npm config set registry "https://registry.npmjs.org/"

echo "=== AI 编程工具安装完成 ==="
