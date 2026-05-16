"""Shared pytest fixtures. Tests do NOT require a real SSH server.

For the few tests that exercise SshClient, we use a `FakeClient` duck-type
that satisfies the interface used by sync.* (exists / mtime / read_file /
write_file / exec / exec_streaming / chmod / ensure_dir).
"""

from __future__ import annotations

from dataclasses import dataclass, field
from pathlib import Path

import pytest


@dataclass
class FakeClient:
    """In-memory stand-in for flux.ssh.SshClient. Only what sync.* needs."""

    files: dict[str, bytes] = field(default_factory=dict)
    mtimes: dict[str, float] = field(default_factory=dict)
    modes: dict[str, int] = field(default_factory=dict)
    exec_log: list[str] = field(default_factory=list)
    exec_streaming_log: list[str] = field(default_factory=list)
    exec_streaming_status: int = 0

    def exists(self, path: str) -> bool:
        return path in self.files

    def mtime(self, path: str) -> float | None:
        return self.mtimes.get(path)

    def read_file(self, path: str) -> bytes:
        if path not in self.files:
            raise FileNotFoundError(path)
        return self.files[path]

    def write_file(self, path: str, data: bytes, mode: int | None = None) -> None:
        self.files[path] = data
        if mode is not None:
            self.modes[path] = mode

    def chmod(self, path: str, mode: int) -> None:
        self.modes[path] = mode

    def ensure_dir(self, path: str) -> None:
        pass

    def exec(self, cmd: str):
        self.exec_log.append(cmd)
        from flux.ssh import ExecResult

        return ExecResult(0, b"", b"")

    def exec_streaming(self, cmd: str, *, use_pty: bool = True, **_) -> int:
        self.exec_streaming_log.append(cmd)
        return self.exec_streaming_status

    def home(self) -> str:
        return "/root"

    def expand(self, path: str) -> str:
        if path == "~":
            return "/root"
        if path.startswith("~/"):
            return "/root/" + path[2:]
        return path


@pytest.fixture
def fake_client() -> FakeClient:
    return FakeClient()


@pytest.fixture
def tmp_asset_root(tmp_path: Path) -> Path:
    """Create a .flux-style layout under tmp_path with empty files/scripts/blocks dirs."""
    for sub in ("files", "scripts", "blocks"):
        (tmp_path / sub).mkdir()
    return tmp_path
