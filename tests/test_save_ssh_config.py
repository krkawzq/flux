"""save_to_ssh_config: must REPLACE existing Host blocks, not append duplicates."""

from __future__ import annotations

from pathlib import Path

import pytest

from flux.config import ResolvedConnection, SshConfigConflict, save_to_ssh_config


@pytest.fixture
def fake_home(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    monkeypatch.setattr("pathlib.Path.home", lambda: tmp_path)
    return tmp_path


def _conn(host: str = "1.2.3.4", port: int = 22, user: str = "root", key: str | None = "/k") -> ResolvedConnection:
    return ResolvedConnection(host=host, port=port, user=user, key=key, password=None)


def test_appends_when_file_missing(fake_home: Path) -> None:
    path = save_to_ssh_config("alpha", _conn())
    text = path.read_text(encoding="utf-8")
    assert text.count("Host alpha") == 1
    assert "HostName 1.2.3.4" in text


def test_replaces_existing_block_does_not_duplicate(fake_home: Path) -> None:
    ssh_cfg = fake_home / ".ssh"
    ssh_cfg.mkdir()
    (ssh_cfg / "config").write_text(
        "Host alpha\n    HostName old.example\n    Port 22\n    User old\n\n"
        "Host beta\n    HostName beta.example\n    User b\n",
        encoding="utf-8",
    )
    save_to_ssh_config("alpha", _conn(host="new.example", port=2222, user="root"))
    text = (ssh_cfg / "config").read_text(encoding="utf-8")
    assert text.count("Host alpha") == 1
    assert "old.example" not in text
    assert "new.example" in text
    assert "Port 2222" in text
    # beta unchanged
    assert "Host beta" in text
    assert "beta.example" in text


def test_replaces_preserves_blocks_after_target(fake_home: Path) -> None:
    ssh_cfg = fake_home / ".ssh"
    ssh_cfg.mkdir()
    (ssh_cfg / "config").write_text(
        "Host *\n    ServerAliveInterval 60\n\n"
        "Host alpha\n    HostName a.old\n    User u\n\n"
        "Host gamma\n    HostName g.example\n    User g\n",
        encoding="utf-8",
    )
    save_to_ssh_config("alpha", _conn(host="a.new"))
    text = (ssh_cfg / "config").read_text(encoding="utf-8")
    # wildcard block survived
    assert "Host *" in text
    assert "ServerAliveInterval 60" in text
    # gamma survived in full
    assert "Host gamma" in text
    assert "g.example" in text
    # alpha was rewritten
    assert "a.old" not in text
    assert "a.new" in text


def test_does_not_touch_unrelated_host_block(fake_home: Path) -> None:
    ssh_cfg = fake_home / ".ssh"
    ssh_cfg.mkdir()
    (ssh_cfg / "config").write_text(
        "Host beta\n    HostName b.example\n    User b\n",
        encoding="utf-8",
    )
    save_to_ssh_config("alpha", _conn())
    text = (ssh_cfg / "config").read_text(encoding="utf-8")
    assert text.count("Host beta") == 1
    assert text.count("Host alpha") == 1


def test_first_save_creates_minimal_block(fake_home: Path) -> None:
    save_to_ssh_config("alpha", _conn(key=None))
    text = (fake_home / ".ssh" / "config").read_text(encoding="utf-8")
    assert "Host alpha" in text
    assert "IdentityFile" not in text  # key None, line omitted


def test_refuses_to_touch_multipattern_host_block(fake_home: Path) -> None:
    """`Host alpha beta` shared between aliases must not be silently rewritten —
    that would delete beta's settings. Raise SshConfigConflict instead."""
    ssh_cfg = fake_home / ".ssh"
    ssh_cfg.mkdir()
    (ssh_cfg / "config").write_text(
        "Host alpha beta\n    HostName shared.example\n    User shared\n",
        encoding="utf-8",
    )
    with pytest.raises(SshConfigConflict):
        save_to_ssh_config("alpha", _conn(host="new.example"))
    # file must be untouched
    text = (ssh_cfg / "config").read_text(encoding="utf-8")
    assert "Host alpha beta" in text
    assert "shared.example" in text
    assert "new.example" not in text


def test_refuses_to_touch_wildcard_multipattern(fake_home: Path) -> None:
    """`Host alpha *.example` is also multi-pattern; same protection."""
    ssh_cfg = fake_home / ".ssh"
    ssh_cfg.mkdir()
    (ssh_cfg / "config").write_text(
        "Host alpha *.example\n    User shared\n",
        encoding="utf-8",
    )
    with pytest.raises(SshConfigConflict):
        save_to_ssh_config("alpha", _conn())


@pytest.mark.parametrize("bad_alias", ["", "alpha beta", " alpha", "alpha\nHost *", "#comment"])
def test_rejects_aliases_that_would_inject_or_split_host_line(
    fake_home: Path,
    bad_alias: str,
) -> None:
    with pytest.raises(ValueError):
        save_to_ssh_config(bad_alias, _conn())
    assert not (fake_home / ".ssh" / "config").exists()


def test_rejects_connection_values_with_newlines(fake_home: Path) -> None:
    ssh_cfg = fake_home / ".ssh"
    ssh_cfg.mkdir()
    config = ssh_cfg / "config"
    config.write_text("Host beta\n    HostName b.example\n", encoding="utf-8")

    with pytest.raises(ValueError):
        save_to_ssh_config("alpha", _conn(host="good.example\nUser injected"))

    assert config.read_text(encoding="utf-8") == "Host beta\n    HostName b.example\n"
