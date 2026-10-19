"""The `view` binary: the same frame the terminal draws, printed.

Why this test exists
--------------------

The view layer's claim is that a frame is data, and that the terminal is one of
several frontends over it. A claim like that is easy to state and easy to break: the
first version of the terminal had the composition and the drawing in one function, so
nothing else could show anything.

What it asserts
---------------

1. `view --once --json` prints a frame whose rows carry the header, the times and the
   book's prices — the same content the terminal draws, from the same snapshot.
2. `view --once` (text) and `view --once --json` agree row for row: two renderers,
   one composition. A divergence here is the layer failing at its one job.
3. `--panel status` prints the dashboard and not the price table.
4. `--act "levels 7"` changes the frame the same way typing it does, because it goes
   through the same dispatcher — and the change is visible as more book rows.
5. A usage error exits 2, which is what the other binaries do.

It needs a ring, so it starts its own daemon and sends a picture over a socket. No
network, no terminal, no pty: this is the frontend that has none of those.
"""

import json
import os
import pathlib
import socket
import subprocess
import sys
import tempfile
import time
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[1]
VIEW = ROOT / "rust" / "target" / "release" / "view"
FEEDD = ROOT / "build" / "feedd"

sys.path.insert(0, str(ROOT / "python"))
import feed_adapter as fa  # noqa: E402  (the path has to be set up first)

DEADLINE = 10.0


def free_port() -> int:
    """A port nothing is listening on, so a run cannot collide with a live one."""
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def skip_reason() -> str | None:
    """Why the binaries cannot be trusted for these assertions, or `None`."""
    if not VIEW.exists():
        return f"{VIEW} is missing; run `make rust-build` first"
    if not FEEDD.exists():
        return f"{FEEDD} is missing; run `make all` first"
    newest = 0.0
    for crate in ("ob", "ob-cli", "ob-view"):
        for path in (ROOT / "rust" / crate / "src").rglob("*.rs"):
            newest = max(newest, path.stat().st_mtime)
    if newest > VIEW.stat().st_mtime:
        return (
            f"{VIEW} is older than its sources, so it does not contain them; "
            f"run `make rust-build` first"
        )
    return None


def picture(symbol: str, base: float, levels: int = 5) -> bytes:
    """One full picture for `symbol`: a reset per side, then `levels` levels each."""
    out = []
    for side in (0, 1):
        out.append(fa.pack_msg(side, 0.0, 0.0, symbol, fa.DEPTH_RESET))
        for i in range(levels):
            price = base + (i if side == 1 else -i)
            out.append(fa.pack_msg(side, price, 1.0 + i, symbol))
    return b"".join(out)


class ViewCli(unittest.TestCase):
    """One case per run of the binary, against its own daemon and ring."""

    def setUp(self) -> None:
        reason = skip_reason()
        if reason is not None:
            self.skipTest(reason)
        self.workdir = pathlib.Path(tempfile.mkdtemp(prefix="ob-view-test-"))
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
        self.log = open(self.workdir / "feedd.log", "wb")
        self.feedd = subprocess.Popen(
            [str(FEEDD), str(self.config)],
            cwd=str(ROOT),
            stdout=self.log,
            stderr=self.log,
            start_new_session=True,
        )
        deadline = time.monotonic() + DEADLINE
        while time.monotonic() < deadline:
            if "feedd listen" in (self.workdir / "feedd.log").read_text(errors="replace"):
                break
            if self.feedd.poll() is not None:
                self.fail(f"feedd exited: {(self.workdir / 'feedd.log').read_text()[:200]}")
            time.sleep(0.05)
        else:
            self.fail("feedd did not print its ready line")

    def tearDown(self) -> None:
        if self.feedd.poll() is None:
            self.feedd.terminate()
            try:
                self.feedd.wait(timeout=DEADLINE)
            except subprocess.TimeoutExpired:
                self.feedd.kill()
        self.log.close()
        import shutil

        shutil.rmtree(self.workdir, ignore_errors=True)

    def feed(self, base: float = 65000.0, levels: int = 5) -> None:
        """Send a picture, so the ring has prices in it."""
        with socket.create_connection(("127.0.0.1", self.port), timeout=5) as sock:
            sock.sendall(picture("BTCUSDT", base, levels))
        time.sleep(0.3)

    def view(self, *args: str) -> subprocess.CompletedProcess:
        return subprocess.run(
            [str(VIEW), str(self.config), *args],
            cwd=str(ROOT),
            capture_output=True,
            text=True,
            timeout=DEADLINE,
        )

    def rows_of(self, result: subprocess.CompletedProcess) -> list[str]:
        """The JSON frame's rows as text, which is what a renderer would draw."""
        self.assertEqual(result.returncode, 0, result.stderr)
        frame = json.loads(result.stdout)
        return ["".join(span["text"] for span in row["spans"]) for row in frame["lines"]]

    def test_a_printed_frame_carries_what_the_terminal_draws(self) -> None:
        self.feed()
        rows = self.rows_of(self.view("--once", "--json"))
        joined = "\n".join(rows)
        self.assertTrue(rows, "a frame with no rows")
        self.assertIn("orderbook-lab", rows[0], joined)
        self.assertIn("feed=", rows[0])
        self.assertIn("ring=4096", rows[1], joined)
        self.assertTrue(
            any("65000" in row for row in rows),
            f"no prices in the frame: {joined}",
        )
        self.assertTrue(
            any("BID PX" in row for row in rows),
            f"no column headers: {joined}",
        )

    def test_the_two_renderers_agree_row_for_row(self) -> None:
        # One composition, two renderers. If these disagree, the layer has failed at
        # the one thing it is for.
        #
        # The numbers are normalised away, because two runs are two moments: the age
        # and the staleness of a frame are *supposed* to differ. What must not differ
        # is the frame's shape — the same rows, in the same order, saying the same
        # words.
        import re

        def shape(rows: list[str]) -> list[str]:
            return [re.sub(r"\d+(\.\d+)?", "N", row).rstrip() for row in rows]

        self.feed()
        json_rows = self.rows_of(self.view("--once", "--json"))
        text = self.view("--once")
        self.assertEqual(text.returncode, 0, text.stderr)
        text_rows = text.stdout.rstrip("\n").split("\n")
        self.assertEqual(
            shape(json_rows),
            shape(text_rows),
            "the JSON and the text renderings of one frame disagree",
        )

    def test_the_status_panel_is_a_choice_and_not_a_second_binary(self) -> None:
        self.feed()
        rows = self.rows_of(self.view("--once", "--json", "--panel", "status"))
        joined = "\n".join(rows)
        self.assertIn("slots,", joined, joined)
        self.assertIn("reader", joined)
        self.assertIn("ticks", joined)
        self.assertIn("daemon", joined)
        self.assertNotIn("BID PX", joined, "the table is not drawn in the status panel")

    def test_a_command_run_from_the_cli_is_the_command_the_terminal_runs(self) -> None:
        # A book row is a price, a size, a price and a size; counting those is how a
        # test asks "how deep is the table" without depending on a sentence about it.
        def price_rows(rows: list[str]) -> list[str]:
            out = []
            for row in rows:
                parts = row.split()
                if len(parts) >= 4 and "." in parts[0]:
                    out.append(parts[0])
            return out

        self.feed(levels=9)
        before = price_rows(self.rows_of(self.view("--once", "--json")))
        after = price_rows(self.rows_of(self.view("--once", "--json", "--act", "levels 7")))
        self.assertEqual(len(before), 3, f"the config asks for three levels: {before}")
        self.assertEqual(len(after), 7, f"`levels 7` asks for seven: {after}")

    def test_a_refusal_from_a_command_is_in_the_frame(self) -> None:
        self.feed()
        rows = self.rows_of(self.view("--once", "--json", "--act", "zzzqqq"))
        joined = " ".join(rows)
        self.assertIn("config/tickers.txt", joined, joined)

    def test_a_usage_error_exits_two(self) -> None:
        # The other binaries exit 2 for a usage error; a script that checks the code
        # should not have to know which binary it is talking to.
        missing = subprocess.run(
            [str(VIEW)], cwd=str(ROOT), capture_output=True, text=True, timeout=DEADLINE
        )
        self.assertEqual(missing.returncode, 2, missing.stderr)
        bad_panel = self.view("--once", "--panel", "charts")
        self.assertEqual(bad_panel.returncode, 2, bad_panel.stderr)
        self.assertIn("book", bad_panel.stderr, "the message says what it accepts")

    def test_a_frame_streams_without_a_terminal(self) -> None:
        # `--frames` is what a dashboard or a script uses to follow the feed, and it
        # must work with no terminal at all: stdout here is a pipe.
        self.feed()
        result = self.view("--json", "--frames", "3", "--interval", "50")
        self.assertEqual(result.returncode, 0, result.stderr)
        frames = [json.loads(line) for line in result.stdout.strip().split("\n") if line]
        self.assertEqual(len(frames), 3, "three frames, one per line")

    def test_the_cost_of_a_frame_has_a_command(self) -> None:
        # The design doc quotes a composition cost. This asserts the command that
        # produces it still runs and reports a number, rather than asserting the
        # number itself, which belongs to the machine it was measured on.
        self.feed()
        result = self.view("--bench", "200", "--levels", "5")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("ns per frame", result.stdout, result.stdout)
        self.assertIn("frames/s", result.stdout)


if __name__ == "__main__":
    unittest.main()