"""block stage: idempotently inject code blocks into remote files via sentinel markers.

Sentinel format:
    # >>> NAME:UNIX_TS:BODY_HASH >>>
    <body>
    # <<< NAME:UNIX_TS <<<

Legacy format `# >>> NAME:UNIX_TS >>>` (no hash) is still parsed for back-compat
but writes always emit the new form.

Why the hash: a previous version used only the sentinel timestamp to decide
"was remote hand-edited since we last wrote". That has a hole: if the user
edits the remote body in place, the timestamp doesn't change, so a local
file with a slightly newer mtime would clobber the manual edit. With a body
hash we can detect "remote body differs from what we wrote" deterministically.
"""

from __future__ import annotations

import hashlib
import time
from dataclasses import dataclass
from pathlib import Path

from flux.config import BlockItem, Config
from flux.ssh import SshClient
from flux.sync._retry import retry


class BlockError(RuntimeError):
    pass


@dataclass
class FoundBlock:
    open_start: int
    open_end: int
    close_start: int
    close_end: int
    timestamp: int
    body_hash: str | None  # None when legacy (pre-hash) sentinel


def _body_hash(body: str) -> str:
    """Stable 12-hex-char hash; trailing newlines normalized first."""
    canonical = body.rstrip("\n").encode("utf-8")
    return hashlib.sha256(canonical).hexdigest()[:12]


def build_markers(
    template: str,
    name: str,
    ts: int,
    body_hash: str | None = None,
) -> tuple[str, str]:
    if "{}" not in template:
        raise BlockError("comment_template must contain '{}'")
    open_payload = f">>> {name}:{ts}"
    if body_hash:
        open_payload += f":{body_hash}"
    open_payload += " >>>"
    close_payload = f"<<< {name}:{ts} <<<"
    return template.format(open_payload), template.format(close_payload)


def _scan_markers(template: str, name: str, content: str):
    """Return (opens, closes) for THIS block name.

    opens : list[(start, end, ts, hash_or_None)]
    closes: list[(start, end, ts)]
    """
    if "{}" not in template:
        raise BlockError("comment_template must contain '{}'")
    open_prefix = template.format(f">>> {name}:").rstrip()
    close_prefix = template.format(f"<<< {name}:").rstrip()
    open_suffix = " >>>"
    close_suffix = " <<<"

    raw = content.encode("utf-8")
    opens: list[tuple[int, int, int, str | None]] = []
    closes: list[tuple[int, int, int]] = []
    pos = 0
    while pos < len(raw):
        nl = raw.find(b"\n", pos)
        line_end = len(raw) if nl == -1 else nl + 1
        line = raw[pos:line_end].decode("utf-8").rstrip("\r\n")
        if line.startswith(open_prefix) and line.endswith(open_suffix):
            mid = line[len(open_prefix) : len(line) - len(open_suffix)]
            parts = mid.split(":")
            try:
                ts = int(parts[0])
            except (ValueError, IndexError):
                pos = line_end
                continue
            body_hash = parts[1] if len(parts) >= 2 and parts[1] else None
            opens.append((pos, line_end, ts, body_hash))
        elif line.startswith(close_prefix) and line.endswith(close_suffix):
            mid = line[len(close_prefix) : len(line) - len(close_suffix)]
            try:
                ts = int(mid)
            except ValueError:
                pos = line_end
                continue
            closes.append((pos, line_end, ts))
        pos = line_end
    return opens, closes


def find_block(template: str, name: str, content: str) -> FoundBlock | None:
    opens, closes = _scan_markers(template, name, content)
    if not opens and not closes:
        return None
    if len(opens) != 1 or len(closes) != 1:
        raise BlockError(
            f"sentinel state corrupt for '{name}': {len(opens)} open / "
            f"{len(closes)} close marker(s) found; expected exactly one each. "
            f"Fix the target file manually before re-syncing."
        )
    o_start, o_end, ts, body_hash = opens[0]
    c_start, c_end, close_ts = closes[0]
    if c_start < o_end:
        raise BlockError(f"sentinel order corrupt for '{name}': close before open")
    if close_ts != ts:
        raise BlockError(
            f"sentinel timestamps mismatch for '{name}': open={ts} close={close_ts}. "
            f"This is corrupted state — fix the target file manually before re-syncing."
        )
    return FoundBlock(o_start, o_end, c_start, c_end, ts, body_hash)


def render_block(open_marker: str, body: str, close_marker: str) -> str:
    if body and not body.endswith("\n"):
        body = body + "\n"
    return f"{open_marker}\n{body}{close_marker}\n"


def splice(content: str, found: FoundBlock | None, rendered: str) -> str:
    if found is not None:
        b = content.encode("utf-8")
        # content was strict-decoded upstream so encode/slice/decode is round-trip safe.
        prefix = b[: found.open_start].decode("utf-8")
        suffix = b[found.close_end :].decode("utf-8")
        return prefix + rendered + suffix
    sep = "" if content.endswith("\n") or content == "" else "\n"
    return content + sep + rendered


def _resolve_block_source(asset_root: Path, path: str) -> Path:
    p = Path(path).expanduser()
    if p.is_absolute():
        return p
    candidate = asset_root / path
    if candidate.exists():
        return candidate
    return asset_root / "blocks" / path


def sync_block_group(
    items: list[BlockItem],
    cfg: Config,
    asset_root: Path,
    client: SshClient,
    target_file: str,
):
    """Apply all `items` targeting `target_file`. Returns one Outcome per item."""
    from flux.sync import Outcome

    # one read; strict UTF-8: refuse to mangle binary targets
    def _read() -> str:
        try:
            raw = client.read_file(target_file)
        except FileNotFoundError:
            return ""
        try:
            return raw.decode("utf-8")  # strict
        except UnicodeDecodeError as exc:
            raise BlockError(
                f"remote {target_file} contains non-UTF-8 bytes at offset {exc.start} "
                f"({exc.reason}). Block sync requires UTF-8; refusing to round-trip "
                f"through decode/encode and corrupt the file."
            )

    try:
        original = retry(_read)
    except BlockError as exc:
        # Decode failure is per-target, not per-item. Mark every item in this
        # group as failed with the same reason so the user can see one error
        # per affected block in the summary.
        return [
            Outcome("failed", item.name, f"{type(exc).__name__}: {exc}")
            for item in items
        ]
    target_existed = original != "" or retry(lambda: client.exists(target_file))
    current = original
    outcomes: list[Outcome] = []

    for item in items:
        try:
            outcome, current = _apply_one(item, cfg, asset_root, current, target_file)
        except Exception as exc:
            outcomes.append(
                Outcome(
                    status="failed",
                    label=item.name,
                    detail=f"{type(exc).__name__}: {exc}",
                )
            )
        else:
            outcomes.append(outcome)

    if current != original:
        if target_existed:
            backup = f"{target_file}.flux-{int(time.time())}.bak"
            try:
                retry(lambda: client.write_file(backup, original.encode("utf-8")))
            except Exception:
                from rich.console import Console

                Console(stderr=True).print(
                    f"[yellow]warn:[/] could not backup {target_file} → {backup}"
                )
        retry(lambda: client.write_file(target_file, current.encode("utf-8")))

    return outcomes


def _apply_one(
    item: BlockItem,
    cfg: Config,
    asset_root: Path,
    current: str,
    target_file: str,
):
    from flux.sync import Outcome

    local = _resolve_block_source(asset_root, item.path)
    if not local.exists():
        raise FileNotFoundError(
            f"block source not found: tried {asset_root / item.path} and "
            f"{asset_root / 'blocks' / item.path}"
        )

    body = local.read_text(encoding="utf-8")
    template = item.comment_template or cfg.comment_template
    found = find_block(template, item.name, current)

    if item.mode == "cover" and found is not None:
        return Outcome("skipped", item.name, f"cover: present in {target_file}"), current

    if found is not None:
        prev_body = current.encode("utf-8")[found.open_end : found.close_start].decode("utf-8")
        body_changed = prev_body.rstrip("\n") != body.rstrip("\n")
        if not body_changed:
            return Outcome("skipped", item.name, f"unchanged in {target_file}"), current

        if item.mode == "sync":
            # Hand-edit detection.
            #   - new sentinels carry body_hash; compare against current remote body
            #     to definitively answer "did remote change since we last wrote?"
            #   - legacy sentinels (no hash); fall back to timestamp-vs-mtime heuristic
            if found.body_hash is not None:
                actual_hash = _body_hash(prev_body)
                if actual_hash != found.body_hash:
                    return (
                        Outcome(
                            "skipped",
                            item.name,
                            f"remote hand-edited in {target_file} "
                            f"(hash {actual_hash} != recorded {found.body_hash}); not clobbering",
                        ),
                        current,
                    )
            else:
                local_mtime = local.stat().st_mtime
                if found.timestamp + 1.0 >= local_mtime:
                    return (
                        Outcome(
                            "skipped",
                            item.name,
                            f"legacy sentinel ts={found.timestamp} >= local mtime; not clobbering",
                        ),
                        current,
                    )

    ts = int(time.time())
    new_hash = _body_hash(body)
    open_marker, close_marker = build_markers(template, item.name, ts, new_hash)
    rendered = render_block(open_marker, body, close_marker)
    new_current = splice(current, found, rendered)
    action = "updated" if found is not None else "injected"
    return Outcome("applied", item.name, f"{action} in {target_file}"), new_current
