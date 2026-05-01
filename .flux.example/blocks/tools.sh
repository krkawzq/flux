
# --------------------------------------------
# Claude CLI 配置函数
# --------------------------------------------
set_claude() {
    local ZSHRC="$HOME/.zshrc"
    local BASHRC="$HOME/.bashrc"
    local RC_FILES=("$ZSHRC" "$BASHRC")
    local ENV_VARS=("ANTHROPIC_BASE_URL" "ANTHROPIC_AUTH_TOKEN")

    # 显示当前配置
    echo "📋 当前 Claude 配置："
    for var in "${ENV_VARS[@]}"; do
        local val="${(P)var}"  # zsh indirect expansion
        if [[ -n "$val" ]]; then
            if [[ "$var" == *TOKEN* || "$var" == *KEY* ]]; then
                echo "   $var: ${val:0:8}…****"
            else
                echo "   $var: $val"
            fi
        else
            # fallback: 从 rc 文件读
            local rc_val
            rc_val=$(grep "^export ${var}=" "$ZSHRC" 2>/dev/null | head -1 | sed "s/^export ${var}=//;s/^['\"]//;s/['\"]$//")
            if [[ -n "$rc_val" ]]; then
                if [[ "$var" == *TOKEN* || "$var" == *KEY* ]]; then
                    echo "   $var: ${rc_val:0:8}…**** (未 source)"
                else
                    echo "   $var: $rc_val (未 source)"
                fi
            else
                echo "   $var: (未设置)"
            fi
        fi
    done
    echo ""

    # 交互式输入
    echo -n "🌐 ANTHROPIC_BASE_URL (回车跳过): "
    read -r new_base_url

    echo -n "🔑 ANTHROPIC_AUTH_TOKEN (回车跳过): "
    read -rs new_api_key  # -s 隐藏输入
    echo ""  # read -s 不换行

    if [[ -z "$new_base_url" && -z "$new_api_key" ]]; then
        echo "⚠️  未输入任何值，已取消"
        return 0
    fi

    # ---- 内部: 写入单个 rc 文件 ----
    _update_env_var() {
        local file="$1" var_name="$2" var_value="$3"
        [[ -z "$var_value" ]] && return 0
        [[ ! -f "$file" ]] && touch "$file"

        if grep -q "^export ${var_name}=" "$file" 2>/dev/null; then
            sed -i'' "s|^export ${var_name}=.*|export ${var_name}='${var_value}'|" "$file"
            echo "   ✏️  更新 ${var_name} → $(basename "$file")"
        else
            echo "" >> "$file"  # 保证前面有空行
            echo "export ${var_name}='${var_value}'" >> "$file"
            echo "   ➕ 追加 ${var_name} → $(basename "$file")"
        fi
    }

    echo ""
    echo "⚙️  写入配置..."

    for rc in "${RC_FILES[@]}"; do
        [[ -n "$new_base_url" ]] && _update_env_var "$rc" "ANTHROPIC_BASE_URL" "$new_base_url"
        [[ -n "$new_api_key"  ]] && _update_env_var "$rc" "ANTHROPIC_AUTH_TOKEN" "$new_api_key"
    done

    # 立即生效
    [[ -n "$new_base_url" ]] && export ANTHROPIC_BASE_URL="$new_base_url"
    [[ -n "$new_api_key"  ]] && export ANTHROPIC_AUTH_TOKEN="$new_api_key"

    unset -f _update_env_var  # 清理内部函数

    echo ""
    echo "🎉 完成！当前环境已生效："
    [[ -n "$ANTHROPIC_BASE_URL"  ]] && echo "   ANTHROPIC_BASE_URL=$ANTHROPIC_BASE_URL"
    [[ -n "$ANTHROPIC_AUTH_TOKEN" ]] && echo "   ANTHROPIC_AUTH_TOKEN=${ANTHROPIC_AUTH_TOKEN:0:8}…****"
}

unset_claude() {
    local ZSHRC="$HOME/.zshrc"
    local BASHRC="$HOME/.bashrc"
    local RC_FILES=("$ZSHRC" "$BASHRC")
    local ENV_VARS=("ANTHROPIC_BASE_URL" "ANTHROPIC_AUTH_TOKEN")
    local dry_run=false

    # 解析参数
    if [[ "$1" == "--dry-run" || "$1" == "-n" ]]; then
        dry_run=true
        echo "🔍 Dry run — 仅预览，不执行删除"
        echo ""
    fi

    # 检查是否有东西可清
    local has_env=false has_rc=false
    for var in "${ENV_VARS[@]}"; do
        [[ -n "${(P)var}" ]] && has_env=true
        for rc in "${RC_FILES[@]}"; do
            grep -q "^export ${var}=" "$rc" 2>/dev/null && has_rc=true
        done
    done

    if ! $has_env && ! $has_rc; then
        echo "✅ 已经是干净状态，无需清理"
        return 0
    fi

    # 展示将要清理的内容
    echo "🗑️  将要清理："
    for var in "${ENV_VARS[@]}"; do
        [[ -n "${(P)var}" ]] && echo "   环境变量: $var"
        for rc in "${RC_FILES[@]}"; do
            grep -q "^export ${var}=" "$rc" 2>/dev/null && echo "   文件记录: $var ← $(basename "$rc")"
        done
    done
    echo ""

    if $dry_run; then
        echo "ℹ️  去掉 --dry-run 以执行清理"
        return 0
    fi

    # 确认
    echo -n "确认清除所有 Claude 配置？[y/N] "
    read -r confirm
    if [[ "$confirm" != [yY] ]]; then
        echo "已取消"
        return 0
    fi

    # 从 rc 文件删除
    for rc in "${RC_FILES[@]}"; do
        [[ ! -f "$rc" ]] && continue
        for var in "${ENV_VARS[@]}"; do
            if grep -q "^export ${var}=" "$rc" 2>/dev/null; then
                sed -i'' "/^export ${var}=/d" "$rc"
                echo "   🧹 已从 $(basename "$rc") 移除 ${var}"
            fi
        done
    done

    # unset 当前环境
    for var in "${ENV_VARS[@]}"; do
        unset "$var"
        echo "   🧹 已 unset ${var}"
    done

    echo ""
    echo "✅ 清理完成。新终端窗口也不会再加载这些变量。"
}

set_proxy() {
    local host="${1:-127.0.0.1}"
    local port="${2:-7890}"
    export http_proxy="http://${host}:${port}"
    export https_proxy="http://${host}:${port}"
    export all_proxy="socks5://${host}:${port}"
    export HTTP_PROXY="$http_proxy"
    export HTTPS_PROXY="$https_proxy"
    export ALL_PROXY="$all_proxy"
    echo "✅ 代理已设置: http://${host}:${port}"
}

unset_proxy() {
    unset http_proxy
    unset https_proxy
    unset all_proxy
    unset HTTP_PROXY
    unset HTTPS_PROXY
    unset ALL_PROXY
    echo "✅ 代理已取消"
}