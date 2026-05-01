#!/usr/bin/env bash
# 安装和配置 zsh (oh-my-zsh + Powerlevel10k + 插件)

set -e

alias sudo=""

export RUNZSH=no
export CHSH=no
export KEEP_ZSHRC=yes

ZSH_CUSTOM="${ZSH_CUSTOM:-$HOME/.oh-my-zsh/custom}"

echo "=== 安装 oh-my-zsh ==="
if [[ ! -d "$HOME/.oh-my-zsh" ]]; then
  sh -c "$(curl -fsSL https://raw.githubusercontent.com/ohmyzsh/ohmyzsh/master/tools/install.sh)"
fi

echo "=== 安装 Powerlevel10k ==="
if [[ ! -d "$ZSH_CUSTOM/themes/powerlevel10k" ]]; then
  git clone --depth=1 https://github.com/romkatv/powerlevel10k.git \
    "$ZSH_CUSTOM/themes/powerlevel10k"
fi

echo "=== 安装 zsh 插件 ==="
for repo in \
  "zsh-users/zsh-completions:plugins/zsh-completions" \
  "zsh-users/zsh-syntax-highlighting:plugins/zsh-syntax-highlighting" \
  "zsh-users/zsh-autosuggestions:plugins/zsh-autosuggestions"; do
  name="${repo%%:*}"
  path="$ZSH_CUSTOM/${repo#*:}"
  if [[ ! -d "$path" ]]; then
    git clone "https://github.com/$name.git" "$path"
  fi
done

echo "=== 设置 zsh 为默认 shell ==="
if [[ "$SHELL" != "$(command -v zsh)" ]]; then
  chsh -s "$(command -v zsh)" || echo "警告: chsh 失败，请手动执行 chsh -s \$(which zsh)" >&2
fi

if command -v conda &>/dev/null; then
  conda init zsh
else
  echo "conda 未安装，跳过 conda init"
fi
echo "=== 安装完成 ==="
