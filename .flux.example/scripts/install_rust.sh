#!/usr/bin/env bash
# 安装 rustup 并配置国内镜像源（中科大 USTC）

set -e

alias sudo=""

CARGO_HOME="${CARGO_HOME:-$HOME/.cargo}"

# ========== 检测是否已安装 ==========
if [[ -x "$CARGO_HOME/bin/rustup" ]]; then
  echo "=== rustup 已安装: $("$CARGO_HOME/bin/rustup" --version 2>/dev/null)，跳过 ==="
  echo "  rustc: $("$CARGO_HOME/bin/rustc" --version 2>/dev/null || echo '未安装')"
  echo "  cargo: $("$CARGO_HOME/bin/cargo" --version 2>/dev/null || echo '未安装')"
else
  # ========== 配置 rustup 国内镜像 ==========
  export RUSTUP_DIST_SERVER=https://mirrors.ustc.edu.cn/rust-static
  export RUSTUP_UPDATE_ROOT=https://mirrors.ustc.edu.cn/rust-static/rustup

  echo "=== 安装 rustup (使用中科大镜像) ==="
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --no-modify-path

  echo "=== rustup 安装完成 ==="
fi

# 加载 cargo 环境
if [[ -f "$CARGO_HOME/env" ]]; then
  # shellcheck source=/dev/null
  . "$CARGO_HOME/env"
fi

# ========== 配置 crates.io 国内镜像 ==========
CARGO_CONFIG="$CARGO_HOME/config.toml"
if [[ -f "$CARGO_CONFIG" ]] && grep -q 'ustc' "$CARGO_CONFIG" 2>/dev/null; then
  echo "=== crates.io 镜像已配置，跳过 ==="
else
  echo "=== 配置 crates.io 中科大镜像 ==="
  mkdir -p "$CARGO_HOME"
  cat > "$CARGO_CONFIG" << 'EOF'
[source.crates-io]
replace-with = "ustc"

[source.ustc]
registry = "sparse+https://mirrors.ustc.edu.cn/crates.io-index/"
EOF
  echo "=== crates.io 镜像配置完成 ==="
fi

# ========== 写入环境变量到 ~/.zshrc ==========
ZSHRC="${HOME}/.zshrc"
if [[ -f "$ZSHRC" ]] && grep -q 'RUSTUP_DIST_SERVER' "$ZSHRC" 2>/dev/null; then
  echo "=== rustup 环境变量已存在于 ~/.zshrc，跳过 ==="
else
  cat >> "$ZSHRC" << 'EOF'

# Rust 镜像��置 (中科大 USTC)
export RUSTUP_DIST_SERVER=https://mirrors.ustc.edu.cn/rust-static
export RUSTUP_UPDATE_ROOT=https://mirrors.ustc.edu.cn/rust-static/rustup
. "$HOME/.cargo/env"
EOF
  echo "=== rustup 环境变量已写入 ~/.zshrc ==="
fi

# ========== 安装常用组件 ==========
echo "=== 安装 rust 常用组件 ==="

# rust-analyzer（LSP）
if rustup component list --installed 2>/dev/null | grep -q 'rust-analyzer'; then
  echo "  rust-analyzer 已安装，跳过"
else
  rustup component add rust-analyzer
fi

# clippy（lint）
if rustup component list --installed 2>/dev/null | grep -q 'clippy'; then
  echo "  clippy 已安装，跳过"
else
  rustup component add clippy
fi

# rustfmt（格式化）
if rustup component list --installed 2>/dev/null | grep -q 'rustfmt'; then
  echo "  rustfmt 已安装，跳过"
else
  rustup component add rustfmt
fi

echo "=== 验证安装 ==="
rustup --version
rustc --version
cargo --version
echo "=== Rust 工具链安装完成 ==="