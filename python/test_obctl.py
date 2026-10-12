"""Checks for the process bookkeeping in obctl: who gets signalled, who does
not, and whether a start can tell a live child from a dead one.

A pid file is not proof of ownership, and a pid is not proof of life: a child
that has exited but not been reaped is a zombie, and os.kill(pid, 0) succeeds
for a zombie. Both are pinned here, along with the two leaks found on macOS: a
second `start` overwrote the pid files of the instance already running, and
`stop` signalled whatever pid the file happened to name.

The port tests bind loopback on an ephemeral port. Nothing here uses the
network. Each test captures the module's stdout so `make test` stays readable.

The `--impl` switch is pinned too: each build's marker must match only its own
path, and a start must refuse a build that was never produced instead of
exec'ing something else.
"""
from __future__ import annotations

import contextlib
import io
import os
import socket
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path
from unittest import mock

import obctl


class FakeProc:
    """A process whose command line carries one of obctl's markers.

    `ready_file` is written from inside the child after its signal handler is
    installed. Without it the test signals a child that may still be starting,
    and the default SIGTERM action kills it before SIG_IGN takes effect.
    """

    def __init__(self, marker: str, ignore_sigterm: bool = False, ready_file: Path | None = None) -> None:
        statements = ["import time"]
        if ignore_sigterm:
            statements += ["import signal", "signal.signal(signal.SIGTERM, signal.SIG_IGN)"]
        if ready_file is not None:
            statements += ["import pathlib", f"pathlib.Path({str(ready_file)!r}).write_text('ready')"]
        statements.append("time.sleep(60)")
        self.proc = subprocess.Popen(
            [sys.executable, "-c", "; ".join(statements), marker],
            start_new_session=True,
        )

    @property
    def pid(self) -> int:
        return self.proc.pid

    def stop(self) -> None:
        self.proc.kill()
        self.proc.wait(timeout=5)


class BookkeepingTest(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        (self.root / "var" / "run").mkdir(parents=True)
        patcher = mock.patch.object(obctl, "ROOT", self.root)
        patcher.start()
        self.addCleanup(patcher.stop)
        # The module prints its operator messages; keep them out of test output.
        self.quiet = contextlib.redirect_stdout(io.StringIO())
        self.quiet.__enter__()
        self.addCleanup(self.quiet.__exit__, None, None, None)

    def write_pid(self, name: str, pid: int) -> None:
        obctl.pid_file(name).write_text(str(pid))

    def wait_for(self, path: Path, seconds: float = 5.0) -> None:
        end = time.monotonic() + seconds
        while time.monotonic() < end:
            if path.exists():
                return
            time.sleep(0.02)
        self.fail(f"{path} was never written by the child")

    def test_owns_accepts_this_lab_and_rejects_a_stranger(self) -> None:
        ours = FakeProc("build/feedd")
        self.addCleanup(ours.stop)
        self.assertTrue(obctl.owns(ours.pid, "feedd"))
        self.assertFalse(obctl.owns(ours.pid, "adapter"))
        self.assertFalse(obctl.owns(os.getpid(), "feedd"))

    def test_owns_is_false_when_ps_cannot_be_asked(self) -> None:
        ours = FakeProc("build/feedd")
        self.addCleanup(ours.stop)
        with mock.patch.object(obctl, "proc_command", return_value=None):
            self.assertFalse(obctl.owns(ours.pid, "feedd"))

    def test_stop_leaves_an_unrelated_pid_alone(self) -> None:
        stranger = FakeProc("some-other-program")
        self.addCleanup(stranger.stop)
        self.write_pid("feedd", stranger.pid)

        obctl.stop(Path("config/dev.json"))

        self.assertTrue(obctl.pid_alive(stranger.pid), "stop killed a process it does not own")
        self.assertFalse(obctl.pid_file("feedd").exists())

    def test_stale_pid_file_is_removed_without_a_signal(self) -> None:
        gone = FakeProc("build/feedd")
        pid = gone.pid
        gone.stop()
        self.assertFalse(obctl.pid_alive(pid))
        self.write_pid("feedd", pid)

        obctl.stop(Path("config/dev.json"))

        self.assertFalse(obctl.pid_file("feedd").exists())

    def test_an_unreadable_command_line_keeps_its_pid_file(self) -> None:
        ours = FakeProc("build/feedd")
        self.addCleanup(ours.stop)
        self.write_pid("feedd", ours.pid)

        with mock.patch.object(obctl, "proc_command", return_value=None):
            survivor = obctl.kill_pid("feedd")

        self.assertEqual(survivor, ours.pid)
        self.assertTrue(obctl.pid_alive(ours.pid), "an unverified process must not be signalled")
        self.assertTrue(obctl.pid_file("feedd").exists(), "the pid file is the only handle left")

    def test_kill_pid_reports_a_process_that_ignores_sigterm(self) -> None:
        ready = self.root / "stubborn-ready"
        stubborn = FakeProc("build/feedd", ignore_sigterm=True, ready_file=ready)
        self.addCleanup(stubborn.stop)
        self.wait_for(ready)
        self.write_pid("feedd", stubborn.pid)

        with mock.patch.object(obctl, "STOP_CEILING", 0.3):
            survivor = obctl.kill_pid("feedd")

        self.assertEqual(survivor, stubborn.pid)
        # The pid file stays so the next stop, and the preflight that refuses
        # the next start, can still name this process.
        self.assertTrue(obctl.pid_file("feedd").exists())

    def test_stop_fails_when_a_tracked_process_survives(self) -> None:
        ready = self.root / "survivor-ready"
        stubborn = FakeProc("build/feedd", ignore_sigterm=True, ready_file=ready)
        self.addCleanup(stubborn.stop)
        self.wait_for(ready)
        self.write_pid("feedd", stubborn.pid)

        with mock.patch.object(obctl, "STOP_CEILING", 0.3):
            with self.assertRaises(SystemExit):
                obctl.stop(Path("config/dev.json"))

        self.assertTrue(obctl.pid_alive(stubborn.pid))

    def test_preflight_refuses_when_a_tracked_process_is_alive(self) -> None:
        ours = FakeProc("build/feedd")
        self.addCleanup(ours.stop)
        self.write_pid("feedd", ours.pid)

        with self.assertRaises(SystemExit) as caught:
            obctl.preflight({"listen_host": "127.0.0.1", "listen_port": 0}, Path("config/dev.json"))

        self.assertIn("already running", str(caught.exception))
        self.assertTrue(obctl.pid_alive(ours.pid), "preflight must not touch the running instance")

    def test_preflight_refuses_while_a_survivor_is_still_tracked(self) -> None:
        stubborn = FakeProc("build/feedd", ignore_sigterm=True)
        self.addCleanup(stubborn.stop)
        self.write_pid("feedd", stubborn.pid)

        with self.assertRaises(SystemExit) as caught:
            obctl.preflight({"listen_host": "127.0.0.1", "listen_port": 0}, Path("config/dev.json"))

        self.assertIn(f"feedd pid={stubborn.pid}", str(caught.exception))

    def test_preflight_refuses_when_the_port_is_taken(self) -> None:
        holder = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        self.addCleanup(holder.close)
        holder.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        holder.bind(("127.0.0.1", 0))
        holder.listen(1)
        port = holder.getsockname()[1]

        with self.assertRaises(SystemExit) as caught:
            obctl.preflight({"listen_host": "127.0.0.1", "listen_port": port}, Path("config/dev.json"))

        self.assertIn("already in use", str(caught.exception))

    def test_preflight_refuses_a_host_feedd_cannot_parse(self) -> None:
        # src/feedd.cpp parses listen_host with inet_addr, which takes a
        # numeric IPv4 address. Python's bind would resolve the name.
        with self.assertRaises(SystemExit) as caught:
            obctl.preflight({"listen_host": "localhost", "listen_port": 0}, Path("config/dev.json"))

        self.assertIn("inet_addr", str(caught.exception))

    def test_preflight_refuses_a_port_that_is_not_a_number(self) -> None:
        with self.assertRaises(SystemExit) as caught:
            obctl.preflight({"listen_host": "127.0.0.1", "listen_port": "nine"}, Path("config/dev.json"))

        self.assertIn("is not a number", str(caught.exception))

    def test_preflight_passes_on_a_clean_state(self) -> None:
        obctl.preflight({"listen_host": "127.0.0.1", "listen_port": 0}, Path("config/dev.json"))

    def test_a_corrupt_pid_file_is_never_signalled(self) -> None:
        for junk in ("0", "-1", "", "not-a-pid"):
            with self.subTest(junk=junk):
                obctl.pid_file("feedd").write_text(junk)
                self.assertIsNone(obctl.read_pid("feedd"))
                obctl.stop(Path("config/dev.json"))
                self.assertFalse(obctl.pid_file("feedd").exists())


class ImplementationSwitchTest(unittest.TestCase):
    """`--impl` picks the build, and each build's marker names only its path.

    The marker is what proves a pid is still this lab's process before a
    signal. Two builds install binaries with the same names under different
    paths, so a marker keyed by implementation must not match the other
    build's command line. Ownership itself accepts either, because the pid
    file and the listen port are shared.
    """

    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        (self.root / "var" / "run").mkdir(parents=True)
        (self.root / "config").mkdir(parents=True)
        patcher = mock.patch.object(obctl, "ROOT", self.root)
        patcher.start()
        self.addCleanup(patcher.stop)
        self.quiet = contextlib.redirect_stdout(io.StringIO())
        self.quiet.__enter__()
        self.addCleanup(self.quiet.__exit__, None, None, None)

    def write_pid(self, name: str, pid: int) -> None:
        obctl.pid_file(name).write_text(str(pid))

    def test_each_marker_matches_only_its_own_build(self) -> None:
        for name in ("feedd", "tui"):
            with self.subTest(name=name):
                cpp = f"/repo/build/{name} /repo/config/dev.json"
                rust = f"/repo/rust/target/release/{name} /repo/config/dev.json"
                self.assertIn(obctl.marker(name, "cpp"), cpp)
                self.assertNotIn(obctl.marker(name, "rust"), cpp)
                self.assertIn(obctl.marker(name, "rust"), rust)
                self.assertNotIn(obctl.marker(name, "cpp"), rust)

    def test_the_adapter_marker_is_shared(self) -> None:
        # One adapter speaks the same wire protocol to either feedd.
        self.assertEqual(obctl.marker("adapter", "cpp"), obctl.marker("adapter", "rust"))
        self.assertIn(
            obctl.marker("adapter", "rust"),
            f"/repo/python/{obctl.marker('adapter', 'rust')} --config /repo/config/dev.json",
        )

    def test_binary_path_is_the_marker_under_root(self) -> None:
        for impl in ("cpp", "rust"):
            with self.subTest(impl=impl):
                self.assertEqual(
                    obctl.binary_path("feedd", impl),
                    self.root / obctl.marker("feedd", impl),
                )

    def test_owns_accepts_the_rust_build(self) -> None:
        ours = FakeProc(obctl.marker("feedd", "rust"))
        self.addCleanup(ours.stop)
        self.assertTrue(obctl.owns(ours.pid, "feedd"))
        self.assertFalse(obctl.owns(ours.pid, "tui"))

    def test_stop_reaches_the_rust_build(self) -> None:
        ours = FakeProc(obctl.marker("feedd", "rust"))
        self.addCleanup(ours.stop)
        self.write_pid("feedd", ours.pid)

        obctl.stop(Path("config/dev.json"))

        self.assertFalse(obctl.pid_file("feedd").exists(), "the rust instance was left running")

    def test_impl_defaults_to_cpp(self) -> None:
        self.assertEqual(obctl.parser().parse_args(["setup"]).impl, "cpp")
        self.assertEqual(obctl.parser().parse_args(["start", "--tui"]).impl, "cpp")

    def test_impl_accepts_rust(self) -> None:
        self.assertEqual(obctl.parser().parse_args(["start", "--impl", "rust"]).impl, "rust")

    def test_an_unknown_impl_is_rejected(self) -> None:
        stderr = io.StringIO()
        with contextlib.redirect_stderr(stderr):
            with self.assertRaises(SystemExit):
                obctl.parser().parse_args(["setup", "--impl", "cobol"])
        self.assertIn("invalid choice", stderr.getvalue())

    def test_setup_is_called_with_the_default_implementation(self) -> None:
        with mock.patch.object(obctl, "setup") as setup, mock.patch.object(
            sys, "argv", ["obctl.py", "setup"]
        ):
            obctl.main()
        setup.assert_called_once_with(Path(self.root / "config" / "dev.json"), "cpp")

    def test_setup_is_called_with_the_implementation_asked_for(self) -> None:
        with mock.patch.object(obctl, "setup") as setup, mock.patch.object(
            sys, "argv", ["obctl.py", "setup", "--impl", "rust"]
        ):
            obctl.main()
        setup.assert_called_once_with(Path(self.root / "config" / "dev.json"), "rust")

    def test_start_passes_the_implementation_to_setup_and_feedd(self) -> None:
        for name in ("feedd", "tui"):
            binary = obctl.binary_path(name, "rust")
            binary.parent.mkdir(parents=True, exist_ok=True)
            binary.write_text("#!/bin/sh\n")
            binary.chmod(0o755)
        commands: list[list[str]] = []

        def record(name: str, cmd: list[str]) -> subprocess.Popen:
            commands.append(cmd)
            raise SystemExit("stop here")

        cfg = self.root / "config" / "dev.json"
        cfg.write_text('{"env": "dev", "listen_host": "127.0.0.1", "listen_port": 0}')
        with mock.patch.object(obctl, "setup") as setup, mock.patch.object(
            obctl, "spawn", side_effect=record
        ):
            with self.assertRaises(SystemExit):
                obctl.start(cfg, "", False, "rust")

        setup.assert_called_once_with(cfg, "rust")
        self.assertEqual(commands[0][0], str(obctl.binary_path("feedd", "rust")))


class AwaitStartedTest(unittest.TestCase):
    """The start check must watch the child, not the pid.

    A child that has exited but not been reaped is a zombie, and
    os.kill(pid, 0) succeeds for a zombie, so a pid-based check calls a dead
    feedd alive and start reports success over a book that will never fill.
    """

    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        (self.root / "var" / "run").mkdir(parents=True)
        patcher = mock.patch.object(obctl, "ROOT", self.root)
        patcher.start()
        self.addCleanup(patcher.stop)
        self.quiet = contextlib.redirect_stdout(io.StringIO())
        self.quiet.__enter__()
        self.addCleanup(self.quiet.__exit__, None, None, None)

    def reap(self, proc: subprocess.Popen) -> None:
        """Kill and collect, so no child is left unreaped."""
        proc.kill()
        proc.wait(timeout=5)

    def test_a_child_that_exits_at_once_is_not_reported_as_started(self) -> None:
        offset = obctl.log_offset("feedd")
        proc = obctl.spawn(
            "feedd",
            [sys.executable, "-c", "import sys; sys.stderr.write('bind failed\\n'); sys.exit(1)"],
        )

        with self.assertRaises(SystemExit) as caught:
            obctl.await_started("feedd", proc, offset, "feedd listen", 1.0)

        self.assertIn("bind failed", str(caught.exception))
        self.assertFalse(obctl.pid_file("feedd").exists())

    def test_a_child_that_prints_its_ready_line_is_reported_as_started(self) -> None:
        offset = obctl.log_offset("feedd")
        proc = obctl.spawn(
            "feedd",
            [
                sys.executable,
                "-c",
                "import sys, time; sys.stderr.write('feedd listen 127.0.0.1:9001 env=dev\\n');"
                " sys.stderr.flush(); time.sleep(30)",
            ],
        )
        self.addCleanup(self.reap, proc)

        obctl.await_started("feedd", proc, offset, "feedd listen", 1.0)

        self.assertTrue(obctl.pid_file("feedd").exists())

    def test_a_child_that_stays_silent_past_the_ceiling_fails(self) -> None:
        offset = obctl.log_offset("feedd")
        proc = obctl.spawn(
            "feedd", [sys.executable, "-c", "import time; time.sleep(30)"]
        )
        self.addCleanup(self.reap, proc)

        with self.assertRaises(SystemExit):
            obctl.await_started("feedd", proc, offset, "feedd listen", 0.3)


class StartCleanupTest(unittest.TestCase):
    """A start that fails after feedd is up must not leave feedd running."""

    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        (self.root / "var" / "run").mkdir(parents=True)
        (self.root / "config").mkdir(parents=True)
        self.cfg = self.root / "config" / "dev.json"
        self.cfg.write_text(
            '{"env": "dev", "bus_path": "var/dev/bus", "wal_path": "var/dev/wal.bin",'
            ' "log_path": "var/dev/async.log", "listen_host": "127.0.0.1",'
            ' "listen_port": 0, "symbol": "BTCUSDT", "allow_orders": false}'
        )
        patcher = mock.patch.object(obctl, "ROOT", self.root)
        patcher.start()
        self.addCleanup(patcher.stop)
        self.quiet = contextlib.redirect_stdout(io.StringIO())
        self.quiet.__enter__()
        self.addCleanup(self.quiet.__exit__, None, None, None)

    def test_a_failed_adapter_check_stops_feedd(self) -> None:
        children: list[subprocess.Popen] = []

        def fake_spawn(name: str, cmd: list[str]) -> subprocess.Popen:
            # A stand-in whose command line carries the marker, so the
            # ownership check treats it as this lab's process.
            proc = subprocess.Popen(
                [sys.executable, "-c", "import time; time.sleep(60)", obctl.marker(name, "cpp")],
                start_new_session=True,
            )
            children.append(proc)
            obctl.pid_file(name).write_text(str(proc.pid))
            return proc

        def fake_await(name: str, proc: subprocess.Popen, offset: int, marker: str, ceiling: float) -> None:
            if name == "adapter":
                raise SystemExit("adapter did not start")

        self.addCleanup(lambda: [p.kill() for p in children if p.poll() is None])

        with mock.patch.object(obctl, "setup"), mock.patch.object(
            obctl, "guard"
        ), mock.patch.object(obctl, "preflight"), mock.patch.object(
            obctl, "require_build"
        ), mock.patch.object(
            obctl, "spawn", side_effect=fake_spawn
        ), mock.patch.object(
            obctl, "await_started", side_effect=fake_await
        ):
            with self.assertRaises(SystemExit):
                obctl.start(self.cfg, "", False)

        self.assertEqual(len(children), 2, "both children should have been spawned")
        for proc in children:
            self.assertIsNotNone(
                proc.poll(), "a child was left running after a failed start"
            )
        for name in ("feedd", "adapter"):
            self.assertFalse(obctl.pid_file(name).exists())

    def test_a_missing_rust_build_is_refused_by_path(self) -> None:
        # Nothing has built rust/target/release in this temporary ROOT, so
        # --impl rust must name the path it looked for instead of exec'ing it.
        with mock.patch.object(obctl, "setup"):
            with self.assertRaises(SystemExit) as caught:
                obctl.start(self.cfg, "", False, "rust")

        self.assertIn(str(obctl.binary_path("feedd", "rust")), str(caught.exception))


if __name__ == "__main__":
    unittest.main()
