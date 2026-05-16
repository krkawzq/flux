"""sync orchestration: file → script → block, with rich progress + outcome aggregation."""

from __future__ import annotations

from dataclasses import dataclass, field
from pathlib import Path
from typing import Callable, Iterable, Literal

from rich.console import Console
from rich.progress import BarColumn, Progress, SpinnerColumn, TaskProgressColumn, TextColumn, TimeElapsedColumn
from rich.table import Table

from flux.config import BlockItem, Config
from flux.ssh import SshClient
from flux.sync.block import sync_block_group
from flux.sync.file import sync_file
from flux.sync.script import sync_script

Status = Literal["applied", "skipped", "failed"]


@dataclass
class Outcome:
    status: Status
    label: str
    detail: str = ""


@dataclass
class StageReport:
    name: str
    outcomes: list[Outcome] = field(default_factory=list)

    @property
    def applied(self) -> int:
        return sum(o.status == "applied" for o in self.outcomes)

    @property
    def skipped(self) -> int:
        return sum(o.status == "skipped" for o in self.outcomes)

    @property
    def failed(self) -> int:
        return sum(o.status == "failed" for o in self.outcomes)


def run_sync(
    cfg: Config,
    asset_root: Path,
    client: SshClient,
    console: Console,
    *,
    stop_scripts_on_failure: bool = True,
) -> int:
    """Run all stages. Returns 0 on full success, 1 if any item failed."""
    reports = [
        _run_per_item_stage(
            "file",
            cfg.file,
            lambda item: sync_file(item, asset_root, client),
            console,
        ),
        _run_per_item_stage(
            "script",
            cfg.script,
            lambda item: sync_script(item, cfg, asset_root, client, console),
            console,
            inline=True,  # scripts already stream their own output
            stop_on_failure=stop_scripts_on_failure,
        ),
        _run_block_stage(cfg.block, cfg, asset_root, client, console),
    ]
    _print_summary(reports, console)
    return 1 if any(r.failed for r in reports) else 0


def _item_label(item: object) -> str:
    for attr in ("name", "path"):
        v = getattr(item, attr, None)
        if v:
            return str(v)
    return repr(item)


def _run_per_item_stage(
    name: str,
    items: list,
    runner: Callable[[object], Outcome],
    console: Console,
    *,
    inline: bool = False,
    stop_on_failure: bool = False,
) -> StageReport:
    report = StageReport(name=name)
    if not items:
        return report
    console.rule(f"[bold]{name}[/] ({len(items)})", align="left")

    if inline or not console.is_terminal:
        for idx, item in enumerate(items):
            outcome = _run_one(item, runner)
            _print_outcome(outcome, console)
            report.outcomes.append(outcome)
            if stop_on_failure and outcome.status == "failed":
                _mark_remaining_skipped(items[idx + 1 :], report, console)
                break
        return report

    with Progress(
        SpinnerColumn(),
        TextColumn("[bold]{task.fields[stage]}[/]"),
        BarColumn(bar_width=24),
        TaskProgressColumn(),
        TextColumn("{task.fields[current]}", style="dim"),
        TimeElapsedColumn(),
        console=console,
        transient=True,
    ) as progress:
        task = progress.add_task("syncing", total=len(items), stage=name, current="")
        for idx, item in enumerate(items):
            label = _item_label(item)
            progress.update(task, current=label)
            outcome = _run_one(item, runner)
            report.outcomes.append(outcome)
            _print_outcome(outcome, progress.console)
            progress.advance(task)
            if stop_on_failure and outcome.status == "failed":
                _mark_remaining_skipped(items[idx + 1 :], report, progress.console)
                break
    return report


def _run_block_stage(
    items: list[BlockItem],
    cfg: Config,
    asset_root: Path,
    client: SshClient,
    console: Console,
) -> StageReport:
    """Blocks targeting the same remote file are batched: 1 read + 1 write per file."""
    report = StageReport(name="block")
    if not items:
        return report
    console.rule(f"[bold]block[/] ({len(items)})", align="left")

    # group preserving yaml order — first-seen target wins for group order
    groups: dict[str, list[BlockItem]] = {}
    for item in items:
        groups.setdefault(item.file, []).append(item)

    for target, group_items in groups.items():
        try:
            outcomes = sync_block_group(group_items, cfg, asset_root, client, target)
        except Exception as exc:
            # group-level failure (e.g. read_file unrecoverable) → mark all as failed
            for item in group_items:
                report.outcomes.append(
                    Outcome("failed", item.name, f"group {target}: {type(exc).__name__}: {exc}")
                )
            for item in group_items:
                _print_outcome(report.outcomes[-len(group_items) + group_items.index(item)], console)
            continue
        for outcome in outcomes:
            report.outcomes.append(outcome)
            _print_outcome(outcome, console)
    return report


def _mark_remaining_skipped(remaining, report: StageReport, console: Console) -> None:
    for item in remaining:
        outcome = Outcome(
            status="skipped",
            label=_item_label(item),
            detail="stop-on-failure: previous script failed",
        )
        report.outcomes.append(outcome)
        _print_outcome(outcome, console)


def _run_one(item: object, runner: Callable[[object], Outcome]) -> Outcome:
    label = _item_label(item)
    try:
        return runner(item)
    except Exception as exc:
        return Outcome(status="failed", label=label, detail=f"{type(exc).__name__}: {exc}")


_ICONS = {"applied": "[green]✓[/]", "skipped": "[dim]=[/]", "failed": "[red]✗[/]"}


def _print_outcome(outcome: Outcome, console: Console) -> None:
    icon = _ICONS[outcome.status]
    detail = f" [dim]{outcome.detail}[/]" if outcome.detail else ""
    console.print(f"{icon} {outcome.label}{detail}")


def _print_summary(reports: Iterable[StageReport], console: Console) -> None:
    table = Table(title=None, header_style="bold")
    table.add_column("stage")
    table.add_column("applied", justify="right", style="green")
    table.add_column("skipped", justify="right", style="dim")
    table.add_column("failed", justify="right", style="red")
    table.add_column("total", justify="right")
    total_failed = 0
    has_any = False
    for r in reports:
        if not r.outcomes:
            continue
        has_any = True
        total_failed += r.failed
        table.add_row(r.name, str(r.applied), str(r.skipped), str(r.failed), str(len(r.outcomes)))
    if not has_any:
        console.print("[yellow]no items in config[/]")
        return
    console.print(table)
    if total_failed:
        console.print(f"[red]sync finished with {total_failed} failure(s)[/]")
    else:
        console.print("[bold green]✔ sync ok[/]")
