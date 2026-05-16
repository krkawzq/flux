from __future__ import annotations

from pathlib import Path

import pytest
import yaml
from pydantic import ValidationError

from flux.config import (
    BlockItem,
    FileItem,
    ScriptItem,
    find_config,
    load,
    resolve_password,
)


def test_minimal_yaml_parses(tmp_path: Path) -> None:
    cfg_path = tmp_path / "x.yml"
    cfg_path.write_text("host: 1.2.3.4\nuser: root\n", encoding="utf-8")
    cfg, root = load(str(cfg_path))
    assert cfg.host == "1.2.3.4"
    assert cfg.user == "root"
    assert cfg.port is None
    assert cfg.interpreter == "/bin/bash"
    assert cfg.flags == ["-l", "-i"]
    assert root == tmp_path.resolve()


def test_file_item_defaults() -> None:
    item = FileItem(src="a.txt", dst="/b.txt")
    assert item.mode == "sync"
    assert item.chmod is None
    assert item.name is None


def test_file_item_rejects_unknown_field() -> None:
    with pytest.raises(ValidationError):
        FileItem.model_validate({"src": "a", "dst": "b", "bogus": 1})


def test_file_item_rejects_bad_mode() -> None:
    with pytest.raises(ValidationError):
        FileItem.model_validate({"src": "a", "dst": "b", "mode": "touch"})


def test_script_item_minimal() -> None:
    item = ScriptItem(path="install.sh")
    assert item.interpreter is None
    assert item.flags is None
    assert item.args == []


def test_block_item_minimal() -> None:
    item = BlockItem(name="aliases", path="aliases.sh", file="~/.zshrc")
    assert item.mode == "sync"


def test_find_config_searches_dotflux_in_cwd(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    (tmp_path / ".flux").mkdir()
    (tmp_path / ".flux" / "alpha.yml").write_text("host: a\n", encoding="utf-8")
    monkeypatch.chdir(tmp_path)
    p = find_config("alpha")
    assert p.name == "alpha.yml"


def test_find_config_raises_when_missing(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.chdir(tmp_path)
    with pytest.raises(FileNotFoundError):
        find_config("ghost-name-zzz")


def test_resolve_password_inline_passthrough() -> None:
    assert resolve_password("hunter2") == "hunter2"
    assert resolve_password(None) is None


def test_resolve_password_rejects_bad_keychain_spec() -> None:
    with pytest.raises(ValueError):
        resolve_password("keychain:no-dot-here")


def test_load_round_trip_with_all_sections(tmp_path: Path) -> None:
    raw = {
        "host": "1.1.1.1",
        "user": "wzq",
        "port": 2222,
        "key": "~/.ssh/id_ed25519",
        "interpreter": "/bin/zsh",
        "flags": ["-il"],
        "file": [{"src": "a", "dst": "/b", "chmod": "600"}],
        "script": [{"path": "s.sh", "args": ["--x"]}],
        "block": [{"name": "z", "path": "z.sh", "file": "/root/.zshrc", "mode": "cover"}],
    }
    cfg_path = tmp_path / "full.yml"
    cfg_path.write_text(yaml.safe_dump(raw), encoding="utf-8")
    cfg, _ = load(str(cfg_path))
    assert cfg.port == 2222
    assert cfg.file[0].chmod == "600"
    assert cfg.script[0].args == ["--x"]
    assert cfg.block[0].mode == "cover"


def test_load_rejects_unknown_top_level(tmp_path: Path) -> None:
    cfg_path = tmp_path / "bad.yml"
    cfg_path.write_text("host: x\nfutures: []\n", encoding="utf-8")
    with pytest.raises(Exception):
        load(str(cfg_path))
