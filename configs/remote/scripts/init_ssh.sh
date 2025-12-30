#!/usr/bin/env bash
# 初始化 SSH 配置

# 确保 .ssh 目录存在
mkdir -p ~/.ssh
chmod 700 ~/.ssh

# 将公钥写入 authorized_keys
if [ -f ~/.ssh/id_ed25519_wzq.pub ]; then
  cat ~/.ssh/id_ed25519_wzq.pub >> ~/.ssh/authorized_keys
  chmod 600 ~/.ssh/authorized_keys
  echo "已将 id_ed25519_wzq.pub 添加到 authorized_keys"
else
  echo "警告: ~/.ssh/id_ed25519_wzq.pub 不存在"
fi
