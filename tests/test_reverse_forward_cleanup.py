"""When local connect fails inside the forward handler, the socket must be closed."""

from __future__ import annotations

from unittest.mock import MagicMock, patch

from flux.config import ResolvedConnection
from flux.ssh import SshClient


def _build_client() -> SshClient:
    conn = ResolvedConnection(host="x", port=22, user="root", key=None, password="pw")
    c = SshClient(conn)
    c._client = MagicMock()
    # transport has a real-ish request_port_forward we can capture
    c._client.get_transport.return_value = MagicMock()
    return c


def test_handler_closes_socket_when_connect_fails() -> None:
    """When socket.connect raises (e.g. local proxy not up yet) the FD must be released."""
    client = _build_client()
    captured: dict = {}

    def capture_handler(_addr, _port, handler=None):
        captured["handler"] = handler

    client._client.get_transport().request_port_forward.side_effect = capture_handler

    client.reverse_forward(local_port=7899, remote_port=7890)
    handler = captured["handler"]
    assert handler is not None

    fake_sock = MagicMock()
    fake_sock.connect.side_effect = ConnectionRefusedError("nope")
    fake_channel = MagicMock()

    with patch("flux.ssh.socket.socket", return_value=fake_sock):
        handler(fake_channel, ("127.0.0.1", 12345), ("127.0.0.1", 7890))

    fake_sock.close.assert_called()   # critical: no FD leak
    fake_channel.close.assert_called()


def test_handler_runs_on_connect_when_succeeds() -> None:
    """Sanity check: success path still calls on_connect."""
    client = _build_client()
    captured: dict = {}

    def capture_handler(_addr, _port, handler=None):
        captured["handler"] = handler

    client._client.get_transport().request_port_forward.side_effect = capture_handler

    opened = []
    client.reverse_forward(
        local_port=7899,
        remote_port=7890,
        on_connect=lambda: opened.append(1),
    )
    handler = captured["handler"]

    fake_sock = MagicMock()
    fake_channel = MagicMock()
    # make recv return empty so the pipe threads exit quickly
    fake_sock.recv.return_value = b""
    fake_channel.recv.return_value = b""

    with patch("flux.ssh.socket.socket", return_value=fake_sock):
        handler(fake_channel, ("127.0.0.1", 12345), ("127.0.0.1", 7890))

    assert opened == [1]
