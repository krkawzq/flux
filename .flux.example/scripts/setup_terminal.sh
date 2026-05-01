#!/usr/bin/env bash
# 终端环境一站式配置：
#   1) 安装 xterm-ghostty terminfo（供 ssh/tmux 等正确识别 Ghostty）
#   2) 从源码编译安装 tmux（合并自旧的 install_tmux.sh）
#   3) 通过官方 release 安装 yazi
#   4) 通过官方 release 安装 zellij

set -e

alias sudo=""

# ============================================================
# 通用工具
# ============================================================
detect_arch() {
  case "$(uname -m)" in
    x86_64|amd64) echo "x86_64" ;;
    aarch64|arm64) echo "aarch64" ;;
    *) echo "unsupported" ;;
  esac
}

# ============================================================
# 1. 安装 xterm-ghostty terminfo
# ============================================================
TERMINFO_SRC="${TERMINFO_SRC:-$HOME/.flux-terminal/xterm-ghostty.terminfo}"

install_ghostty_terminfo() {
  echo "=== [1/4] 安装 xterm-ghostty terminfo ==="

  if ! command -v tic &>/dev/null; then
    echo "  -> 未检测到 tic，安装 ncurses 工具"
    if command -v apt-get &>/dev/null; then
      apt-get install -y ncurses-bin >/dev/null
    elif command -v dnf &>/dev/null; then
      dnf install -y ncurses >/dev/null
    fi
  fi

  if [[ ! -f "$TERMINFO_SRC" ]]; then
    echo "  -> 未找到 terminfo 源文件: $TERMINFO_SRC，跳过"
    return 0
  fi

  if infocmp -x xterm-ghostty &>/dev/null; then
    echo "  -> xterm-ghostty terminfo 已存在，跳过"
    return 0
  fi

  tic -x -o "$HOME/.terminfo" "$TERMINFO_SRC"
  echo "  -> 已安装到 $HOME/.terminfo"
  infocmp -x xterm-ghostty | head -1
}

# ============================================================
# 2. 从源码安装 tmux（合并自 install_tmux.sh）
# ============================================================
TMUX_SRC="${TMUX_SRC:-$HOME/.local/src/tmux}"
TMUX_REPO="${TMUX_REPO:-https://github.com/tmux/tmux.git}"
TMUX_PREFIX="${TMUX_PREFIX:-$HOME/.local}"

install_tmux_deps() {
  if command -v apt-get &>/dev/null; then
    echo "  -> 更新 apt 缓存并安装编译依赖"
    apt-get update >/dev/null
    apt-get install -y git automake bison build-essential pkg-config \
      libevent-dev libncurses-dev >/dev/null
  elif command -v dnf &>/dev/null; then
    echo "  -> 安装编译依赖 (dnf)"
    dnf install -y git automake bison gcc make pkg-config \
      libevent-devel ncurses-devel >/dev/null
  elif command -v brew &>/dev/null; then
    echo "  -> 安装编译依赖 (brew)"
    brew install libevent ncurses >/dev/null
  else
    echo "请手动安装: git, automake, bison, build-essential, pkg-config, libevent-dev, ncurses-dev"
    exit 1
  fi
}

install_tmux_from_src() {
  install_tmux_deps

  echo "  -> 获取 tmux 源码"
  mkdir -p "$(dirname "$TMUX_SRC")"
  (
    if [[ -d "$TMUX_SRC/.git" ]]; then
      cd "$TMUX_SRC"
      git fetch --tags >/dev/null 2>&1
      git checkout "$(git describe --tags --abbrev=0)" >/dev/null 2>&1
    else
      git clone --depth 1 -q "$TMUX_REPO" "$TMUX_SRC"
      cd "$TMUX_SRC"
    fi

    echo "  -> 配置与编译（可能需要几分钟）"
    ./autogen.sh >/dev/null
    ./configure --prefix="$TMUX_PREFIX" >/dev/null
    make -j"$(nproc 2>/dev/null || sysctl -n hw.ncpu 2>/dev/null || echo 2)" >/dev/null
    make install >/dev/null
  )

  export PATH="$TMUX_PREFIX/bin:$PATH"
}

install_tmux() {
  echo "=== [2/4] 安装 tmux ==="
  if [[ "${FORCE_TMUX_INSTALL:-0}" != "1" ]] && command -v tmux &>/dev/null; then
    echo "  -> 已安装: $(tmux -V)，跳过（FORCE_TMUX_INSTALL=1 可强制重装）"
    return 0
  fi
  install_tmux_from_src
  echo "  -> $(tmux -V)"
}

# ============================================================
# 3. 安装 yazi（官方 release，纯静态二进制）
# ============================================================
YAZI_PREFIX="${YAZI_PREFIX:-$HOME/.local}"
YAZI_REPO="${YAZI_REPO:-sxyazi/yazi}"

install_yazi() {
  echo "=== [3/4] 安装 yazi ==="
  if [[ "${FORCE_YAZI_INSTALL:-0}" != "1" ]] && command -v yazi &>/dev/null; then
    echo "  -> 已安装: $(yazi --version 2>&1 | head -1)，跳过"
    return 0
  fi

  local arch
  arch="$(detect_arch)"
  if [[ "$arch" == "unsupported" ]]; then
    echo "  -> 不支持的架构: $(uname -m)，跳过"
    return 0
  fi

  local target="${arch}-unknown-linux-gnu"
  local asset="yazi-${target}.zip"
  local url="https://github.com/${YAZI_REPO}/releases/latest/download/${asset}"
  local tmpdir
  tmpdir="$(mktemp -d)"

  echo "  -> 下载 $url"
  curl -fsSL -o "$tmpdir/$asset" "$url"

  if ! command -v unzip &>/dev/null; then
    if command -v apt-get &>/dev/null; then
      apt-get install -y unzip >/dev/null
    elif command -v dnf &>/dev/null; then
      dnf install -y unzip >/dev/null
    fi
  fi

  unzip -q -o "$tmpdir/$asset" -d "$tmpdir"
  mkdir -p "$YAZI_PREFIX/bin"
  install -m 755 "$tmpdir/yazi-${target}/yazi" "$YAZI_PREFIX/bin/yazi"
  install -m 755 "$tmpdir/yazi-${target}/ya"   "$YAZI_PREFIX/bin/ya"
  rm -rf "$tmpdir"

  export PATH="$YAZI_PREFIX/bin:$PATH"
  echo "  -> $(yazi --version 2>&1 | head -1)"
}

# ============================================================
# 4. 安装 zellij（官方 release，musl 静态二进制）
# ============================================================
ZELLIJ_PREFIX="${ZELLIJ_PREFIX:-$HOME/.local}"
ZELLIJ_REPO="${ZELLIJ_REPO:-zellij-org/zellij}"

install_zellij() {
  echo "=== [4/4] 安装 zellij ==="
  if [[ "${FORCE_ZELLIJ_INSTALL:-0}" != "1" ]] && command -v zellij &>/dev/null; then
    echo "  -> 已安装: $(zellij --version 2>&1 | head -1)，跳过"
    return 0
  fi

  local arch
  arch="$(detect_arch)"
  if [[ "$arch" == "unsupported" ]]; then
    echo "  -> 不支持的架构: $(uname -m)，跳过"
    return 0
  fi

  local target="${arch}-unknown-linux-musl"
  local asset="zellij-${target}.tar.gz"
  local url="https://github.com/${ZELLIJ_REPO}/releases/latest/download/${asset}"
  local tmpdir
  tmpdir="$(mktemp -d)"

  echo "  -> 下载 $url"
  curl -fsSL -o "$tmpdir/$asset" "$url"
  tar -C "$tmpdir" -xzf "$tmpdir/$asset"
  mkdir -p "$ZELLIJ_PREFIX/bin"
  install -m 755 "$tmpdir/zellij" "$ZELLIJ_PREFIX/bin/zellij"
  rm -rf "$tmpdir"

  export PATH="$ZELLIJ_PREFIX/bin:$PATH"
  echo "  -> $(zellij --version 2>&1 | head -1)"
}

# ============================================================
# 主流程
# ============================================================
install_ghostty_terminfo
install_tmux
install_yazi
install_zellij

echo "=== ✅ 终端环境配置完成 ==="
