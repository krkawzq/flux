#!/usr/bin/env bash
# 修复同步文件的权限
# 此脚本应在文件同步后运行

set -e

echo "=== 修复文件权限 ==="

# SSH 目录和密钥权限
if [ -d ~/.ssh ]; then
    echo "设置 SSH 目录权限..."
    chmod 700 ~/.ssh
    
    # 私钥 - 必须是 600
    for key in ~/.ssh/id_* ; do
        if [[ -f "$key" && ! "$key" == *.pub ]]; then
            chmod 600 "$key"
            echo "  chmod 600 $key"
        fi
    done
    
    # 公钥 - 644 即可
    for pubkey in ~/.ssh/*.pub; do
        if [ -f "$pubkey" ]; then
            chmod 644 "$pubkey"
            echo "  chmod 644 $pubkey"
        fi
    done
    
    # authorized_keys
    if [ -f ~/.ssh/authorized_keys ]; then
        chmod 600 ~/.ssh/authorized_keys
        echo "  chmod 600 ~/.ssh/authorized_keys"
    fi
    
    # config
    if [ -f ~/.ssh/config ]; then
        chmod 600 ~/.ssh/config
        echo "  chmod 600 ~/.ssh/config"
    fi
fi

# Shell 配置文件权限
for rcfile in ~/.bashrc ~/.zshrc ~/.profile ~/.bash_profile; do
    if [ -f "$rcfile" ]; then
        chmod 644 "$rcfile"
        echo "  chmod 644 $rcfile"
    fi
done

# p10k 配置
if [ -f ~/.p10k.zsh ]; then
    chmod 644 ~/.p10k.zsh
    echo "  chmod 644 ~/.p10k.zsh"
fi

# tmux 配置
if [ -f ~/.tmux.conf ]; then
    chmod 644 ~/.tmux.conf
    echo "  chmod 644 ~/.tmux.conf"
fi

echo "=== 权限修复完成 ==="

