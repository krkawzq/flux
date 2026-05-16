"""file stage: upload a single local file to a remote path with mtime-based skip."""

from __future__ import annotations

from pathlib import Path

from flux.config import FileItem
from flux.ssh import SshClient
from flux.sync._retry import retry

# Tolerance for mtime comparison: clock skew + SFTP integer-second truncation.
# If local is at most this many seconds newer than remote, we treat as unchanged.
_MTIME_TOL_SEC = 1.0


def _resolve_src(asset_root: Path, src: str) -> Path:
    """Resolve src: absolute / ~ / relative-to-asset-root / under <asset>/files/."""
    p = Path(src).expanduser()
    if p.is_absolute():
        return p
    candidate = asset_root / src
    if not candidate.exists():
        in_files = asset_root / "files" / src
        if in_files.exists():
            return in_files
    return candidate


def sync_file(item: FileItem, asset_root: Path, client: SshClient):
    """Upload item.src → item.dst with `mode` semantics. Returns Outcome.

    - mode=cover: write only if remote doesn't exist
    - mode=sync:  write if remote missing OR local mtime > remote mtime (with tolerance)
    """
    # imported here to avoid circular import (sync/__init__ imports this module)
    from flux.sync import Outcome

    label = item.name or item.src
    local = _resolve_src(asset_root, item.src)
    if not local.exists():
        raise FileNotFoundError(f"source not found: {local}")
    if not local.is_file():
        raise IsADirectoryError(f"only single files supported: {local}")

    # one round-trip: mtime() returns None when remote is missing
    remote_mtime = retry(lambda: client.mtime(item.dst))
    remote_exists = remote_mtime is not None

    if item.mode == "cover" and remote_exists:
        return Outcome("skipped", label, "cover: remote present")

    if item.mode == "sync" and remote_exists:
        assert remote_mtime is not None
        local_mtime = local.stat().st_mtime
        if remote_mtime + _MTIME_TOL_SEC >= local_mtime:
            return Outcome("skipped", label, "remote up-to-date")

    data = local.read_bytes()
    mode_int = _parse_octal(item.chmod) if item.chmod else None
    retry(lambda: client.write_file(item.dst, data, mode=mode_int))
    return Outcome("applied", label, f"→ {item.dst} ({_human_bytes(len(data))})")


def _parse_octal(s: str) -> int:
    """Parse '600' / '0o600' / '0600' as 8-base; raise ValueError on garbage."""
    raw = s.strip()
    if raw.startswith(("0o", "0O")):
        raw = raw[2:]
    elif raw.startswith("0") and len(raw) > 1:
        raw = raw[1:]
    return int(raw, 8)


def _human_bytes(n: int) -> str:
    for unit in ("B", "KiB", "MiB", "GiB"):
        if n < 1024:
            return f"{n:.0f} {unit}" if unit == "B" else f"{n:.1f} {unit}"
        n /= 1024  # type: ignore[assignment]
    return f"{n:.1f} TiB"
