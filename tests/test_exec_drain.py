"""SshClient.exec must drain stdout and stderr concurrently.

Naïve `stdout.read(); stderr.read()` deadlocks when the remote writes more
to stderr than the channel's stderr buffer can hold while we're still
blocked reading stdout. This test simulates that exact pattern via mocks.
"""

from __future__ import annotations

from unittest.mock import MagicMock, patch

from flux.config import ResolvedConnection
from flux.ssh import ExecResult, SshClient


def _build_client() -> SshClient:
    conn = ResolvedConnection(host="x", port=22, user="root", key=None, password="p")
    c = SshClient(conn)
    c._client = MagicMock()
    return c


def test_exec_drains_stdout_and_stderr_in_one_call() -> None:
    """Both streams must be read in the same select loop, not sequentially."""
    client = _build_client()
    channel = MagicMock()

    # Sequence: first poll yields both streams ready; after that channel reports exit.
    channel.exit_status_ready.side_effect = [False, True, True]
    channel.recv_ready.side_effect = [True, False, False]
    channel.recv_stderr_ready.side_effect = [True, False, False]
    channel.recv.return_value = b"OUT"
    channel.recv_stderr.return_value = b"ERR"
    channel.recv_exit_status.return_value = 0
    client._client.get_transport().open_session.return_value = channel

    with patch("flux.ssh.select.select", return_value=([channel], [], [])):
        r = client.exec("echo hi")

    assert r.status == 0
    assert r.stdout == b"OUT"
    assert r.stderr == b"ERR"
    channel.close.assert_called_once()


def test_exec_returns_exit_status_through_ExecResult() -> None:
    client = _build_client()
    channel = MagicMock()
    channel.exit_status_ready.return_value = True
    channel.recv_ready.return_value = False
    channel.recv_stderr_ready.return_value = False
    channel.recv_exit_status.return_value = 42
    client._client.get_transport().open_session.return_value = channel

    with patch("flux.ssh.select.select", return_value=([], [], [])):
        r = client.exec("false")
    assert isinstance(r, ExecResult)
    assert r.status == 42
    assert not r.ok


def test_exec_closes_channel_even_on_exception() -> None:
    """Channel must always be released even if recv raises."""
    client = _build_client()
    channel = MagicMock()
    channel.exit_status_ready.side_effect = RuntimeError("blip")
    client._client.get_transport().open_session.return_value = channel

    import pytest

    with pytest.raises(RuntimeError):
        client.exec("anything")
    channel.close.assert_called_once()
