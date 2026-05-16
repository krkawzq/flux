"""Shared pytest fixtures. Tests don't need a real SSH server.

A `FakeClient` duck-types whatever sync.* uses on SshClient.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from pathlib import Path

import pytest


@dataclass
class FakeClient:
    """In-memory stand-in for flux.ssh.SshClient.

    All path-taking methods expand `~/...` to `/root/...` so test setup can use
    either form interchangeably (matches real SshClient behavior).
    """

    files: dict[str, bytes] = field(default_factory=dict)
    mtimes: dict[str, float] = field(default_factory=dict)
    modes: dict[str, int] = field(default_factory=dict)
    exec_log: list[str] = field(default_factory=list)
    exec_streaming_log: list[str] = field(default_factory=list)
    exec_streaming_status: int = 0
    ensured_dirs: set[str] = field(default_factory=set)

    def home(self) -> str:
        return "/root"

    def expand(self, path: str) -> str:
        if path == "~":
            return "/root"
        if path.startswith("~/"):
            return "/root/" + path[2:]
        return path

    def exists(self, path: str) -> bool:
        return self.expand(path) in self.files

    def mtime(self, path: str) -> float | None:
        key = self.expand(path)
        if key not in self.files:
            return None
        return self.mtimes.get(key)

    def read_file(self, path: str) -> bytes:
        key = self.expand(path)
        if key not in self.files:
            raise FileNotFoundError(key)
        return self.files[key]

    def write_file(self, path: str, data: bytes, mode: int | None = None) -> None:
        key = self.expand(path)
        self.files[key] = data
        if mode is not None:
            self.modes[key] = mode

    def chmod(self, path: str, mode: int) -> None:
        self.modes[self.expand(path)] = mode

    def ensure_dir(self, path: str) -> None:
        self.ensured_dirs.add(self.expand(path))

    def exec(self, cmd: str):
        from flux.ssh import ExecResult

        self.exec_log.append(cmd)
        return ExecResult(0, b"", b"")

    def exec_streaming(self, cmd: str, *, use_pty: bool = True, **_) -> int:
        self.exec_streaming_log.append(cmd)
        return self.exec_streaming_status


@pytest.fixture
def fake_client() -> FakeClient:
    return FakeClient()


@pytest.fixture
def tmp_asset_root(tmp_path: Path) -> Path:
    for sub in ("files", "scripts", "blocks"):
        (tmp_path / sub).mkdir()
    return tmp_path
