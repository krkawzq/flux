"""write_file must NOT delete the existing target when posix_rename fails."""

from __future__ import annotations

from unittest.mock import MagicMock

import pytest

from flux.config import ResolvedConnection
from flux.ssh import SshClient


def _make_client(sftp_mock: MagicMock) -> SshClient:
    conn = ResolvedConnection(host="x", port=22, user="root", key=None, password="p")
    c = SshClient(conn)
    c._client = MagicMock()
    c._sftp = sftp_mock
    c._home = "/root"
    # mark parent dir as already-ensured so write_file doesn't call exec
    c._ensured_dirs.add("/root")
    return c


def test_posix_rename_permission_error_does_not_delete_target() -> None:
    """If posix_rename fails (e.g. EACCES), target file must remain on remote."""
    sftp = MagicMock()
    fake_open = MagicMock()
    fake_open.__enter__ = lambda self: MagicMock(write=MagicMock())
    fake_open.__exit__ = lambda *a: None
    sftp.open.return_value = fake_open
    sftp.posix_rename.side_effect = PermissionError("EACCES")

    client = _make_client(sftp)
    with pytest.raises(PermissionError):
        client.write_file("/root/important.txt", b"new")

    target_removes = [call for call in sftp.remove.call_args_list
                      if call.args and call.args[0] == "/root/important.txt"]
    assert target_removes == [], "write_file destroyed the existing target on a permission failure"
    for call in sftp.remove.call_args_list:
        assert ".flux-" in call.args[0] and call.args[0].endswith(".tmp")


def test_posix_rename_generic_io_error_propagates_keeps_target() -> None:
    """Generic IOError (transport blip etc) must propagate, NOT fall back to remove+rename."""
    sftp = MagicMock()
    fake_open = MagicMock()
    fake_open.__enter__ = lambda self: MagicMock(write=MagicMock())
    fake_open.__exit__ = lambda *a: None
    sftp.open.return_value = fake_open
    sftp.posix_rename.side_effect = IOError("transport blip")  # NOT "unsupported"

    client = _make_client(sftp)
    with pytest.raises(IOError):
        client.write_file("/root/important.txt", b"new")

    target_removes = [call for call in sftp.remove.call_args_list
                      if call.args and call.args[0] == "/root/important.txt"]
    assert target_removes == []
    # fallback should NOT have been triggered → rename never called
    assert sftp.rename.call_count == 0


def test_posix_rename_unsupported_falls_back() -> None:
    """When server returns 'Operation unsupported' for posix_rename, use fallback."""
    sftp = MagicMock()
    fake_open = MagicMock()
    fake_open.__enter__ = lambda self: MagicMock(write=MagicMock())
    fake_open.__exit__ = lambda *a: None
    sftp.open.return_value = fake_open
    sftp.posix_rename.side_effect = IOError("Operation unsupported")
    sftp.rename.return_value = None  # plain rename succeeds

    client = _make_client(sftp)
    client.write_file("/root/important.txt", b"new")

    sftp.rename.assert_called_once()


def test_attribute_error_falls_back_to_rename_keeping_target_until_safe() -> None:
    """Old paramiko without posix_rename: try sftp.rename first (preserves target)."""
    sftp = MagicMock()
    fake_open = MagicMock()
    fake_open.__enter__ = lambda self: MagicMock(write=MagicMock())
    fake_open.__exit__ = lambda *a: None
    sftp.open.return_value = fake_open
    sftp.posix_rename.side_effect = AttributeError("no posix_rename")
    # sftp.rename succeeds → target replaced; remove never called
    sftp.rename.return_value = None

    client = _make_client(sftp)
    client.write_file("/root/important.txt", b"new")

    sftp.rename.assert_called_once()
    target_removes = [call for call in sftp.remove.call_args_list
                      if call.args and call.args[0] == "/root/important.txt"]
    assert target_removes == []


def test_attribute_error_then_rename_target_exists_falls_to_remove() -> None:
    """If old-style rename fails AND we're in the AttributeError branch, only
    THEN do we remove the target as the last-resort fallback."""
    sftp = MagicMock()
    fake_open = MagicMock()
    fake_open.__enter__ = lambda self: MagicMock(write=MagicMock())
    fake_open.__exit__ = lambda *a: None
    sftp.open.return_value = fake_open
    sftp.posix_rename.side_effect = AttributeError("no posix_rename")
    # first rename fails (target exists), then succeeds after remove
    sftp.rename.side_effect = [IOError("exists"), None]

    client = _make_client(sftp)
    client.write_file("/root/important.txt", b"new")

    # remove was called on target as last-resort
    target_removes = [call for call in sftp.remove.call_args_list
                      if call.args and call.args[0] == "/root/important.txt"]
    assert len(target_removes) == 1
