#!/usr/bin/env bash
# 安装 uv (Python 包/项目管理工具) + 常用 Python 包

set -e

alias sudo=""

# ========== 安装 uv ==========
if command -v uv &>/dev/null; then
  echo "=== uv 已安装: $(uv --version 2>/dev/null || true)，跳过 ==="
else
  echo "=== 安装 uv ==="
  curl -fsSL https://astral.sh/uv/install.sh | sh
  echo "=== uv 安装完成 ==="
fi

# 加载 uv 环境
UV_ENV="${HOME}/.local/bin/env"
if [[ -f "$UV_ENV" ]]; then
  set +e
  # shellcheck source=/dev/null
  . "$UV_ENV"
  set -e
fi

# ========== 安装常用 Python 包 ==========
if ! command -v uv &>/dev/null; then
  echo "uv 未找到，跳过 Python 包安装"
  exit 0
fi

PYTHON_PACKAGES=(ruff flake8 isort)
MISSING=()
for pkg in "${PYTHON_PACKAGES[@]}"; do
  if ! command -v "$pkg" &>/dev/null; then
    MISSING+=("$pkg")
  fi
done

if [[ ${#MISSING[@]} -eq 0 ]]; then
  echo "=== Python 工具包已全部安装，跳过 ==="
else
  echo "=== 安装 Python 工具包: ${MISSING[*]} ==="
  uv pip install "${MISSING[@]}" --system
  echo "=== Python 工具包安装完成 ==="
fi
