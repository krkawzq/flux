#!/usr/bin/env python3
"""
SSH 反向代理隧道工具 - 稳健版

功能：
- 建立 SSH 反向隧道，将本地代理端口转发到远程服务器
- 自动重连机制
- 心跳保活
- 优先使用 SSH 密钥认证，支持密码回退
"""

import argparse
import sys
import time
import socket
import os
from datetime import datetime

from rich.console import Console
from rich.panel import Panel
from rich.table import Table
from rich.prompt import Confirm
from rich import box

console = Console()


def check_local_port(port: int) -> bool:
    """检查本地端口是否有服务在监听"""
    try:
        with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
            s.settimeout(1)
            result = s.connect_ex(('127.0.0.1', port))
            return result == 0
    except Exception:
        return False


def log(msg: str, style: str = None):
    """带时间戳的日志输出"""
    timestamp = datetime.now().strftime('%H:%M:%S')
    if style:
        console.print(f"[dim][[/dim][cyan]{timestamp}[/cyan][dim]][/dim] [{style}]{msg}[/{style}]")
    else:
        console.print(f"[dim][[/dim][cyan]{timestamp}[/cyan][dim]][/dim] {msg}")


def print_banner(remote_host: str, local_port: int, remote_port: int, 
                 retry_interval: int, no_retry: bool):
    """打印启动横幅"""
    
    table = Table(show_header=False, box=box.SIMPLE, padding=(0, 2))
    table.add_column("Key", style="bold cyan")
    table.add_column("Value", style="white")
    
    table.add_row("远程主机", f"[bold green]{remote_host}[/bold green]")
    table.add_row("隧道方向", f"[yellow]远程:{remote_port}[/yellow] [dim]<---[/dim] [yellow]本地:{local_port}[/yellow]")
    
    retry_info = "[red]禁用[/red]" if no_retry else f"[green]启用[/green] [dim](间隔 {retry_interval}s)[/dim]"
    table.add_row("自动重连", retry_info)
    
    panel = Panel(
        table,
        title="[bold cyan]🔗 SSH 反向代理隧道[/bold cyan]",
        border_style="cyan",
        padding=(1, 2)
    )
    
    console.print()
    console.print(panel)
    console.print("[dim]按 Ctrl+C 停止隧道[/dim]")
    console.print()


def main():
    parser = argparse.ArgumentParser(
        description='SSH 反向代理隧道工具',
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog='''
示例:
  pss myserver                    # 使用默认端口连接
  pss myserver -l 1080 -r 8080    # 自定义端口
  pss myserver --no-retry         # 禁用自动重连
  pss user@192.168.1.1            # 直接使用主机地址
        '''
    )
    
    parser.add_argument('remote_host', nargs='?', 
                        help='SSH 配置名称或远程主机地址')
    parser.add_argument('-l', '--local-port', type=int, default=7899,
                        help='本地代理端口 (默认: 7899)')
    parser.add_argument('-r', '--remote-port', type=int, default=7890,
                        help='远程监听端口 (默认: 7890)')
    parser.add_argument('-i', '--retry-interval', type=int, default=5,
                        help='断线重连间隔秒数 (默认: 5)')
    parser.add_argument('-m', '--max-retries', type=int, default=0,
                        help='最大重试次数，0 为无限 (默认: 0)')
    parser.add_argument('--no-retry', action='store_true',
                        help='禁用自动重连')
    parser.add_argument('-y', '--yes', action='store_true',
                        help='跳过确认提示')
    parser.add_argument('-v', '--verbose', action='store_true',
                        help='显示 SSH 详细输出')
    
    args = parser.parse_args()
    
    if not args.remote_host:
        parser.print_help()
        return 1
    
    # 检查本地端口
    if not check_local_port(args.local_port):
        console.print(f"[yellow]⚠ 警告: 本地端口 {args.local_port} 没有服务在监听[/yellow]")
        if not args.yes:
            try:
                if not Confirm.ask("是否继续？", default=False):
                    console.print("[dim]已取消[/dim]")
                    return 0
            except (KeyboardInterrupt, EOFError):
                console.print("\n[dim]已取消[/dim]")
                return 0
    
    # 打印横幅
    print_banner(
        args.remote_host,
        args.local_port,
        args.remote_port,
        args.retry_interval,
        args.no_retry
    )
    
    retry_count = 0
    start_time = datetime.now()
    
    # 构建 SSH 命令
    ssh_args = ['ssh', '-N']
    
    if args.verbose:
        ssh_args.append('-v')
    
    ssh_args.extend([
        '-R', f'0.0.0.0:{args.remote_port}:127.0.0.1:{args.local_port}',
        '-o', 'PreferredAuthentications=publickey,keyboard-interactive,password',
        '-o', 'BatchMode=no',
        '-o', 'StrictHostKeyChecking=accept-new',
        '-o', 'ServerAliveInterval=30',
        '-o', 'ServerAliveCountMax=3',
        '-o', 'ExitOnForwardFailure=yes',
        '-o', 'ConnectTimeout=15',
        '-o', 'TCPKeepAlive=yes',
        args.remote_host
    ])
    
    while True:
        retry_count += 1
        
        if args.max_retries > 0 and retry_count > args.max_retries:
            log(f"已达到最大重试次数 ({args.max_retries})，退出", 'bold red')
            return 1
        
        if retry_count > 1:
            log(f"第 {retry_count - 1} 次重连...", 'bold yellow')
        else:
            log("正在建立连接...", 'bold cyan')
        
        try:
            cmd = ' '.join(f'"{arg}"' if ' ' in arg else arg for arg in ssh_args)
            exit_code = os.system(cmd)
            
            if os.name != 'nt':
                exit_code = exit_code >> 8
                
        except KeyboardInterrupt:
            console.print()
            log("用户中断，退出", 'dim')
            return 0
        except Exception as e:
            log(f"SSH 执行异常: {e}", 'bold red')
            exit_code = -1
        
        duration = datetime.now() - start_time
        minutes = duration.total_seconds() / 60
        
        if exit_code == 0:
            log(f"✓ 连接正常关闭 (运行时长: {minutes:.1f} 分钟)", 'bold green')
        else:
            log(f"✗ 连接断开 (退出码: {exit_code})", 'bold red')
        
        if args.no_retry:
            log("自动重连已禁用，退出", 'dim')
            return exit_code
        
        if exit_code in (130, 255, -1, 5, 2):
            if exit_code == 5:
                log("认证失败，请检查密钥或密码", 'bold red')
            log("退出", 'dim')
            return 0 if exit_code in (130, 2) else exit_code
        
        log(f"⏳ {args.retry_interval} 秒后重连... [dim](按 Ctrl+C 取消)[/dim]", 'yellow')
        
        try:
            time.sleep(args.retry_interval)
        except KeyboardInterrupt:
            console.print()
            log("用户中断，退出", 'dim')
            return 0
        
        start_time = datetime.now()


if __name__ == '__main__':
    sys.exit(main())
