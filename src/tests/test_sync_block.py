"""End-to-end block stage against FakeClient."""

from __future__ import annotations

from pathlib import Path

from rich.console import Console

from flux.config import BlockItem, Config
from flux.sync.block import sync_block


def _make_cfg() -> Config:
    return Config(host="x", user="y")


def test_injects_when_target_missing(tmp_asset_root: Path, fake_client) -> None:
    (tmp_asset_root / "blocks" / "aliases.sh").write_text("alias g=git\n", encoding="utf-8")
    item = BlockItem(name="aliases", path="aliases.sh", file="/root/.zshrc")
    sync_block(item, _make_cfg(), tmp_asset_root, fake_client, Console(record=True))
    assert b">>> aliases:" in fake_client.files["/root/.zshrc"]
    assert b"alias g=git" in fake_client.files["/root/.zshrc"]


def test_replaces_existing_block_keeps_other_content(tmp_asset_root: Path, fake_client) -> None:
    (tmp_asset_root / "blocks" / "aliases.sh").write_text("alias g=git\n", encoding="utf-8")
    fake_client.files["/root/.zshrc"] = (
        b"head\n# >>> aliases:1 >>>\nold-stuff\n# <<< aliases:1 <<<\ntail\n"
    )
    item = BlockItem(name="aliases", path="aliases.sh", file="/root/.zshrc")
    sync_block(item, _make_cfg(), tmp_asset_root, fake_client, Console(record=True))
    contents = fake_client.files["/root/.zshrc"]
    assert b"head\n" in contents
    assert b"tail\n" in contents
    assert b"alias g=git" in contents
    assert b"old-stuff" not in contents


def test_skips_when_body_unchanged(tmp_asset_root: Path, fake_client) -> None:
    (tmp_asset_root / "blocks" / "aliases.sh").write_text("alias g=git\n", encoding="utf-8")
    fake_client.files["/root/.zshrc"] = (
        b"# >>> aliases:7 >>>\nalias g=git\n# <<< aliases:7 <<<\n"
    )
    before = fake_client.files["/root/.zshrc"]
    item = BlockItem(name="aliases", path="aliases.sh", file="/root/.zshrc")
    sync_block(item, _make_cfg(), tmp_asset_root, fake_client, Console(record=True))
    assert fake_client.files["/root/.zshrc"] == before


def test_cover_mode_does_not_replace(tmp_asset_root: Path, fake_client) -> None:
    (tmp_asset_root / "blocks" / "aliases.sh").write_text("alias g=git\n", encoding="utf-8")
    fake_client.files["/root/.zshrc"] = (
        b"# >>> aliases:1 >>>\nOLD\n# <<< aliases:1 <<<\n"
    )
    item = BlockItem(name="aliases", path="aliases.sh", file="/root/.zshrc", mode="cover")
    sync_block(item, _make_cfg(), tmp_asset_root, fake_client, Console(record=True))
    assert b"OLD" in fake_client.files["/root/.zshrc"]
    assert b"alias g=git" not in fake_client.files["/root/.zshrc"]


def test_custom_template_per_item(tmp_asset_root: Path, fake_client) -> None:
    """An item-level comment_template overrides cfg.comment_template."""
    (tmp_asset_root / "blocks" / "section.sh").write_text("a=1\n", encoding="utf-8")
    item = BlockItem(
        name="sec",
        path="section.sh",
        file="/root/conf.ini",
        comment_template="; {}",
    )
    sync_block(item, _make_cfg(), tmp_asset_root, fake_client, Console(record=True))
    contents = fake_client.files["/root/conf.ini"]
    assert b"; >>> sec:" in contents
    assert b"; <<< sec:" in contents
