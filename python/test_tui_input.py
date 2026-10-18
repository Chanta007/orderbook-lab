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
import socket
import subprocess
import sys
import tempfile
import termios
import threading
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


def prices_on_screen(frame: str) -> list[str]:
    """The prices in a frame's book rows.

    A book row is a price, a size, a price and a size, with the colour escapes
    already stripped. Four fields with a decimal point in the first is the shape,
    and it is specific enough that the header and help lines are not mistaken for
    rows.
    """
    out = []
    for line in plain(frame).split("\n"):
        parts = line.split()
        if len(parts) >= 4 and "." in parts[0]:
            out.append(parts[0])
    return out


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
            if self.feedd_log is not None:
                # A second run in the same test opens a second log; the first
                # one's file handle is not left to the collector.
                self.feedd_log.close()
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

    def test_two_tickers_keep_their_own_books(self):
        """Two symbols in one ring, and only the selected one on screen.

        The picker test proves the subscription follows the selection. This proves
        the other half: that a book built from a ring carrying several symbols
        contains one symbol's prices and not another's, which is what the filter in
        the terminal's drain loop is for.

        The records go straight to the daemon with the adapter's own packer rather
        than through a websocket, so the test needs no network and still exercises
        the real ingest path. The feed stays up while the terminal runs, because
        the terminal starts at the live end of the ring: sending everything first
        and then opening the window tests nothing.
        """
        name, binary = TERMINALS[0]
        reason = skip_reason(binary)
        if reason is not None:
            self.skipTest(reason)
        self.start_feeder(name)

        sys.path.insert(0, str(ROOT / "python"))
        import feed_adapter as fa

        # Five levels each, at prices nothing else could produce.
        def picture(symbol: str, base: float) -> bytes:
            out = []
            for side in (0, 1):
                out.append(fa.pack_msg(side, 0.0, 0.0, symbol, fa.DEPTH_RESET))
                for i in range(5):
                    price = base + (i if side == 1 else -i)
                    out.append(fa.pack_msg(side, price, 1.0 + i, symbol))
            return b"".join(out)

        sock = socket.create_connection(("127.0.0.1", self.port), timeout=5)
        stop = threading.Event()

        def feed() -> None:
            while not stop.is_set():
                try:
                    sock.sendall(picture("BTCUSDT", 65000.0))
                    sock.sendall(picture("ETHUSDT", 3000.0))
                except OSError:
                    return
                time.sleep(0.05)

        sender = threading.Thread(target=feed, daemon=True)
        sender.start()

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
        try:
            self.drain(master, time.monotonic() + FRAME_DEADLINE)
            first = self.drain(master, time.monotonic() + 1.5)
            first_prices = prices_on_screen(first)
            self.assertTrue(
                any(p.startswith("6") for p in first_prices),
                f"{name}: no BTC prices while BTCUSDT is selected. Got {first_prices}",
            )
            self.assertFalse(
                any(p.startswith("3") for p in first_prices),
                f"{name}: another symbol's prices appeared. Got {first_prices}",
            )

            # Switch. Drain until the header has followed, because the frames
            # already in the pipe were drawn before the switch and would be read as
            # though the old book had survived it.
            os.write(master, b"ethusdt\r")
            deadline = time.monotonic() + FRAME_DEADLINE
            while time.monotonic() < deadline:
                if "ETHUSDT" in (seen := self.drain(master, time.monotonic() + 0.4)):
                    break
            second = self.drain(master, time.monotonic() + 1.5)
            second_prices = prices_on_screen(second)
            self.assertTrue(
                any(p.startswith("3") for p in second_prices),
                f"{name}: no ETH prices after switching. Got {second_prices}",
            )
            self.assertFalse(
                any(p.startswith("6") for p in second_prices),
                f"{name}: the previous symbol's prices survived the switch. "
                f"Got {second_prices}",
            )
            self.assertIn("ETHUSDT", second, "the header did not follow the switch")
            self.assertIn("feed=live", second, "the switched book is not being fed")
        finally:
            stop.set()
            if child.poll() is None:
                child.send_signal(signal.SIGKILL)
                child.wait(timeout=EXIT_DEADLINE)
            os.close(master)
            try:
                sock.close()
            except OSError:
                pass

    def test_the_window_re_attaches_when_the_feed_restarts(self):
        """A feed that stops, and comes back on the same ring.

        The reported stall, as a test. The window went to `feed=stalled` with a
        39-second staleness; the feed was started again by hand and the window
        stayed stalled, because nothing it could read said the run it was
        attached to had ended. The daemon truncates the ring's header at
        startup, so the write sequence begins again at zero while the reader's
        cursor is a number from the run that ended, and `consume` reports
        `Empty` on every frame. A window that does not handle that shows the
        last book of a dead run for ever, which is indistinguishable from a feed
        that never came back.

        The restart here is what `make rust-feed` performs. The window has to
        come back by itself: no keystroke, no relaunch, and the price rows have
        to be the new run's rather than the old run's.

        The feed is a socket this test drives, rather than the fixture file.
        The window starts at the live end of the ring, so a feed that publishes
        once, before the window has attached, leaves it nothing to consume and
        nothing to report — a timing race that says nothing about re-attaching.
        """
        name, binary = TERMINALS[0]
        reason = skip_reason(binary)
        if reason is not None:
            self.skipTest(reason)
        self.start_feeder(name)

        sys.path.insert(0, str(ROOT / "python"))
        import feed_adapter as fa

        def picture(symbol: str, base: float) -> bytes:
            out = []
            for side in (0, 1):
                out.append(fa.pack_msg(side, 0.0, 0.0, symbol, fa.DEPTH_RESET))
                for i in range(5):
                    price = base + (i if side == 1 else -i)
                    out.append(fa.pack_msg(side, price, 1.0 + i, symbol))
            return b"".join(out)

        def send(sock: socket.socket, base: float, stop: threading.Event) -> None:
            """Publish until the daemon goes away or the test says stop."""
            while not stop.is_set():
                try:
                    sock.sendall(picture("BTCUSDT", base))
                except OSError:
                    return
                time.sleep(0.05)

        def connect() -> socket.socket:
            return socket.create_connection(("127.0.0.1", self.port), timeout=5)

        stop = threading.Event()
        sock = connect()
        sender = threading.Thread(target=send, args=(sock, 65000.0, stop), daemon=True)
        sender.start()

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
        try:
            # 0. The window is fed, by the first run's prices.
            first = ""
            deadline = time.monotonic() + FRAME_DEADLINE
            while time.monotonic() < deadline:
                first += self.drain(master, time.monotonic() + 0.4)
                if "feed=live" in first and prices_on_screen(first):
                    break
            self.assertIn(
                "feed=live",
                first,
                f"{name}: the window never went live. Got: {plain(first)[-400:]!r}",
            )
            self.assertTrue(
                any(p.startswith("6") for p in prices_on_screen(first)),
                f"{name}: the first run's prices are not on screen. "
                f"Got {prices_on_screen(first)}",
            )

            # 1. The feed stops. This is the state the report showed.
            self.feedd.terminate()
            self.feedd.wait(timeout=EXIT_DEADLINE)
            stalled = ""
            deadline = time.monotonic() + FRAME_DEADLINE + 3.0
            while time.monotonic() < deadline:
                stalled += self.drain(master, time.monotonic() + 0.5)
                if "Feed stalled." in stalled:
                    break
            self.assertIn(
                "Feed stalled.",
                stalled,
                f"{name}: a stopped feed did not show as stalled. "
                f"Got: {plain(stalled)[-400:]!r}",
            )
            self.assertIn(
                "make rust-feed",
                stalled,
                f"{name}: the remedy for a stopped feed was not the command that "
                f"starts one without closing the window. Got: {plain(stalled)[-400:]!r}",
            )
            self.assertIn(
                "reattaches=0",
                stalled,
                "the window re-attached to a feed that had not restarted",
            )

            # 2. A new run over the same ring, which is what rust-feed does.
            # The first sender's connection went with the daemon it wrote to,
            # so it is closed before a second one is opened.
            sock.close()
            self.start_feeder(name)
            sock = connect()
            sender = threading.Thread(target=send, args=(sock, 95000.0, stop), daemon=True)
            sender.start()

            # 3. The window re-attaches by itself.
            #
            # The counter is the assertion that matters. A window that does not
            # re-attach can still *appear* to recover: once the new run has
            # published as many records as the old cursor had reached, `consume`
            # finds a slot whose sequence matches and carries on from there. It
            # has then silently lost the beginning of the run, which is the
            # difference between a book that is being maintained and one built
            # from the middle of a stream. Verified by disabling the check: the
            # prices below come back on their own and only this counter does not.
            back = ""
            deadline = time.monotonic() + FRAME_DEADLINE
            while time.monotonic() < deadline:
                back += self.drain(master, time.monotonic() + 0.5)
                if "reattaches=1" in back:
                    break
            self.assertIn(
                "reattaches=1",
                back,
                f"{name}: the window did not re-attach to the new run. "
                f"Got: {plain(back)[-500:]!r}",
            )

            # Read a fresh window of frames, because everything drained above
            # includes the stalled ones, which still carried the old prices.
            after = self.drain(master, time.monotonic() + 1.5)
            prices = prices_on_screen(after)
            self.assertTrue(
                any(p.startswith("9") for p in prices),
                f"{name}: the new run's prices are not on screen. Got {prices}",
            )
            self.assertFalse(
                any(p.startswith("6") for p in prices),
                f"{name}: the dead run's prices survived the re-attach. Got {prices}",
            )
            self.assertIn(
                "feed=live",
                after,
                f"{name}: the re-attached window is not being fed. "
                f"Got: {plain(after)[-400:]!r}",
            )
        finally:
            stop.set()
            try:
                sock.close()
            except OSError:
                pass
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