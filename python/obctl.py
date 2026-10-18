#!/usr/bin/env python3
"""One-command setup / start / stop driven by config/*.json.

`--impl` picks the build: cpp (the default) runs build/feedd and build/tui,
rust runs rust/target/release/feedd and rust/target/release/tui. The Python
adapter is shared, because it speaks the same wire protocol to either, and
both builds write the same 72-byte record into the same ring file and WAL.

feedd and the adapter are background processes with pid files. The TUI is
not. On a real terminal, start replaces this process with the TUI, so
`stop` cannot find a tui pid. Type quit in that window.

A pid file can outlive its process, and a second `start` used to overwrite
the pid files of the instance already running: the first feedd and adapter
then ran with nothing pointing at them and the port stayed bound. `start`
now refuses that state instead of creating it, and `stop` says what it did.

The pid files and the process logs are keyed by the config file, under
`var/run/<config>/`, so two configs never reach each other's processes.
See `run_dir`, and OPT-038 in docs/optimization/ledger.jsonl for what a
shared directory cost.
"""
from __future__ import annotations

import argparse
import errno
import json
import os
import shutil
import signal
import socket
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]

# The processes this script starts, by implementation, each with the marker
# that proves a pid is still that process. The marker is the path of the
# binary, relative to ROOT, and not the bare program name: `feedd` alone
# would match any unrelated process carrying the word in its argv. Both
# builds install binaries with the same names under different paths, so the
# marker also says which build the pid belongs to. The adapter is one Python
# program shared by both implementations.
PROC_MARKERS = {
    "cpp": {
        "feedd": "build/feedd",
        "adapter": "feed_adapter.py",
        "tui": "build/tui",
    },
    "rust": {
        "feedd": "rust/target/release/feedd",
        "adapter": "feed_adapter.py",
        "tui": "rust/target/release/tui",
    },
}

DEFAULT_IMPL = "cpp"


def marker(name: str, impl: str) -> str:
    """The marker that proves a pid is this lab's `name` as built by `impl`.
    It is also that binary's path, relative to ROOT."""
    return PROC_MARKERS[impl][name]


def markers(name: str) -> tuple[str, ...]:
    """Every marker that proves this pid is this lab's `name`, one per
    implementation. Empty for a name this script never starts.

    Ownership accepts either implementation on purpose. The pid file and the
    listen port are shared, so a stop that only recognised the implementation
    named on its own command line would leave the other one running with
    nothing pointing at it.
    """
    return tuple(marks[name] for marks in PROC_MARKERS.values() if name in marks)


def binary_path(name: str, impl: str) -> Path:
    """Where `impl` installs the named binary, as start will exec it."""
    return ROOT / marker(name, impl)

# Each child prints a ready line, and the wait ends when it appears. The
# ceiling is only paid when the line never comes, and it covers a child that
# is slow to die as well as one that is slow to start. STOP_CEILING covers a
# loop that sleeps 50 ms between checks of its run flag.
#
# START_CEILING is generous because of a macOS behaviour that has nothing to do
# with this code: the first execution of a freshly linked binary pays for code
# signature validation, and that took over three seconds here for a 600 KB
# binary. A two-second ceiling turned that into "feedd did not start", which
# reads as a bug in the daemon and is not one. Measured: a hello-world Rust
# binary took 5.1 s on its first run and 0.01 s on every run after it.
#
# A high ceiling costs nothing in the normal case: the loop returns as soon as
# the marker appears, so the ceiling is only ever paid by a start that is
# genuinely broken.
START_CEILING = 15.0
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


# Where a build tool may be when it is not on PATH. rustup installs into
# ~/.cargo/bin and adds it through ~/.cargo/env, which a shell sources only if its
# profile has been loaded: a terminal opened before that line existed, or an IDE
# that does not read the profile, hands this script an environment without cargo.
# The two Homebrew prefixes are here for the same reason.
TOOL_SEARCH_DIRS = (
    Path.home() / ".cargo" / "bin",
    Path("/opt/homebrew/bin"),
    Path("/usr/local/bin"),
    Path("/usr/bin"),
)


def resolve_tool(name: str) -> str | None:
    """Find a build tool, on PATH or in the places it is usually installed.

    Returning the absolute path matters for more than finding the file: the
    failure this guards against used to be a Python traceback that named neither
    the tool nor the fix, which tells a reader nothing they can act on.
    """
    found = shutil.which(name)
    if found:
        return found
    for directory in TOOL_SEARCH_DIRS:
        candidate = directory / name
        if candidate.is_file() and os.access(candidate, os.X_OK):
            return str(candidate)
    return None


def require_tool(name: str, why: str) -> str:
    """The path to a build tool, or a message that says what to do about it."""
    path = resolve_tool(name)
    if path is not None:
        return path
    searched = ", ".join(str(d) for d in TOOL_SEARCH_DIRS)
    raise SystemExit(
        f"{name} is not installed, or not where this script can find it.\n"
        f"  needed to {why}\n"
        f"  looked on PATH and in: {searched}\n"
        f"  install it, or put it on PATH — for cargo, `source ~/.cargo/env`\n"
        f"  see docs/runbooks/rust-build-failures.md"
    )


def run(cmd: list[str], **kw) -> subprocess.CompletedProcess:
    print("+", " ".join(cmd))
    try:
        return subprocess.run(cmd, cwd=str(ROOT), check=True, **kw)
    except FileNotFoundError as exc:
        # A missing executable is a setup problem, not a traceback. This is the
        # backstop for any command that did not go through require_tool.
        raise SystemExit(
            f"cannot run {cmd[0]!r}: not found. Install it or put it on PATH."
        ) from exc


def setup(cfg_path: Path, impl: str = DEFAULT_IMPL) -> None:
    """Create the data directories and compile. Does not start processes."""
    cfg = load(cfg_path)
    guard(cfg)
    run_dir(cfg_path).mkdir(parents=True, exist_ok=True)
    for key in ("bus_path", "wal_path", "log_path"):
        p = ROOT / cfg[key]
        p.parent.mkdir(parents=True, exist_ok=True)
    if impl == "rust":
        cargo = require_tool("cargo", "build the Rust workspace")
        run([cargo, "build", "--release", "--manifest-path", str(ROOT / "rust" / "Cargo.toml")])
    else:
        make = require_tool("make", "build the C++ binaries")
        run([make, "-C", str(ROOT), "all"])
    print("setup ok")


def run_dir(cfg_path: Path) -> Path:
    """Where one config's pid files and process logs live.

    Keyed by the config file's name, not by its `env`. Two configs can share an
    env and must not share a pid file: config/test.json is `env: dev`, and the
    instance it runs must not be reachable by the same `stop` as the window
    somebody has open. Before this every config used `var/run`, so `make e2e` —
    a test — signalled the live session's feedd and adapter, and the window it
    left behind was stalled with nothing to feed it. See
    docs/optimization/ledger.jsonl OPT-038.
    """
    return ROOT / "var" / "run" / Path(cfg_path).stem


def pid_file(name: str, cfg_path: Path) -> Path:
    return run_dir(cfg_path) / f"{name}.pid"


def log_file(name: str, cfg_path: Path) -> Path:
    return run_dir(cfg_path) / f"{name}.log"


def pid_alive(pid: int) -> bool:
    """Signal 0 asks the kernel whether the pid exists. Nothing is sent.

    PermissionError means the process is there and belongs to someone else,
    which still counts as alive for every check here.

    This answers "does the pid exist", not "is the process running". A child
    that has exited but not been reaped is a zombie, and its pid exists until
    its parent collects it. The stop path asks ps instead, which collects the
    child on the way in (see kill_pid).
    """
    try:
        os.kill(pid, 0)
        return True
    except PermissionError:
        return True
    except (OSError, ValueError, OverflowError):
        return False


def proc_command(pid: int) -> str | None:
    """The command line ps reports, "" when the process is gone, None when ps
    could not be asked.

    "Gone" and "cannot tell" must not collapse into one value. The first is
    safe to clean up after. The second must keep its pid file and be reported,
    or a live process ends up running with nothing pointing at it.

    Asking ps also collects any child of ours that has exited, because the
    subprocess module reaps finished children before it starts a new one. That
    is what makes an empty answer here mean "not running" rather than "a
    zombie the kernel still knows".
    """
    try:
        out = subprocess.run(
            ["ps", "-p", str(pid), "-o", "command="],
            capture_output=True,
            text=True,
            timeout=5,
        )
    except (OSError, subprocess.SubprocessError):
        return None
    return out.stdout.strip()


ZOMBIE_COMMAND = "<defunct>"


def proc_state(pid: int) -> str | None:
    """The ps state letter: R or S for a live process, Z for a zombie that has
    not been collected, "" when the pid is gone, None when ps could not be
    asked.

    The letter is the portable signal. BSD ps and procps both report Z, while
    the text ps puts in place of the command for a zombie differs: macOS prints
    `<defunct>`, procps prints the command with a marker in front of it.
    """
    try:
        out = subprocess.run(
            ["ps", "-p", str(pid), "-o", "state="],
            capture_output=True,
            text=True,
            timeout=5,
        )
    except (OSError, subprocess.SubprocessError):
        return None
    return out.stdout.strip()


def proc_running(pid: int) -> bool | None:
    """True when a process is running at this pid, False when none is, None
    when ps could not be asked.

    A process that has exited but not been collected is a zombie: only its exit
    status is left, its pid is still valid, and signal 0 succeeds for it. That
    is not running, and a stop that waits for it waits the whole grace period
    and then reports a failure that is not one.
    """
    command = proc_command(pid)
    if command is None:
        return None
    if not command:
        return False
    state = proc_state(pid)
    if state is None:
        return ZOMBIE_COMMAND not in command
    return bool(state) and not state.startswith("Z")


def owns(pid: int, name: str) -> bool:
    """True when the process at this pid still looks like this lab's program.

    Checked before every signal. A pid recycled by an unrelated program must
    not be killed, and `make stop` must not reach outside this lab. An
    unreadable command line is not ownership.

    Either implementation counts as this lab; see markers() for why.
    """
    command = proc_command(pid)
    if command is None:
        return False
    return any(m in command for m in markers(name))


def read_pid(name: str, cfg_path: Path) -> int | None:
    """The pid in the file, or None when it is missing, unreadable, or not a pid.

    Zero and negatives are rejected on purpose: os.kill treats 0 and -1 as a
    process group, so a corrupt pid file must not reach the signal path.
    """
    path = pid_file(name, cfg_path)
    if not path.is_file():
        return None
    try:
        pid = int(path.read_text().strip())
    except (OSError, ValueError):
        return None
    return pid if pid > 0 else None


def tracked(name: str, cfg_path: Path) -> int | None:
    """The pid file's process, when it is alive and still ours."""
    pid = read_pid(name, cfg_path)
    if pid is not None and pid_alive(pid) and owns(pid, name):
        return pid
    return None


def port_refusal(host: str, port: int, cfg_path: Path) -> str:
    """Why feedd cannot bind this address, or "" when it is free.

    A pid file only covers processes this repo started. A feedd from another
    worktree or an older session leaves no pid file here, and its bind failure
    is one line in a log nobody is reading. The probe sets the same
    SO_REUSEADDR that src/feedd.cpp sets, so it fails when feedd's own bind
    would fail.

    The reason is reported rather than guessed. Only EADDRINUSE means another
    feedd; a privileged port or an address that is not on this host would send
    the user hunting for a process that does not exist.
    """
    probe = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    try:
        probe.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        probe.bind((host, port))
        return ""
    except OSError as exc:
        if exc.errno == errno.EADDRINUSE:
            return (
                f"{host}:{port} is already in use. Another feedd is running (an older "
                f"session or a second worktree). Stop it, or change listen_port in {cfg_path}."
            )
        return (
            f"{host}:{port} cannot be bound ({exc.strerror}). feedd would fail the same way. "
            f"Change listen_host or listen_port in {cfg_path}."
        )
    finally:
        probe.close()


def log_offset(name: str, cfg_path: Path) -> int:
    try:
        return log_file(name, cfg_path).stat().st_size
    except OSError:
        return 0


def new_log_lines(name: str, cfg_path: Path, offset: int, keep: int = 5) -> str:
    """The last few lines this run wrote, so a dead child can speak for itself."""
    try:
        with open(log_file(name, cfg_path), "rb") as f:
            f.seek(offset)
            text = f.read().decode("utf-8", "replace")
    except OSError:
        return ""
    return "\n".join(text.strip().splitlines()[-keep:])


def await_started(
    name: str, cfg_path: Path, proc: subprocess.Popen, offset: int, marker: str, ceiling: float
) -> None:
    """Wait for the child's ready line, or report why it is not coming.

    The child is watched through its own handle, never through the pid. A
    child that has exited but not been reaped is a zombie, and os.kill(pid, 0)
    succeeds for a zombie: a pid check calls a feedd that just died `alive`,
    returns success, and leaves a pid file pointing at nothing.
    """
    deadline = time.monotonic() + ceiling
    while time.monotonic() < deadline:
        if marker in new_log_lines(name, cfg_path, offset):
            return
        if proc.poll() is not None:
            break
        time.sleep(0.05)
    if marker in new_log_lines(name, cfg_path, offset):
        return
    pid_file(name, cfg_path).unlink(missing_ok=True)
    detail = new_log_lines(name, cfg_path, offset)
    # Three different failures used to produce the same unhelpful line. A child
    # that is still running but silent is not the same as one that died, and a
    # child that died without writing anything is not the same as one that wrote
    # a reason: a slow start under disk load looked exactly like a crash, and
    # "Last lines: (no output)" sent the reader looking in the wrong place.
    status = proc.poll()
    if detail:
        why = "its last lines follow"
    elif status is not None:
        why = f"it exited with status {status} and wrote nothing"
    else:
        why = (
            f"it is still running but has not announced itself within {ceiling}s; "
            f"the start is slow rather than broken"
        )
    raise SystemExit(
        f"{name} did not start (pid={proc.pid}): {why}.\n"
        f"  ready line looked for: {marker!r}\n"
        f"  log: {log_file(name, cfg_path)}\n"
        f"{(chr(10) + detail) if detail else ''}"
    )


def spawn(name: str, cfg_path: Path, cmd: list[str]) -> subprocess.Popen:
    """Background process, new session, pid file in this config's run dir.

    start_new_session means the child is not killed when this script execs
    the TUI. stop reads the pid file later. The handle is returned so the
    caller can see the child exit, which the pid cannot show.
    """
    pid_file(name, cfg_path).parent.mkdir(parents=True, exist_ok=True)
    log = open(log_file(name, cfg_path), "ab")
    try:
        proc = subprocess.Popen(cmd, cwd=str(ROOT), stdout=log, stderr=log, start_new_session=True)
    finally:
        log.close()
    pid_file(name, cfg_path).write_text(str(proc.pid))
    print(f"started {name} pid={proc.pid}")
    return proc


def preflight(cfg: dict, cfg_path: Path) -> None:
    """Refuse to start on top of an instance that is already running.

    Two gates, both fatal. The pid check catches the second adapter, which
    would connect to the running feedd and double the publish rate; it takes
    either implementation's marker, because a rust instance holds the same
    pid file and the same port as a cpp one. The port check catches a feedd
    this repo never recorded, from another worktree or an older session,
    whose bind failure would otherwise be silent.

    The address is also checked against what src/feedd.cpp can actually bind:
    it parses listen_host with inet_addr, which takes a numeric IPv4 address
    and nothing else. Python's bind resolves a name, so without this check
    `localhost` passes here and feedd dies at the bind.
    """
    for name in ("feedd", "adapter"):
        pid = tracked(name, cfg_path)
        if pid is not None:
            raise SystemExit(
                f"already running: {name} pid={pid}. "
                f"Run 'python3 python/obctl.py stop --config {cfg_path}' first."
            )
    host = str(cfg.get("listen_host", "127.0.0.1"))
    try:
        socket.inet_aton(host)
    except OSError:
        raise SystemExit(
            f"refusing to start: listen_host {host!r} is not a numeric IPv4 address. "
            f"src/feedd.cpp parses the host with inet_addr, so a name fails its bind. "
            f"Use 127.0.0.1 in {cfg_path}."
        )
    try:
        port = int(cfg.get("listen_port", 9001))
    except (TypeError, ValueError):
        raise SystemExit(
            f"refusing to start: listen_port {cfg.get('listen_port')!r} in {cfg_path} "
            f"is not a number."
        )
    reason = port_refusal(host, port, cfg_path)
    if reason:
        raise SystemExit(f"refusing to start: {reason}")


def require_build(impl: str, with_tui: bool, cfg_path: Path) -> None:
    """Refuse to start when setup did not produce the binaries this run execs.

    Only the binaries that will be exec'd are required: feedd always, tui
    only when it is about to replace this process. The path is named in the
    message, because `--impl rust` with an empty target directory must not
    fall back to the other build or die with a bare ENOENT from the spawn.
    """
    wanted = ("feedd", "tui") if with_tui and sys.stdin.isatty() else ("feedd",)
    missing = [str(p) for name in wanted if not (p := binary_path(name, impl)).is_file()]
    if missing:
        raise SystemExit(
            f"refusing to start: the {impl} build is missing {', '.join(missing)}. "
            f"Run 'python3 python/obctl.py setup --config {cfg_path} --impl {impl}'."
        )


def start(cfg_path: Path, fixture: str, with_tui: bool, impl: str = DEFAULT_IMPL) -> None:
    cfg = load(cfg_path)
    guard(cfg)
    preflight(cfg, cfg_path)
    setup(cfg_path, impl)
    require_build(impl, with_tui, cfg_path)
    cfg_abs = str(cfg_path.resolve())
    adapter = [sys.executable, str(ROOT / "python" / "feed_adapter.py"), "--config", cfg_abs]
    if fixture:
        adapter += ["--fixture", str(Path(fixture).resolve())]
    # Both children print a ready line, and feedd's must come first: the
    # adapter dials it. A failure here stops whatever this run started, so a
    # refused start never leaves half the pair running.
    try:
        feedd_offset = log_offset("feedd", cfg_path)
        feedd = spawn("feedd", cfg_path, [str(binary_path("feedd", impl)), cfg_abs])
        await_started("feedd", cfg_path, feedd, feedd_offset, "feedd listen", START_CEILING)
        adapter_offset = log_offset("adapter", cfg_path)
        proc = spawn("adapter", cfg_path, adapter)
        await_started(
            "adapter", cfg_path, proc, adapter_offset, "adapter connected", START_CEILING
        )
    except SystemExit:
        for name in ("adapter", "feedd"):
            kill_pid(name, cfg_path, why="obctl start rollback after a failed start")
        raise
    # exec only when stdin is a terminal. make e2e is not, so it stays headless.
    if with_tui and sys.stdin.isatty():
        tui = str(binary_path("tui", impl))
        os.execv(tui, [tui, cfg_abs])
    print("start ok (no tui)")


def note_stop(name: str, cfg_path: Path, pid: int, why: str) -> None:
    """Append who stopped this process to its own log, before it exits.

    SIGTERM carries no sender, so a stopped daemon is otherwise silent about
    it. That is what made the reported stall expensive to diagnose: `make e2e`
    from a second worktree signalled a live session's feedd, and the log it
    wrote said only that it had stopped. The caller's pid and working directory
    are what separate the two cases a reader has to tell apart — a person
    stopping the feed, and a test doing it from another checkout.
    """
    try:
        with open(log_file(name, cfg_path), "a", encoding="utf-8") as f:
            f.write(
                f"{time.strftime('%Y-%m-%dT%H:%M:%S')} {why}: "
                f"{name} pid={pid} signalled by obctl pid={os.getpid()} cwd={Path.cwd()}\n"
            )
    except OSError:
        # A missing or unwritable log must not turn a stop into a failure.
        pass


def kill_pid(name: str, cfg_path: Path, why: str = "obctl stop") -> int | None:
    """SIGTERM the tracked process and confirm it went away.

    Returns the pid when it is still alive after the grace period, so stop can
    say so instead of printing `stop ok` over a process that is still holding
    the port. The pid file goes away with the process and stays when it does
    not: a survivor must still be findable by the next stop, and by the
    preflight that refuses the next start. The same applies when the command
    line cannot be read, because that is not the same as the process being
    gone.

    Liveness is asked of ps, never of os.kill. A child we started and killed is
    a zombie until it is collected, and signal 0 succeeds for a zombie, so a
    pid check would report every successful stop as a failure.
    """
    path = pid_file(name, cfg_path)
    pid = read_pid(name, cfg_path)
    if pid is None:
        path.unlink(missing_ok=True)
        return None
    running = proc_running(pid)
    if running is None:
        print(f"cannot read the state of {name} pid={pid}; leaving its pid file in place")
        return pid
    if not running:
        path.unlink(missing_ok=True)
        return None
    command = proc_command(pid) or ""
    if not any(m in command for m in markers(name)):
        print(f"left alone: {name} pid={pid} is not this lab ({command or 'no command line'})")
        path.unlink(missing_ok=True)
        return None
    try:
        os.kill(pid, signal.SIGTERM)
    except OSError as exc:
        print(f"cannot signal {name} pid={pid}: {exc}")
        return pid
    note_stop(name, cfg_path, pid, why)
    deadline = time.monotonic() + STOP_CEILING
    while time.monotonic() < deadline:
        running = proc_running(pid)
        if running is False:
            path.unlink(missing_ok=True)
            return None
        if running is None:
            break
        time.sleep(0.05)
    return pid


def stop(cfg_path: Path) -> None:
    """Stop the instance this config started.

    Named rather than global: the pid files are this config's, so a stop from a
    config no window is using reaches nothing of that window's. The directory
    is printed, because a stop that found nothing and a stop that found the
    wrong thing look the same until you know where it looked.
    """
    stuck: list[str] = []
    for name in ("tui", "adapter", "feedd"):
        pid = kill_pid(name, cfg_path)
        if pid is not None:
            stuck.append(f"{name} pid={pid}")
    if stuck:
        print("not confirmed stopped: " + ", ".join(stuck))
        print("These may still hold the port. Send SIGKILL, or close the window that owns them.")
        raise SystemExit(1)
    print(f"stop ok (pid files in {run_dir(cfg_path)})")


def parser() -> argparse.ArgumentParser:
    """The command line, built here so the tests can pin the --impl default
    and its accepted values without running a command."""
    ap = argparse.ArgumentParser(prog="ob")
    ap.add_argument("cmd", choices=["setup", "start", "stop"])
    ap.add_argument("--config", default=str(ROOT / "config" / "dev.json"))
    ap.add_argument("--fixture", default="")
    ap.add_argument("--tui", action="store_true")
    ap.add_argument(
        "--impl",
        choices=sorted(PROC_MARKERS),
        default=DEFAULT_IMPL,
        help="implementation to build and run (default: cpp)",
    )
    return ap


def main() -> int:
    args = parser().parse_args()
    cfg_path = Path(args.config)
    if args.cmd == "setup":
        setup(cfg_path, args.impl)
    elif args.cmd == "start":
        start(cfg_path, args.fixture, args.tui, args.impl)
    else:
        # stop takes no --impl: ownership accepts either build's marker, and
        # the pid file names whichever implementation is running.
        stop(cfg_path)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
