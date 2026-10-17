"""The terminal book's input line, tested through a real pty.

Why this test exists
--------------------

The first version of the book cleared the whole screen on every frame, ten times
a second. That erases the terminal's own echo of what the user is typing, so the
commands were unusable: the keystrokes still reached the program — the tty's line
buffer is not affected by drawing over the screen — but nobody could see what they
had typed. A user reported it as "every price refresh overrides my input".

Nothing in the unit tests can see that. It is a property of the terminal, so the
test needs a terminal. This module opens a pseudo-terminal, runs the book on it,
types into it, and reads back what the book drew.

What it asserts
---------------

1. What the user types appears in a frame. The book owns the input line now, so
   `> levels 7` must be in the output.
2. The terminal is in non-canonical mode with echo off while the book runs. That
   is what makes the book's own line, rather than the kernel's, the visible one.
3. The terminal is restored when the book exits. This is the assertion that
   matters most: a program that leaves a terminal in non-canonical mode makes the
   shell look broken, and the only recovery is `stty sane`.

Scope
-----

Only the Rust terminal is covered. The C++ one in `src/tui.cpp` still clears the
screen and fails assertions 1 and 2; adding it here is one entry in `TERMINALS`
once that is fixed.
"""

import os
import pathlib
import pty
import select
import shutil
import signal
import subprocess
import sys
import tempfile
import termios
import time
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[1]

# The terminals this test covers: a name and the path to the binary.
#
# `OB_TUI_TEST_BINARY` overrides the list, which is how the test was checked
# against a terminal that still has the old behaviour: pointed at `build/tui` it
# fails assertions 1, 2 and 3, and pointed at the Rust one it passes. A test for a
# bug that was fixed has to be shown failing before it is believed.
TERMINALS = [
    ("rust", ROOT / "rust" / "target" / "release" / "tui"),
]
if os.environ.get("OB_TUI_TEST_BINARY"):
    TERMINALS = [("from-env", pathlib.Path(os.environ["OB_TUI_TEST_BINARY"]))]

# How long to wait for the book to say something, and for it to exit.
FRAME_DEADLINE = 10.0
EXIT_DEADLINE = 5.0


def free_port() -> int:
    """A port nothing is listening on, so a run cannot collide with a live one."""
    import socket

    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


class TuiInput(unittest.TestCase):
    """One case per terminal binary."""

    def setUp(self):
        self.workdir = pathlib.Path(tempfile.mkdtemp(prefix="ob-tui-test-"))
        self.port = free_port()
        self.config = self.workdir / "config.json"
        self.config.write_text(
            """{
  "env": "dev",
  "bus_path": "%s",
  "wal_path": "%s",
  "log_path": "%s",
  "listen_host": "127.0.0.1",
  "listen_port": %d,
  "symbol": "BTCUSDT",
  "allow_orders": false,
  "levels": 3,
  "ring_capacity": 4096
}
"""
            % (
                self.workdir / "bus",
                self.workdir / "wal.bin",
                self.workdir / "async.log",
                self.port,
            )
        )
        self.feedd = None
        self.feedd_log = None

    def tearDown(self):
        if getattr(self, "feedd_log", None) is not None:
            self.feedd_log.close()
        if self.feedd is not None and self.feedd.poll() is None:
            self.feedd.terminate()
            try:
                self.feedd.wait(timeout=EXIT_DEADLINE)
            except subprocess.TimeoutExpired:
                self.feedd.kill()
        shutil.rmtree(self.workdir, ignore_errors=True)

    def start_feeder(self, name: str):
        """Run the feed daemon, because the book attaches to a ring that exists.

        The terminal does not create the ring: attaching to a file that is not
        there would be indistinguishable from attaching to an empty one.
        """
        daemon = ROOT / "build" / "feedd"
        if not daemon.exists():
            self.skipTest(f"build/feedd is missing; run `make` first ({name})")
        self.feedd_log = open(self.workdir / "feedd.log", "wb")
        log = self.feedd_log
        self.feedd = subprocess.Popen(
            [str(daemon), str(self.config)],
            cwd=str(ROOT),
            stdout=log,
            stderr=log,
            start_new_session=True,
        )
        # The daemon prints its ready line to stderr, which is the same file.
        deadline = time.monotonic() + FRAME_DEADLINE
        while time.monotonic() < deadline:
            text = (self.workdir / "feedd.log").read_text(errors="replace")
            if "feedd listen" in text:
                return
            if self.feedd.poll() is not None:
                self.fail(f"feedd exited: {text}")
            time.sleep(0.05)
        self.fail("feedd did not print its ready line")

    def drain(self, master: int, deadline: float) -> str:
        """Read everything the child has drawn until the deadline."""
        out = []
        while time.monotonic() < deadline:
            ready, _, _ = select.select([master], [], [], 0.2)
            if not ready:
                continue
            try:
                chunk = os.read(master, 65536)
            except OSError:
                break
            if not chunk:
                break
            out.append(chunk.decode("utf-8", "replace"))
        return "".join(out)

    def test_typed_input_survives_a_refresh(self):
        for name, binary in TERMINALS:
            with self.subTest(terminal=name):
                if not binary.exists():
                    self.skipTest(
                        f"{binary} is missing; run `make rust-build` first"
                    )
                self.start_feeder(name)

                master, slave = pty.openpty()
                # Start from the state an interactive shell is in, so the test
                # measures the book rather than whatever the pty defaulted to.
                initial = termios.tcgetattr(slave)
                initial[3] |= termios.ICANON | termios.ECHO
                termios.tcsetattr(slave, termios.TCSANOW, initial)

                child = subprocess.Popen(
                    [str(binary), str(self.config)],
                    cwd=str(ROOT),
                    stdin=slave,
                    stdout=slave,
                    stderr=slave,
                )
                os.close(slave)

                try:
                    # 1. The book draws something.
                    first = self.drain(master, time.monotonic() + FRAME_DEADLINE)
                    self.assertIn(
                        "orderbook-lab",
                        first,
                        f"{name}: the book drew no frame. Got: {first[:400]!r}",
                    )

                    # 2. The terminal is in the mode the book needs.
                    running = termios.tcgetattr(master)
                    self.assertFalse(
                        running[3] & termios.ICANON,
                        f"{name}: ICANON is still set, so the book is relying on "
                        "the terminal's echo of the input line",
                    )
                    self.assertFalse(
                        running[3] & termios.ECHO,
                        f"{name}: ECHO is still set",
                    )

                    # 3. Typing appears in a frame.
                    os.write(master, b"levels 7")
                    typed = ""
                    deadline = time.monotonic() + FRAME_DEADLINE
                    while time.monotonic() < deadline:
                        typed += self.drain(master, time.monotonic() + 0.4)
                        if "> levels 7" in typed:
                            break
                    self.assertIn(
                        "> levels 7",
                        typed,
                        f"{name}: what was typed never appeared in a frame, so it "
                        f"is being erased by the refresh. Got: {typed[-600:]!r}",
                    )

                    # 4. The command takes effect: the frame reports the new depth.
                    os.write(master, b"\r")
                    after = ""
                    deadline = time.monotonic() + FRAME_DEADLINE
                    while time.monotonic() < deadline:
                        after += self.drain(master, time.monotonic() + 0.4)
                        if "Showing 7 levels." in after:
                            break
                    self.assertIn(
                        "Showing 7 levels.",
                        after,
                        f"{name}: `levels 7` did not take effect. Got: {after[-400:]!r}",
                    )

                    # 5. Quitting works, and the terminal is handed back.
                    os.write(master, b"quit\r")
                    child.wait(timeout=EXIT_DEADLINE)
                    restored = termios.tcgetattr(master)
                    self.assertTrue(
                        restored[3] & termios.ICANON,
                        f"{name}: ICANON was not restored on exit; the user's shell "
                        "is left without line editing",
                    )
                    self.assertTrue(
                        restored[3] & termios.ECHO,
                        f"{name}: ECHO was not restored on exit; the user's shell "
                        "is left without echo",
                    )
                finally:
                    if child.poll() is None:
                        child.send_signal(signal.SIGKILL)
                        child.wait(timeout=EXIT_DEADLINE)
                    os.close(master)

    def test_a_signal_restores_the_terminal(self):
        """Ctrl-C must not leave the shell without echo.

        The book installs a handler so the signal is noticed by the draw loop
        rather than killing the process with the terminal still in the mode it
        set. Without that, the recovery is `stty sane` and the user has to know
        it exists.
        """
        name, binary = TERMINALS[0]
        if not binary.exists():
            self.skipTest(f"{binary} is missing; run `make rust-build` first")
        self.start_feeder(name)

        master, slave = pty.openpty()
        initial = termios.tcgetattr(slave)
        initial[3] |= termios.ICANON | termios.ECHO
        termios.tcsetattr(slave, termios.TCSANOW, initial)

        child = subprocess.Popen(
            [str(binary), str(self.config)],
            cwd=str(ROOT),
            stdin=slave,
            stdout=slave,
            stderr=slave,
            start_new_session=True,
        )
        os.close(slave)
        try:
            first = self.drain(master, time.monotonic() + FRAME_DEADLINE)
            self.assertIn("orderbook-lab", first, first[:300])
            # A pty cannot easily deliver a signal to the foreground process
            # group, so send it directly. The book's handler is what makes the
            # difference between restoring and not.
            child.send_signal(signal.SIGTERM)
            child.wait(timeout=EXIT_DEADLINE)
            restored = termios.tcgetattr(master)
            self.assertTrue(
                restored[3] & termios.ICANON,
                "a signal left the terminal without line editing",
            )
            self.assertTrue(
                restored[3] & termios.ECHO,
                "a signal left the terminal without echo",
            )
        finally:
            if child.poll() is None:
                child.send_signal(signal.SIGKILL)
                child.wait(timeout=EXIT_DEADLINE)
            os.close(master)


if __name__ == "__main__":
    unittest.main()