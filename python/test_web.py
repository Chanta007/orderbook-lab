"""The web frontend's server, from a socket.

Why this test exists
--------------------

`python/test_web_browser.py` proves the page works in a browser; this proves the
*server* works without one, which is what CI can run. Between them the two files cover
the layer's third frontend the way the pty test and the `view` test cover the others.

What it asserts
---------------

1. `GET /` serves the page with no fetched asset: the lab's Rust side has no
   dependencies and the page keeps that true in the browser.
2. `GET /events` streams frames: several arrive, each is one SSE event carrying the
   composed frame as JSON, and the sequence numbers advance.
3. `POST /act` runs a command through the same dispatcher the terminal uses, and the
   reply is the frame the command produced — so a script sees the effect, not an
   acknowledgement.
4. A refusal reaches the page as words (a symbol that is not in the catalog names the
   file to edit).
5. `GET /health` counts what the server has done.
6. A viewer that stops reading does not slow the feed: the frame counter keeps
   advancing while nothing is read. (The drop counter itself is unit-tested in
   `rust/ob-cli/src/web.rs`, because forcing a socket buffer full takes ten seconds of
   frames and this test is about isolation.)
7. A connection past the cap is refused with 503 rather than queued.
"""

import json
import pathlib
import socket
import subprocess
import sys
import tempfile
import time
import unittest
import urllib.error
import urllib.request

ROOT = pathlib.Path(__file__).resolve().parents[1]
WEB = ROOT / "rust" / "target" / "release" / "web"
FEEDD = ROOT / "build" / "feedd"

sys.path.insert(0, str(ROOT / "python"))
import feed_adapter as fa  # noqa: E402  (the path has to be set up first)

DEADLINE = 20.0


def free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def skip_reason() -> str | None:
    if not WEB.exists():
        return f"{WEB} is missing; run `make rust-build` first"
    if not FEEDD.exists():
        return f"{FEEDD} is missing; run `make all` first"
    newest = 0.0
    for crate in ("ob", "ob-cli", "ob-view"):
        for path in (ROOT / "rust" / crate / "src").rglob("*.rs"):
            newest = max(newest, path.stat().st_mtime)
    if newest > WEB.stat().st_mtime:
        return f"{WEB} is older than its sources; run `make rust-build` first"
    return None


def picture(symbol: str, base: float, levels: int = 5) -> bytes:
    out = []
    for side in (0, 1):
        out.append(fa.pack_msg(side, 0.0, 0.0, symbol, fa.DEPTH_RESET))
        for i in range(levels):
            price = base + (i if side == 1 else -i)
            out.append(fa.pack_msg(side, price, 1.0 + i, symbol))
    return b"".join(out)


class Stream:
    """A raw connection to `/events`, so a test can read the bytes the browser gets."""

    def __init__(self, port: int) -> None:
        self.sock = socket.create_connection(("127.0.0.1", port), timeout=DEADLINE)
        self.sock.sendall(
            b"GET /events HTTP/1.1\r\nHost: 127.0.0.1\r\nAccept: text/event-stream\r\n\r\n"
        )
        self.buffer = b""
        self.head = self.read_until(b"\r\n\r\n")

    def read_until(self, marker: bytes) -> bytes:
        while marker not in self.buffer:
            chunk = self.sock.recv(65536)
            if not chunk:
                break
            self.buffer += chunk
        head, _, rest = self.buffer.partition(marker)
        self.buffer = rest
        return head

    def event(self) -> dict:
        """The next complete event, as a parsed frame."""
        raw = self.read_until(b"\n\n").decode("utf-8", "replace")
        data = [line[6:] for line in raw.split("\n") if line.startswith("data: ")]
        if not data:
            raise AssertionError(f"no data line in event: {raw!r}")
        return json.loads(data[0])

    def close(self) -> None:
        try:
            self.sock.close()
        except OSError:
            pass


def rows_of(frame: dict) -> list[str]:
    return ["".join(span["text"] for span in row["spans"]) for row in frame["frame"]["lines"]]


def price_rows(rows: list[str]) -> list[str]:
    out = []
    for row in rows:
        parts = row.split()
        if len(parts) >= 4 and "." in parts[0]:
            out.append(parts[0])
    return out


class WebServer(unittest.TestCase):
    def setUp(self) -> None:
        reason = skip_reason()
        if reason is not None:
            self.skipTest(reason)
        self.workdir = pathlib.Path(tempfile.mkdtemp(prefix="ob-web-test-"))
        self.port = free_port()
        self.web_port = free_port()
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
  "levels": 5,
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
        self.feedd_log = open(self.workdir / "feedd.log", "wb")
        self.feedd = subprocess.Popen(
            [str(FEEDD), str(self.config)],
            cwd=str(ROOT),
            stdout=self.feedd_log,
            stderr=self.feedd_log,
            start_new_session=True,
        )
        self.wait_for(self.workdir / "feedd.log", "feedd listen", self.feedd)
        with socket.create_connection(("127.0.0.1", self.port), timeout=5) as sock:
            sock.sendall(picture("BTCUSDT", 65000.0, levels=9))
        time.sleep(0.3)

        self.web_log = open(self.workdir / "web.log", "wb")
        self.web = subprocess.Popen(
            [str(WEB), str(self.config), "--port", str(self.web_port)],
            cwd=str(ROOT),
            stdout=self.web_log,
            stderr=self.web_log,
            start_new_session=True,
        )
        self.wait_for(self.workdir / "web.log", "web listening", self.web)
        self.streams: list[Stream] = []

    def tearDown(self) -> None:
        for stream in self.streams:
            stream.close()
        for proc in (self.web, self.feedd):
            if proc.poll() is None:
                proc.terminate()
                try:
                    proc.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    proc.kill()
        self.web_log.close()
        self.feedd_log.close()
        import shutil

        shutil.rmtree(self.workdir, ignore_errors=True)

    # -- helpers ---------------------------------------------------------

    def wait_for(self, path: pathlib.Path, marker: str, proc: subprocess.Popen) -> None:
        deadline = time.monotonic() + DEADLINE
        while time.monotonic() < deadline:
            if path.exists() and marker in path.read_text(errors="replace"):
                return
            if proc.poll() is not None:
                self.fail(f"{proc.args[0]} exited: {path.read_text(errors='replace')[:300]}")
            time.sleep(0.05)
        self.fail(f"{proc.args[0]} never printed {marker!r}")

    def url(self, path: str) -> str:
        return f"http://127.0.0.1:{self.web_port}{path}"

    def get(self, path: str) -> tuple[int, str, dict]:
        try:
            with urllib.request.urlopen(self.url(path), timeout=DEADLINE) as response:
                return response.status, response.read().decode(), dict(response.headers)
        except urllib.error.HTTPError as e:
            with e:
                return e.code, e.read().decode(), dict(e.headers)

    def post(self, path: str, body: str) -> tuple[int, str]:
        request = urllib.request.Request(
            self.url(path),
            data=body.encode(),
            headers={"content-type": "application/x-www-form-urlencoded"},
            method="POST",
        )
        try:
            with urllib.request.urlopen(request, timeout=DEADLINE) as response:
                return response.status, response.read().decode()
        except urllib.error.HTTPError as e:
            with e:
                return e.code, e.read().decode()

    def stream(self) -> Stream:
        stream = Stream(self.web_port)
        self.streams.append(stream)
        return stream

    def health(self) -> dict:
        status, body, _ = self.get("/health")
        self.assertEqual(status, 200, body)
        return dict(part.split("=", 1) for part in body.split()[1:])

    def act(self, command: str) -> list[str]:
        status, body = self.post("/act", "cmd=" + urllib.parse.quote(command))
        self.assertEqual(status, 200, body)
        return rows_of({"frame": json.loads(body)})

    # -- the tests -------------------------------------------------------

    def test_the_page_is_served_with_nothing_fetched_from_elsewhere(self) -> None:
        status, body, headers = self.get("/")
        self.assertEqual(status, 200)
        self.assertEqual(headers.get("Content-Type"), "text/html; charset=utf-8")
        self.assertIn("orderbook-lab", body)
        self.assertIn('id="screen"', body)
        self.assertIn('id="cmd"', body)
        # No fetched asset: the page carries its own styles and script, which is what
        # keeps the zero-dependency claim true on the browser side as well.
        self.assertNotIn("<script src", body)
        self.assertNotIn("<link rel=\"stylesheet\"", body)
        self.assertNotIn("cdn.", body)

    def test_the_stream_carries_composed_frames(self) -> None:
        stream = self.stream()
        self.assertIn(b"200 OK", stream.head)
        self.assertIn(b"text/event-stream", stream.head)
        frames = [stream.event() for _ in range(3)]
        sequences = [frame["seq"] for frame in frames]
        self.assertEqual(sequences, sorted(sequences), "frames arrive in order")
        self.assertGreater(sequences[-1], sequences[0], "and keep arriving")
        rows = rows_of(frames[-1])
        self.assertIn("orderbook-lab (rust)", rows[0])
        self.assertIn("BID PX", "\n".join(rows))
        self.assertTrue(
            price_rows(rows),
            f"the streamed frame has no prices: {rows[:6]}",
        )

    def test_a_command_from_a_script_is_the_command_the_terminal_runs(self) -> None:
        before = price_rows(self.act("levels 3"))
        self.assertEqual(len(before), 3, f"the config asks for three levels: {before}")
        after = price_rows(self.act("levels 7"))
        self.assertEqual(len(after), 7, f"`levels 7` asks for seven: {after}")

        panel = self.act("status")
        joined = "\n".join(panel)
        self.assertIn("slots,", joined)
        self.assertNotIn("BID PX", joined, "the table is gone in the status panel")

        back = self.act("book")
        self.assertIn("BID PX", "\n".join(back))

    def test_a_refusal_reaches_the_page_as_words(self) -> None:
        rows = self.act("zzzqqq")
        joined = " ".join(rows)
        self.assertIn("config/tickers.txt", joined, joined)

    def test_health_counts_what_the_server_has_done(self) -> None:
        stream = self.stream()
        stream.event()
        health = self.health()
        self.assertGreater(int(health["frames"]), 0)
        self.assertIn("dropped", health)
        self.assertIn("clients", health)
        self.assertIn("refused", health)

    def test_a_viewer_that_stops_reading_does_not_slow_the_feed(self) -> None:
        # The property that matters: the frame loop publishes on a timer and never
        # waits for a viewer. This connection reads its header and then nothing.
        stalled = self.stream()
        first = self.health()
        time.sleep(3.0)
        second = self.health()
        published = int(second["frames"]) - int(first["frames"])
        self.assertGreaterEqual(
            published,
            20,
            f"the feed slowed while a viewer was not reading: {published} frames in 3 s",
        )
        # And the server is still answering other connections.
        self.assertEqual(self.get("/health")[0], 200)
        # The stalled connection is still usable: it can catch up to the newest frame
        # rather than being served a backlog.
        frame = stalled.event()
        self.assertGreater(frame["seq"], 0)

    def test_a_connection_past_the_cap_is_refused_rather_than_queued(self) -> None:
        # The cap is `MAX_CLIENTS` in `rust/ob-cli/src/web.rs`; this holds exactly that
        # many, so the next connection is the one over it. A change to the constant
        # fails here, which is the point: the number is a promise about memory.
        cap = 32
        held = [self.stream() for _ in range(cap)]
        for stream in held:
            stream.event()
        refused = None
        probe = socket.create_connection(("127.0.0.1", self.web_port), timeout=DEADLINE)
        try:
            probe.sendall(
                b"GET /events HTTP/1.1\r\nHost: 127.0.0.1\r\nAccept: text/event-stream\r\n\r\n"
            )
            # Read until the end of the headers: one `recv` can split the status line,
            # and a test that asserts on half a line reports the wrong thing.
            raw = b""
            while b"\r\n\r\n" not in raw and len(raw) < 4096:
                chunk = probe.recv(4096)
                if not chunk:
                    break
                raw += chunk
            refused = raw.decode("utf-8", "replace")
            probe.close()
        finally:
            pass
        self.assertIsNotNone(refused)
        self.assertIn("503", refused, refused[:200])
        # One slot back before reading the counters, because `/health` is itself a
        # connection and would be the one refused.
        held.pop().close()
        time.sleep(0.3)
        self.assertGreaterEqual(int(self.health()["refused"]), 1)

    def complete(self, line: str) -> list[dict]:
        import urllib.parse

        status, body, _ = self.get("/complete?line=" + urllib.parse.quote(line))
        self.assertEqual(status, 200, body)
        return json.loads(body)["suggestions"]

    def test_a_symbol_prefix_completes_from_the_shared_vocabulary(self) -> None:
        # The same function the terminal's picker calls, so a prefix means the same
        # thing in a browser and in a window. The catalog here is the repository's
        # own `config/tickers.txt`.
        got = self.complete("eth")
        self.assertTrue(got, "no suggestions for a listed symbol")
        self.assertEqual(got[0]["label"], "ETHUSDT")
        self.assertEqual(got[0]["kind"], "symbol")
        self.assertEqual(got[0]["insert"], "ETHUSDT")
        self.assertTrue(got[0]["detail"], "a suggestion carries its explanation")

    def test_a_command_prefix_completes_and_takes_a_space(self) -> None:
        got = self.complete("bo")
        labels = [s["label"] for s in got]
        # Commands first, then the symbols that match: `bo` is also the start of
        # BONKUSDT in the repository's catalog, and both are things you could mean.
        self.assertEqual(labels[0], "book", "{labels}")
        self.assertEqual(got[0]["kind"], "command")
        self.assertEqual(got[0]["insert"], "book")
        self.assertIn("BONKUSDT", labels)

        levels = self.complete("lev")
        self.assertEqual(levels[0]["insert"], "levels ", "so the argument comes next")

    def test_an_argument_position_offers_symbols(self) -> None:
        got = self.complete("only eth")
        self.assertTrue(got)
        self.assertEqual(got[0]["label"], "only ETHUSDT")
        self.assertEqual(got[0]["insert"], "only ETHUSDT")

    def test_the_three_times_are_commands_and_complete(self) -> None:
        # They were in the reference from the start and the dispatcher answered them
        # as symbols until the vocabulary cross-check found it.
        got = self.complete("age")
        self.assertEqual([s["label"] for s in got], ["age"])
        rows = self.act("age")
        joined = " ".join(rows)
        self.assertIn("age:", joined, joined)
        self.assertIn("applied a record", joined, joined)

    def test_nothing_typed_and_nothing_matching_both_offer_nothing(self) -> None:
        self.assertEqual(self.complete(""), [])
        self.assertEqual(self.complete("zzzz"), [])

    def test_the_limit_is_a_parameter(self) -> None:
        status, body, _ = self.get("/complete?line=e&limit=2")
        self.assertEqual(status, 200)
        self.assertEqual(len(json.loads(body)["suggestions"]), 2)

    def test_an_unknown_path_is_a_404_and_a_get_cannot_run_a_command(self) -> None:
        status, _, _ = self.get("/nope")
        self.assertEqual(status, 404)
        # A GET must not change anything: a browser, a proxy or a crawler may issue one
        # for any URL it has seen.
        status, _, _ = self.get("/act?cmd=levels+3")
        self.assertEqual(status, 404)


if __name__ == "__main__":
    unittest.main()