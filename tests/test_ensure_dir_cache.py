"""ensure_dir should issue at most one mkdir per remote path per client lifetime."""

from __future__ import annotations

from unittest.mock import MagicMock, patch

from flux.config import ResolvedConnection
from flux.ssh import ExecResult, SshClient


def _build_client() -> SshClient:
    conn = ResolvedConnection(host="x", port=22, user="root", key=None, password="pw")
    return SshClient(conn)


def test_ensure_dir_caches_repeat_calls() -> None:
    client = _build_client()
    # bypass connect — fake the underlying SSHClient
    client._client = MagicMock()
    client._home = "/root"  # don't trigger home() round-trip

    with patch.object(client, "exec", return_value=ExecResult(0, b"", b"")) as ex:
        client.ensure_dir("/var/lib/x")
        client.ensure_dir("/var/lib/x")
        client.ensure_dir("/var/lib/x")
    assert ex.call_count == 1


def test_ensure_dir_distinct_paths_are_separate_calls() -> None:
    client = _build_client()
    client._client = MagicMock()
    client._home = "/root"

    with patch.object(client, "exec", return_value=ExecResult(0, b"", b"")) as ex:
        client.ensure_dir("/var/a")
        client.ensure_dir("/var/b")
        client.ensure_dir("/var/a")
    assert ex.call_count == 2


def test_expand_tilde_uses_cached_home() -> None:
    client = _build_client()
    client._home = "/home/wzq"
    assert client.expand("~") == "/home/wzq"
    assert client.expand("~/foo/bar") == "/home/wzq/foo/bar"
    assert client.expand("/absolute") == "/absolute"
