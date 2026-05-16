"""typer entrypoint: `flux sync <name>`, `flux proxy <host>`, `flux list`, `flux exec`."""

from __future__ import annotations

import logging
import sys
from pathlib import Path
from typing import Optional

import typer
from rich.console import Console
from rich.panel import Panel
from rich.table import Table


def _harden_stdout() -> None:
    """Force UTF-8 on stdout/stderr so rich glyphs survive cp936/GBK Windows consoles."""
    for stream in (sys.stdout, sys.stderr):
        try:
            stream.reconfigure(encoding="utf-8", errors="replace")  # type: ignore[union-attr]
        except (AttributeError, ValueError):
            pass


_harden_stdout()

import paramiko

from flux.config import (
    Config,
    ResolvedConnection,
    SshConfigConflict,
    find_config,
    load,
    resolve_connection,
    save_to_ssh_config,
)
from flux.proxy import run_proxy
from flux.ssh import SshClient
from flux.sync import run_sync

app = typer.Typer(
    name="flux",
    no_args_is_help=True,
    add_completion=True,
    help="Personal SSH sync + reverse-forward tool.",
)
console = Console()
err_console = Console(stderr=True)


def _configure_logging(verbose: bool) -> None:
    level = logging.DEBUG if verbose else logging.WARNING
    logging.basicConfig(
        level=level,
        format="%(asctime)s %(levelname)s %(name)s :: %(message)s",
        datefmt="%H:%M:%S",
        stream=sys.stderr,
    )
    if verbose:
        try:
            from rich.traceback import install as _install

            _install(show_locals=False, console=err_console)
        except Exception:
            pass


def _list_known_configs() -> list[str]:
    """Names (sans .yml) found under ./.flux/ and ~/.flux/. cwd wins on collision."""
    seen: dict[str, Path] = {}
    for base in (Path.home() / ".flux", Path.cwd() / ".flux"):
        if not base.is_dir():
            continue
        for p in base.glob("*.yml"):
            seen[p.stem] = p  # cwd processed last → wins
    return sorted(seen)


def _complete_config_name(incomplete: str):
    """Tab completion for the `name` positional."""
    return [n for n in _list_known_configs() if n.startswith(incomplete)]


def _connect_banner(name: str, cfg: Config, conn: ResolvedConnection) -> None:
    table = Table.grid(padding=(0, 1))
    table.add_column(style="dim", justify="right")
    table.add_column()
    table.add_row("config", name)
    table.add_row("host", f"{conn.user}@{conn.host}:{conn.port}")
    table.add_row("auth", "key" if conn.key else "password")
    if conn.key:
        table.add_row("key", conn.key)
    counts = []
    if cfg.file:
        counts.append(f"{len(cfg.file)} file")
    if cfg.script:
        counts.append(f"{len(cfg.script)} script")
    if cfg.block:
        counts.append(f"{len(cfg.block)} block")
    table.add_row("plan", ", ".join(counts) if counts else "(empty)")
    console.print(Panel(table, title="[bold]flux sync[/]", border_style="cyan", expand=False))


@app.command()
def sync(
    name: str = typer.Argument(
        ..., help="Config name (resolves to .flux/<name>.yml) or path.",
        autocompletion=_complete_config_name,
    ),
    save: Optional[str] = typer.Option(
        None, "--save",
        help="Write a Host entry to ~/.ssh/config under this alias (replaces existing).",
    ),
    cont: bool = typer.Option(
        False, "--continue",
        help="Don't stop the script stage on first failure; keep running the rest.",
    ),
    verbose: bool = typer.Option(False, "--verbose", "-v", help="Log SSH calls and retries to stderr."),
) -> None:
    """Run all sync stages (file → script → block) against the host in <name>.yml."""
    _configure_logging(verbose)
    try:
        cfg, asset_root = load(name)
    except FileNotFoundError as exc:
        err_console.print(f"[red]error:[/] {exc}")
        raise typer.Exit(2)
    except Exception as exc:
        err_console.print(f"[red]config error:[/] {exc}")
        raise typer.Exit(2)

    try:
        conn = resolve_connection(cfg)
    except (KeyboardInterrupt, EOFError):
        err_console.print("[yellow]aborted[/]")
        raise typer.Exit(130)

    if save:
        try:
            path = save_to_ssh_config(save, conn)
            console.print(f"[dim]saved Host {save} → {path}[/]")
        except SshConfigConflict as exc:
            err_console.print(f"[yellow]--save skipped:[/] {exc}")
        except ValueError as exc:
            err_console.print(f"[yellow]--save skipped:[/] {exc}")
        except OSError as exc:
            err_console.print(f"[yellow]warn:[/] could not write ssh config: {exc}")

    _connect_banner(name, cfg, conn)
    console.print("[dim]connecting...[/]")
    try:
        client = SshClient(conn)
        client.connect()
    except KeyboardInterrupt:
        err_console.print("[yellow]aborted during connect[/]")
        raise typer.Exit(130)
    except paramiko.SSHException as exc:
        err_console.print(f"[red]ssh connect failed:[/] {exc}")
        raise typer.Exit(1)
    except Exception as exc:
        err_console.print(f"[red]ssh connect failed:[/] {exc}")
        raise typer.Exit(1)

    try:
        status = run_sync(
            cfg, asset_root, client, console,
            stop_scripts_on_failure=not cont,
        )
    except KeyboardInterrupt:
        err_console.print("[yellow]interrupted[/]")
        status = 130
    finally:
        client.close()
    raise typer.Exit(status)


@app.command()
def proxy(
    name_or_host: str = typer.Argument(
        ..., help="Config name (uses host/user/port/key from yaml) or raw host string.",
        autocompletion=_complete_config_name,
    ),
    local: int = typer.Option(7899, "--local", "-l", help="Local port on this machine."),
    remote: int = typer.Option(7890, "--remote", "-r", help="Port to open on the remote."),
    retry: int = typer.Option(5, "--retry", help="Seconds between reconnect attempts. 0 = no retry."),
    verbose: bool = typer.Option(False, "--verbose", "-v", help="Log SSH calls to stderr."),
) -> None:
    """Start a reverse SSH tunnel: remote:<remote> → local:<local>. Stays in foreground."""
    _configure_logging(verbose)
    try:
        conn = _resolve_proxy_target(name_or_host)
    except (KeyboardInterrupt, EOFError):
        err_console.print("[yellow]aborted[/]")
        raise typer.Exit(130)
    code = run_proxy(conn, local, remote, retry, console)
    raise typer.Exit(code)


def _resolve_proxy_target(name_or_host: str) -> ResolvedConnection:
    try:
        cfg, _root = load(name_or_host)
    except FileNotFoundError:
        cfg = Config(host=name_or_host)
    return resolve_connection(cfg)


@app.command("list")
def list_cmd() -> None:
    """List configs found under ./.flux/ and ~/.flux/."""
    names = _list_known_configs()
    if not names:
        console.print("[yellow]no .flux configs found in ./.flux/ or ~/.flux/[/]")
        raise typer.Exit(0)

    table = Table(header_style="bold")
    table.add_column("name", style="cyan")
    table.add_column("host", style="dim")
    table.add_column("items", justify="right")
    table.add_column("path", style="dim")
    for name in names:
        try:
            path = find_config(name)
            cfg, _ = load(name)
            host = f"{cfg.user or '?'}@{cfg.host or '?'}"
            if cfg.port:
                host += f":{cfg.port}"
            items = f"{len(cfg.file)}/{len(cfg.script)}/{len(cfg.block)}"
        except Exception as exc:
            host = f"[red]error: {exc}[/]"
            items = "—"
            path = Path("?")
        table.add_row(name, host, items, str(path))
    console.print(table)
    console.print("[dim]items = file/script/block counts[/]")


@app.command(
    "exec",
    context_settings={"allow_extra_args": True, "ignore_unknown_options": True},
)
def exec_cmd(
    ctx: typer.Context,
    name: str = typer.Argument(
        ..., help="Config name (uses host/user/port/key from yaml).",
        autocompletion=_complete_config_name,
    ),
) -> None:
    """Run a one-off remote command using the connection from <name>.yml.

    Example:
        flux exec westlake df -h
        flux exec westlake -- ls -la /etc
    """
    raw_args = list(ctx.args)
    if raw_args and raw_args[0] == "--":
        raw_args = raw_args[1:]
    if not raw_args:
        err_console.print("[red]error:[/] no command given. Try: flux exec <name> <cmd> [args]")
        raise typer.Exit(2)

    try:
        cfg, _root = load(name)
    except FileNotFoundError as exc:
        err_console.print(f"[red]error:[/] {exc}")
        raise typer.Exit(2)
    try:
        conn = resolve_connection(cfg)
    except (KeyboardInterrupt, EOFError):
        err_console.print("[yellow]aborted[/]")
        raise typer.Exit(130)

    import shlex

    cmd = " ".join(shlex.quote(a) for a in raw_args)
    try:
        client = SshClient(conn)
        client.connect()
    except Exception as exc:
        err_console.print(f"[red]ssh connect failed:[/] {exc}")
        raise typer.Exit(1)
    try:
        status = client.exec_streaming(cmd, use_pty=True)
    except KeyboardInterrupt:
        status = 130
    finally:
        client.close()
    raise typer.Exit(status)


if __name__ == "__main__":
    app()
