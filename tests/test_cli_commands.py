"""Smoke tests for `flux list` and `flux exec`."""

from __future__ import annotations

from pathlib import Path

import pytest
from typer.testing import CliRunner

from flux.cli import app


runner = CliRunner()


@pytest.fixture
def fake_home(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    monkeypatch.setattr("pathlib.Path.home", lambda: tmp_path)
    monkeypatch.chdir(tmp_path)  # also no cwd .flux interference
    return tmp_path


def test_list_empty_says_no_configs(fake_home: Path) -> None:
    result = runner.invoke(app, ["list"])
    assert result.exit_code == 0
    assert "no .flux configs" in result.output


def test_list_enumerates_configs_with_host_and_counts(fake_home: Path) -> None:
    (fake_home / ".flux").mkdir()
    (fake_home / ".flux" / "alpha.yml").write_text(
        "host: a.example\nuser: root\nfile:\n  - {src: x, dst: /y}\n",
        encoding="utf-8",
    )
    (fake_home / ".flux" / "beta.yml").write_text(
        "host: b.example\nuser: u\nport: 2222\n", encoding="utf-8"
    )
    result = runner.invoke(app, ["list"])
    assert result.exit_code == 0
    assert "alpha" in result.output
    assert "a.example" in result.output
    assert "beta" in result.output
    assert "2222" in result.output
    # items column: 1/0/0 for alpha, 0/0/0 for beta
    assert "1/0/0" in result.output


def test_list_cwd_dotflux_wins_over_home_collision(fake_home: Path) -> None:
    (fake_home / ".flux").mkdir()
    (fake_home / ".flux" / "alpha.yml").write_text("host: HOME\nuser: u\n", encoding="utf-8")
    # also create cwd .flux/alpha.yml — but we chdir'd to fake_home, so they're the same dir.
    # Use a separate cwd to test collision properly:
    cwd = fake_home / "work"
    cwd.mkdir()
    (cwd / ".flux").mkdir()
    (cwd / ".flux" / "alpha.yml").write_text("host: CWD\nuser: u\n", encoding="utf-8")
    import os
    os.chdir(cwd)
    result = runner.invoke(app, ["list"])
    assert result.exit_code == 0
    assert "CWD" in result.output
    # we still see the home-only one if any; here alpha appears once with cwd's host
    assert result.output.count("alpha") == 1


def test_exec_requires_a_command(fake_home: Path) -> None:
    (fake_home / ".flux").mkdir()
    (fake_home / ".flux" / "alpha.yml").write_text("host: x\nuser: u\nkey: ~/.ssh/id\n", encoding="utf-8")
    result = runner.invoke(app, ["exec", "alpha"])
    assert result.exit_code == 2
    assert "no command" in result.output


def test_help_lists_new_commands() -> None:
    result = runner.invoke(app, ["--help"])
    assert "list" in result.output
    assert "exec" in result.output
    assert "proxy" in result.output
    assert "sync" in result.output
