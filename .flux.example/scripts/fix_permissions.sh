#!/usr/bin/env bash
# 修复同步文件的权限（此脚本应在文件同步后运行）

set -e

alias sudo=""

echo "=== 修复文件权限 ==="

# SSH 目录和密钥权限
if [[ -d "$HOME/.ssh" ]]; then
  echo "设置 SSH 目录权限..."
  chmod 700 "$HOME/.ssh"

  for key in "$HOME/.ssh"/id_*; do
    [[ -e "$key" ]] || continue
    if [[ -f "$key" && "$key" != *.pub ]]; then
      chmod 600 "$key"
      echo "  chmod 600 $key"
    fi
  done

  for pubkey in "$HOME/.ssh"/*.pub; do
    [[ -e "$pubkey" ]] || continue
    if [[ -f "$pubkey" ]]; then
      chmod 644 "$pubkey"
      echo "  chmod 644 $pubkey"
    fi
  done

  if [[ -f "$HOME/.ssh/authorized_keys" ]]; then
    chmod 600 "$HOME/.ssh/authorized_keys"
    echo "  chmod 600 $HOME/.ssh/authorized_keys"
  fi

  if [[ -f "$HOME/.ssh/config" ]]; then
    chmod 600 "$HOME/.ssh/config"
    echo "  chmod 600 $HOME/.ssh/config"
  fi
fi

# Shell 配置文件权限
for rcfile in "$HOME/.bashrc" "$HOME/.zshrc" "$HOME/.profile" "$HOME/.bash_profile"; do
  if [[ -f "$rcfile" ]]; then
    chmod 644 "$rcfile"
    echo "  chmod 644 $rcfile"
  fi
done

if [[ -f "$HOME/.p10k.zsh" ]]; then
  chmod 644 "$HOME/.p10k.zsh"
  echo "  chmod 644 $HOME/.p10k.zsh"
fi

if [[ -f "$HOME/.tmux.conf" ]]; then
  chmod 644 "$HOME/.tmux.conf"
  echo "  chmod 644 $HOME/.tmux.conf"
fi

echo "=== 权限修复完成 ==="
