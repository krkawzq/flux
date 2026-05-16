"""Sentinel parsing/injection — the subtlest part of sync."""

from __future__ import annotations

import pytest

from flux.sync.block import (
    BlockError,
    build_markers,
    find_block,
    render_block,
    splice,
)


def test_build_markers_default_shell_template() -> None:
    open_m, close_m = build_markers("# {}", "aliases", 1700000000)
    assert open_m == "# >>> aliases:1700000000 >>>"
    assert close_m == "# <<< aliases:1700000000 <<<"


def test_build_markers_rejects_bad_template() -> None:
    with pytest.raises(BlockError):
        build_markers("no placeholder", "x", 1)


def test_find_block_returns_none_when_absent() -> None:
    found = find_block("# {}", "missing", "echo hi\n")
    assert found is None


def test_find_block_locates_existing() -> None:
    body = "\n".join([
        "alias a=1",
        "# >>> tools:1700000000 >>>",
        "alias g=git",
        "alias k=kubectl",
        "# <<< tools:1700000000 <<<",
        "alias b=2",
    ]) + "\n"
    found = find_block("# {}", "tools", body)
    assert found is not None
    assert found.timestamp == 1700000000
    assert body.encode()[found.close_end:].startswith(b"alias b=2")


def test_round_trip_replace_preserves_other_content() -> None:
    initial = "alias a=1\n# >>> tools:1700000000 >>>\nold-body\n# <<< tools:1700000000 <<<\nalias b=2\n"
    found = find_block("# {}", "tools", initial)
    assert found is not None
    open_m, close_m = build_markers("# {}", "tools", found.timestamp)
    rendered = render_block(open_m, "new-body", close_m)
    new = splice(initial, found, rendered)
    assert "alias a=1\n" in new
    assert "alias b=2\n" in new
    assert "new-body" in new
    assert "old-body" not in new


def test_idempotent_replace_when_body_identical() -> None:
    open_m, close_m = build_markers("# {}", "x", 1700000000)
    rendered = render_block(open_m, "hello\n", close_m)
    initial = "head\n" + rendered + "tail\n"
    found = find_block("# {}", "x", initial)
    assert found is not None
    again = splice(initial, found, rendered)
    assert again == initial


def test_splice_appends_when_block_absent() -> None:
    open_m, close_m = build_markers("# {}", "first", 42)
    rendered = render_block(open_m, "body\n", close_m)
    new = splice("existing\n", None, rendered)
    assert new == "existing\n# >>> first:42 >>>\nbody\n# <<< first:42 <<<\n"


def test_splice_appends_to_empty_content() -> None:
    open_m, close_m = build_markers("# {}", "first", 42)
    rendered = render_block(open_m, "body\n", close_m)
    new = splice("", None, rendered)
    assert new == "# >>> first:42 >>>\nbody\n# <<< first:42 <<<\n"


def test_find_block_with_alt_comment_template() -> None:
    initial = "k=v\n; >>> sec:1 >>>\na=1\n; <<< sec:1 <<<\nx=y\n"
    found = find_block("; {}", "sec", initial)
    assert found is not None
    assert found.timestamp == 1


def test_render_block_adds_trailing_newline_to_body_without_one() -> None:
    open_m, close_m = build_markers("# {}", "n", 1)
    out = render_block(open_m, "no-nl-here", close_m)
    assert out == "# >>> n:1 >>>\nno-nl-here\n# <<< n:1 <<<\n"


def test_find_block_distinguishes_similar_names() -> None:
    """A block named 'tools' must NOT match 'tools_other'."""
    initial = "# >>> tools_other:1 >>>\nzzz\n# <<< tools_other:1 <<<\n"
    found = find_block("# {}", "tools", initial)
    assert found is None


# ---- orphan / duplicate sentinel detection ----

def test_find_block_raises_on_orphan_open_marker() -> None:
    initial = "head\n# >>> tools:1 >>>\nbody\n# (close marker missing!)\ntail\n"
    with pytest.raises(BlockError, match="sentinel state corrupt"):
        find_block("# {}", "tools", initial)


def test_find_block_raises_on_orphan_close_marker() -> None:
    initial = "head\n# (open marker missing!)\nbody\n# <<< tools:1 <<<\ntail\n"
    with pytest.raises(BlockError, match="sentinel state corrupt"):
        find_block("# {}", "tools", initial)


def test_find_block_raises_on_duplicate_open_markers() -> None:
    initial = (
        "# >>> tools:1 >>>\nbody1\n# <<< tools:1 <<<\n"
        "# >>> tools:2 >>>\nbody2\n# <<< tools:2 <<<\n"
    )
    with pytest.raises(BlockError, match="sentinel state corrupt"):
        find_block("# {}", "tools", initial)


def test_find_block_raises_on_close_before_open() -> None:
    initial = "# <<< tools:1 <<<\n# >>> tools:1 >>>\nbody\n"
    with pytest.raises(BlockError, match="(corrupt|order)"):
        find_block("# {}", "tools", initial)


def test_find_block_raises_on_mismatched_timestamps() -> None:
    """open=1, close=2 is corrupt — splicing would yield silent bad state."""
    initial = "# >>> tools:1 >>>\nbody\n# <<< tools:2 <<<\n"
    with pytest.raises(BlockError, match="timestamps mismatch"):
        find_block("# {}", "tools", initial)
