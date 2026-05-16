"""sync_script integration against FakeClient."""

from __future__ import annotations

from pathlib import Path

import pytest
from rich.console import Console

from flux.config import Config, ScriptItem
from flux.sync.script import ScriptFailed, sync_script


def _make_cfg() -> Config:
    return Config(host="x", user="y")


def test_uploads_then_runs(tmp_asset_root: Path, fake_client) -> None:
    (tmp_asset_root / "scripts" / "hello.sh").write_bytes(b"echo hi\n")
    item = ScriptItem(path="hello.sh")
    sync_script(item, _make_cfg(), tmp_asset_root, fake_client, Console(record=True))
    # exactly one streaming exec; one uploaded file under /tmp/flux_script_*
    assert len(fake_client.exec_streaming_log) == 1
    uploaded = [p for p in fake_client.files if p.startswith("/tmp/flux_script_")]
    assert len(uploaded) == 1
    assert fake_client.files[uploaded[0]] == b"echo hi\n"


def test_uses_per_item_interpreter_override(tmp_asset_root: Path, fake_client) -> None:
    (tmp_asset_root / "scripts" / "x.zsh").write_text("print hello\n", encoding="utf-8")
    item = ScriptItem(path="x.zsh", interpreter="/bin/zsh", flags=["-il"])
    sync_script(item, _make_cfg(), tmp_asset_root, fake_client, Console(record=True))
    assert fake_client.exec_streaming_log[0].startswith("/bin/zsh -il ")


def test_appends_args(tmp_asset_root: Path, fake_client) -> None:
    (tmp_asset_root / "scripts" / "x.sh").write_text("#!/bin/sh\n", encoding="utf-8")
    item = ScriptItem(path="x.sh", args=["--flag", "value"])
    sync_script(item, _make_cfg(), tmp_asset_root, fake_client, Console(record=True))
    cmd = fake_client.exec_streaming_log[0]
    assert "--flag value" in cmd


def test_raises_script_failed_on_nonzero_exit(tmp_asset_root: Path, fake_client) -> None:
    (tmp_asset_root / "scripts" / "x.sh").write_text("exit 2\n", encoding="utf-8")
    fake_client.exec_streaming_status = 2
    item = ScriptItem(path="x.sh")
    with pytest.raises(ScriptFailed) as info:
        sync_script(item, _make_cfg(), tmp_asset_root, fake_client, Console(record=True))
    assert info.value.status == 2


def test_raises_when_script_source_missing(tmp_asset_root: Path, fake_client) -> None:
    item = ScriptItem(path="ghost.sh")
    with pytest.raises(FileNotFoundError):
        sync_script(item, _make_cfg(), tmp_asset_root, fake_client, Console(record=True))
