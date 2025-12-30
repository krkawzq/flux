#!/usr/bin/env bash
set -e

# ---------- 配置 ----------
NPM_REGISTRY="https://registry.npmjs.org/"
PKG="@openai/codex"

# ---------- 检测 codex ----------
if command -v codex >/dev/null 2>&1; then
    echo "✅ codex 已存在：$(command -v codex)"
    codex --version || true
    exit 0
fi

echo "⚠️ 未检测到 codex，开始安装..."

# ---------- 检测 npm ----------
if ! command -v npm >/dev/null 2>&1; then
    echo "❌ 未检测到 npm，请先安装 Node.js / npm"
    exit 1
fi

# ---------- 安装 ----------
npm i -g "$PKG" \
  --registry="$NPM_REGISTRY" \
  --no-fund \
  --no-audit

# ---------- 校验 ----------
if command -v codex >/dev/null 2>&1; then
    echo "🎉 codex 安装成功：$(command -v codex)"
    codex --version || true
else
    echo "❌ codex 安装失败"
    exit 1
fi

