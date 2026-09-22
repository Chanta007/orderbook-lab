#!/usr/bin/env python3
"""One-command setup / start / stop driven by config/*.json.

feedd and the adapter are background processes with pid files. The TUI is
not. On a real terminal, start replaces this process with the TUI, so
`stop` cannot find a tui pid. Type quit in that window.
"""
from __future__ import annotations

import argparse
import json
import os
import signal
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


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


def spawn(name: str, cmd: list[str]) -> None:
    """Background process, new session, pid file under var/run.

    start_new_session means the child is not killed when this script execs
    the TUI. stop reads the pid file later.
    """
    pid_file(name).parent.mkdir(parents=True, exist_ok=True)
    log = open(ROOT / "var" / "run" / f"{name}.log", "ab")
    proc = subprocess.Popen(cmd, cwd=str(ROOT), stdout=log, stderr=log, start_new_session=True)
    pid_file(name).write_text(str(proc.pid))
    print(f"started {name} pid={proc.pid}")


def start(cfg_path: Path, fixture: str, with_tui: bool) -> None:
    cfg = load(cfg_path)
    guard(cfg)
    setup(cfg_path)
    cfg_abs = str(cfg_path.resolve())
    spawn("feedd", [str(ROOT / "build" / "feedd"), cfg_abs])
    time.sleep(0.2)
    adapter = [sys.executable, str(ROOT / "python" / "feed_adapter.py"), "--config", cfg_abs]
    if fixture:
        adapter += ["--fixture", str(Path(fixture).resolve())]
    spawn("adapter", adapter)
    # exec only when stdin is a terminal. make e2e is not, so it stays headless.
    if with_tui and sys.stdin.isatty():
        os.execv(str(ROOT / "build" / "tui"), [str(ROOT / "build" / "tui"), cfg_abs])
    print("start ok (no tui)")


def kill_pid(path: Path) -> None:
    if not path.is_file():
        return
    try:
        pid = int(path.read_text().strip())
        os.kill(pid, signal.SIGTERM)
    except (ValueError, ProcessLookupError, PermissionError):
        pass
    path.unlink(missing_ok=True)


def stop(_cfg_path: Path) -> None:
    for name in ("tui", "adapter", "feedd"):
        kill_pid(pid_file(name))
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
