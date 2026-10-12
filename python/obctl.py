#!/usr/bin/env python3
"""One-command setup / start / stop driven by config/*.json.

feedd and the adapter are background processes with pid files. The TUI is
not. On a real terminal, start replaces this process with the TUI, so
`stop` cannot find a tui pid. Type quit in that window.

A pid file can outlive its process, and a second `start` used to overwrite
the pid files of the instance already running: the first feedd and adapter
then ran with nothing pointing at them and the port stayed bound. `start`
now refuses that state instead of creating it, and `stop` says what it did.
"""
from __future__ import annotations

import argparse
import json
import os
import signal
import socket
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]

# A pid file is not proof of ownership: macOS reuses pids, and the file
# outlives a crash. Before any signal, the command line must still look like
# this lab's program for that name.
PROC_MARKERS = {
    "feedd": "build/feedd",
    "adapter": "feed_adapter.py",
    "tui": "build/tui",
}

# feedd prints its ready line as soon as it is bound, so this ceiling is only
# paid when it never gets there. The adapter has no ready line, so it is given
# just long enough to die if it is going to. STOP_CEILING covers a loop that
# sleeps 50 ms between checks of its run flag.
START_CEILING = 2.0
ADAPTER_CEILING = 0.4
STOP_CEILING = 2.0


def load(path: Path) -> dict:
    return json.loads(path.read_text(encoding="utf-8"))


def guard(cfg: dict) -> None:
    """Same policy as the C++ loader, checked again before any process starts.

    Two gates, both fatal. Prod needs the environment variable. Dev must
    not set allow_orders. Say this out loud: the lab cannot place an order
    by accident because the start path refuses that config.
    """
    if cfg.get("env") == "prod" and os.environ.get("ORDERBOOK_ALLOW_PROD") != "1":
        raise SystemExit("refusing prod (ORDERBOOK_ALLOW_PROD=1 required)")
    if cfg.get("env") == "dev" and cfg.get("allow_orders"):
        raise SystemExit("dev config must keep allow_orders false")


def run(cmd: list[str], **kw) -> subprocess.CompletedProcess:
    print("+", " ".join(cmd))
    return subprocess.run(cmd, cwd=str(ROOT), check=True, **kw)


def setup(cfg_path: Path) -> None:
    """Create the data directories and compile. Does not start processes."""
    cfg = load(cfg_path)
    guard(cfg)
    for key in ("bus_path", "wal_path", "log_path"):
        p = ROOT / cfg[key]
        p.parent.mkdir(parents=True, exist_ok=True)
    run(["make", "-C", str(ROOT), "all"])
    print("setup ok")


def pid_file(name: str) -> Path:
    return ROOT / "var" / "run" / f"{name}.pid"


def log_file(name: str) -> Path:
    return ROOT / "var" / "run" / f"{name}.log"


def pid_alive(pid: int) -> bool:
    """Signal 0 asks the kernel whether the pid exists. Nothing is sent.

    PermissionError means the process is there and belongs to someone else,
    which still counts as alive for every check here.
    """
    try:
        os.kill(pid, 0)
        return True
    except PermissionError:
        return True
    except (OSError, ValueError, OverflowError):
        return False


def proc_command(pid: int) -> str:
    """The command line ps reports, or "" when the process is gone."""
    try:
        out = subprocess.run(
            ["ps", "-p", str(pid), "-o", "command="],
            capture_output=True,
            text=True,
            timeout=5,
        )
    except (OSError, subprocess.SubprocessError):
        return ""
    return out.stdout.strip()


def owns(pid: int, name: str) -> bool:
    """True when the process at this pid still looks like this lab's program.

    Checked before every signal. A pid recycled by an unrelated program must
    not be killed, and `make stop` must not reach outside this lab.
    """
    marker = PROC_MARKERS.get(name)
    return bool(marker) and marker in proc_command(pid)


def read_pid(name: str) -> int | None:
    """The pid in the file, or None when it is missing, unreadable, or not a pid.

    Zero and negatives are rejected on purpose: os.kill treats 0 and -1 as a
    process group, so a corrupt pid file must not reach the signal path.
    """
    path = pid_file(name)
    if not path.is_file():
        return None
    try:
        pid = int(path.read_text().strip())
    except (OSError, ValueError):
        return None
    return pid if pid > 0 else None


def tracked(name: str) -> int | None:
    """The pid file's process, when it is alive and still ours."""
    pid = read_pid(name)
    if pid is not None and pid_alive(pid) and owns(pid, name):
        return pid
    return None


def port_in_use(host: str, port: int) -> bool:
    """True when something already listens on the configured address.

    A pid file only covers processes this repo started. A feedd from another
    worktree or an older session leaves no pid file here, and its bind failure
    is one line in a log nobody is reading. The probe sets the same
    SO_REUSEADDR that src/feedd.cpp sets, so it fails exactly when feedd's own
    bind would fail.
    """
    probe = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    try:
        probe.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        probe.bind((host, port))
        return False
    except OSError:
        return True
    finally:
        probe.close()


def log_offset(name: str) -> int:
    try:
        return log_file(name).stat().st_size
    except OSError:
        return 0


def new_log_lines(name: str, offset: int, keep: int = 5) -> str:
    """The last few lines this run wrote, so a dead child can speak for itself."""
    try:
        with open(log_file(name), "rb") as f:
            f.seek(offset)
            text = f.read().decode("utf-8", "replace")
    except OSError:
        return ""
    return "\n".join(text.strip().splitlines()[-keep:])


def await_started(name: str, offset: int, marker: str, ceiling: float) -> None:
    """Wait for the child to prove it is up, or report why it is not.

    Without this, a feedd that cannot bind leaves a pid file for a dead
    process: the next stop has nothing to signal and the port stays taken.
    feedd prints `feedd listen` once bound. The adapter has no ready line, so
    staying alive past the ceiling is its proof.
    """
    pid = read_pid(name)
    deadline = time.monotonic() + ceiling
    while time.monotonic() < deadline:
        if marker and marker in new_log_lines(name, offset):
            return
        if pid is None or not pid_alive(pid):
            break
        time.sleep(0.05)
    if pid is not None and pid_alive(pid):
        return
    pid_file(name).unlink(missing_ok=True)
    detail = new_log_lines(name, offset) or "(no output)"
    raise SystemExit(f"{name} did not start (pid={pid}). Last lines:\n{detail}")


def spawn(name: str, cmd: list[str]) -> None:
    """Background process, new session, pid file under var/run.

    start_new_session means the child is not killed when this script execs
    the TUI. stop reads the pid file later.
    """
    pid_file(name).parent.mkdir(parents=True, exist_ok=True)
    log = open(log_file(name), "ab")
    proc = subprocess.Popen(cmd, cwd=str(ROOT), stdout=log, stderr=log, start_new_session=True)
    log.close()
    pid_file(name).write_text(str(proc.pid))
    print(f"started {name} pid={proc.pid}")


def preflight(cfg: dict, cfg_path: Path) -> None:
    """Refuse to start on top of an instance that is already running.

    Two gates, both fatal. The pid check catches the second adapter, which
    would connect to the running feedd and double the publish rate. The port
    check catches a feedd this repo never recorded, from another worktree or
    an older session, whose bind failure would otherwise be silent.
    """
    for name in ("feedd", "adapter"):
        pid = tracked(name)
        if pid is not None:
            raise SystemExit(
                f"already running: {name} pid={pid}. "
                f"Run 'python3 python/obctl.py stop --config {cfg_path}' first."
            )
    host = str(cfg.get("listen_host", "127.0.0.1"))
    port = int(cfg.get("listen_port", 9001))
    if port_in_use(host, port):
        raise SystemExit(
            f"refusing to start: {host}:{port} is already in use, so feedd cannot bind. "
            f"Another feedd is running (an older session or a second worktree). Stop it, "
            f"or change listen_port in {cfg_path}."
        )


def start(cfg_path: Path, fixture: str, with_tui: bool) -> None:
    cfg = load(cfg_path)
    guard(cfg)
    preflight(cfg, cfg_path)
    setup(cfg_path)
    cfg_abs = str(cfg_path.resolve())
    feedd_offset = log_offset("feedd")
    spawn("feedd", [str(ROOT / "build" / "feedd"), cfg_abs])
    # The adapter dials feedd, so feedd must be listening first. The ready line
    # is a stronger gate than a fixed sleep, and it reports a bind failure.
    await_started("feedd", feedd_offset, "feedd listen", START_CEILING)
    adapter = [sys.executable, str(ROOT / "python" / "feed_adapter.py"), "--config", cfg_abs]
    if fixture:
        adapter += ["--fixture", str(Path(fixture).resolve())]
    adapter_offset = log_offset("adapter")
    spawn("adapter", adapter)
    await_started("adapter", adapter_offset, "", ADAPTER_CEILING)
    # exec only when stdin is a terminal. make e2e is not, so it stays headless.
    if with_tui and sys.stdin.isatty():
        os.execv(str(ROOT / "build" / "tui"), [str(ROOT / "build" / "tui"), cfg_abs])
    print("start ok (no tui)")


def kill_pid(name: str) -> int | None:
    """SIGTERM the tracked process and confirm it went away.

    Returns the pid when it is still alive after the grace period, so stop can
    say so instead of printing `stop ok` over a process that is still holding
    the port. The pid file goes away with the process and stays when it does
    not: a survivor must still be findable by the next stop, and by the
    preflight that refuses the next start.
    """
    path = pid_file(name)
    pid = read_pid(name)
    if pid is None:
        path.unlink(missing_ok=True)
        return None
    if not pid_alive(pid):
        path.unlink(missing_ok=True)
        return None
    if not owns(pid, name):
        print(f"left alone: {name} pid={pid} is not this lab ({proc_command(pid) or 'unreadable'})")
        path.unlink(missing_ok=True)
        return None
    try:
        os.kill(pid, signal.SIGTERM)
    except OSError as exc:
        print(f"cannot signal {name} pid={pid}: {exc}")
        return pid
    deadline = time.monotonic() + STOP_CEILING
    while time.monotonic() < deadline:
        if not pid_alive(pid):
            path.unlink(missing_ok=True)
            return None
        time.sleep(0.05)
    return pid


def stop(_cfg_path: Path) -> None:
    stuck: list[str] = []
    for name in ("tui", "adapter", "feedd"):
        pid = kill_pid(name)
        if pid is not None:
            stuck.append(f"{name} pid={pid}")
    if stuck:
        print("still running after SIGTERM: " + ", ".join(stuck))
        print("These keep the port bound. Send SIGKILL, or close the window that owns them.")
        raise SystemExit(1)
    print("stop ok")


def main() -> int:
    ap = argparse.ArgumentParser(prog="ob")
    ap.add_argument("cmd", choices=["setup", "start", "stop"])
    ap.add_argument("--config", default=str(ROOT / "config" / "dev.json"))
    ap.add_argument("--fixture", default="")
    ap.add_argument("--tui", action="store_true")
    args = ap.parse_args()
    cfg_path = Path(args.config)
    if args.cmd == "setup":
        setup(cfg_path)
    elif args.cmd == "start":
        start(cfg_path, args.fixture, args.tui)
    else:
        stop(cfg_path)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
