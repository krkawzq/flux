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

# ---------- 通用别名 ----------
alias ll='ls -alF'
alias la='ls -A'
alias lla='ls -la'
alias ..='cd ..'
alias ...='cd ../..'
alias ....='cd ../../..'
alias rp="realpath"