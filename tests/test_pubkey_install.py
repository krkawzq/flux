"""Auto-install local pubkey to remote authorized_keys for passwordless future logins.

Triggers when BOTH key and password are configured in the yaml — clear intent
to "set this host up", first connect via password, subsequent via key.
"""

from __future__ import annotations

from pathlib import Path

from flux.ssh import _install_pubkey_if_configured


def _keypair(tmp_path: Path, pub_content: str = "ssh-ed25519 AAAAabcdef host-alias\n") -> Path:
    key = tmp_path / "id_test"
    key.write_text("PRIVATE KEY CONTENT")
    pub = tmp_path / "id_test.pub"
    pub.write_text(pub_content)
    return key


# ---- positive cases ----

def test_installs_pubkey_when_remote_authorized_keys_missing(
    tmp_path: Path, fake_client
) -> None:
    key = _keypair(tmp_path)
    installed = _install_pubkey_if_configured(fake_client, str(key), "secret")
    assert installed is True
    content = fake_client.files["/root/.ssh/authorized_keys"]
    assert content == b"ssh-ed25519 AAAAabcdef host-alias\n"
    assert fake_client.modes["/root/.ssh/authorized_keys"] == 0o600
    # mkdir -p ~/.ssh && chmod 700 ~/.ssh ran exactly once
    assert any("mkdir -p ~/.ssh" in c for c in fake_client.exec_log)


def test_appends_when_authorized_keys_has_other_keys(tmp_path: Path, fake_client) -> None:
    key = _keypair(tmp_path)
    fake_client.files["/root/.ssh/authorized_keys"] = (
        b"ssh-rsa AAAAB3... other-user@host\n"
    )
    _install_pubkey_if_configured(fake_client, str(key), "secret")
    content = fake_client.files["/root/.ssh/authorized_keys"].decode()
    assert "other-user@host" in content
    assert "ssh-ed25519 AAAAabcdef host-alias" in content
    # exactly two lines (other + ours)
    assert content.count("\n") == 2


def test_skips_when_pubkey_already_present(tmp_path: Path, fake_client) -> None:
    """Same algo+blob already in authorized_keys (even with different comment) → no rewrite."""
    key = _keypair(tmp_path, "ssh-ed25519 AAAAabcdef wzq@laptop\n")
    fake_client.files["/root/.ssh/authorized_keys"] = (
        b"ssh-ed25519 AAAAabcdef different-comment\n"
    )
    before = fake_client.files["/root/.ssh/authorized_keys"]
    installed = _install_pubkey_if_configured(fake_client, str(key), "secret")
    assert installed is True  # "already present" still counts as success
    assert fake_client.files["/root/.ssh/authorized_keys"] == before


def test_appends_newline_when_file_lacks_trailing_newline(tmp_path: Path, fake_client) -> None:
    key = _keypair(tmp_path)
    fake_client.files["/root/.ssh/authorized_keys"] = b"ssh-rsa AAAAxxx no-trailing-nl"
    _install_pubkey_if_configured(fake_client, str(key), "secret")
    content = fake_client.files["/root/.ssh/authorized_keys"].decode()
    assert content.startswith("ssh-rsa AAAAxxx no-trailing-nl\n")
    assert content.endswith("\n")


def test_ignores_existing_comment_and_blank_lines(tmp_path: Path, fake_client) -> None:
    key = _keypair(tmp_path)
    fake_client.files["/root/.ssh/authorized_keys"] = (
        b"# managed keys below\n\nssh-rsa AAAArsa other\n# trailing\n"
    )
    _install_pubkey_if_configured(fake_client, str(key), "secret")
    content = fake_client.files["/root/.ssh/authorized_keys"].decode()
    assert "ssh-ed25519 AAAAabcdef host-alias" in content
    assert "# managed keys below" in content


# ---- skip cases ----

def test_skips_when_password_missing(tmp_path: Path, fake_client) -> None:
    """Only key configured → user is already keyed; nothing to set up."""
    key = _keypair(tmp_path)
    installed = _install_pubkey_if_configured(fake_client, str(key), None)
    assert installed is False
    assert "/root/.ssh/authorized_keys" not in fake_client.files


def test_skips_when_key_missing(tmp_path: Path, fake_client) -> None:
    installed = _install_pubkey_if_configured(fake_client, None, "secret")
    assert installed is False
    assert "/root/.ssh/authorized_keys" not in fake_client.files


def test_skips_when_local_pubfile_does_not_exist(tmp_path: Path, fake_client) -> None:
    """Key configured but .pub missing → can't install."""
    key = tmp_path / "id_test"
    key.write_text("private only")
    # no .pub created
    installed = _install_pubkey_if_configured(fake_client, str(key), "secret")
    assert installed is False
    assert "/root/.ssh/authorized_keys" not in fake_client.files


def test_skips_when_pubkey_file_is_malformed(tmp_path: Path, fake_client) -> None:
    """Pub file with only one token (no base64) → can't determine blob → skip."""
    key = _keypair(tmp_path, pub_content="garbage-only-one-token\n")
    installed = _install_pubkey_if_configured(fake_client, str(key), "secret")
    assert installed is False


def test_skips_when_pubkey_file_is_empty(tmp_path: Path, fake_client) -> None:
    key = _keypair(tmp_path, pub_content="")
    installed = _install_pubkey_if_configured(fake_client, str(key), "secret")
    assert installed is False
