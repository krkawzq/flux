"""flux proxy: keep a reverse SSH tunnel alive with a live status panel."""

from __future__ import annotations

import socket
import threading
import time
from datetime import timedelta

from rich.console import Console
from rich.live import Live
from rich.panel import Panel
from rich.table import Table

from flux.config import ResolvedConnection
from flux.ssh import SshClient


def _is_local_listening(port: int) -> bool:
    s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    s.settimeout(0.3)
    try:
        s.connect(("127.0.0.1", port))
        return True
    except OSError:
        return False
    finally:
        s.close()


class _Stats:
    def __init__(self) -> None:
        self.lock = threading.Lock()
        self.opened = 0
        self.closed = 0
        self.started_at = time.monotonic()

    def on_open(self) -> None:
        with self.lock:
            self.opened += 1

    def on_close(self) -> None:
        with self.lock:
            self.closed += 1

    @property
    def active(self) -> int:
        with self.lock:
            return self.opened - self.closed

    @property
    def uptime(self) -> str:
        return str(timedelta(seconds=int(time.monotonic() - self.started_at)))


def _render_panel(conn: ResolvedConnection, local: int, remote: int, stats: _Stats, state: str) -> Panel:
    grid = Table.grid(padding=(0, 1))
    grid.add_column(style="dim", justify="right")
    grid.add_column()
    grid.add_row("state", state)
    grid.add_row("remote", f"{conn.user}@{conn.host}:{conn.port}")
    grid.add_row("tunnel", f"remote:{remote} → local:{local}")
    grid.add_row("active", str(stats.active))
    grid.add_row("opened", str(stats.opened))
    grid.add_row("uptime", stats.uptime)
    return Panel(grid, title="[bold]flux proxy[/]", border_style="cyan", expand=False)


def run_proxy(
    conn: ResolvedConnection,
    local_port: int,
    remote_port: int,
    retry_seconds: int,
    console: Console,
) -> int:
    if not _is_local_listening(local_port):
        console.print(
            f"[yellow]warn:[/] local 127.0.0.1:{local_port} is not listening; "
            f"forwarded conns will be dropped until you start your proxy service"
        )

    stats = _Stats()
    state = "connecting"

    attempt = 0
    while True:
        attempt += 1
        if attempt > 1:
            console.print(f"[dim]reconnect attempt {attempt}[/]")

        try:
            client = SshClient(conn)
            client.connect()
        except KeyboardInterrupt:
            console.print("[yellow]aborted[/]")
            return 0
        except Exception as exc:
            console.print(f"[red]connect failed:[/] {exc}")
            if retry_seconds <= 0:
                return 1
            try:
                time.sleep(retry_seconds)
                continue
            except KeyboardInterrupt:
                return 0

        try:
            client.reverse_forward(
                local_port,
                remote_port,
                on_connect=stats.on_open,
                on_disconnect=stats.on_close,
            )
        except Exception as exc:
            console.print(f"[red]reverse forward failed:[/] {exc}")
            client.close()
            if retry_seconds <= 0:
                return 1
            try:
                time.sleep(retry_seconds)
                continue
            except KeyboardInterrupt:
                return 0

        state = "up"
        try:
            with Live(
                _render_panel(conn, local_port, remote_port, stats, state),
                console=console,
                refresh_per_second=2,
                transient=False,
            ) as live:
                while True:
                    time.sleep(0.5)
                    # detect transport death (e.g., remote sshd restarted)
                    transport = client._ensure_client().get_transport()  # noqa: SLF001
                    if transport is None or not transport.is_active():
                        state = "[red]disconnected[/]"
                        live.update(_render_panel(conn, local_port, remote_port, stats, state))
                        break
                    live.update(_render_panel(conn, local_port, remote_port, stats, state))
        except KeyboardInterrupt:
            console.print("[yellow]closing tunnel[/]")
            client.close()
            return 0
        finally:
            client.close()

        if retry_seconds <= 0:
            return 1
        try:
            console.print(f"[dim]reconnecting in {retry_seconds}s[/]")
            time.sleep(retry_seconds)
        except KeyboardInterrupt:
            return 0
