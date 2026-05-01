#!/usr/bin/env bash
# 从官方 release 安装 Neovim 并克隆配置

set -e

alias sudo=""

NVIM_INSTALL_DIR="/opt"
NVIM_CONFIG_REPO="git@github.com:krkawzq/nvim-config.git"

# ========== 检测 nvim 是否已安装 ==========
if command -v nvim &>/dev/null; then
  echo "=== Neovim 已安装: $(nvim --version | head -1)，跳过安装 ==="
else
  # 检测平台
  case "$(uname -s)" in
    Linux)
      case "$(uname -m)" in
        x86_64) PLATFORM="linux-x86_64" ;;
        aarch64|arm64) PLATFORM="linux-aarch64" ;;
        *) echo "不支持的架构: $(uname -m)" >&2; exit 1 ;;
      esac
      ;;
    Darwin)
      case "$(uname -m)" in
        x86_64) PLATFORM="macos-x86_64" ;;
        arm64) PLATFORM="macos-arm64" ;;
        *) echo "不支持的架构: $(uname -m)" >&2; exit 1 ;;
      esac
      ;;
    *)
      echo "不支持的系统: $(uname -s)" >&2
      exit 1
      ;;
  esac

  TARBALL="nvim-${PLATFORM}.tar.gz"
  DOWNLOAD_URL="https://github.com/neovim/neovim/releases/latest/download/$TARBALL"

  echo "=== 下载 Neovim: $DOWNLOAD_URL ==="
  curl -fsSL -o "$TARBALL" "$DOWNLOAD_URL"

  NVIM_DIR_NAME="nvim-${PLATFORM}"
  echo "=== 安装到 $NVIM_INSTALL_DIR ==="
  rm -rf "$NVIM_INSTALL_DIR/$NVIM_DIR_NAME"
  tar -C "$NVIM_INSTALL_DIR" -xzf "$TARBALL"
  rm -f "$TARBALL"

  NVIM_BIN="$NVIM_INSTALL_DIR/$NVIM_DIR_NAME/bin"
  # 写入 PATH 到 ~/.zshrc
  ZSHRC="${HOME}/.zshrc"
  if [[ -f "$ZSHRC" ]] && grep -q "$NVIM_DIR_NAME" "$ZSHRC" 2>/dev/null; then
    echo "PATH 中已包含 nvim，跳过"
  else
    echo "export PATH=\"$NVIM_BIN:\$PATH\"" >> "$ZSHRC"
  fi
  export PATH="$NVIM_BIN:$PATH"

  echo "=== Neovim 安装完成 ==="
  nvim --version | head -1
fi

# ========== 克隆 nvim 配置 ==========
mkdir -p "${HOME}/.config"
if [[ -d "${HOME}/.config/nvim/.git" ]]; then
  echo "=== nvim 配置已存在，跳过克隆 ==="
else
  echo "=== 克隆 nvim 配置 ==="
  git clone "$NVIM_CONFIG_REPO" "${HOME}/.config/nvim"
  echo "=== 配置克隆完成 ==="
fi
