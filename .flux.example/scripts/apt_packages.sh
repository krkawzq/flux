#!/usr/bin/env bash
# 安装常用软件及开发工具（Debian/Ubuntu）

set -e

alias sudo=""

if ! command -v apt-get &>/dev/null; then
  echo "当前系统非 Debian/Ubuntu，请使用对应包管理器安装"
  exit 1
fi

PACKAGES=(
  zsh git curl fzf htop nvtop
  clang clangd build-essential ripgrep gh
)

# 检测哪些包还未安装
MISSING=()
for pkg in "${PACKAGES[@]}"; do
  if ! dpkg -s "$pkg" &>/dev/null; then
    MISSING+=("$pkg")
  fi
done

if [[ ${#MISSING[@]} -eq 0 ]]; then
  echo "=== 所有软件包已安装，跳过 ==="
  exit 0
fi

echo "=== 需要安装: ${MISSING[*]} ==="
apt-get update
apt-get install -y "${MISSING[@]}"
echo "=== 安装完成 ==="


sudo curl -L https://raw.githubusercontent.com/AlDanial/cloc/master/cloc -o /usr/local/bin/cloc
sudo chmod +x /usr/local/bin/cloc