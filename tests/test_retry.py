from __future__ import annotations

import socket

import pytest

from flux.sync._retry import retry


def test_retry_returns_value_on_first_success() -> None:
    calls = []

    def ok() -> int:
        calls.append(1)
        return 7

    assert retry(ok, attempts=3, base_delay=0.001) == 7
    assert len(calls) == 1


def test_retry_recovers_after_transient_then_succeeds() -> None:
    calls = []

    def flaky() -> str:
        calls.append(1)
        if len(calls) < 3:
            raise socket.error("flap")
        return "ok"

    assert retry(flaky, attempts=3, base_delay=0.001) == "ok"
    assert len(calls) == 3


def test_retry_gives_up_and_reraises() -> None:
    def always_bad() -> None:
        raise ConnectionError("nope")

    with pytest.raises(ConnectionError):
        retry(always_bad, attempts=2, base_delay=0.001)


def test_retry_does_not_catch_other_exceptions() -> None:
    def bug() -> None:
        raise ValueError("logic")

    with pytest.raises(ValueError):
        retry(bug, attempts=3, base_delay=0.001)
