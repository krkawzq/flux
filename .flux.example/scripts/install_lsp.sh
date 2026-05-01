#!/usr/bin/env bash
# 安装常用 Language Server：pyright, clangd, bash-language-server, lua-language-server 等

set -e

alias sudo=""

LSP_BIN_DIR="$HOME/.local/bin"
mkdir -p "$LSP_BIN_DIR"
export PATH="$LSP_BIN_DIR:$PATH"

# --- 通过 npm 安装 (需先有 Node/npm) ---
install_npm_lsps() {
  if ! command -v npm &>/dev/null; then
    echo "跳过 npm 类 LSP：未检测到 npm，请先安装 Node.js"
    return 0
  fi

  echo "=== 通过 npm 安装 Language Servers ==="

  # pyright (Python)
  if command -v pyright &>/dev/null; then
    echo "  pyright 已安装，跳过"
  else
    npm install -g pyright
  fi

  # bash-language-server
  if command -v bash-language-server &>/dev/null; then
    echo "  bash-language-server 已安装，跳过"
  else
    npm install -g bash-language-server
  fi

  # typescript-language-server + typescript
  if command -v typescript-language-server &>/dev/null; then
    echo "  typescript-language-server 已安装，跳过"
  else
    npm install -g typescript-language-server typescript
  fi

  # vscode-langservers-extracted (HTML/CSS/JSON)
  if npm list -g vscode-langservers-extracted &>/dev/null; then
    echo "  vscode-langservers-extracted 已安装，跳过"
  else
    npm install -g vscode-langservers-extracted
  fi

  echo "npm 类 LSP 安装完成"
}

# --- 安装 clangd (C/C++) ---
install_clangd() {
  if command -v clangd &>/dev/null; then
    echo "clangd 已安装: $(clangd --version 2>/dev/null | head -1)，跳过"
    return 0
  fi

  echo "=== 安装 clangd ==="
  if [[ "$OSTYPE" == "linux-gnu"* ]]; then
    if command -v apt-get &>/dev/null; then
      apt-get update
      apt-get install -y clangd
    elif command -v dnf &>/dev/null; then
      dnf install -y clang-tools-extra
    else
      echo "请手动安装 clangd: https://clangd.llvm.org/installation"
      return 1
    fi
  elif [[ "$OSTYPE" == "darwin"* ]]; then
    if command -v brew &>/dev/null; then
      brew install llvm
    else
      echo "请安装 Homebrew 后执行: brew install llvm"
      return 1
    fi
  else
    echo "当前系统未配置自动安装 clangd"
    return 1
  fi
  echo "clangd 安装完成"
}

# --- 安装 lua-language-server ---
install_lua_lsp() {
  local version="3.6.5"
  local dir="$HOME/.local/share/lua-language-server"
  local bin="$LSP_BIN_DIR/lua-language-server"

  if [[ -x "$dir/bin/lua-language-server" ]]; then
    echo "lua-language-server 已安装，跳过"
    return 0
  fi

  echo "=== 安装 lua-language-server ==="
  local arch
  case "$OSTYPE" in
    linux-gnu*)
      case "$(uname -m)" in
        x86_64) arch="linux-x64" ;;
        aarch64|arm64) arch="linux-arm64" ;;
        *) echo "不支持的架构: $(uname -m)"; return 1 ;;
      esac
      ;;
    darwin*)
      case "$(uname -m)" in
        x86_64) arch="darwin-x64" ;;
        arm64) arch="darwin-arm64" ;;
        *) echo "不支持的架构: $(uname -m)"; return 1 ;;
      esac
      ;;
    *)
      echo "请从 https://github.com/LuaLS/lua-language-server/releases 手动下载"
      return 1
      ;;
  esac

  local url="https://github.com/LuaLS/lua-language-server/releases/download/${version}/lua-language-server-${version}-${arch}.tar.gz"
  mkdir -p "$dir"
  curl -fsSL "$url" | tar -xzf - -C "$dir" --strip-components=1
  ln -sf "$dir/bin/lua-language-server" "$bin"
  echo "lua-language-server 安装完成"
}

# --- 主流程 ---
echo "=== 开始安装 Language Servers ==="
install_npm_lsps
install_clangd
install_lua_lsp
# 注: rust-analyzer 已由 install_rust.sh 通过 rustup component 安装
echo "=== Language Servers 安装完成 ==="
