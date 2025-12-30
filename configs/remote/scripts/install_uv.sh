#!/usr/bin/env bash
# 安装 uv

if command -v uv >/dev/null 2>&1; then
  echo "uv 已安装"
else
  echo "uv 未安装，开始安装..."
  curl -LsSf https://astral.sh/uv/install.sh | sh
  source $HOME/.local/bin/env
fi

