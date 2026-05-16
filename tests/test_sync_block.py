"""End-to-end block stage against FakeClient (group-based API)."""

from __future__ import annotations

from pathlib import Path

from flux.config import BlockItem, Config
from flux.sync.block import sync_block_group


def _cfg() -> Config:
    return Config(host="x", user="y")


def _single(item: BlockItem, asset_root: Path, fake_client) -> "Outcome":  # type: ignore[name-defined]
    """Convenience: run group of one item, return its outcome."""
    outs = sync_block_group([item], _cfg(), asset_root, fake_client, item.file)
    assert len(outs) == 1
    return outs[0]


def test_injects_when_target_missing(tmp_asset_root: Path, fake_client) -> None:
    (tmp_asset_root / "blocks" / "aliases.sh").write_text("alias g=git\n", encoding="utf-8")
    item = BlockItem(name="aliases", path="aliases.sh", file="/root/.zshrc")
    o = _single(item, tmp_asset_root, fake_client)
    assert o.status == "applied"
    assert b">>> aliases:" in fake_client.files["/root/.zshrc"]
    assert b"alias g=git" in fake_client.files["/root/.zshrc"]


def test_replaces_existing_block_keeps_other_content(tmp_asset_root: Path, fake_client) -> None:
    (tmp_asset_root / "blocks" / "aliases.sh").write_text("alias g=git\n", encoding="utf-8")
    fake_client.files["/root/.zshrc"] = (
        b"head\n# >>> aliases:1 >>>\nold-stuff\n# <<< aliases:1 <<<\ntail\n"
    )
    item = BlockItem(name="aliases", path="aliases.sh", file="/root/.zshrc")
    o = _single(item, tmp_asset_root, fake_client)
    assert o.status == "applied"
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
    o = _single(item, tmp_asset_root, fake_client)
    assert o.status == "skipped"
    assert fake_client.files["/root/.zshrc"] == before


def test_cover_mode_does_not_replace(tmp_asset_root: Path, fake_client) -> None:
    (tmp_asset_root / "blocks" / "aliases.sh").write_text("alias g=git\n", encoding="utf-8")
    fake_client.files["/root/.zshrc"] = (
        b"# >>> aliases:1 >>>\nOLD\n# <<< aliases:1 <<<\n"
    )
    item = BlockItem(name="aliases", path="aliases.sh", file="/root/.zshrc", mode="cover")
    o = _single(item, tmp_asset_root, fake_client)
    assert o.status == "skipped"
    assert b"OLD" in fake_client.files["/root/.zshrc"]


def test_custom_template_per_item(tmp_asset_root: Path, fake_client) -> None:
    (tmp_asset_root / "blocks" / "section.sh").write_text("a=1\n", encoding="utf-8")
    item = BlockItem(
        name="sec", path="section.sh", file="/root/conf.ini", comment_template="; {}",
    )
    _single(item, tmp_asset_root, fake_client)
    contents = fake_client.files["/root/conf.ini"]
    assert b"; >>> sec:" in contents
    assert b"; <<< sec:" in contents


def test_handles_missing_remote_file(tmp_asset_root: Path, fake_client) -> None:
    (tmp_asset_root / "blocks" / "x.sh").write_text("body\n", encoding="utf-8")
    item = BlockItem(name="x", path="x.sh", file="/root/.bashrc")
    o = _single(item, tmp_asset_root, fake_client)
    assert o.status == "applied"
    assert b">>> x:" in fake_client.files["/root/.bashrc"]


def test_legacy_sentinel_falls_back_to_mtime_guard(tmp_asset_root: Path, fake_client) -> None:
    """Old sentinels (no hash) use the timestamp-vs-local-mtime heuristic."""
    import os
    import time as _time

    src = tmp_asset_root / "blocks" / "aliases.sh"
    src.write_text("alias g=git\n", encoding="utf-8")
    long_ago = _time.time() - 7200
    os.utime(src, (long_ago, long_ago))

    future_ts = int(_time.time())
    fake_client.files["/root/.zshrc"] = (
        f"# >>> aliases:{future_ts} >>>\nalias g=git\nalias k=kubectl\n# <<< aliases:{future_ts} <<<\n"
    ).encode()
    before = fake_client.files["/root/.zshrc"]

    item = BlockItem(name="aliases", path="aliases.sh", file="/root/.zshrc")
    o = _single(item, tmp_asset_root, fake_client)
    assert o.status == "skipped"
    assert "legacy sentinel" in o.detail
    assert fake_client.files["/root/.zshrc"] == before


def test_hash_detects_remote_hand_edit(tmp_asset_root: Path, fake_client) -> None:
    """New-format sentinel with hash MUST detect remote body changes regardless of mtime."""
    from flux.sync.block import _body_hash

    src = tmp_asset_root / "blocks" / "aliases.sh"
    # local source body that we (the truth) want injected
    local_body = "alias g=git\n"
    src.write_text(local_body, encoding="utf-8")
    # remote has a sentinel claiming hash of local_body, but the actual body inside
    # has been hand-edited (different content)
    declared_hash = _body_hash(local_body)
    fake_client.files["/root/.zshrc"] = (
        f"# >>> aliases:100:{declared_hash} >>>\n"
        f"alias g=git\nalias k=kubectl  # human edit!\n"
        f"# <<< aliases:100 <<<\n"
    ).encode()
    before = fake_client.files["/root/.zshrc"]

    item = BlockItem(name="aliases", path="aliases.sh", file="/root/.zshrc")
    o = _single(item, tmp_asset_root, fake_client)
    assert o.status == "skipped"
    assert "hand-edited" in o.detail
    assert "hash" in o.detail
    assert fake_client.files["/root/.zshrc"] == before  # untouched


def test_hash_lets_update_when_remote_matches_recorded(tmp_asset_root: Path, fake_client) -> None:
    """Recorded hash matches remote body → user did NOT hand-edit → safe to update."""
    from flux.sync.block import _body_hash

    src = tmp_asset_root / "blocks" / "aliases.sh"
    src.write_text("alias g=git\nalias k=kubectl\n", encoding="utf-8")
    # remote was last written by us with old body; hash agrees with actual remote body
    old_body = "alias g=git\n"
    fake_client.files["/root/.zshrc"] = (
        f"# >>> aliases:100:{_body_hash(old_body)} >>>\n"
        f"{old_body}"
        f"# <<< aliases:100 <<<\n"
    ).encode()

    item = BlockItem(name="aliases", path="aliases.sh", file="/root/.zshrc")
    o = _single(item, tmp_asset_root, fake_client)
    assert o.status == "applied"
    assert b"alias k=kubectl" in fake_client.files["/root/.zshrc"]


def test_updates_when_local_newer_than_sentinel(tmp_asset_root: Path, fake_client) -> None:
    import os
    import time as _time

    src = tmp_asset_root / "blocks" / "aliases.sh"
    src.write_text("alias g=git\nalias k=kubectl\n", encoding="utf-8")
    os.utime(src, (_time.time(), _time.time()))

    fake_client.files["/root/.zshrc"] = b"# >>> aliases:1000 >>>\nalias g=git\n# <<< aliases:1000 <<<\n"
    item = BlockItem(name="aliases", path="aliases.sh", file="/root/.zshrc")
    o = _single(item, tmp_asset_root, fake_client)
    assert o.status == "applied"
    assert b"alias k=kubectl" in fake_client.files["/root/.zshrc"]


def test_orphan_sentinel_raises_outcome(tmp_asset_root: Path, fake_client) -> None:
    """Orphan markers raise inside _apply_one → group catches → Outcome(failed)."""
    (tmp_asset_root / "blocks" / "x.sh").write_text("body\n", encoding="utf-8")
    fake_client.files["/root/.zshrc"] = b"head\n# >>> x:1 >>>\nbroken\ntail\n"
    item = BlockItem(name="x", path="x.sh", file="/root/.zshrc")
    o = _single(item, tmp_asset_root, fake_client)
    assert o.status == "failed"
    assert "BlockError" in o.detail or "corrupt" in o.detail.lower()


# ---- batching + backup ----

def test_multiple_blocks_to_same_file_share_one_read_and_one_write(
    tmp_asset_root: Path, fake_client
) -> None:
    """Three blocks on /root/.zshrc must produce exactly 1 read + 1 final write."""
    for n in ("a", "b", "c"):
        (tmp_asset_root / "blocks" / f"{n}.sh").write_text(f"body-{n}\n", encoding="utf-8")
    items = [
        BlockItem(name=n, path=f"{n}.sh", file="/root/.zshrc") for n in ("a", "b", "c")
    ]
    outs = sync_block_group(items, _cfg(), tmp_asset_root, fake_client, "/root/.zshrc")
    assert all(o.status == "applied" for o in outs)
    contents = fake_client.files["/root/.zshrc"].decode()
    # all three injected, in order
    assert contents.index("a:") < contents.index("b:") < contents.index("c:")


def test_backup_written_before_first_change(tmp_asset_root: Path, fake_client) -> None:
    """When target existed, first write to it during sync must produce a .bak."""
    (tmp_asset_root / "blocks" / "a.sh").write_text("body-a\n", encoding="utf-8")
    fake_client.files["/root/.zshrc"] = b"original content\n"

    items = [BlockItem(name="a", path="a.sh", file="/root/.zshrc")]
    sync_block_group(items, _cfg(), tmp_asset_root, fake_client, "/root/.zshrc")

    # backup file must exist with original content
    bak_paths = [p for p in fake_client.files if p.startswith("/root/.zshrc.flux-") and p.endswith(".bak")]
    assert len(bak_paths) == 1
    assert fake_client.files[bak_paths[0]] == b"original content\n"


def test_no_backup_when_target_missing(tmp_asset_root: Path, fake_client) -> None:
    """Don't backup what doesn't exist."""
    (tmp_asset_root / "blocks" / "a.sh").write_text("body\n", encoding="utf-8")
    items = [BlockItem(name="a", path="a.sh", file="/root/.zshrc")]
    sync_block_group(items, _cfg(), tmp_asset_root, fake_client, "/root/.zshrc")
    bak_paths = [p for p in fake_client.files if p.startswith("/root/.zshrc.flux-")]
    assert bak_paths == []


def test_refuses_non_utf8_remote_target(tmp_asset_root: Path, fake_client) -> None:
    """Round-tripping non-UTF-8 bytes through decode/encode would corrupt the file."""
    from flux.sync.block import BlockError

    (tmp_asset_root / "blocks" / "x.sh").write_text("body\n", encoding="utf-8")
    fake_client.files["/root/.zshrc"] = b"valid\xffinvalid\xfebytes\n"  # not UTF-8
    item = BlockItem(name="x", path="x.sh", file="/root/.zshrc")
    o = _single(item, tmp_asset_root, fake_client)
    assert o.status == "failed"
    assert "non-UTF-8" in o.detail or "UnicodeDecodeError" in o.detail
    # nothing written, nothing backed up
    assert fake_client.files["/root/.zshrc"] == b"valid\xffinvalid\xfebytes\n"
    assert not any(p.startswith("/root/.zshrc.flux-") for p in fake_client.files)


def test_no_backup_or_write_when_nothing_changed(tmp_asset_root: Path, fake_client) -> None:
    """If all blocks skip, no .bak and no write should occur."""
    (tmp_asset_root / "blocks" / "a.sh").write_text("body\n", encoding="utf-8")
    fake_client.files["/root/.zshrc"] = b"# >>> a:1 >>>\nbody\n# <<< a:1 <<<\n"
    original = fake_client.files["/root/.zshrc"]
    items = [BlockItem(name="a", path="a.sh", file="/root/.zshrc")]
    outs = sync_block_group(items, _cfg(), tmp_asset_root, fake_client, "/root/.zshrc")
    assert outs[0].status == "skipped"
    assert fake_client.files["/root/.zshrc"] == original
    assert not any(p.startswith("/root/.zshrc.flux-") for p in fake_client.files)
