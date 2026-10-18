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

A stale binary is not tested
----------------------------

The test drives a compiled artifact, so it checks the artifact is current first:
if the binary is older than the newest file under `rust/*/src`, it skips and says
to run `make rust-build`. A stale binary is worse than a missing one — missing
skips, and stale fails or passes for a reason that has nothing to do with the
source. That is not hypothetical: the frame assertions were added, the checkout's
binary had not been rebuilt, and the new test failed against code that predated
the change. It looked like flakiness for two runs, and the real cause was a test
testing an old build.

`make rust-test` depends on `rust-build`, so it always tests what is in the tree.
`make test` does not, so on a checkout with a stale Rust binary it skips these two
and says why.

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


def plain(text: str) -> str:
    """The text without the cursor and colour escapes.

    A failure message that quotes a raw frame is a wall of `\x1b[K` and unreadable.
    Stripping them is what makes an assertion failure something you can act on.
    """
    import re

    return re.sub(r"\x1b\[[0-9;?]*[A-Za-z]", "", text)


def newest_source_mtime() -> float:
    """The newest modification time under the Rust sources."""
    newest = 0.0
    for crate in ("ob", "ob-cli"):
        root = ROOT / "rust" / crate / "src"
        if not root.is_dir():
            continue
        for path in root.rglob("*.rs"):
            newest = max(newest, path.stat().st_mtime)
    return newest


def skip_reason(binary: pathlib.Path) -> str | None:
    """Why this binary cannot be trusted for the assertions below, or `None`.

    A test that drives a compiled artifact has to check the artifact is current.
    A stale binary is worse than a missing one: missing skips, and stale fails or
    passes for a reason that has nothing to do with the source. Exactly that
    happened — the frame assertions were added, the checkout's binary had not been
    rebuilt, and the new test failed against code that predated the change. It
    looked like flakiness for two runs.
    """
    if not binary.exists():
        return f"{binary} is missing; run `make rust-build` first"
    built = binary.stat().st_mtime
    newest = newest_source_mtime()
    if newest > built:
        return (
            f"{binary} is older than its sources, so it does not contain them; "
            f"run `make rust-build` first"
        )
    return None


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
        self.write_config()
        self.feedd = None
        self.feedd_log = None

    def write_config(self):
        """Write a config this test owns: its own ring, WAL and port.

        The ring and the log live in a temporary directory so a run cannot read or
        overwrite the state of an instance the user has running.
        """
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

    def feed_fixture(self) -> None:
        """Send the fixture, so the ring holds records with stamps.

        The lag in feed time is a difference of two record stamps, so a test for
        it needs records. Without them the header reports `-`, which is the honest
        answer and not the one under test.
        """
        subprocess.run(
            [
                sys.executable,
                "python/feed_adapter.py",
                "--config",
                str(self.config),
                "--fixture",
                "e2e/fixture.jsonl",
            ],
            cwd=str(ROOT),
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            timeout=FRAME_DEADLINE,
        )

    def start_feeder(self, name: str):
        """Run the feed daemon, because the book attaches to a ring that exists.

        The terminal does not create the ring: attaching to a file that is not
        there would be indistinguishable from attaching to an empty one.

        The port is picked by binding zero and closing the socket, which leaves a
        window between that and the daemon binding it. Something else taking the
        port in that window is unlikely and does happen, and a rare failure in a
        gate is worse than no gate, so an early exit is retried on a fresh port.

        The retry keys on the exit rather than on the daemon's message, because
        the two daemons say different things: the Rust one reports the address and
        names the likely cause, and the C++ one prints `bind failed` and nothing
        else. Matching text would couple this test to two log formats and would
        still miss the next variant. Retrying any early exit cannot hide a real
        failure either: three attempts all fail and the log is reported.
        """
        daemon = ROOT / "build" / "feedd"
        if not daemon.exists():
            self.skipTest(f"build/feedd is missing; run `make` first ({name})")

        for attempt in range(3):
            self.feedd_log = open(self.workdir / "feedd.log", "wb")
            self.feedd = subprocess.Popen(
                [str(daemon), str(self.config)],
                cwd=str(ROOT),
                stdout=self.feedd_log,
                stderr=self.feedd_log,
                start_new_session=True,
            )
            # The daemon prints its ready line to stderr, which is the same file.
            deadline = time.monotonic() + FRAME_DEADLINE
            while time.monotonic() < deadline:
                text = (self.workdir / "feedd.log").read_text(errors="replace")
                if "feedd listen" in text:
                    return
                if self.feedd.poll() is not None:
                    break
                time.sleep(0.05)
            text = (self.workdir / "feedd.log").read_text(errors="replace")
            self.feedd_log.close()
            if self.feedd.poll() is not None:
                # An early exit. Most likely the port was taken between the probe
                # and the bind; possibly something else. Try a fresh port.
                print(f"  feedd exited early ({text.strip()[:80]}); retrying on another port")
                self.port = free_port()
                self.write_config()
                continue
            self.fail("feedd did not print its ready line")
        self.fail(
            "feedd could not be started in three attempts; last output:\n" + text
        )

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
                reason = skip_reason(binary)
                if reason is not None:
                    self.skipTest(reason)
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
                    # 0. Give the ring records to report on.
                    self.feed_fixture()

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

                    # 2a. The lag is reported as a span of feed time as well as a
                    # count of records, and the newest record's age is a real
                    # duration rather than the `-` an empty ring gives.
                    header = ""
                    deadline = time.monotonic() + FRAME_DEADLINE
                    while time.monotonic() < deadline:
                        header += self.drain(master, time.monotonic() + 0.4)
                        if "of feed time" in header and "staleness=" in header:
                            break
                    self.assertIn(
                        "of feed time",
                        header,
                        f"{name}: the header does not report the lag in time. "
                        f"Got: {header[-500:]!r}",
                    )
                    self.assertRegex(
                        header,
                        r"lag=\d+ records \([0-9.]+(ns|us|ms|s) of feed time\)",
                        f"{name}: the lag span is not a formatted duration",
                    )
                    self.assertRegex(
                        header,
                        r"staleness=[0-9.]+(ns|us|ms|s)",
                        f"{name}: staleness is not a formatted duration. It reads "
                        f"`-` when the ring is empty, so the fixture did not reach it",
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
                        "Showing 7 levels",
                        after,
                        f"{name}: `levels 7` did not take effect. Got: "
                        f"{plain(after)[-300:]!r}",
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

    def test_the_picker_switches_ticker_and_writes_the_watch_file(self):
        """Typing a prefix, completing it, and switching.

        The picker is the visible half of a two-process arrangement: the terminal
        writes the watch file and the adapter subscribes to what is in it. This
        checks the half that can be checked without a network, and that the
        subscription follows the selection rather than being fixed at startup.
        """
        name, binary = TERMINALS[0]
        reason = skip_reason(binary)
        if reason is not None:
            self.skipTest(reason)
        self.start_feeder(name)
        self.feed_fixture()

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
        )
        os.close(slave)
        watch = self.workdir / "watch.txt"
        try:
            first = self.drain(master, time.monotonic() + FRAME_DEADLINE)
            self.assertIn("orderbook-lab", first, first[:300])

            # The terminal is the authority on what is watched while it runs, so
            # it writes the file on open.
            self.assertTrue(watch.exists(), "the terminal wrote no watch file")
            self.assertIn("BTCUSDT", watch.read_text())

            # 1. A prefix offers the matching symbols.
            os.write(master, b"eth")
            offered = ""
            deadline = time.monotonic() + FRAME_DEADLINE
            while time.monotonic() < deadline:
                offered += self.drain(master, time.monotonic() + 0.4)
                if "ETHUSDT" in offered:
                    break
            self.assertIn(
                "ETHUSDT",
                offered,
                f"{name}: the picker offered nothing for 'eth'. Got: {offered[-400:]!r}",
            )

            # 2. Tab completes the line to it.
            os.write(master, b"\t")
            completed = ""
            deadline = time.monotonic() + FRAME_DEADLINE
            while time.monotonic() < deadline:
                completed += self.drain(master, time.monotonic() + 0.4)
                if "> ETHUSDT" in completed:
                    break
            self.assertIn(
                "> ETHUSDT",
                completed,
                f"{name}: Tab did not complete the line. Got: {completed[-300:]!r}",
            )

            # 3. Enter switches, and the subscription grows with it.
            os.write(master, b"\r")
            switched = ""
            deadline = time.monotonic() + FRAME_DEADLINE
            while time.monotonic() < deadline:
                switched += self.drain(master, time.monotonic() + 0.4)
                if "ETHUSDT" in switched and "watching 2" in switched:
                    break
            self.assertIn(
                "watching 2",
                switched,
                f"{name}: the switch did not add a second symbol. Got: {switched[-400:]!r}",
            )
            written = watch.read_text()
            self.assertIn("ETHUSDT", written, "the watch file did not grow")
            self.assertIn("BTCUSDT", written, "the first symbol was dropped")

            # 4. A symbol the catalog does not list is refused, with the way past
            #    it named. Refused rather than tried because every watched symbol
            #    shares one websocket.
            os.write(master, b"zzzqqq\r")
            refused = ""
            deadline = time.monotonic() + FRAME_DEADLINE
            while time.monotonic() < deadline:
                refused += self.drain(master, time.monotonic() + 0.4)
                if "not in config/tickers.txt" in refused:
                    break
            self.assertIn("not in config/tickers.txt", refused, f"{name}: {refused[-300:]!r}")
            self.assertIn("force ZZZQQQ", refused, "the escape hatch was not named")
            self.assertNotIn(
                "ZZZQQQ",
                [line for line in watch.read_text().splitlines() if not line.startswith("#")],
                "a refused symbol was written to the watch file anyway",
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
        reason = skip_reason(binary)
        if reason is not None:
            self.skipTest(reason)
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