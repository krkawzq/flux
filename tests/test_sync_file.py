"""sync_file behaviour against FakeClient — covers mode + mtime + chmod parsing."""

from __future__ import annotations

import time
from pathlib import Path

import pytest

from flux.config import FileItem
from flux.sync.file import _parse_octal, sync_file


def test_uploads_when_remote_missing(tmp_asset_root: Path, fake_client) -> None:
    src = tmp_asset_root / "files" / "a.txt"
    src.write_bytes(b"hello")
    item = FileItem(src="a.txt", dst="/root/a.txt")
    o = sync_file(item, tmp_asset_root, fake_client)
    assert o.status == "applied"
    assert fake_client.files["/root/a.txt"] == b"hello"


def test_skips_when_remote_newer_in_sync_mode(tmp_asset_root: Path, fake_client) -> None:
    src = tmp_asset_root / "files" / "a.txt"
    src.write_bytes(b"hello")
    fake_client.files["/root/a.txt"] = b"OLD"
    fake_client.mtimes["/root/a.txt"] = time.time() + 3600
    item = FileItem(src="a.txt", dst="/root/a.txt", mode="sync")
    o = sync_file(item, tmp_asset_root, fake_client)
    assert o.status == "skipped"
    assert fake_client.files["/root/a.txt"] == b"OLD"


def test_overwrites_when_local_newer_in_sync_mode(tmp_asset_root: Path, fake_client) -> None:
    src = tmp_asset_root / "files" / "a.txt"
    src.write_bytes(b"new-content")
    fake_client.files["/root/a.txt"] = b"OLD"
    fake_client.mtimes["/root/a.txt"] = time.time() - 3600
    item = FileItem(src="a.txt", dst="/root/a.txt", mode="sync")
    o = sync_file(item, tmp_asset_root, fake_client)
    assert o.status == "applied"
    assert fake_client.files["/root/a.txt"] == b"new-content"


def test_mtime_tolerance_skips_near_equal(tmp_asset_root: Path, fake_client) -> None:
    """Local 0.5s newer than remote should still skip — within 1s tolerance."""
    src = tmp_asset_root / "files" / "a.txt"
    src.write_bytes(b"x")
    local_mtime = src.stat().st_mtime
    fake_client.files["/root/a.txt"] = b"OLD"
    fake_client.mtimes["/root/a.txt"] = local_mtime - 0.5
    item = FileItem(src="a.txt", dst="/root/a.txt")
    o = sync_file(item, tmp_asset_root, fake_client)
    assert o.status == "skipped"


def test_cover_skips_when_remote_exists(tmp_asset_root: Path, fake_client) -> None:
    src = tmp_asset_root / "files" / "a.txt"
    src.write_bytes(b"hello")
    fake_client.files["/root/a.txt"] = b"OLD"
    fake_client.mtimes["/root/a.txt"] = time.time()
    item = FileItem(src="a.txt", dst="/root/a.txt", mode="cover")
    o = sync_file(item, tmp_asset_root, fake_client)
    assert o.status == "skipped"
    assert fake_client.files["/root/a.txt"] == b"OLD"


def test_cover_writes_when_remote_missing(tmp_asset_root: Path, fake_client) -> None:
    src = tmp_asset_root / "files" / "a.txt"
    src.write_bytes(b"x")
    item = FileItem(src="a.txt", dst="/root/a.txt", mode="cover")
    o = sync_file(item, tmp_asset_root, fake_client)
    assert o.status == "applied"
    assert fake_client.files["/root/a.txt"] == b"x"


def test_chmod_parsed_as_octal(tmp_asset_root: Path, fake_client) -> None:
    src = tmp_asset_root / "files" / "key"
    src.write_bytes(b"secret")
    item = FileItem(src="key", dst="/root/.ssh/id", chmod="600")
    sync_file(item, tmp_asset_root, fake_client)
    assert fake_client.modes["/root/.ssh/id"] == 0o600


def test_raises_when_src_missing(tmp_asset_root: Path, fake_client) -> None:
    item = FileItem(src="ghost.txt", dst="/root/ghost.txt")
    with pytest.raises(FileNotFoundError):
        sync_file(item, tmp_asset_root, fake_client)


def test_resolves_src_from_files_subdir_when_bare(tmp_asset_root: Path, fake_client) -> None:
    (tmp_asset_root / "files" / "foo.txt").write_bytes(b"sub")
    item = FileItem(src="foo.txt", dst="/root/foo.txt")
    sync_file(item, tmp_asset_root, fake_client)
    assert fake_client.files["/root/foo.txt"] == b"sub"


# ---- _parse_octal unit tests ----

@pytest.mark.parametrize("s,expected", [
    ("600", 0o600),
    ("755", 0o755),
    ("0o600", 0o600),
    ("0600", 0o600),
    ("0", 0),
])
def test_parse_octal_accepts_common_forms(s: str, expected: int) -> None:
    assert _parse_octal(s) == expected


@pytest.mark.parametrize("bad", ["abc", "9", "0xff", ""])
def test_parse_octal_rejects_garbage(bad: str) -> None:
    with pytest.raises(ValueError):
        _parse_octal(bad)
