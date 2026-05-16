"""script stage: upload a local script to /tmp on remote and run it with live output."""

from __future__ import annotations

import os
import secrets
import shlex
from pathlib import Path

from rich.console import Console

from flux.config import Config, ScriptItem
from flux.ssh import SshClient
from flux.sync._retry import retry


class ScriptFailed(RuntimeError):
    def __init__(self, item: str, status: int) -> None:
        super().__init__(f"{item} exited with status {status}")
        self.item = item
        self.status = status


def _resolve_script(asset_root: Path, path: str) -> Path:
    p = Path(path).expanduser()
    if p.is_absolute() and p.exists():
        return p
    candidate = asset_root / path
    if candidate.exists():
        return candidate
    return asset_root / "scripts" / path


def sync_script(
    item: ScriptItem,
    cfg: Config,
    asset_root: Path,
    client: SshClient,
    console: Console,
):
    from flux.sync import Outcome

    local = _resolve_script(asset_root, item.path)
    if not local.exists():
        raise FileNotFoundError(f"script not found: {local}")

    # skip_if probe: a tiny non-streaming exec; exit-0 means already-applied.
    if item.skip_if:
        probe = retry(lambda: client.exec(item.skip_if))
        if probe.ok:
            return Outcome("skipped", item.path, f"skip_if matched: {item.skip_if}")

    data = local.read_bytes()
    interpreter = item.interpreter or cfg.interpreter
    flags = item.flags if item.flags is not None else cfg.flags

    safe_name = "".join(c if c.isalnum() else "_" for c in item.path)
    remote_path = f"/tmp/flux_script_{os.getpid()}_{secrets.token_hex(4)}_{safe_name}"

    retry(lambda: client.write_file(remote_path, data, mode=0o755))

    argv = [interpreter, *flags, remote_path, *item.args]
    cmd = " ".join(shlex.quote(part) for part in argv)
    console.print(f"[cyan]▶ running[/] {item.path}")
    try:
        status = client.exec_streaming(cmd, use_pty=True)
    finally:
        # cleanup must run even on exec_streaming exceptions / KeyboardInterrupt
        try:
            client.exec(f"rm -f {shlex.quote(remote_path)}")
        except Exception:
            pass

    if status != 0:
        raise ScriptFailed(item.path, status)
    return Outcome("applied", item.path, "exit 0")
