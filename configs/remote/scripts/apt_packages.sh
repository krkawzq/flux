#!/usr/bin/env bash
# 安装常用软件及开发工具

apt update && apt install -y \
    tmux zsh git curl fzf htop nvtop \
    clang clangd build-essential rustc cargo

 