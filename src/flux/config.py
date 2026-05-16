from __future__ import annotations

import os
import shutil
import subprocess
import sys
from pathlib import Path
from typing import Literal

import yaml
from pydantic import BaseModel, ConfigDict, Field
from rich.prompt import Prompt


class FileItem(BaseModel):
    model_config = ConfigDict(extra="forbid")

    name: str | None = None
    src: str
    dst: str
    mode: Literal["sync", "cover"] = "sync"
    chmod: str | None = None


class ScriptItem(BaseModel):
    model_config = ConfigDict(extra="forbid")

    path: str
    interpreter: str | None = None
    flags: list[str] | None = None
    args: list[str] = Field(default_factory=list)
    # If set, run this command on the remote before uploading; exit-0 means
    # the script is already-applied and we should skip. Typical: a `command -v`
    # check or a version probe.
    skip_if: str | None = None


class BlockItem(BaseModel):
    model_config = ConfigDict(extra="forbid")

    name: str
    path: str
    file: str
    mode: Literal["sync", "cover"] = "sync"
    comment_template: str | None = None


class Config(BaseModel):
    model_config = ConfigDict(extra="forbid")

    host: str | None = None
    port: int | None = None
    user: str | None = None
    key: str | None = None
    password: str | None = None
    interpreter: str = "/bin/bash"
    flags: list[str] = Field(default_factory=lambda: ["-l", "-i"])
    comment_template: str = "# {}"

    file: list[FileItem] = Field(default_factory=list)
    script: list[ScriptItem] = Field(default_factory=list)
    block: list[BlockItem] = Field(default_factory=list)


class ResolvedConnection(BaseModel):
    """The fully-filled-in connection info after applying prompts and keychain."""

    model_config = ConfigDict(extra="forbid")

    host: str
    port: int
    user: str
    key: str | None
    password: str | None


def find_config(name_or_path: str) -> Path:
    """Resolve `name` to `./.flux/<name>.yml`, `~/.flux/<name>.yml`, or treat as a path."""
    p = Path(name_or_path).expanduser()
    if p.exists():
        return p.resolve()
    if not p.suffix:
        for base in (Path.cwd() / ".flux", Path.home() / ".flux"):
            candidate = base / f"{name_or_path}.yml"
            if candidate.exists():
                return candidate.resolve()
    raise FileNotFoundError(f"config not found: {name_or_path}")


def load(name_or_path: str) -> tuple[Config, Path]:
    """Load and validate. Returns (config, asset_root). asset_root = yaml's parent dir."""
    path = find_config(name_or_path)
    with path.open("r", encoding="utf-8") as f:
        raw = yaml.safe_load(f) or {}
    cfg = Config.model_validate(raw)
    return cfg, path.parent


def resolve_password(value: str | None) -> str | None:
    """Resolve `keychain:service.account` via system keyring; otherwise return as-is."""
    if value is None:
        return None
    if not value.startswith("keychain:"):
        return value
    spec = value[len("keychain:") :]
    if "." not in spec:
        raise ValueError(f"invalid keychain spec '{spec}'; expected service.account")
    service, account = spec.split(".", 1)
    return _lookup_keychain(service, account)


def _lookup_keychain(service: str, account: str) -> str:
    if sys.platform == "darwin":
        result = subprocess.run(
            ["security", "find-generic-password", "-s", service, "-a", account, "-w"],
            capture_output=True,
            text=True,
        )
        if result.returncode != 0:
            raise RuntimeError(f"keychain lookup failed: {result.stderr.strip()}")
        return result.stdout.strip()
    if sys.platform.startswith("linux") and shutil.which("secret-tool"):
        result = subprocess.run(
            ["secret-tool", "lookup", "service", service, "account", account],
            capture_output=True,
            text=True,
        )
        if result.returncode != 0:
            raise RuntimeError(f"secret-tool lookup failed: {result.stderr.strip()}")
        return result.stdout.strip()
    raise RuntimeError(
        f"no keychain backend on {sys.platform}; install secret-tool or fill password inline"
    )


def resolve_connection(cfg: Config) -> ResolvedConnection:
    """Fill in missing host/port/user/key/password via interactive prompt or env defaults."""
    host = cfg.host or Prompt.ask("Host")
    port = cfg.port if cfg.port is not None else int(Prompt.ask("Port", default="22"))
    user = cfg.user or Prompt.ask("User", default=os.environ.get("USER", "root"))
    key = cfg.key
    password = resolve_password(cfg.password)
    if key is None and password is None:
        choice = Prompt.ask("Auth", choices=["key", "password"], default="key")
        if choice == "key":
            key = Prompt.ask("Key path", default="~/.ssh/id_ed25519")
        else:
            password = Prompt.ask("Password", password=True)
    if key:
        key = str(Path(key).expanduser())
    return ResolvedConnection(host=host, port=port, user=user, key=key, password=password)


class SshConfigConflict(RuntimeError):
    """Raised when the existing ~/.ssh/config has a block that we cannot safely modify."""


def save_to_ssh_config(alias: str, conn: ResolvedConnection) -> Path:
    """Write a Host entry to ~/.ssh/config.

    Behavior:
    - No existing `Host` line for alias → append a fresh block.
    - Existing single-pattern `Host <alias>` block → replace it in place.
    - Existing multi-pattern block like `Host alpha beta` containing alias →
      RAISE SshConfigConflict. Rewriting would silently delete the other
      aliases' settings; the user must split it manually first.
    """
    _validate_ssh_alias(alias)
    host = _ssh_config_scalar("host", conn.host)
    user = _ssh_config_scalar("user", conn.user)
    key = _ssh_config_scalar("key", conn.key) if conn.key else None

    ssh_dir = Path.home() / ".ssh"
    ssh_dir.mkdir(mode=0o700, exist_ok=True)
    cfg_path = ssh_dir / "config"
    if not cfg_path.exists():
        cfg_path.touch()
        try:
            cfg_path.chmod(0o600)
        except OSError:
            pass

    new_block_lines = [
        f"Host {alias}",
        f"    HostName {host}",
        f"    Port {conn.port}",
        f"    User {user}",
    ]
    if key:
        new_block_lines.append(f"    IdentityFile {key}")
    new_block = "\n".join(new_block_lines) + "\n"

    existing = cfg_path.read_text(encoding="utf-8")
    _check_no_multipattern_conflict(existing, alias)
    updated, replaced = _replace_host_block(existing, alias, new_block)
    if not replaced:
        sep = "" if updated.endswith("\n") or updated == "" else "\n"
        updated = updated + sep + ("\n" if updated else "") + new_block
    cfg_path.write_text(updated, encoding="utf-8")
    return cfg_path


def _validate_ssh_alias(alias: str) -> None:
    """`Host <alias>` must remain one single pattern on one line."""
    if (
        not alias
        or alias.strip() != alias
        or alias.startswith("#")
        or any(ch.isspace() for ch in alias)
    ):
        raise ValueError("ssh alias must be a non-empty single Host pattern without whitespace")
    _ssh_config_scalar("alias", alias)


def _ssh_config_scalar(name: str, value: str) -> str:
    """Reject values that can inject extra ssh_config directives."""
    if any(ch in value for ch in ("\r", "\n", "\x00")):
        raise ValueError(f"ssh config {name} cannot contain newline or NUL")
    return value


def _check_no_multipattern_conflict(content: str, alias: str) -> None:
    """Refuse to touch a config if alias is part of a multi-pattern Host line."""
    for line in content.splitlines():
        stripped = line.strip()
        if not stripped or stripped.startswith("#"):
            continue
        parts = stripped.split()
        if len(parts) < 2 or parts[0].lower() != "host":
            continue
        patterns = parts[1:]
        if alias in patterns and len(patterns) > 1:
            raise SshConfigConflict(
                f"~/.ssh/config has `Host {' '.join(patterns)}` covering '{alias}' alongside "
                f"{[p for p in patterns if p != alias]!r}. Refusing to rewrite — that would "
                f"silently delete those other aliases' settings. Split the line manually first."
            )


def _replace_host_block(content: str, alias: str, new_block: str) -> tuple[str, bool]:
    """Replace the single-pattern `Host <alias>` block with new_block.

    Multi-pattern blocks are ignored here (the conflict guard runs first and
    raises before we get to this function); this matcher requires the Host
    line to have exactly one pattern, equal to `alias`.
    """
    lines = content.splitlines(keepends=True)
    out: list[str] = []
    i = 0
    replaced = False
    while i < len(lines):
        line = lines[i]
        if _is_host_line_exactly_for(line, alias):
            i += 1
            while i < len(lines) and not _starts_new_section(lines[i]):
                i += 1
            if not replaced:
                out.append(new_block)
                replaced = True
            while i < len(lines) and lines[i].strip() == "":
                i += 1
            if i < len(lines):
                out.append("\n")
            continue
        out.append(line)
        i += 1
    return "".join(out), replaced


def _is_host_line_exactly_for(line: str, alias: str) -> bool:
    """True if `line` is `Host <alias>` with NO other patterns or wildcards."""
    stripped = line.strip()
    if not stripped or stripped.startswith("#"):
        return False
    parts = stripped.split()
    if len(parts) != 2 or parts[0].lower() != "host":
        return False
    return parts[1] == alias


def _starts_new_section(line: str) -> bool:
    stripped = line.strip()
    if not stripped or stripped.startswith("#"):
        return False
    head = stripped.split()[0].lower()
    return head in ("host", "match")
