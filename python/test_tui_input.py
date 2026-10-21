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
import re
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
    """The text without the cursor and colour escapes, one row per line.

    A failure message that quotes a raw frame is a wall of `\x1b[K` and unreadable,
    and the frame writes each row where the cursor was moved to rather than ending it
    with a newline: a newline on the last row of the screen scrolls the screen, and
    every later row is then in the wrong place. So a cursor move becomes the row
    break here, which is what a terminal shows and what an assertion about a row
    needs.
    """
    text = re.sub(r"\x1b\[\d+;\d+H", "\n", text)
    return re.sub(r"\x1b\[[0-9;?]*[A-Za-z]", "", text)


sys.path.insert(0, str(ROOT / "python"))
import feed_adapter as fa  # noqa: E402  (the path has to be set up first)


def depth_picture(symbol: str, base: float, levels: int = 5) -> bytes:
    """One full picture for `symbol`: a reset per side, then `levels` levels each.

    Straight to the daemon with the adapter's own packer, so a test needs no
    websocket and still exercises the real ingest path.
    """
    out = []
    for side in (0, 1):
        out.append(fa.pack_msg(side, 0.0, 0.0, symbol, fa.DEPTH_RESET))
        for i in range(levels):
            price = base + (i if side == 1 else -i)
            out.append(fa.pack_msg(side, price, 1.0 + i, symbol))
    return b"".join(out)


class Screen:
    """What a terminal would be showing, given the stream the book wrote.

    The book writes only the rows that changed, so a slice of the stream is not a
    frame — it is the difference between two frames. A test that asserted on a slice
    would be asserting on the diff, and would pass or fail according to whether a
    price happened to move. This applies the stream to a screen the way a terminal
    does: a cursor move selects a row, the text after it replaces that row, and a
    clear empties everything.
    """

    def __init__(self) -> None:
        self.rows: dict[int, str] = {}

    def feed(self, stream: str) -> None:
        """Apply a chunk of the stream."""
        import re

        row = 1
        pos = 0
        while True:
            move = re.search(r"\x1b\[(\d+);(\d+)H", stream[pos:])
            if move is None:
                self._write(row, stream[pos:])
                return
            self._write(row, stream[pos : pos + move.start()])
            row = int(move.group(1))
            pos += move.end()

    def _write(self, row: int, text: str) -> None:
        import re

        if "\x1b[2J" in text:
            self.rows.clear()
        erased = "\x1b[K" in text
        plain_text = re.sub(r"\x1b\[[0-9;?]*[A-Za-z]", "", text)
        if not plain_text and not erased:
            # A bare cursor move. The frame ends by placing the cursor on the prompt,
            # and that is not a row being written: treating it as one would erase the
            # prompt it just drew.
            return
        # Every row is written whole, so the text is the row; an erase with no text
        # after it empties the row, which is what a shorter frame does to the rows it
        # no longer has.
        self.rows[row] = plain_text.rstrip()

    def text(self) -> str:
        """The screen, one row per line."""
        if not self.rows:
            return ""
        height = max(self.rows)
        return "\n".join(self.rows.get(i, "") for i in range(1, height + 1))


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


def prompt_of(frame: str) -> str:
    """What is on the input line, from the last frame that drew one.

    The prompt is the last row of every frame, written as `> <line>` followed by
    `erase to end of line`. It is preceded by the erase-below marker when nothing
    is being offered and by the picker's last row when something is, so both are
    accepted — a pattern that only matched after a picker row read the prompt from
    the last frame that had one, which is how a cleared line was reported as still
    holding what had been typed. This reads the **raw** frame: stripping is what
    removes `\\x1b[K`, and without it the capture runs on into the next frame.
    """
    matches = re.findall(r"(?:\x1b\[J|\n)> ([^\x1b\r\n]*)", frame)
    return matches[-1] if matches else ""


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
        self.screen = Screen()

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

    def reset_screen(self) -> None:
        """Start a new screen, for a test that opens a second window."""
        self.screen = Screen()

    def drain(self, master: int, deadline: float) -> str:
        """Read until the deadline, and return what the screen shows.

        What comes back is the screen, not the bytes since the last call: the book
        writes only the rows that changed, so a slice of the stream is a diff. The
        screen is what a person reads, and what every assertion here is about.
        """
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
        self.screen.feed("".join(out))
        return self.screen.text()

    def test_typed_input_survives_a_refresh(self):
        for name, binary in TERMINALS:
            with self.subTest(terminal=name):
                reason = skip_reason(binary)
                if reason is not None:
                    self.skipTest(reason)
                self.start_feeder(name)
                self.reset_screen()

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
                    # Read the rows rather than the raw stream: a row now carries
                    # one escape per span, so a regex over the raw bytes would be
                    # matching the writer's styling rather than what a person sees.
                    header_text = plain(header)
                    self.assertIn(
                        "of feed time",
                        header_text,
                        f"{name}: the header does not report the lag in time. "
                        f"Got: {header_text[-500:]!r}",
                    )
                    self.assertRegex(
                        header_text,
                        r"lag=\d+ records \([0-9.]+(ns|us|ms|s) of feed time\)",
                        f"{name}: the lag span is not a formatted duration",
                    )
                    self.assertRegex(
                        header_text,
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

                    # 4. The command takes effect: seven price rows, counted. The
                    #    frame does not print a sentence about the depth any more —
                    #    the depth is the rows, and a test that counts them tests
                    #    what a person sees rather than a status line that could
                    #    agree with itself. The fixture's pictures carry three
                    #    levels, so a deeper book is sent over the socket: seven
                    #    rows cannot be asserted from a book that has three.
                    os.write(master, b"\r")
                    sock = socket.create_connection(("127.0.0.1", self.port), timeout=5)
                    stop = threading.Event()
                    try:
                        for _ in range(40):
                            sock.sendall(depth_picture("BTCUSDT", 65000.0, levels=9))
                            time.sleep(0.05)
                            rows = prices_on_screen(self.drain(master, time.monotonic() + 0.2))
                            if len(rows) == 7:
                                break
                        self.assertEqual(
                            len(rows),
                            7,
                            f"{name}: `levels 7` did not produce seven price rows. "
                            f"Got {rows}",
                        )
                    finally:
                        stop.set()
                        sock.close()

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
        self.reset_screen()

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

        # Five levels each, at prices nothing else could produce.
        sock = socket.create_connection(("127.0.0.1", self.port), timeout=5)
        stop = threading.Event()

        def feed() -> None:
            while not stop.is_set():
                try:
                    sock.sendall(depth_picture("BTCUSDT", 65000.0))
                    sock.sendall(depth_picture("ETHUSDT", 3000.0))
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
            # Drain until the book has prices in it, rather than for a fixed time:
            # the daemon has to accept the socket, read the records and publish them,
            # and a loaded machine can take longer than any constant a test picks. A
            # fixed window made this test fail once in a full sweep and pass on its
            # own, which is the shape of a flake rather than a defect.
            self.drain(master, time.monotonic() + FRAME_DEADLINE)
            deadline = time.monotonic() + FRAME_DEADLINE
            first = self.drain(master, time.monotonic() + 1.5)
            first_prices = prices_on_screen(first)
            while time.monotonic() < deadline and not first_prices:
                first = self.drain(master, time.monotonic() + 1.0)
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

        def send(sock: socket.socket, base: float, stop: threading.Event) -> None:
            """Publish until the daemon goes away or the test says stop."""
            while not stop.is_set():
                try:
                    sock.sendall(depth_picture("BTCUSDT", base))
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
                # The remedy, not the first line of the diagnosis: a frame is one
                # write, but a pty splits a write that does not fit its buffer, and
                # stopping at the first line reads a frame whose tail is still in
                # flight.
                if "make rust-feed" in stalled:
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

    def test_escape_neither_eats_the_keys_that_follow_nor_leaves_the_line(self):
        """The reported bug: type an unlisted symbol, and then type a real one.

        The input thread read the rest of an escape sequence with a two-byte
        blocking read. A terminal in non-canonical mode sends a bare `Escape` as one
        byte, so that call waited for the next two keystrokes and then consumed them
        whatever they were — never added to the line, and an `Enter` among them
        never seen as one. A person who backed out of a refusal with `Escape` and
        typed `ethusdt` watched `et` disappear; repeating it, `husdt` disappeared,
        and no key they pressed produced a symbol. From outside it is a window that
        has stopped listening, and it was reported as "after typing a symbol that
        does not exist I cannot change the ticker any more".

        Both halves are checked here: `Escape` clears what is on the line, and the
        keys after it arrive intact. The second assertion is the one that fails
        against the unfixed binary.
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
                    out.append(fa.pack_msg(side, base + (i if side == 1 else -i), 1.0 + i, symbol))
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

        threading.Thread(target=feed, daemon=True).start()

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
            live = ""
            deadline = time.monotonic() + FRAME_DEADLINE
            while time.monotonic() < deadline:
                live += self.drain(master, time.monotonic() + 0.4)
                if "feed=live" in live:
                    break
            self.assertIn("feed=live", live, plain(live)[-300:])

            # 1. A symbol that is not in the catalog: refused, with the way past it.
            os.write(master, b"SYMBOL\r")
            refused = ""
            deadline = time.monotonic() + FRAME_DEADLINE
            while time.monotonic() < deadline:
                refused += self.drain(master, time.monotonic() + 0.4)
                if "not in config/tickers.txt" in refused:
                    break
            self.assertIn(
                "not in config/tickers.txt",
                refused,
                f"{name}: an unlisted symbol was not refused. Got: {plain(refused)[-300:]!r}",
            )

            # 2. Part of a name on the line, then Escape: the line goes.
            os.write(master, b"eth")
            typed = ""
            deadline = time.monotonic() + FRAME_DEADLINE
            while time.monotonic() < deadline:
                typed += self.drain(master, time.monotonic() + 0.4)
                if "> eth" in plain(typed):
                    break
            self.assertIn("> eth", plain(typed), f"{name}: {plain(typed)[-200:]!r}")

            os.write(master, b"\x1b")
            cleared = self.drain(master, time.monotonic() + 0.6)
            self.assertEqual(
                prompt_of(cleared),
                "",
                f"{name}: Escape did not clear the line. Got {prompt_of(cleared)!r}",
            )

            # 3. And what is typed next arrives intact. Against the unfixed binary
            #    the first two characters are eaten and this never switches.
            os.write(master, b"ethusdt\r")
            after = ""
            deadline = time.monotonic() + FRAME_DEADLINE
            while time.monotonic() < deadline:
                after += self.drain(master, time.monotonic() + 0.5)
                if "ETHUSDT" in plain(after) and "feed=live" in after:
                    break
            self.assertIn(
                "ETHUSDT",
                plain(after),
                f"{name}: the keys typed after Escape were eaten, so the ticker never "
                f"changed. Got: {plain(after)[-300:]!r}",
            )
            # A fresh window of frames: everything drained above includes the ones
            # drawn before the switch, which still carried the old prices.
            fresh = self.drain(master, time.monotonic() + 1.5)
            prices = prices_on_screen(fresh)
            self.assertTrue(
                any(p.startswith("3") for p in prices),
                f"{name}: ETHUSDT is selected but its prices are not on screen. "
                f"Got {prices}",
            )
            self.assertFalse(
                any(p.startswith("6") for p in prices),
                f"{name}: the previous symbol's prices survived the switch. Got {prices}",
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

    def test_a_prefix_of_one_listed_symbol_switches_to_it(self):
        """Typing `eth` and pressing Enter means ETHUSDT.

        The other half of the same report. The picker was already showing the name
        the person meant, so answering `ETH is not in config/tickers.txt` told them
        their own shortcut was wrong. Now a name that exactly one listed symbol
        starts with is used, and the frame says so. What has *not* changed is the
        refusal: a name matching nothing is still refused, because every watched
        symbol shares one websocket and an unlisted stream name can take the whole
        connection down.
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
                    out.append(fa.pack_msg(side, base + (i if side == 1 else -i), 1.0 + i, symbol))
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

        threading.Thread(target=feed, daemon=True).start()

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
            live = ""
            deadline = time.monotonic() + FRAME_DEADLINE
            while time.monotonic() < deadline:
                live += self.drain(master, time.monotonic() + 0.4)
                if "feed=live" in live:
                    break
            self.assertIn("feed=live", live, plain(live)[-300:])

            # 1. A prefix that only one listed symbol starts with: it is used, and
            #    the frame says which name it stood for.
            os.write(master, b"eth\r")
            after = ""
            deadline = time.monotonic() + FRAME_DEADLINE
            while time.monotonic() < deadline:
                after += self.drain(master, time.monotonic() + 0.5)
                if "ETHUSDT" in plain(after) and "feed=live" in after:
                    break
            self.assertIn(
                "ETHUSDT",
                plain(after),
                f"{name}: a unique prefix did not switch. Got: {plain(after)[-300:]!r}",
            )
            # The status wraps to the window, so a sentence can span two rows: the
            # assertion is about the words, not about where the wrap fell.
            self.assertIn(
                "the only symbol starting with it",
                " ".join(plain(after).split()),
                f"{name}: the completion was not explained. Got: {plain(after)[-300:]!r}",
            )
            self.assertNotIn(
                "\nETH\n",
                self.watch_file().read_text(),
                "the prefix itself reached the watch file, which the adapter subscribes to",
            )
            self.assertIn("ETHUSDT", self.watch_file().read_text())

            # 2. A prefix several listed symbols start with names them instead of
            #    guessing between them.
            os.write(master, b"et\r")
            several = ""
            deadline = time.monotonic() + FRAME_DEADLINE
            while time.monotonic() < deadline:
                several += self.drain(master, time.monotonic() + 0.5)
                if "start with" in plain(several):
                    break
            text = plain(several)
            self.assertIn("ETCUSDT", text, f"{name}: the matches were not named. Got {text[-300:]!r}")
            self.assertIn("force ET", text, f"{name}: the way past it was not named")

            # 3. `only` goes through the same check, because it replaces the whole
            #    subscription rather than adding to it.
            os.write(master, b"only zzztusdt\r")
            only = ""
            deadline = time.monotonic() + FRAME_DEADLINE
            while time.monotonic() < deadline:
                only += self.drain(master, time.monotonic() + 0.5)
                if "not in config/tickers.txt" in plain(only):
                    break
            self.assertIn(
                "not in config/tickers.txt",
                plain(only),
                f"{name}: `only` accepted an unlisted symbol. Got: {plain(only)[-300:]!r}",
            )
            self.assertNotIn(
                "ZZZTUSDT",
                [line for line in self.watch_file().read_text().splitlines() if not line.startswith("#")],
                "an unlisted symbol reached the watch file through `only`",
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

    def watch_file(self) -> pathlib.Path:
        """Where this config's watch list lives: beside the bus, as both sides
        derive it."""
        return self.workdir / "watch.txt"

    def test_tab_completes_a_command_as_well_as_a_symbol(self):
        """The picker offers commands now, not only symbols.

        It used to be a symbol lookup: `bo` offered nothing, so `book` had to be typed
        in full even though the reference lists it. The vocabulary the reference is
        built from is what the completion matches against now, so the list under the
        prompt and the list in `help` are the same list in the same order.
        """
        name, binary = TERMINALS[0]
        reason = skip_reason(binary)
        if reason is not None:
            self.skipTest(reason)
        self.start_feeder(name)
        self.feed_fixture()
        self.reset_screen()

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
            deadline = time.monotonic() + FRAME_DEADLINE
            screen_now = self.drain(master, time.monotonic() + 0.4)
            while time.monotonic() < deadline and "BID PX" not in screen_now:
                screen_now = self.drain(master, time.monotonic() + 0.4)

            # `sta` is ambiguous — status and staleness — and the reference's order
            # decides: the panel first.
            os.write(master, b"sta")
            offered = ""
            deadline = time.monotonic() + FRAME_DEADLINE
            while time.monotonic() < deadline:
                offered = self.drain(master, time.monotonic() + 0.4)
                if "status" in offered:
                    break
            self.assertIn("status", offered, f"{name}: the picker does not offer commands")
            self.assertIn("staleness", offered, "and lists the other match too")

            os.write(master, b"\t")
            typed = ""
            deadline = time.monotonic() + FRAME_DEADLINE
            while time.monotonic() < deadline:
                typed = self.drain(master, time.monotonic() + 0.4)
                if "> status" in typed:
                    break
            self.assertIn(
                "> status",
                typed,
                f"{name}: Tab did not complete to the highlighted entry. Got: "
                f"{typed[-300:]!r}",
            )

            os.write(master, b"\r")
            panel = ""
            deadline = time.monotonic() + FRAME_DEADLINE
            while time.monotonic() < deadline:
                panel = self.drain(master, time.monotonic() + 0.4)
                if "slots," in panel:
                    break
            self.assertIn("slots,", panel, f"{name}: the completed command did not run")
        finally:
            if child.poll() is None:
                child.send_signal(signal.SIGKILL)
                child.wait(timeout=EXIT_DEADLINE)
            os.close(master)

    def test_a_burst_of_keys_keeps_every_action_in_it(self):
        """Tab and Enter in one write: the completion must survive the submission.

        The input thread reads bytes one at a time and the loop acts on what it finds
        once per frame, so two keys that arrive together are two actions in one frame.
        They were kept in a single slot, and the second overwrote the first: `sta`,
        Tab and Enter in one burst ran `sta` and answered "STA is not in
        config/tickers.txt". The tests that passed had sent the keys in separate
        writes with a drain between, which is not what a paste or a fast typist does.
        """
        name, binary = TERMINALS[0]
        reason = skip_reason(binary)
        if reason is not None:
            self.skipTest(reason)
        self.start_feeder(name)
        self.feed_fixture()
        self.reset_screen()

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
            deadline = time.monotonic() + FRAME_DEADLINE
            screen_now = self.drain(master, time.monotonic() + 0.4)
            while time.monotonic() < deadline and "BID PX" not in screen_now:
                screen_now = self.drain(master, time.monotonic() + 0.4)

            # One write: the prefix, the completion and the submission, together.
            os.write(master, b"sta\t\r")
            panel = ""
            deadline = time.monotonic() + FRAME_DEADLINE
            while time.monotonic() < deadline:
                panel = self.drain(master, time.monotonic() + 0.4)
                if "slots," in panel or "not in config/tickers.txt" in panel:
                    break
            self.assertIn(
                "slots,",
                panel,
                f"{name}: the burst did not complete to `status` before submitting it. "
                f"Got: {panel[-400:]!r}",
            )
            self.assertNotIn(
                "not in config/tickers.txt",
                panel,
                f"{name}: the burst submitted the prefix instead of the completion",
            )
        finally:
            if child.poll() is None:
                child.send_signal(signal.SIGKILL)
                child.wait(timeout=EXIT_DEADLINE)
            os.close(master)

    def test_a_completion_with_an_argument_in_the_same_burst(self):
        """`lev`, Tab, `7`, Enter: the completion is applied where it was typed.

        The reader used to edit the line itself and queue only the actions, so the `7`
        was appended to `lev` before the completion ran: the command executed was
        `LEV7`, and the answer was "LEV7 is not in config/tickers.txt". Every key is an
        event now and the frame loop applies them in order, so the completion sees the
        text that was on the line when Tab was pressed.
        """
        name, binary = TERMINALS[0]
        reason = skip_reason(binary)
        if reason is not None:
            self.skipTest(reason)
        self.start_feeder(name)
        self.feed_fixture()
        self.reset_screen()

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
        sock = None
        try:
            deadline = time.monotonic() + FRAME_DEADLINE
            screen_now = self.drain(master, time.monotonic() + 0.4)
            while time.monotonic() < deadline and "BID PX" not in screen_now:
                screen_now = self.drain(master, time.monotonic() + 0.4)

            # A book deeper than the window's three levels, so seven rows are
            # distinguishable from three.
            sock = socket.create_connection(("127.0.0.1", self.port), timeout=5)
            rows: list[str] = []
            for _ in range(40):
                sock.sendall(depth_picture("BTCUSDT", 65000.0, levels=9))
                time.sleep(0.05)
                rows = prices_on_screen(self.drain(master, time.monotonic() + 0.2))
                if len(rows) >= 3:
                    break

            # One write: prefix, completion, argument, submission.
            os.write(master, b"lev\t7\r")
            deadline = time.monotonic() + FRAME_DEADLINE
            while time.monotonic() < deadline:
                screen_now = self.drain(master, time.monotonic() + 0.3)
                rows = prices_on_screen(screen_now)
                if len(rows) == 7 or "not in config/tickers.txt" in screen_now:
                    break
            self.assertNotIn(
                "not in config/tickers.txt",
                screen_now,
                f"{name}: the burst submitted the text from before the completion",
            )
            self.assertEqual(
                len(rows),
                7,
                f"{name}: `lev` Tab `7` Enter did not ask for seven levels. Got {rows}",
            )
        finally:
            if sock is not None:
                sock.close()
            if child.poll() is None:
                child.send_signal(signal.SIGKILL)
                child.wait(timeout=EXIT_DEADLINE)
            os.close(master)

    def test_the_reference_is_below_the_prompt_and_the_status_panel_is_a_command(self):
        """The command reference and the dashboard, both behind commands.

        The reference used to be eleven rows in the middle of the frame, between the
        header and the book, which is where it was read least and cost the most: it
        pushed the prices down the screen and could not be turned off. It is now
        behind `help`, drawn below the prompt, and the same slot takes a second
        panel — `status`, the feed, the queues, the consumers and the tick rates —
        so the frame has one place for "something else" rather than one per feature.
        """
        name, binary = TERMINALS[0]
        reason = skip_reason(binary)
        if reason is not None:
            self.skipTest(reason)
        self.start_feeder(name)
        self.feed_fixture()
        self.reset_screen()

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

        def wait_for(predicate, seconds: float = FRAME_DEADLINE) -> str:
            """Drain until the screen satisfies the predicate, then return it."""
            deadline = time.monotonic() + seconds
            screen_now = self.drain(master, time.monotonic() + 0.4)
            while time.monotonic() < deadline:
                if predicate(screen_now):
                    break
                screen_now = self.drain(master, time.monotonic() + 0.4)
            return screen_now

        try:
            # 1. The reference is not in the frame until it is asked for, and the
            #    prices are not pushed down the screen by it.
            first = wait_for(lambda t: "BID PX" in t)
            self.assertIn("BID PX", first, f"{name}: no book. Got {first[:300]!r}")
            self.assertNotIn(
                "leave this window",
                first,
                f"{name}: the reference is in the frame by default, which is what "
                f"this change moved. Got: {first[:400]!r}",
            )

            # 2. `help` puts it below the prompt, where the input line is the last
            #    thing above it.
            os.write(master, b"help\r")
            shown = wait_for(lambda t: "leave this window" in t)
            self.assertIn("leave this window", shown, f"{name}: {shown[-400:]!r}")
            rows = shown.split("\n")
            prompt_at = max(i for i, row in enumerate(rows) if row.startswith(">"))
            reference_at = min(i for i, row in enumerate(rows) if "leave this window" in row)
            self.assertGreater(
                reference_at,
                prompt_at,
                f"{name}: the reference is not below the prompt. Rows: {rows!r}",
            )
            self.assertIn("status", shown, "the reference names the new commands")
            self.assertIn("book", shown)

            # 3. `help` again hides it, so the frame is a keypress away from clean.
            os.write(master, b"help\r")
            hidden = wait_for(lambda t: "leave this window" not in t)
            self.assertNotIn(
                "leave this window",
                hidden,
                f"{name}: `help` did not hide the reference. Got: {hidden[-400:]!r}",
            )

            # 4. `status` is the dashboard: the ring, this reader, the ticks and the
            #    daemon's own report, and no price table.
            os.write(master, b"status\r")
            panel = wait_for(lambda t: "slots," in t)
            self.assertIn("slots,", panel, f"{name}: no ring bar. Got: {panel[-500:]!r}")
            self.assertIn("reader", panel)
            self.assertIn("ticks", panel)
            self.assertIn("BTCUSDT", panel, "the tick table names what it counted")
            self.assertIn("adapter", panel)
            self.assertIn("daemon", panel)
            self.assertNotIn(
                "BID PX",
                panel,
                f"{name}: the price table is still drawn in the status panel",
            )

            # 5. `book` brings the table back.
            os.write(master, b"book\r")
            back = wait_for(lambda t: "BID PX" in t)
            self.assertIn("BID PX", back, f"{name}: `book` did not return to the table")
            self.assertNotIn("slots,", back, "and the dashboard is gone")
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