"""sync_script integration against FakeClient."""

from __future__ import annotations

from pathlib import Path

import pytest
from rich.console import Console

from flux.config import Config, ScriptItem
from flux.sync.script import ScriptFailed, sync_script


def _cfg() -> Config:
    return Config(host="x", user="y")


def _console() -> Console:
    return Console(record=True, width=120)


def test_uploads_then_runs(tmp_asset_root: Path, fake_client) -> None:
    (tmp_asset_root / "scripts" / "hello.sh").write_bytes(b"echo hi\n")
    item = ScriptItem(path="hello.sh")
    o = sync_script(item, _cfg(), tmp_asset_root, fake_client, _console())
    assert o.status == "applied"
    assert len(fake_client.exec_streaming_log) == 1
    uploaded = [p for p in fake_client.files if p.startswith("/tmp/flux_script_")]
    assert len(uploaded) == 1
    assert fake_client.files[uploaded[0]] == b"echo hi\n"


def test_uses_per_item_interpreter_override(tmp_asset_root: Path, fake_client) -> None:
    (tmp_asset_root / "scripts" / "x.zsh").write_text("print hello\n", encoding="utf-8")
    item = ScriptItem(path="x.zsh", interpreter="/bin/zsh", flags=["-il"])
    sync_script(item, _cfg(), tmp_asset_root, fake_client, _console())
    assert fake_client.exec_streaming_log[0].startswith("/bin/zsh -il ")


def test_appends_args(tmp_asset_root: Path, fake_client) -> None:
    (tmp_asset_root / "scripts" / "x.sh").write_text("#!/bin/sh\n", encoding="utf-8")
    item = ScriptItem(path="x.sh", args=["--flag", "value"])
    sync_script(item, _cfg(), tmp_asset_root, fake_client, _console())
    cmd = fake_client.exec_streaming_log[0]
    assert "--flag value" in cmd


def test_raises_script_failed_on_nonzero_exit(tmp_asset_root: Path, fake_client) -> None:
    (tmp_asset_root / "scripts" / "x.sh").write_text("exit 2\n", encoding="utf-8")
    fake_client.exec_streaming_status = 2
    item = ScriptItem(path="x.sh")
    with pytest.raises(ScriptFailed) as info:
        sync_script(item, _cfg(), tmp_asset_root, fake_client, _console())
    assert info.value.status == 2


def test_raises_when_script_source_missing(tmp_asset_root: Path, fake_client) -> None:
    item = ScriptItem(path="ghost.sh")
    with pytest.raises(FileNotFoundError):
        sync_script(item, _cfg(), tmp_asset_root, fake_client, _console())


# ---- skip_if ----

def test_skip_if_exit_zero_skips_and_does_not_run(tmp_asset_root: Path, fake_client) -> None:
    """skip_if exits 0 → already-applied → skip; script body is not even uploaded."""
    (tmp_asset_root / "scripts" / "install_node.sh").write_bytes(b"...\n")
    fake_client.exec_log_status = 0  # FakeClient.exec always returns 0
    item = ScriptItem(path="install_node.sh", skip_if="command -v node")
    o = sync_script(item, _cfg(), tmp_asset_root, fake_client, _console())
    assert o.status == "skipped"
    assert "skip_if matched" in o.detail
    # nothing should have been uploaded or streamed
    assert not any(p.startswith("/tmp/flux_script_") for p in fake_client.files)
    assert fake_client.exec_streaming_log == []
    # exactly one exec call: the skip_if probe
    assert fake_client.exec_log == ["command -v node"]


def test_skip_if_exit_nonzero_runs_normally(tmp_asset_root: Path, fake_client) -> None:
    """skip_if non-zero → run as usual."""

    # make FakeClient.exec return non-zero ONLY for the probe
    def exec_with_probe(cmd: str):
        from flux.ssh import ExecResult

        fake_client.exec_log.append(cmd)
        if cmd == "command -v node":
            return ExecResult(1, b"", b"")
        return ExecResult(0, b"", b"")

    fake_client.exec = exec_with_probe
    (tmp_asset_root / "scripts" / "install_node.sh").write_bytes(b"echo hi\n")
    item = ScriptItem(path="install_node.sh", skip_if="command -v node")
    o = sync_script(item, _cfg(), tmp_asset_root, fake_client, _console())
    assert o.status == "applied"
    assert len(fake_client.exec_streaming_log) == 1
