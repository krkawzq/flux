"""Tiny retry helper. Used by file/script/block stages around each SSH call."""

from __future__ import annotations

import logging
import socket
import time
from typing import Callable, TypeVar

import paramiko

T = TypeVar("T")
log = logging.getLogger("flux.retry")

# Errors we consider transient; everything else propagates immediately.
_TRANSIENT = (
    socket.error,
    paramiko.SSHException,
    EOFError,
    TimeoutError,
    ConnectionError,
)


def retry(fn: Callable[[], T], *, attempts: int = 3, base_delay: float = 0.2) -> T:
    """Call fn up to `attempts` times. Exponential backoff: base * 2^n."""
    last_exc: BaseException | None = None
    for attempt in range(attempts):
        try:
            return fn()
        except _TRANSIENT as exc:
            last_exc = exc
            if attempt == attempts - 1:
                break
            delay = base_delay * (2**attempt)
            log.warning("transient %s (attempt %d/%d); sleeping %.2fs", type(exc).__name__, attempt + 1, attempts, delay)
            time.sleep(delay)
    assert last_exc is not None
    raise last_exc
