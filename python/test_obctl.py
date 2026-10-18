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
import re
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
        # The config names the run directory the pid files live in, so the tests
        # pass one: `obctl.pid_file` takes the config rather than assuming a
        # single shared directory, which is the defect these tests exist around.
        self.cfg = Path("config/dev.json")
        (self.root / "var" / "run" / "dev").mkdir(parents=True)
        patcher = mock.patch.object(obctl, "ROOT", self.root)
        patcher.start()
        self.addCleanup(patcher.stop)
        # The module prints its operator messages; keep them out of test output.
        self.quiet = contextlib.redirect_stdout(io.StringIO())
        self.quiet.__enter__()
        self.addCleanup(self.quiet.__exit__, None, None, None)

    def write_pid(self, name: str, pid: int) -> None:
        obctl.pid_file(name, self.cfg).write_text(str(pid))

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

        obctl.stop(self.cfg)

        self.assertTrue(obctl.pid_alive(stranger.pid), "stop killed a process it does not own")
        self.assertFalse(obctl.pid_file("feedd", self.cfg).exists())

    def test_stale_pid_file_is_removed_without_a_signal(self) -> None:
        gone = FakeProc("build/feedd")
        pid = gone.pid
        gone.stop()
        self.assertFalse(obctl.pid_alive(pid))
        self.write_pid("feedd", pid)

        obctl.stop(self.cfg)

        self.assertFalse(obctl.pid_file("feedd", self.cfg).exists())

    def test_an_unreadable_command_line_keeps_its_pid_file(self) -> None:
        ours = FakeProc("build/feedd")
        self.addCleanup(ours.stop)
        self.write_pid("feedd", ours.pid)

        with mock.patch.object(obctl, "proc_command", return_value=None):
            survivor = obctl.kill_pid("feedd", self.cfg)

        self.assertEqual(survivor, ours.pid)
        self.assertTrue(obctl.pid_alive(ours.pid), "an unverified process must not be signalled")
        self.assertTrue(obctl.pid_file("feedd", self.cfg).exists(), "the pid file is the only handle left")

    def test_kill_pid_reports_a_process_that_ignores_sigterm(self) -> None:
        ready = self.root / "stubborn-ready"
        stubborn = FakeProc("build/feedd", ignore_sigterm=True, ready_file=ready)
        self.addCleanup(stubborn.stop)
        self.wait_for(ready)
        self.write_pid("feedd", stubborn.pid)

        with mock.patch.object(obctl, "STOP_CEILING", 0.3):
            survivor = obctl.kill_pid("feedd", self.cfg)

        self.assertEqual(survivor, stubborn.pid)
        # The pid file stays so the next stop, and the preflight that refuses
        # the next start, can still name this process.
        self.assertTrue(obctl.pid_file("feedd", self.cfg).exists())

    def test_stop_fails_when_a_tracked_process_survives(self) -> None:
        ready = self.root / "survivor-ready"
        stubborn = FakeProc("build/feedd", ignore_sigterm=True, ready_file=ready)
        self.addCleanup(stubborn.stop)
        self.wait_for(ready)
        self.write_pid("feedd", stubborn.pid)

        with mock.patch.object(obctl, "STOP_CEILING", 0.3):
            with self.assertRaises(SystemExit):
                obctl.stop(self.cfg)

        self.assertTrue(obctl.pid_alive(stubborn.pid))

    def test_preflight_refuses_when_a_tracked_process_is_alive(self) -> None:
        ours = FakeProc("build/feedd")
        self.addCleanup(ours.stop)
        self.write_pid("feedd", ours.pid)

        with self.assertRaises(SystemExit) as caught:
            obctl.preflight({"listen_host": "127.0.0.1", "listen_port": 0}, self.cfg)

        self.assertIn("already running", str(caught.exception))
        self.assertTrue(obctl.pid_alive(ours.pid), "preflight must not touch the running instance")

    def test_preflight_refuses_while_a_survivor_is_still_tracked(self) -> None:
        stubborn = FakeProc("build/feedd", ignore_sigterm=True)
        self.addCleanup(stubborn.stop)
        self.write_pid("feedd", stubborn.pid)

        with self.assertRaises(SystemExit) as caught:
            obctl.preflight({"listen_host": "127.0.0.1", "listen_port": 0}, self.cfg)

        self.assertIn(f"feedd pid={stubborn.pid}", str(caught.exception))

    def test_preflight_refuses_when_the_port_is_taken(self) -> None:
        holder = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        self.addCleanup(holder.close)
        holder.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        holder.bind(("127.0.0.1", 0))
        holder.listen(1)
        port = holder.getsockname()[1]

        with self.assertRaises(SystemExit) as caught:
            obctl.preflight({"listen_host": "127.0.0.1", "listen_port": port}, self.cfg)

        self.assertIn("already in use", str(caught.exception))

    def test_preflight_refuses_a_host_feedd_cannot_parse(self) -> None:
        # src/feedd.cpp parses listen_host with inet_addr, which takes a
        # numeric IPv4 address. Python's bind would resolve the name.
        with self.assertRaises(SystemExit) as caught:
            obctl.preflight({"listen_host": "localhost", "listen_port": 0}, self.cfg)

        self.assertIn("inet_addr", str(caught.exception))

    def test_preflight_refuses_a_port_that_is_not_a_number(self) -> None:
        with self.assertRaises(SystemExit) as caught:
            obctl.preflight({"listen_host": "127.0.0.1", "listen_port": "nine"}, self.cfg)

        self.assertIn("is not a number", str(caught.exception))

    def test_preflight_passes_on_a_clean_state(self) -> None:
        obctl.preflight({"listen_host": "127.0.0.1", "listen_port": 0}, self.cfg)

    def test_a_corrupt_pid_file_is_never_signalled(self) -> None:
        for junk in ("0", "-1", "", "not-a-pid"):
            with self.subTest(junk=junk):
                obctl.pid_file("feedd", self.cfg).write_text(junk)
                self.assertIsNone(obctl.read_pid("feedd", self.cfg))
                obctl.stop(self.cfg)
                self.assertFalse(obctl.pid_file("feedd", self.cfg).exists())


class RunDirectoryTest(unittest.TestCase):
    """One config cannot stop another's processes.

    The reported incident: a window was open on the live config, a harness test
    ran `make e2e`, and the window went to `feed=stalled` with a 39-second
    staleness and never recovered. `stop` reads a pid file, and every config
    wrote to the same `var/run`, so the test's stop signalled the live
    session's feedd and adapter. Nothing in the test was wrong except where it
    kept its pid files.

    The two shipped configs also have to stay apart: the config a test runs on
    must not be the config a person runs on, in paths or in port.
    """

    REPO = Path(__file__).resolve().parents[1]

    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        (self.root / "config").mkdir(parents=True)
        patcher = mock.patch.object(obctl, "ROOT", self.root)
        patcher.start()
        self.addCleanup(patcher.stop)
        self.quiet = contextlib.redirect_stdout(io.StringIO())
        self.quiet.__enter__()
        self.addCleanup(self.quiet.__exit__, None, None, None)
        self.dev = self.config("dev")
        self.test = self.config("test")

    def config(self, name: str) -> Path:
        path = self.root / "config" / f"{name}.json"
        path.write_text('{"env": "dev", "listen_host": "127.0.0.1", "listen_port": 0}')
        return path

    def write_pid(self, name: str, pid: int, cfg: Path) -> None:
        path = obctl.pid_file(name, cfg)
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(str(pid))

    def test_two_configs_do_not_share_a_pid_file(self) -> None:
        for name in ("feedd", "adapter", "tui"):
            with self.subTest(name=name):
                self.assertNotEqual(
                    obctl.pid_file(name, self.dev),
                    obctl.pid_file(name, self.test),
                    "two configs sharing a pid file is how one stops the other",
                )
                self.assertNotEqual(
                    obctl.log_file(name, self.dev), obctl.log_file(name, self.test)
                )

    def test_a_stop_on_one_config_leaves_the_other_running(self) -> None:
        live = FakeProc("build/feedd")
        self.addCleanup(live.stop)
        self.write_pid("feedd", live.pid, self.dev)

        obctl.stop(self.test)

        self.assertTrue(
            obctl.pid_alive(live.pid),
            "a stop on the test config killed the daemon the live config started",
        )
        self.assertTrue(
            obctl.pid_file("feedd", self.dev).exists(),
            "the live config's pid file was removed by a stop that was not its",
        )

    def test_a_stop_on_the_same_config_reaches_it(self) -> None:
        # The other half: keying by config must not turn stop into a no-op.
        live = FakeProc("build/feedd")
        self.addCleanup(live.stop)
        self.write_pid("feedd", live.pid, self.dev)

        obctl.stop(self.dev)

        # Asked of ps, not of the pid: this test's child is a zombie until it is
        # reaped, and signal 0 succeeds for a zombie.
        self.assertFalse(obctl.proc_running(live.pid), "the config's own daemon was not stopped")
        self.assertFalse(obctl.pid_file("feedd", self.dev).exists())

    def test_the_shipped_test_config_is_not_the_shipped_live_config(self) -> None:
        dev = obctl.load(self.REPO / "config" / "dev.json")
        test = obctl.load(self.REPO / "config" / "test.json")
        for key in ("bus_path", "wal_path", "log_path", "listen_port"):
            with self.subTest(key=key):
                self.assertNotEqual(
                    dev.get(key),
                    test.get(key),
                    f"{key} is shared between the live and test configs, so a test "
                    f"run reaches a session's ring or its port",
                )
        # The config file names are what the run directories are keyed by, and
        # these two are the ones the Makefile points at.
        self.assertNotEqual(obctl.run_dir(self.REPO / "config" / "dev.json").name, "test")

    def heartbeat_name(self) -> str:
        """The heartbeat's name as the adapter defines it.

        Read rather than repeated here, so a rename on either side fails this
        test instead of being mirrored into it.
        """
        text = (self.REPO / "python" / "feed_adapter.py").read_text()
        found = re.search(r'^HEARTBEAT_NAME = "([^"]+)"', text, re.MULTILINE)
        self.assertIsNotNone(found, "the adapter no longer defines HEARTBEAT_NAME")
        return found.group(1)

    def test_the_heartbeat_name_is_the_same_on_both_sides(self) -> None:
        """The adapter writes it and the window reads it, in two languages.

        A one-word divergence here is invisible: the window finds no file, reports
        `Absent`, and loses the distinction between a quiet exchange and a dead
        adapter — which is the whole reason the file exists. The watch file's path
        was derived independently on the two sides and disagreed once (OPT-034), so
        the name is pinned across both rather than only in the language that
        happens to be tested.
        """
        name = self.heartbeat_name()
        window = (self.REPO / "rust" / "ob-cli" / "src" / "lib.rs").read_text()
        self.assertIn(
            f'HEARTBEAT_NAME: &str = "{name}"',
            window,
            "the window's heartbeat name does not match the adapter's",
        )

    def test_the_window_reads_the_heartbeat_beside_the_watch_file(self) -> None:
        # The other half of the same agreement: which directory. The adapter puts
        # it beside the watch list, and the window has to look in the same place.
        window = (self.REPO / "rust" / "ob-cli" / "src" / "bin" / "tui.rs").read_text()
        self.assertIn("watch_path.with_file_name(HEARTBEAT_NAME)", window)

    def test_stopping_a_process_says_who_did_it(self) -> None:
        # SIGTERM has no sender, and the reported stall left a log that said a
        # process had stopped but not what stopped it. The caller's pid and
        # working directory are what tell a person apart from a harness test in
        # another checkout.
        live = FakeProc("build/feedd")
        self.addCleanup(live.stop)
        self.write_pid("feedd", live.pid, self.dev)

        obctl.stop(self.dev)

        log = obctl.log_file("feedd", self.dev).read_text()
        self.assertIn("obctl stop", log)
        self.assertIn(f"pid={live.pid}", log)
        self.assertIn(f"signalled by obctl pid={os.getpid()}", log)
        self.assertIn(f"cwd={Path.cwd()}", log)


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
        self.cfg = Path("config/dev.json")
        (self.root / "var" / "run" / "dev").mkdir(parents=True)
        (self.root / "config").mkdir(parents=True)
        patcher = mock.patch.object(obctl, "ROOT", self.root)
        patcher.start()
        self.addCleanup(patcher.stop)
        self.quiet = contextlib.redirect_stdout(io.StringIO())
        self.quiet.__enter__()
        self.addCleanup(self.quiet.__exit__, None, None, None)

    def write_pid(self, name: str, pid: int) -> None:
        obctl.pid_file(name, self.cfg).write_text(str(pid))

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

        obctl.stop(self.cfg)

        self.assertFalse(obctl.pid_file("feedd", self.cfg).exists(), "the rust instance was left running")

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

        def record(name: str, cfg_path: Path, cmd: list[str]) -> subprocess.Popen:
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
        self.cfg = Path("config/dev.json")
        (self.root / "var" / "run" / "dev").mkdir(parents=True)
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
        offset = obctl.log_offset("feedd", self.cfg)
        proc = obctl.spawn(
            "feedd",
            self.cfg,
            [sys.executable, "-c", "import sys; sys.stderr.write('bind failed\\n'); sys.exit(1)"],
        )

        with self.assertRaises(SystemExit) as caught:
            obctl.await_started("feedd", self.cfg, proc, offset, "feedd listen", 1.0)

        self.assertIn("bind failed", str(caught.exception))
        self.assertFalse(obctl.pid_file("feedd", self.cfg).exists())

    def test_a_child_that_prints_its_ready_line_is_reported_as_started(self) -> None:
        offset = obctl.log_offset("feedd", self.cfg)
        proc = obctl.spawn(
            "feedd",
            self.cfg,
            [
                sys.executable,
                "-c",
                "import sys, time; sys.stderr.write('feedd listen 127.0.0.1:9001 env=dev\\n');"
                " sys.stderr.flush(); time.sleep(30)",
            ],
        )
        self.addCleanup(self.reap, proc)

        obctl.await_started("feedd", self.cfg, proc, offset, "feedd listen", 1.0)

        self.assertTrue(obctl.pid_file("feedd", self.cfg).exists())

    def test_a_child_that_stays_silent_past_the_ceiling_fails(self) -> None:
        offset = obctl.log_offset("feedd", self.cfg)
        proc = obctl.spawn(
            "feedd", self.cfg, [sys.executable, "-c", "import time; time.sleep(30)"]
        )
        self.addCleanup(self.reap, proc)

        with self.assertRaises(SystemExit):
            obctl.await_started("feedd", self.cfg, proc, offset, "feedd listen", 0.3)


class StartCleanupTest(unittest.TestCase):
    """A start that fails after feedd is up must not leave feedd running."""

    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        (self.root / "var" / "run" / "dev").mkdir(parents=True)
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

        def fake_spawn(name: str, cfg_path: Path, cmd: list[str]) -> subprocess.Popen:
            # A stand-in whose command line carries the marker, so the
            # ownership check treats it as this lab's process.
            proc = subprocess.Popen(
                [sys.executable, "-c", "import time; time.sleep(60)", obctl.marker(name, "cpp")],
                start_new_session=True,
            )
            children.append(proc)
            obctl.pid_file(name, cfg_path).write_text(str(proc.pid))
            return proc

        def fake_await(
            name: str,
            cfg_path: Path,
            proc: subprocess.Popen,
            offset: int,
            marker: str,
            ceiling: float,
        ) -> None:
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
            self.assertFalse(obctl.pid_file(name, self.cfg).exists())

    def test_a_missing_rust_build_is_refused_by_path(self) -> None:
        # Nothing has built rust/target/release in this temporary ROOT, so
        # --impl rust must name the path it looked for instead of exec'ing it.
        with mock.patch.object(obctl, "setup"):
            with self.assertRaises(SystemExit) as caught:
                obctl.start(self.cfg, "", False, "rust")

        self.assertIn(str(obctl.binary_path("feedd", "rust")), str(caught.exception))


class BuildToolResolution(unittest.TestCase):
    """A build tool has to be found, or the failure has to say what to do.

    `cargo` is installed by rustup into ~/.cargo/bin and reaches PATH through
    ~/.cargo/env, which a shell reads only if its profile has loaded. A terminal
    opened before that line existed hands the script an environment without it,
    and the first version of this code answered with a Python traceback ending
    `FileNotFoundError: [Errno 2] No such file or directory: 'cargo'` — naming
    neither the tool's purpose nor the fix. A user hit exactly that.
    """

    def setUp(self) -> None:
        # The module prints its operator messages; keep them out of test output.
        self.quiet = contextlib.redirect_stdout(io.StringIO())
        self.quiet.__enter__()
        self.addCleanup(self.quiet.__exit__, None, None, None)

    def test_a_tool_on_path_is_used_as_is(self):
        found = obctl.resolve_tool("sh")
        self.assertIsNotNone(found)
        self.assertTrue(found.endswith("/sh"))

    def test_a_tool_missing_from_path_is_found_where_it_is_installed(self):
        # A directory that is not on PATH, holding an executable.
        with tempfile.TemporaryDirectory() as tmp:
            exe = Path(tmp) / "pretend-tool"
            exe.write_text("#!/bin/sh\nexit 0\n")
            exe.chmod(0o755)
            with mock.patch.object(obctl, "TOOL_SEARCH_DIRS", (Path(tmp),)):
                with mock.patch.object(obctl.shutil, "which", return_value=None):
                    self.assertEqual(obctl.resolve_tool("pretend-tool"), str(exe))

    def test_a_tool_that_is_nowhere_is_reported_not_raised(self):
        with tempfile.TemporaryDirectory() as tmp:
            with mock.patch.object(obctl, "TOOL_SEARCH_DIRS", (Path(tmp),)):
                with mock.patch.object(obctl.shutil, "which", return_value=None):
                    self.assertIsNone(obctl.resolve_tool("no-such-tool-anywhere"))
                    with self.assertRaises(SystemExit) as caught:
                        obctl.require_tool("cargo", "build the Rust workspace")
        message = str(caught.exception)
        self.assertIn("cargo", message)
        self.assertIn("build the Rust workspace", message)
        self.assertIn("looked on PATH and in:", message)
        self.assertIn("source ~/.cargo/env", message)

    def test_a_missing_executable_never_escapes_as_a_traceback(self):
        # The backstop for any command that did not go through require_tool.
        with self.assertRaises(SystemExit) as caught:
            obctl.run(["no-such-command-anywhere"])
        self.assertIn("not found", str(caught.exception))
        self.assertIn("PATH", str(caught.exception))

    def test_setup_uses_the_resolved_cargo_rather_than_the_bare_name(self):
        with tempfile.TemporaryDirectory() as tmp:
            cfg = Path(tmp) / "dev.json"
            cfg.write_text(
                '{"env": "dev", "bus_path": "%s", "wal_path": "%s",'
                ' "log_path": "%s", "listen_port": 1, "allow_orders": false}'
                % (Path(tmp) / "bus", Path(tmp) / "wal", Path(tmp) / "log")
            )
            with mock.patch.object(obctl, "ROOT", Path(tmp)):
                with mock.patch.object(
                    obctl, "require_tool", return_value="/found/cargo"
                ) as req:
                    with mock.patch.object(obctl, "run") as ran:
                        obctl.setup(cfg, "rust")
            req.assert_called_once()
            self.assertEqual(ran.call_args[0][0][0], "/found/cargo")


if __name__ == "__main__":
    unittest.main()
