"""paramiko wrapper that gives the rest of flux a small, opinionated SSH surface."""

from __future__ import annotations

import logging
import select
import shlex
import socket
import sys
import threading
import time
from contextlib import contextmanager
from pathlib import Path, PurePosixPath
from typing import Callable, Iterator

import paramiko
from paramiko.config import SSHConfig

from flux.config import ResolvedConnection

log = logging.getLogger("flux.ssh")


class SshError(Exception):
    pass


class ExecResult:
    __slots__ = ("status", "stdout", "stderr")

    def __init__(self, status: int, stdout: bytes, stderr: bytes) -> None:
        self.status = status
        self.stdout = stdout
        self.stderr = stderr

    @property
    def ok(self) -> bool:
        return self.status == 0

    def text(self) -> str:
        return self.stdout.decode("utf-8", errors="replace")


def _read_user_ssh_config(host: str) -> dict:
    cfg_path = Path.home() / ".ssh" / "config"
    if not cfg_path.exists():
        return {}
    parser = SSHConfig()
    with cfg_path.open(encoding="utf-8") as f:
        parser.parse(f)
    return parser.lookup(host)


def _write_stdout(data: bytes) -> None:
    try:
        sys.stdout.buffer.write(data)
        sys.stdout.flush()
    except (BlockingIOError, BrokenPipeError):
        pass


def _write_stderr(data: bytes) -> None:
    try:
        sys.stderr.buffer.write(data)
        sys.stderr.flush()
    except (BlockingIOError, BrokenPipeError):
        pass


class SshClient:
    """Single-connection paramiko wrapper. SFTP and $HOME are lazy + cached."""

    def __init__(self, conn: ResolvedConnection) -> None:
        self._conn = conn
        self._client: paramiko.SSHClient | None = None
        self._sftp: paramiko.SFTPClient | None = None
        self._home: str | None = None
        self._ensured_dirs: set[str] = set()

    # ---- lifecycle ----

    def connect(self) -> None:
        ssh_defaults = _read_user_ssh_config(self._conn.host)
        hostname = str(ssh_defaults.get("hostname") or self._conn.host)
        port = int(ssh_defaults.get("port") or self._conn.port)
        user = str(ssh_defaults.get("user") or self._conn.user)
        identityfiles = ssh_defaults.get("identityfile") or []
        key_filename = self._conn.key or (identityfiles[0] if identityfiles else None)

        client = paramiko.SSHClient()
        # Load user known_hosts so changed/known keys are validated by paramiko.
        known = Path.home() / ".ssh" / "known_hosts"
        if known.exists():
            try:
                client.load_host_keys(str(known))
            except Exception as exc:
                log.debug("ignored bad known_hosts: %s", exc)
        # TOFU: trust unknown keys on first contact and persist them. Subsequent
        # connects validate against the saved key — if it ever changes paramiko
        # rejects the connection. A one-line notice with SHA256 fingerprint is
        # printed so the user can see what was just trusted.
        client.set_missing_host_key_policy(_AppendToKnownHostsPolicy(known))

        log.debug("connecting %s@%s:%s key=%s", user, hostname, port, key_filename)
        client.connect(
            hostname=hostname,
            port=port,
            username=user,
            key_filename=key_filename,
            password=self._conn.password,
            look_for_keys=key_filename is None and self._conn.password is None,
            allow_agent=True,
            timeout=15,
            banner_timeout=15,
            auth_timeout=15,
        )
        # 30s keepalive prevents idle disconnects during long install scripts.
        transport = client.get_transport()
        if transport is not None:
            transport.set_keepalive(30)
        self._client = client

        # Auto-install local pubkey for passwordless future logins. Only fires
        # when BOTH key and password are configured (clear "set up this host"
        # intent). Best-effort — failures here must not break the sync.
        try:
            _install_pubkey_if_configured(self, self._conn.key, self._conn.password)
        except Exception as exc:
            log.warning("pubkey auto-install failed: %s", exc)

    def close(self) -> None:
        if self._sftp is not None:
            try:
                self._sftp.close()
            except Exception:
                pass
            self._sftp = None
        if self._client is not None:
            try:
                self._client.close()
            except Exception:
                pass
            self._client = None

    def __enter__(self) -> "SshClient":
        if self._client is None:
            self.connect()
        return self

    def __exit__(self, *exc: object) -> None:
        self.close()

    # ---- helpers ----

    def _ensure_client(self) -> paramiko.SSHClient:
        if self._client is None:
            raise SshError("ssh client not connected")
        return self._client

    def _ensure_sftp(self) -> paramiko.SFTPClient:
        if self._sftp is None:
            self._sftp = self._ensure_client().open_sftp()
        return self._sftp

    def home(self) -> str:
        if self._home is None:
            r = self.exec("printf %s \"$HOME\"")
            if not r.ok:
                raise SshError(f"failed to resolve $HOME: {r.stderr.decode()}")
            self._home = r.text().strip().rstrip("/")
        return self._home

    def expand(self, remote_path: str) -> str:
        if remote_path == "~":
            return self.home()
        if remote_path.startswith("~/"):
            tail = remote_path[2:]
            return f"{self.home()}/{tail}" if tail else self.home()
        return remote_path

    # ---- exec ----

    def exec(self, cmd: str) -> ExecResult:
        """Run cmd, return (status, stdout, stderr).

        Drains stdout AND stderr concurrently via select. The naïve approach
        `stdout.read(); stderr.read()` deadlocks when the remote writes more
        than the channel's stderr buffer can hold (default 64KiB) — paramiko
        blocks the remote process on the stderr pipe while we're still
        reading stdout.
        """
        log.debug("exec: %s", cmd)
        transport = self._ensure_client().get_transport()
        if transport is None:
            raise SshError("transport not available")
        channel = transport.open_session()
        try:
            channel.exec_command(cmd)
            stdout_buf: list[bytes] = []
            stderr_buf: list[bytes] = []
            while True:
                done = channel.exit_status_ready()
                if done and not channel.recv_ready() and not channel.recv_stderr_ready():
                    break
                r, _w, _x = select.select([channel], [], [], 0.2)
                if channel in r:
                    if channel.recv_ready():
                        chunk = channel.recv(65536)
                        if chunk:
                            stdout_buf.append(chunk)
                    if channel.recv_stderr_ready():
                        chunk = channel.recv_stderr(65536)
                        if chunk:
                            stderr_buf.append(chunk)
            while channel.recv_ready():
                stdout_buf.append(channel.recv(65536))
            while channel.recv_stderr_ready():
                stderr_buf.append(channel.recv_stderr(65536))
            status = channel.recv_exit_status()
            return ExecResult(status, b"".join(stdout_buf), b"".join(stderr_buf))
        finally:
            try:
                channel.close()
            except Exception:
                pass

    def exec_streaming(
        self,
        cmd: str,
        *,
        use_pty: bool = True,
        forward_stdin: bool | None = None,
        stdout_writer: Callable[[bytes], None] = _write_stdout,
        stderr_writer: Callable[[bytes], None] = _write_stderr,
    ) -> int:
        """Run cmd, streaming stdout (and stderr if no PTY) live. Returns exit status.

        With use_pty=True stderr is merged into stdout (PTY behavior).
        forward_stdin defaults to True only when local stdin is a tty — in CI /
        piped contexts we MUST NOT pump arbitrary file/pipe contents into the
        remote process, because it would silently feed it the parent's input.
        """
        log.debug("exec_streaming: %s", cmd)
        if forward_stdin is None:
            forward_stdin = bool(sys.stdin and sys.stdin.isatty())

        transport = self._ensure_client().get_transport()
        if transport is None:
            raise SshError("transport not available")
        channel = transport.open_session()
        try:
            if use_pty:
                try:
                    rows, cols = _terminal_size()
                    channel.get_pty(term="xterm-256color", width=cols, height=rows)
                except Exception:
                    channel.get_pty(term="xterm-256color")
            channel.exec_command(cmd)

            stop_stdin = threading.Event()
            stdin_thread: threading.Thread | None = None
            if forward_stdin:
                stdin_thread = threading.Thread(
                    target=_pump_stdin_to_channel,
                    args=(channel, stop_stdin),
                    daemon=True,
                )
                stdin_thread.start()

            try:
                while True:
                    done = channel.exit_status_ready()
                    if done and not channel.recv_ready() and not channel.recv_stderr_ready():
                        break
                    r, _w, _x = select.select([channel], [], [], 0.2)
                    if channel in r:
                        if channel.recv_ready():
                            data = channel.recv(65536)
                            if data:
                                stdout_writer(data)
                        if channel.recv_stderr_ready():
                            data = channel.recv_stderr(65536)
                            if data:
                                stderr_writer(data)
                while channel.recv_ready():
                    stdout_writer(channel.recv(65536))
                while channel.recv_stderr_ready():
                    stderr_writer(channel.recv_stderr(65536))
            finally:
                stop_stdin.set()
                # don't join — thread is daemon, blocks on select; will exit on next tick

            return channel.recv_exit_status()
        finally:
            try:
                channel.close()
            except Exception:
                pass

    # ---- files ----

    def exists(self, path: str) -> bool:
        """True iff the path exists. Permission denied / transport errors propagate."""
        sftp = self._ensure_sftp()
        try:
            sftp.stat(self.expand(path))
            return True
        except FileNotFoundError:
            return False
        # NOTE: do not swallow other OSError/IOError — those are real failures
        # (EACCES, transport blips, server hiccups) and must reach retry/caller.

    def mtime(self, path: str) -> float | None:
        """Remote mtime as unix timestamp, or None ONLY when path is missing.

        Any other SFTP error propagates (so retry can take a swing and a real
        EACCES isn't silently treated as 'remote needs upload')."""
        sftp = self._ensure_sftp()
        try:
            st = sftp.stat(self.expand(path))
        except FileNotFoundError:
            return None
        return float(st.st_mtime) if st.st_mtime is not None else None

    def read_file(self, path: str) -> bytes:
        sftp = self._ensure_sftp()
        with sftp.open(self.expand(path), "rb") as f:
            return f.read()

    def write_file(self, path: str, data: bytes, mode: int | None = None) -> None:
        """Atomic upload: write to <target>.flux-<ts>.tmp then posix_rename.

        On any failure mid-flight, the tmp file is removed and the original
        target is left untouched. We intentionally do NOT remove the target
        before rename — a permission/transport failure from posix_rename
        would otherwise destroy the existing file.
        """
        sftp = self._ensure_sftp()
        target = self.expand(path)
        parent = str(PurePosixPath(target).parent)
        if parent and parent not in (".", "/", ""):
            self.ensure_dir(parent)
        tmp = f"{target}.flux-{int(time.time() * 1000)}.tmp"
        try:
            with sftp.open(tmp, "wb") as f:
                f.write(data)
            if mode is not None:
                sftp.chmod(tmp, mode)
            try:
                sftp.posix_rename(tmp, target)
            except AttributeError:
                # client-side: paramiko too old, method missing
                _fallback_rename(sftp, tmp, target)
            except (IOError, OSError) as exc:
                # server-side: distinguish "extension unsupported" from real errors.
                # Permission denied / transport blips MUST propagate (else we'd
                # delete the target in the fallback path).
                msg = str(exc).lower()
                if "unsupported" in msg or "not implemented" in msg or "op_unsupported" in msg:
                    _fallback_rename(sftp, tmp, target)
                else:
                    raise
        except Exception:
            # best-effort tmp cleanup; the target is intact since we never
            # touched it before a confirmed successful rename.
            try:
                sftp.remove(tmp)
            except Exception:
                pass
            raise

    def chmod(self, path: str, mode: int) -> None:
        self._ensure_sftp().chmod(self.expand(path), mode)

    def ensure_dir(self, path: str) -> None:
        """`mkdir -p` over exec, cached per-client to avoid repeated RTTs."""
        target = self.expand(path)
        if target in self._ensured_dirs:
            return
        r = self.exec(f"mkdir -p {shlex.quote(target)}")
        if not r.ok:
            raise SshError(f"mkdir -p {path} failed: {r.stderr.decode()}")
        self._ensured_dirs.add(target)

    # ---- reverse port forwarding ----

    def reverse_forward(
        self,
        local_port: int,
        remote_port: int,
        *,
        on_connect: Callable[[], None] | None = None,
        on_disconnect: Callable[[], None] | None = None,
    ) -> None:
        """Remote listens on 127.0.0.1:remote_port; pipes to 127.0.0.1:local_port here.

        on_connect / on_disconnect fire for each forwarded TCP session, in a
        paramiko background thread. The handler is non-blocking.
        """
        transport = self._ensure_client().get_transport()
        if transport is None:
            raise SshError("transport not available")

        def handler(channel: paramiko.Channel, src_addr: tuple, dst_addr: tuple) -> None:
            sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
            sock.settimeout(3.0)
            try:
                sock.connect(("127.0.0.1", local_port))
            except Exception as exc:
                log.warning("forward to 127.0.0.1:%s failed: %s", local_port, exc)
                # close BOTH ends — sock was created before connect raised and
                # was leaking before this fix (descriptor exhaustion on retry storms).
                _quiet_close(sock)
                _quiet_close(channel)
                return
            sock.settimeout(None)
            if on_connect:
                try:
                    on_connect()
                except Exception:
                    pass
            _pipe_channel_socket(channel, sock, on_disconnect)

        transport.request_port_forward("127.0.0.1", remote_port, handler=handler)


def _pipe_channel_socket(
    channel: paramiko.Channel,
    sock: socket.socket,
    on_close: Callable[[], None] | None,
) -> None:
    """Pump bytes both ways. Fires on_close once when either side dies."""
    closed = threading.Event()

    def signal_close() -> None:
        if closed.is_set():
            return
        closed.set()
        if on_close:
            try:
                on_close()
            except Exception:
                pass

    def chan_to_sock() -> None:
        try:
            while True:
                data = channel.recv(65536)
                if not data:
                    break
                sock.sendall(data)
        except Exception:
            pass
        finally:
            _quiet_close(channel)
            _quiet_close(sock)
            signal_close()

    def sock_to_chan() -> None:
        try:
            while True:
                data = sock.recv(65536)
                if not data:
                    break
                channel.sendall(data)
        except Exception:
            pass
        finally:
            _quiet_close(channel)
            _quiet_close(sock)
            signal_close()

    threading.Thread(target=chan_to_sock, daemon=True).start()
    threading.Thread(target=sock_to_chan, daemon=True).start()


def _quiet_close(obj: object) -> None:
    try:
        getattr(obj, "close")()
    except Exception:
        pass


def _install_pubkey_if_configured(client, key_path: str | None, password: str | None) -> bool:
    """Append local `<key>.pub` to remote ~/.ssh/authorized_keys for passwordless login.

    Triggers only when BOTH key and password are configured (user clearly wants
    setup: first connect via password, subsequent via key). Idempotent — skips
    when the key blob is already present. Returns True if installed or already
    present; False if conditions weren't met (no install attempted).
    """
    if not key_path or not password:
        return False
    local_key = Path(key_path).expanduser()
    pub_path = local_key.with_name(local_key.name + ".pub")
    if not pub_path.exists():
        log.debug("local pubkey %s not found; skipping auto-install", pub_path)
        return False
    try:
        pubkey_text = pub_path.read_text(encoding="utf-8").strip()
    except OSError as exc:
        log.debug("could not read local pubkey: %s", exc)
        return False
    if not pubkey_text:
        return False

    parts = pubkey_text.split()
    if len(parts) < 2:
        log.debug("malformed pubkey in %s", pub_path)
        return False
    pub_blob = f"{parts[0]} {parts[1]}"  # algo + base64, ignore comment

    auth_keys_path = "~/.ssh/authorized_keys"
    try:
        existing_text = client.read_file(auth_keys_path).decode("utf-8", errors="replace")
    except FileNotFoundError:
        existing_text = ""

    for line in existing_text.splitlines():
        stripped = line.strip()
        if not stripped or stripped.startswith("#"):
            continue
        line_parts = stripped.split()
        if len(line_parts) >= 2 and f"{line_parts[0]} {line_parts[1]}" == pub_blob:
            log.debug("pubkey already in remote authorized_keys; skipping")
            return True

    new_text = existing_text
    if new_text and not new_text.endswith("\n"):
        new_text += "\n"
    new_text += pubkey_text + "\n"

    r = client.exec("mkdir -p ~/.ssh && chmod 700 ~/.ssh")
    if not r.ok:
        log.warning("could not prepare remote ~/.ssh: %s", r.stderr.decode(errors="replace"))
        return False

    client.write_file(auth_keys_path, new_text.encode("utf-8"), mode=0o600)

    from rich.console import Console

    Console(stderr=True).print(
        f"[green]✓[/] installed pubkey to remote ~/.ssh/authorized_keys  "
        f"[dim]{parts[0]} {parts[1][:16]}...[/]"
    )
    return True


def _fallback_rename(sftp: "paramiko.SFTPClient", tmp: str, target: str) -> None:
    """Non-atomic fallback for servers without posix-rename@openssh.com.

    Tries plain `rename` first (most servers fail if target exists); only as a
    last resort does `remove(target)` + `rename(tmp, target)`. The latter loses
    atomicity but is the only option when posix_rename is genuinely missing.
    """
    try:
        sftp.rename(tmp, target)
    except IOError:
        sftp.remove(target)
        sftp.rename(tmp, target)


def _terminal_size() -> tuple[int, int]:
    """Return (rows, cols) for the local terminal; (24, 80) fallback."""
    try:
        size = __import__("shutil").get_terminal_size()
        return size.lines, size.columns
    except Exception:
        return 24, 80


def _pump_stdin_to_channel(channel: paramiko.Channel, stop: threading.Event) -> None:
    """Forward local stdin bytes into the SSH channel until stop fires or stdin closes."""
    import io
    import os as _os

    try:
        fd = sys.stdin.fileno()
    except (AttributeError, io.UnsupportedOperation, ValueError):
        return
    # On Windows, select() does not work on stdin fds — skip stdin forwarding there.
    if sys.platform == "win32":
        return
    while not stop.is_set():
        try:
            r, _w, _x = select.select([fd], [], [], 0.2)
            if not r:
                continue
            data = _os.read(fd, 4096)
            if not data:
                break
            try:
                channel.send(data)
            except OSError:
                break
        except (OSError, ValueError):
            break


def _sha256_fp(key) -> str:
    """Format key as OpenSSH-style SHA256:<base64-no-pad>."""
    import base64
    import hashlib

    digest = hashlib.sha256(key.asbytes()).digest()
    return "SHA256:" + base64.b64encode(digest).decode("ascii").rstrip("=")


def _persist_host_key(known_hosts: Path, hostname: str, key) -> None:
    """Append a single `<host> <type> <b64>` line; create dir + chmod as needed."""
    known_hosts.parent.mkdir(mode=0o700, exist_ok=True)
    with known_hosts.open("a", encoding="ascii") as f:
        f.write(f"{hostname} {key.get_name()} {key.get_base64()}\n")
    try:
        known_hosts.chmod(0o600)
    except OSError:
        pass


class _AppendToKnownHostsPolicy(paramiko.MissingHostKeyPolicy):
    """TOFU: silently trust the key, persist to known_hosts, print a notice.

    Subsequent connects use paramiko's normal validation against the saved key,
    so a swapped key on connect N+1 is REJECTED. Risk window is connect #1 only.
    """

    def __init__(self, known_hosts: Path) -> None:
        self._known_hosts = known_hosts

    def missing_host_key(self, client, hostname, key) -> None:  # type: ignore[override]
        from rich.console import Console

        notice = Console(stderr=True)
        notice.print(
            f"[yellow]trusting new host key for[/] {hostname}  "
            f"[dim]{key.get_name()} {_sha256_fp(key)}[/]"
        )
        client.get_host_keys().add(hostname, key.get_name(), key)
        try:
            _persist_host_key(self._known_hosts, hostname, key)
        except OSError as exc:
            notice.print(f"[yellow]warn:[/] could not persist host key: {exc}")


@contextmanager
def open_client(conn: ResolvedConnection) -> Iterator[SshClient]:
    client = SshClient(conn)
    try:
        client.connect()
        yield client
    finally:
        client.close()
