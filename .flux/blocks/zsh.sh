# ---------- oh-my-zsh ----------
export ZSH="$HOME/.oh-my-zsh"
ZSH_THEME="powerlevel10k/powerlevel10k"
plugins=(
  git
  zsh-completions
  zsh-syntax-highlighting
  zsh-autosuggestions
)

alias sudo=

source $ZSH/oh-my-zsh.sh

# ---------- Powerlevel10k 配置 ----------
[[ ! -f ~/.p10k.zsh ]] || source ~/.p10k.zsh

# ---------- 启用补全（确保插件 fpath 生效后重新初始化） ----------
autoload -U compinit && compinit -u

# ---------- 语法高亮需放在最后 ----------
source ${ZSH_CUSTOM:-$HOME/.oh-my-zsh/custom}/plugins/zsh-syntax-highlighting/zsh-syntax-highlighting.zsh

alias activate="source .venv/bin/activate"

bindkey "${terminfo[kcbt]}" autosuggest-accept
