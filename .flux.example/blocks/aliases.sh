# ---------- tmux 常用别名 ----------
alias tls='tmux ls'
alias tattatch='tmux attach -t'
alias tatt='tmux attach -t'
alias tnew='tmux new -s'
alias tkill='tmux kill-session -t'
alias trename='tmux rename-session'
alias tkillall='tmux kill-server'
alias tlsw='tmux list-windows'
alias tneww='tmux new-window -n'
alias tkillw='tmux kill-window -t'
alias tmvw='tmux move-window -t'
alias tsplith='tmux split-window -h'
alias tsplitv='tmux split-window -v'
alias codex='codex --yolo'
alias cc='IS_SANDBOX=1 claude --dangerously-skip-permissions'

# Zellij（与上面 tmux 别名一一对齐；tab/pane 级命令需在 zellij 会话内执行）
alias zls='zellij ls'
alias zattach='zellij attach'
alias zatt='zellij attach'
alias znew='zellij -s'                          # 同 tnew：新建命名会话
alias zac='zellij attach -c'                    # attach-or-create（同 tmux new -A -s）
alias zkill='zellij kill-session'
alias zrename='zellij action rename-session'
alias zkillall='zellij kill-all-sessions'
alias zdel='zellij delete-session'
alias zdelall='zellij delete-all-sessions'
alias zneww='zellij action new-tab --name'      # tmux 的 window ≈ zellij 的 tab
alias zkillw='zellij action close-tab'
alias zrenamew='zellij action rename-tab'
alias zsplith='zellij action new-pane -d right' # 水平分屏（左右）
alias zsplitv='zellij action new-pane -d down'  # 垂直分屏（上下）
alias zkillp='zellij action close-pane'
alias zselp='zellij action focus-next-pane'
alias zresizep='zellij action resize'
alias zswapp='zellij action move-pane'

# ---------- 通用别名 ----------
alias ll='ls -alF'
alias la='ls -A'
alias lla='ls -la'
alias ..='cd ..'
alias ...='cd ../..'
alias ....='cd ../../..'
alias rp="realpath"