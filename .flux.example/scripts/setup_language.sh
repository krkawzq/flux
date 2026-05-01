#!/usr/bin/env bash
# 设置系统 locale 为 UTF-8

set -e

alias sudo=""

# 检测 locale 是否已正确设置
CURRENT_LANG=$(locale 2>/dev/null | grep '^LANG=' | cut -d= -f2)
if [[ "$CURRENT_LANG" == "C.UTF-8" || "$CURRENT_LANG" == "en_US.UTF-8" ]]; then
  echo "=== locale 已设置为 $CURRENT_LANG，跳过 ==="
  exit 0
fi

echo "=== 配置系统 locale ==="

# 安装 locale 支持包
if command -v apt-get &>/dev/null; then
  apt-get update
  apt-get install -y locales
  locale-gen en_US.UTF-8
elif command -v yum &>/dev/null; then
  yum install -y glibc-langpack-en langpacks-en
elif command -v apk &>/dev/null; then
  apk add --no-cache musl-locales
fi

# 写入环境变量到 shell 配置
SHELL_RC="$HOME/.zshrc"
[[ -f "$SHELL_RC" ]] || SHELL_RC="$HOME/.bashrc"

for var in "LANG=C.UTF-8" "LC_ALL=C.UTF-8"; do
  grep -qxF "export $var" "$SHELL_RC" 2>/dev/null || echo "export $var" >> "$SHELL_RC"
  export "$var"
done

echo "=== locale 配置完成: LANG=C.UTF-8 ==="
