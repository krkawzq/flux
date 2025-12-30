#!/usr/bin/env bash
# 配置 GitHub SSH

mkdir -p ~/.ssh
touch ~/.ssh/config

# 删除旧的 github.com 配置块
sed -i '/^Host github.com$/,/^Host[[:space:]]\+.*$/d' ~/.ssh/config

# 追加新的配置
cat <<EOF >> ~/.ssh/config
Host github.com
    HostName github.com
    User git
    IdentityFile ~/.ssh/id_ed25519_wzq
    IdentitiesOnly yes
EOF

ssh -T git@github.com

git config --global user.name "wzq"
git config --global user.email "2868116803@qq.com"

