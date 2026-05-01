#!/bin/bash
# 启动 VPN (mihomo) 后台 session
tmux new-session -d -s vpn 'bash /zhoujingbo/wzq/mihomo/start.sh && tail -f /zhoujingbo/wzq/mihomo/clash.log'