"""sync_file behaviour against FakeClient — covers mode + mtime + chmod parsing."""

from __future__ import annotations

import time
from pathlib import Path

import pytest
from rich.console import Console

from flux.config import FileItem
from flux.sync.file import sync_file


@pytest.fixture
def console() -> Console:
    return Console(record=True, width=120)


def test_uploads_when_remote_missing(tmp_asset_root: Path, fake_client, console: Console) -> None:
    src = tmp_asset_root / "files" / "a.txt"
    src.write_bytes(b"hello")
    item = FileItem(src="a.txt", dst="/root/a.txt")
    sync_file(item, tmp_asset_root, fake_client, console)
    assert fake_client.files["/root/a.txt"] == b"hello"


def test_skips_when_remote_newer_in_sync_mode(
    tmp_asset_root: Path, fake_client, console: Console
) -> None:
    src = tmp_asset_root / "files" / "a.txt"
    src.write_bytes(b"hello")
    # remote pretends to be 1 hour ahead
    fake_client.files["/root/a.txt"] = b"OLD"
    fake_client.mtimes["/root/a.txt"] = time.time() + 3600
    item = FileItem(src="a.txt", dst="/root/a.txt", mode="sync")
    sync_file(item, tmp_asset_root, fake_client, console)
    assert fake_client.files["/root/a.txt"] == b"OLD"  # untouched


def test_overwrites_when_local_newer_in_sync_mode(
    tmp_asset_root: Path, fake_client, console: Console
) -> None:
    src = tmp_asset_root / "files" / "a.txt"
    src.write_bytes(b"new-content")
    fake_client.files["/root/a.txt"] = b"OLD"
    fake_client.mtimes["/root/a.txt"] = time.time() - 3600
    item = FileItem(src="a.txt", dst="/root/a.txt", mode="sync")
    sync_file(item, tmp_asset_root, fake_client, console)
    assert fake_client.files["/root/a.txt"] == b"new-content"


def test_cover_skips_when_remote_exists(
    tmp_asset_root: Path, fake_client, console: Console
) -> None:
    src = tmp_asset_root / "files" / "a.txt"
    src.write_bytes(b"hello")
    fake_client.files["/root/a.txt"] = b"OLD"
    item = FileItem(src="a.txt", dst="/root/a.txt", mode="cover")
    sync_file(item, tmp_asset_root, fake_client, console)
    assert fake_client.files["/root/a.txt"] == b"OLD"


def test_cover_writes_when_remote_missing(
    tmp_asset_root: Path, fake_client, console: Console
) -> None:
    src = tmp_asset_root / "files" / "a.txt"
    src.write_bytes(b"x")
    item = FileItem(src="a.txt", dst="/root/a.txt", mode="cover")
    sync_file(item, tmp_asset_root, fake_client, console)
    assert fake_client.files["/root/a.txt"] == b"x"


def test_chmod_parsed_as_octal(tmp_asset_root: Path, fake_client, console: Console) -> None:
    src = tmp_asset_root / "files" / "key"
    src.write_bytes(b"secret")
    item = FileItem(src="key", dst="/root/.ssh/id", chmod="600")
    sync_file(item, tmp_asset_root, fake_client, console)
    assert fake_client.modes["/root/.ssh/id"] == 0o600


def test_raises_when_src_missing(
    tmp_asset_root: Path, fake_client, console: Console
) -> None:
    item = FileItem(src="ghost.txt", dst="/root/ghost.txt")
    with pytest.raises(FileNotFoundError):
        sync_file(item, tmp_asset_root, fake_client, console)


def test_resolves_src_from_files_subdir_when_bare(
    tmp_asset_root: Path, fake_client, console: Console
) -> None:
    """Bare 'foo.txt' falls back to <asset_root>/files/foo.txt if not at root."""
    (tmp_asset_root / "files" / "foo.txt").write_bytes(b"sub")
    item = FileItem(src="foo.txt", dst="/root/foo.txt")
    sync_file(item, tmp_asset_root, fake_client, console)
    assert fake_client.files["/root/foo.txt"] == b"sub"
