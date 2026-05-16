"""SshClient.exists / mtime must ONLY swallow FileNotFoundError."""

from __future__ import annotations

from unittest.mock import MagicMock

import pytest

from flux.config import ResolvedConnection
from flux.ssh import SshClient


def _client_with_fake_sftp(sftp: MagicMock) -> SshClient:
    conn = ResolvedConnection(host="x", port=22, user="root", key=None, password="pw")
    c = SshClient(conn)
    c._client = MagicMock()
    c._sftp = sftp
    c._home = "/root"
    return c


def test_exists_true_when_stat_succeeds() -> None:
    sftp = MagicMock()
    sftp.stat.return_value = MagicMock(st_mtime=123)
    c = _client_with_fake_sftp(sftp)
    assert c.exists("/x") is True


def test_exists_false_only_on_FileNotFoundError() -> None:
    sftp = MagicMock()
    sftp.stat.side_effect = FileNotFoundError()
    c = _client_with_fake_sftp(sftp)
    assert c.exists("/x") is False


def test_exists_propagates_permission_denied() -> None:
    """EACCES must surface — silently treating as 'missing' would mask real bugs."""
    sftp = MagicMock()
    sftp.stat.side_effect = PermissionError("Permission denied")
    c = _client_with_fake_sftp(sftp)
    with pytest.raises(PermissionError):
        c.exists("/x")


def test_exists_propagates_generic_ioerror() -> None:
    sftp = MagicMock()
    sftp.stat.side_effect = IOError("transport blip")
    c = _client_with_fake_sftp(sftp)
    with pytest.raises(IOError):
        c.exists("/x")


def test_mtime_none_only_when_missing() -> None:
    sftp = MagicMock()
    sftp.stat.side_effect = FileNotFoundError()
    c = _client_with_fake_sftp(sftp)
    assert c.mtime("/x") is None


def test_mtime_propagates_permission_denied() -> None:
    sftp = MagicMock()
    sftp.stat.side_effect = PermissionError("EACCES")
    c = _client_with_fake_sftp(sftp)
    with pytest.raises(PermissionError):
        c.mtime("/x")


def test_mtime_returns_float_when_stat_ok() -> None:
    sftp = MagicMock()
    sftp.stat.return_value = MagicMock(st_mtime=1700000000)
    c = _client_with_fake_sftp(sftp)
    assert c.mtime("/x") == 1700000000.0
