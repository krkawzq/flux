#!/usr/bin/env bash
# NVM + Node.js + NPM 国内镜像安装

set -e

alias sudo=""

export NVM_DIR="${NVM_DIR:-$HOME/.nvm}"

# 检测 node 是否已安装
if command -v node &>/dev/null; then
  echo "=== Node.js 已安装: $(node -v)，跳过 ==="
  exit 0
fi

# 检测 NVM 是否已安装
if [[ ! -s "$NVM_DIR/nvm.sh" ]]; then
  echo "=== 安装 NVM (使用国内镜像) ==="
  export NVM_SOURCE=https://gitee.com/mirrors/nvm.git
  curl -fsSL -o- https://gitee.com/mirrors/nvm/raw/master/install.sh | bash
fi

# 加载 NVM
[[ -s "$NVM_DIR/nvm.sh" ]] && \. "$NVM_DIR/nvm.sh"

# 安装 Node.js LTS
echo "=== 安装 Node.js LTS 版本 ==="
export NVM_NODEJS_ORG_MIRROR=https://npmmirror.com/mirrors/node/
nvm install --lts

# 配置 npm 淘宝镜像
echo "=== 配置 npm 淘宝镜像 ==="
npm config set registry https://registry.npmmirror.com

# 安装 pnpm
if command -v pnpm &>/dev/null; then
  echo "=== pnpm 已安装: $(pnpm -v)，跳过 ==="
else
  echo "=== 安装 pnpm ==="
  npm install -g pnpm
  pnpm setup
  export PNPM_HOME="${HOME}/.local/share/pnpm"
  export PATH="$PNPM_HOME:$PATH"
  pnpm config set registry https://registry.npmmirror.com
fi

# 追加 NVM 配置到 ~/.zshrc（如果还没有）
ZSHRC="${HOME}/.zshrc"
if [[ -f "$ZSHRC" ]] && grep -q 'NVM_DIR' "$ZSHRC" 2>/dev/null; then
  echo "NVM 配置已存在于 ~/.zshrc，跳过追加"
else
  cat >> "$ZSHRC" << 'EOF'

# NVM 配置
export NVM_DIR="$HOME/.nvm"
[ -s "$NVM_DIR/nvm.sh" ] && \. "$NVM_DIR/nvm.sh"
[ -s "$NVM_DIR/bash_completion" ] && \. "$NVM_DIR/bash_completion"
export NVM_NODEJS_ORG_MIRROR=https://npmmirror.com/mirrors/node/
EOF
fi

echo "=== 验证安装 ==="
node -v
npm -v
echo "npm 镜像源: $(npm config get registry)"
echo "=== 安装完成 ==="