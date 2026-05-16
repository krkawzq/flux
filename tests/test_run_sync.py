"""End-to-end run_sync with the FakeClient — verifies stage ordering, summary, exit codes."""

from __future__ import annotations

from pathlib import Path

from rich.console import Console

from flux.config import BlockItem, Config, FileItem, ScriptItem
from flux.sync import Outcome, StageReport, _print_summary, run_sync


def _cfg(**kw) -> Config:
    base = dict(host="x", user="y")
    base.update(kw)
    return Config(**base)


def _console() -> Console:
    return Console(record=True, width=140)


def test_empty_config_returns_zero(tmp_asset_root: Path, fake_client) -> None:
    code = run_sync(_cfg(), tmp_asset_root, fake_client, _console())
    assert code == 0


def test_runs_file_script_block_in_order(tmp_asset_root: Path, fake_client) -> None:
    (tmp_asset_root / "files" / "a.txt").write_bytes(b"hi")
    (tmp_asset_root / "scripts" / "s.sh").write_bytes(b"echo\n")
    (tmp_asset_root / "blocks" / "b.sh").write_text("alias x=y\n", encoding="utf-8")

    cfg = _cfg(
        file=[FileItem(src="a.txt", dst="/root/a.txt")],
        script=[ScriptItem(path="s.sh")],
        block=[BlockItem(name="block-one", path="b.sh", file="/root/.bashrc")],
    )
    code = run_sync(cfg, tmp_asset_root, fake_client, _console())
    assert code == 0
    assert fake_client.files["/root/a.txt"] == b"hi"
    assert len(fake_client.exec_streaming_log) == 1
    assert b">>> block-one:" in fake_client.files["/root/.bashrc"]


def test_failure_in_one_item_returns_one_but_runs_others(tmp_asset_root: Path, fake_client) -> None:
    """Missing file fails its item, the next valid item still runs."""
    (tmp_asset_root / "files" / "ok.txt").write_bytes(b"ok")
    cfg = _cfg(
        file=[
            FileItem(name="ghost", src="missing.txt", dst="/root/x"),
            FileItem(name="ok", src="ok.txt", dst="/root/ok.txt"),
        ],
    )
    code = run_sync(cfg, tmp_asset_root, fake_client, _console())
    assert code == 1
    assert fake_client.files["/root/ok.txt"] == b"ok"


def test_stage_report_counters() -> None:
    r = StageReport(name="x")
    r.outcomes.extend([
        Outcome("applied", "a"),
        Outcome("skipped", "b"),
        Outcome("failed", "c"),
        Outcome("applied", "d"),
    ])
    assert r.applied == 2
    assert r.skipped == 1
    assert r.failed == 1


def test_print_summary_does_not_crash_on_empty() -> None:
    _print_summary([], _console())  # should print "no items"


def test_script_stage_stops_on_first_failure_by_default(tmp_asset_root: Path, fake_client) -> None:
    """A failing script must short-circuit the rest of the stage; remainder are skipped."""
    (tmp_asset_root / "scripts" / "ok.sh").write_bytes(b"echo ok\n")
    (tmp_asset_root / "scripts" / "bad.sh").write_bytes(b"exit 1\n")
    (tmp_asset_root / "scripts" / "after.sh").write_bytes(b"echo after\n")
    cfg = _cfg(
        script=[
            ScriptItem(path="ok.sh"),
            ScriptItem(path="bad.sh"),
            ScriptItem(path="after.sh"),
        ]
    )

    def streaming(cmd: str, **kw):
        fake_client.exec_streaming_log.append(cmd)
        # bad.sh's tmp path contains "bad_sh"
        return 1 if "bad_sh" in cmd else 0

    fake_client.exec_streaming = streaming

    code = run_sync(cfg, tmp_asset_root, fake_client, _console())
    assert code == 1
    # ok ran, bad failed, after was skipped (never streamed)
    assert sum("ok_sh" in c for c in fake_client.exec_streaming_log) == 1
    assert sum("bad_sh" in c for c in fake_client.exec_streaming_log) == 1
    assert sum("after_sh" in c for c in fake_client.exec_streaming_log) == 0


def test_script_continue_keeps_running_after_failure(tmp_asset_root: Path, fake_client) -> None:
    """--continue (stop_scripts_on_failure=False) keeps the chain alive."""
    (tmp_asset_root / "scripts" / "ok.sh").write_bytes(b"\n")
    (tmp_asset_root / "scripts" / "bad.sh").write_bytes(b"\n")
    (tmp_asset_root / "scripts" / "after.sh").write_bytes(b"\n")
    cfg = _cfg(
        script=[ScriptItem(path="ok.sh"), ScriptItem(path="bad.sh"), ScriptItem(path="after.sh")]
    )

    def streaming(cmd: str, **kw):
        fake_client.exec_streaming_log.append(cmd)
        return 1 if "bad_sh" in cmd else 0

    fake_client.exec_streaming = streaming

    code = run_sync(cfg, tmp_asset_root, fake_client, _console(), stop_scripts_on_failure=False)
    assert code == 1
    # all three actually ran
    assert any("after_sh" in c for c in fake_client.exec_streaming_log)


def test_failure_in_file_stage_does_not_stop_block_stage(tmp_asset_root: Path, fake_client) -> None:
    """file/block remain per-item independent regardless of stop-on-failure."""
    (tmp_asset_root / "blocks" / "b.sh").write_text("body\n", encoding="utf-8")
    cfg = _cfg(
        file=[FileItem(name="ghost", src="missing.txt", dst="/x")],
        block=[BlockItem(name="b", path="b.sh", file="/root/.bashrc")],
    )
    code = run_sync(cfg, tmp_asset_root, fake_client, _console())
    assert code == 1  # one failure
    assert b">>> b:" in fake_client.files["/root/.bashrc"]  # block stage still ran
