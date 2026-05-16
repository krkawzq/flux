"""Host key policy: silent TOFU — trust unknown keys, persist, print notice.

Subsequent connects rely on paramiko's normal validation against the saved key;
a swapped key on connect N+1 is REJECTED. Risk window is connect #1 only.
"""

from __future__ import annotations

from pathlib import Path
from unittest.mock import MagicMock


def _fake_key(name: str = "ssh-ed25519", b64: str = "AAAAFAKEKEY") -> MagicMock:
    k = MagicMock()
    k.get_name.return_value = name
    k.get_base64.return_value = b64
    k.asbytes.return_value = b"key-blob-for-sha256"
    return k


def test_writes_single_line_to_known_hosts(tmp_path: Path) -> None:
    from flux.ssh import _AppendToKnownHostsPolicy

    known = tmp_path / "known_hosts"
    policy = _AppendToKnownHostsPolicy(known)
    client = MagicMock()
    policy.missing_host_key(client, "1.2.3.4", _fake_key("ssh-ed25519", "ABCDEF"))
    text = known.read_text(encoding="ascii")
    assert text == "1.2.3.4 ssh-ed25519 ABCDEF\n"
    client.get_host_keys().add.assert_called_once()


def test_appends_to_existing_known_hosts(tmp_path: Path) -> None:
    from flux.ssh import _AppendToKnownHostsPolicy

    known = tmp_path / "known_hosts"
    known.write_text("old.example ssh-rsa OLDKEY\n", encoding="ascii")
    policy = _AppendToKnownHostsPolicy(known)
    client = MagicMock()
    policy.missing_host_key(client, "new.example", _fake_key("ssh-ed25519", "NEWKEY"))
    text = known.read_text(encoding="ascii")
    assert "old.example ssh-rsa OLDKEY" in text
    assert "new.example ssh-ed25519 NEWKEY" in text


def test_creates_ssh_dir_if_missing(tmp_path: Path) -> None:
    from flux.ssh import _AppendToKnownHostsPolicy

    known = tmp_path / "newdir" / "known_hosts"
    policy = _AppendToKnownHostsPolicy(known)
    client = MagicMock()
    policy.missing_host_key(client, "x", _fake_key())
    assert known.parent.exists()
    assert known.exists()


def test_tolerates_write_failure(tmp_path: Path, monkeypatch) -> None:
    """OSError on persist should NOT block the connection — log and continue."""
    from flux.ssh import _AppendToKnownHostsPolicy

    known = tmp_path / "known_hosts"
    policy = _AppendToKnownHostsPolicy(known)

    def boom(*a, **kw):
        raise OSError("disk full")

    monkeypatch.setattr(Path, "open", boom)
    client = MagicMock()
    # must not raise
    policy.missing_host_key(client, "x", _fake_key())
    # and the in-memory add still happened
    client.get_host_keys().add.assert_called_once()


def test_sha256_fingerprint_format() -> None:
    """OpenSSH-style SHA256:<b64-no-pad> matches `ssh-keygen -l` output."""
    from flux.ssh import _sha256_fp

    fake = MagicMock()
    fake.asbytes.return_value = b"the-key-bytes"
    fp = _sha256_fp(fake)
    assert fp.startswith("SHA256:")
    assert "=" not in fp  # padding stripped
