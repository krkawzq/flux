#!/usr/bin/env bash
# 配置 GitHub SSH 与 git 用户信息

set -a

alias sudo=""

SSH_DIR="${HOME}/.ssh"
IDENTITY_FILE="${IDENTITY_FILE:-$SSH_DIR/id_ed25519_wzq}"
GIT_USER_NAME="${GIT_USER_NAME:-wzq}"
GIT_USER_EMAIL="${GIT_USER_EMAIL:-2868116803@qq.com}"

mkdir -p "$SSH_DIR"
touch "$SSH_DIR/config"
chmod 600 "$SSH_DIR/config"

# 删除旧的 github.com 配置块（兼容 GNU sed 与 macOS sed）
if command -v sed &>/dev/null; then
  if sed --version &>/dev/null 2>&1; then
    sed -i '/^Host github\.com$/,/^Host[[:space:]]\+.*$/d' "$SSH_DIR/config"
  else
    sed -i '' '/^Host github\.com$/,/^Host[[:space:]]\+.*$/d' "$SSH_DIR/config"
  fi
fi

# 追加新配置
cat >> "$SSH_DIR/config" << EOF
Host github.com
    HostName github.com
    User git
    IdentityFile $IDENTITY_FILE
    IdentitiesOnly yes
EOF

echo "=== 测试 GitHub SSH 连接 ==="
ssh -T git@github.com || true

git config --global user.name "$GIT_USER_NAME"
git config --global user.email "$GIT_USER_EMAIL"
echo "=== GitHub 配置完成 ==="