"""Checks for the process bookkeeping in obctl: who gets signalled, and who
does not.

A pid file is not proof of ownership. Two failures found on macOS are pinned
here: a second `start` overwrote the pid files of the instance already
running, and `stop` signalled whatever pid the file happened to name. The
port tests bind loopback on an ephemeral port. Nothing here uses the network.
"""
from __future__ import annotations

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
    """A process whose command line carries one of obctl's markers."""

    def __init__(self, marker: str, ignore_sigterm: bool = False) -> None:
        body = "import time; time.sleep(60)"
        if ignore_sigterm:
            body = "import signal, time; signal.signal(signal.SIGTERM, signal.SIG_IGN); time.sleep(60)"
        self.proc = subprocess.Popen(
            [sys.executable, "-c", body, marker],
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

    def write_pid(self, name: str, pid: int) -> None:
        obctl.pid_file(name).write_text(str(pid))

    def test_owns_accepts_this_lab_and_rejects_a_stranger(self) -> None:
        ours = FakeProc("build/feedd")
        self.addCleanup(ours.stop)
        self.assertTrue(obctl.owns(ours.pid, "feedd"))
        self.assertFalse(obctl.owns(ours.pid, "adapter"))
        self.assertFalse(obctl.owns(os.getpid(), "feedd"))

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

    def test_kill_pid_reports_a_process_that_ignores_sigterm(self) -> None:
        stubborn = FakeProc("build/feedd", ignore_sigterm=True)
        self.addCleanup(stubborn.stop)
        self.write_pid("feedd", stubborn.pid)

        with mock.patch.object(obctl, "STOP_CEILING", 0.3):
            survivor = obctl.kill_pid("feedd")

        self.assertEqual(survivor, stubborn.pid)
        # The pid file stays so the next stop, and the preflight that refuses
        # the next start, can still name this process.
        self.assertTrue(obctl.pid_file("feedd").exists())

    def test_preflight_refuses_while_a_survivor_is_still_tracked(self) -> None:
        stubborn = FakeProc("build/feedd", ignore_sigterm=True)
        self.addCleanup(stubborn.stop)
        self.write_pid("feedd", stubborn.pid)

        with self.assertRaises(SystemExit) as caught:
            obctl.preflight({"listen_host": "127.0.0.1", "listen_port": 0}, Path("config/dev.json"))

        self.assertIn(f"feedd pid={stubborn.pid}", str(caught.exception))

    def test_stop_fails_when_a_tracked_process_survives(self) -> None:
        stubborn = FakeProc("build/feedd", ignore_sigterm=True)
        self.addCleanup(stubborn.stop)
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

    def test_preflight_passes_on_a_clean_state(self) -> None:
        obctl.preflight({"listen_host": "127.0.0.1", "listen_port": 0}, Path("config/dev.json"))

    def test_a_corrupt_pid_file_is_never_signalled(self) -> None:
        for junk in ("0", "-1", "", "not-a-pid"):
            with self.subTest(junk=junk):
                obctl.pid_file("feedd").write_text(junk)
                self.assertIsNone(obctl.read_pid("feedd"))
                obctl.stop(Path("config/dev.json"))
                self.assertFalse(obctl.pid_file("feedd").exists())

    def test_await_started_reports_a_child_that_died(self) -> None:
        self.write_pid("feedd", 999999999)
        obctl.log_file("feedd").write_text("bind failed\n")

        with self.assertRaises(SystemExit) as caught:
            obctl.await_started("feedd", 0, "feedd listen", 0.2)

        self.assertIn("bind failed", str(caught.exception))
        self.assertFalse(obctl.pid_file("feedd").exists())


if __name__ == "__main__":
    unittest.main()
