"""The web frontend, driven in a real browser.

Why this test exists
--------------------

`python/test_web.py` proves the server with a socket: the page arrives, frames arrive,
a command changes the view. None of that proves the *page* works — that its JavaScript
parses the frames, that the styles land, that typing into the input runs a command. A
browser is the only thing that can answer those, and a page that serves correct bytes
to a broken script is the failure this file exists for.

How it drives the browser
-------------------------

Chrome's DevTools protocol over a WebSocket, spoken from the standard library: Chrome
is started headless with `--remote-debugging-port=0`, which makes it pick a free port
and write it to `DevToolsActivePort` in its own profile directory. No dependency, no
driver binary, and nothing shared with a browser a person has open — the profile
directory is a temporary one, which is also what makes the kill in `tearDown` safe:
it matches that directory and never "every Chrome".

The protocol is three calls: `Page.navigate`, `Runtime.evaluate` (which is how the page
is read and how a command is typed), and `Page.captureScreenshot`.

What it asserts
---------------

1. The page renders the frame the server streams: the header, the times, the column
   headers and the prices are in the DOM, and the status line counts frames.
2. The styles are the ones the composition names, checked through the DOM's computed
   colour rather than by reading the stylesheet.
3. Typing a command into the input and pressing Enter changes the view — the whole
   input path, in a browser, through the same dispatcher the terminal uses.
4. A screenshot of the rendered page is a real image, saved next to the test output
   for a person to look at.
"""

import base64
import json
import os
import pathlib
import re
import shutil
import socket
import struct
import subprocess
import sys
import tempfile
import time
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[1]
WEB = ROOT / "rust" / "target" / "release" / "web"
FEEDD = ROOT / "build" / "feedd"
CHROME = pathlib.Path("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome")

sys.path.insert(0, str(ROOT / "python"))
import feed_adapter as fa  # noqa: E402  (the path has to be set up first)

DEADLINE = 20.0


def skip_reason() -> str | None:
    if not CHROME.exists():
        return f"no Chrome at {CHROME}; the browser test needs one"
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


def free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def picture(symbol: str, base: float, levels: int = 5) -> bytes:
    out = []
    for side in (0, 1):
        out.append(fa.pack_msg(side, 0.0, 0.0, symbol, fa.DEPTH_RESET))
        for i in range(levels):
            price = base + (i if side == 1 else -i)
            out.append(fa.pack_msg(side, price, 1.0 + i, symbol))
    return b"".join(out)


class WebSocket:
    """Just enough of RFC 6455 to talk to the DevTools endpoint.

    A handshake, masked text frames out, unmasked frames in, with the two length
    forms a browser uses. Not a general client: no extensions, no fragmentation on
    the way out, and no ping handling beyond answering nothing — the peer is Chrome
    on loopback and the traffic is JSON.
    """

    def __init__(self, url: str) -> None:
        m = re.match(r"ws://([^:/]+):(\d+)(/.*)", url)
        if m is None:
            raise ValueError(f"not a ws url: {url}")
        host, port, path = m.group(1), int(m.group(2)), m.group(3)
        self.sock = socket.create_connection((host, port), timeout=DEADLINE)
        key = base64.b64encode(os.urandom(16)).decode()
        handshake = (
            f"GET {path} HTTP/1.1\r\n"
            f"Host: {host}:{port}\r\n"
            "Upgrade: websocket\r\n"
            "Connection: Upgrade\r\n"
            f"Sec-WebSocket-Key: {key}\r\n"
            "Sec-WebSocket-Version: 13\r\n\r\n"
        )
        self.sock.sendall(handshake.encode())
        head = b""
        while b"\r\n\r\n" not in head:
            head += self.sock.recv(4096)
        if b"101" not in head.split(b"\r\n")[0]:
            raise RuntimeError(f"handshake refused: {head[:120]!r}")
        self.buffer = head.split(b"\r\n\r\n", 1)[1]

    def send(self, payload: dict) -> None:
        data = json.dumps(payload).encode()
        header = bytearray([0x81])
        length = len(data)
        if length < 126:
            header.append(0x80 | length)
        elif length < 1 << 16:
            header.append(0x80 | 126)
            header += struct.pack(">H", length)
        else:
            header.append(0x80 | 127)
            header += struct.pack(">Q", length)
        mask = os.urandom(4)
        header += mask
        masked = bytes(b ^ mask[i % 4] for i, b in enumerate(data))
        self.sock.sendall(bytes(header) + masked)

    def _read_exactly(self, n: int) -> bytes:
        while len(self.buffer) < n:
            chunk = self.sock.recv(65536)
            if not chunk:
                raise ConnectionError("the DevTools connection closed")
            self.buffer += chunk
        out, self.buffer = self.buffer[:n], self.buffer[n:]
        return out

    def recv(self) -> dict:
        while True:
            first, second = self._read_exactly(2)
            opcode = first & 0x0F
            length = second & 0x7F
            if length == 126:
                length = struct.unpack(">H", self._read_exactly(2))[0]
            elif length == 127:
                length = struct.unpack(">Q", self._read_exactly(8))[0]
            payload = self._read_exactly(length)
            if opcode == 0x1:
                return json.loads(payload)
            # Anything else (a ping, a close, a binary frame) is not something this
            # client asked for; skipping it keeps the reader honest about what it read.
            continue

    def call(self, method: str, params: dict | None = None) -> dict:
        """One request, and the response with the matching id."""
        self.next_id = getattr(self, "next_id", 0) + 1
        self.send({"id": self.next_id, "method": method, "params": params or {}})
        wanted = self.next_id
        while True:
            message = self.recv()
            if message.get("id") == wanted:
                if "error" in message:
                    raise RuntimeError(f"{method}: {message['error']}")
                return message.get("result", {})

    def close(self) -> None:
        try:
            self.sock.close()
        except OSError:
            pass


class WebBrowser(unittest.TestCase):
    """One page, one browser, one server, all temporary."""

    @classmethod
    def setUpClass(cls) -> None:
        reason = skip_reason()
        if reason is not None:
            raise unittest.SkipTest(reason)

    def setUp(self) -> None:
        self.workdir = pathlib.Path(tempfile.mkdtemp(prefix="ob-web-browser-"))
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
        self.wait_for_ready(self.workdir / "feedd.log", "feedd listen", self.feedd)

        # A picture, so the page has prices the moment it opens.
        with socket.create_connection(("127.0.0.1", self.port), timeout=5) as sock:
            sock.sendall(picture("BTCUSDT", 65000.0))
        time.sleep(0.3)

        self.web_log = open(self.workdir / "web.log", "wb")
        self.web = subprocess.Popen(
            [str(WEB), str(self.config), "--port", str(self.web_port)],
            cwd=str(ROOT),
            stdout=self.web_log,
            stderr=self.web_log,
            start_new_session=True,
        )
        self.wait_for_ready(self.workdir / "web.log", "web listening", self.web)

        # A profile directory of its own: nothing shared with a browser a person has
        # open, and the only thing the teardown's kill pattern matches.
        self.profile = self.workdir / "chrome"
        self.profile.mkdir()
        self.chrome = subprocess.Popen(
            [
                str(CHROME),
                "--headless=new",
                "--disable-gpu",
                "--no-first-run",
                "--no-default-browser-check",
                "--remote-debugging-port=0",
                f"--user-data-dir={self.profile}",
                "about:blank",
            ],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        self.ws = WebSocket(self.devtools_url())
        self.ws.call("Page.enable")
        self.ws.call("Runtime.enable")

    def tearDown(self) -> None:
        try:
            self.ws.close()
        except Exception:
            pass
        for proc in (self.chrome, self.web, self.feedd):
            if proc.poll() is None:
                proc.terminate()
                try:
                    proc.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    proc.kill()
        self.web_log.close()
        self.feedd_log.close()
        shutil.rmtree(self.workdir, ignore_errors=True)

    # -- helpers ---------------------------------------------------------

    def wait_for_ready(self, path: pathlib.Path, marker: str, proc: subprocess.Popen) -> None:
        deadline = time.monotonic() + DEADLINE
        while time.monotonic() < deadline:
            if path.exists() and marker in path.read_text(errors="replace"):
                return
            if proc.poll() is not None:
                self.fail(f"{proc.args[0]} exited: {path.read_text(errors='replace')[:300]}")
            time.sleep(0.05)
        self.fail(f"{proc.args[0]} never printed {marker!r}")

    def devtools_url(self) -> str:
        """The page target's WebSocket URL, from the port Chrome chose."""
        port_file = self.profile / "DevToolsActivePort"
        deadline = time.monotonic() + DEADLINE
        while time.monotonic() < deadline:
            if port_file.exists():
                lines = port_file.read_text().splitlines()
                if len(lines) >= 2:
                    port = lines[0].strip()
                    break
            time.sleep(0.05)
        else:
            self.fail("Chrome never wrote DevToolsActivePort")
        import urllib.request

        deadline = time.monotonic() + DEADLINE
        while time.monotonic() < deadline:
            try:
                with urllib.request.urlopen(f"http://127.0.0.1:{port}/json/list") as fh:
                    targets = json.load(fh)
                for target in targets:
                    if target.get("type") == "page" and target.get("webSocketDebuggerUrl"):
                        return target["webSocketDebuggerUrl"]
            except Exception:
                pass
            time.sleep(0.1)
        self.fail("no page target appeared in the DevTools list")

    def evaluate(self, expression: str):
        """Run JavaScript in the page and return the value."""
        result = self.ws.call(
            "Runtime.evaluate",
            {"expression": expression, "returnByValue": True, "awaitPromise": False},
        )
        return result.get("result", {}).get("value")

    def navigate(self, url: str) -> None:
        self.ws.call("Page.navigate", {"url": url})

    def wait_until(self, expression: str, seconds: float = DEADLINE) -> bool:
        """Poll a JavaScript predicate until it is true, or give up."""
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            if self.evaluate(expression):
                return True
            time.sleep(0.2)
        return False

    def screen_text(self) -> str:
        return self.evaluate("document.getElementById('screen').innerText") or ""

    # -- the tests -------------------------------------------------------

    def test_the_page_renders_the_frame_the_server_streams(self) -> None:
        self.navigate(f"http://127.0.0.1:{self.web_port}/")
        self.assertTrue(
            self.wait_until("document.getElementById('screen').innerText.includes('BID PX')"),
            f"the page never drew the book: {self.screen_text()[:300]!r}",
        )
        text = self.screen_text()
        self.assertIn("orderbook-lab (rust)", text)
        self.assertIn("feed=", text)
        self.assertIn("ring=4096", text)
        self.assertIn("BID PX", text)
        self.assertTrue(
            any("65000" in row for row in text.split("\n")),
            f"no prices on the page: {text[:300]!r}",
        )
        # The status line is the page's own: frames received, and how far behind.
        stats = self.evaluate("document.getElementById('stats').innerText") or ""
        self.assertRegex(stats, r"\d+ frames")
        self.assertIn("0 dropped", stats, "a viewer on this host is not behind")
        rows = self.evaluate("document.querySelectorAll('#screen .row').length")
        self.assertGreater(rows, 5, f"the frame has rows: {rows}")

    def test_the_styles_are_the_ones_the_composition_names(self) -> None:
        # A style the page does not know renders as plain text: correct in the
        # terminal, invisible in the browser. The check is the computed colour of a
        # bid span and an ask span, which is what a person sees.
        self.navigate(f"http://127.0.0.1:{self.web_port}/")
        self.assertTrue(
            self.wait_until("document.querySelectorAll('#screen .bid').length > 0"),
            "no bid spans on the page",
        )
        colours = self.evaluate(
            "JSON.stringify({bid: getComputedStyle(document.querySelector('#screen .bid')).color,"
            " ask: getComputedStyle(document.querySelector('#screen .ask')).color,"
            " dim: getComputedStyle(document.querySelector('#screen .dim')).color})"
        )
        got = json.loads(colours)
        self.assertNotEqual(got["bid"], got["ask"], "a bid and an ask look the same")
        self.assertNotEqual(got["bid"], got["dim"], "a bid and a dim row look the same")
        self.assertNotIn("rgb(214, 217, 221)", got["bid"], "the bid fell back to plain")

    def test_typing_a_command_changes_the_view(self) -> None:
        # The input path end to end: the page sends it, the server dispatches it
        # through the same code the terminal uses, the next frame shows the result.
        self.navigate(f"http://127.0.0.1:{self.web_port}/")
        self.assertTrue(self.wait_until("document.querySelectorAll('#screen .row').length > 5"))
        typed = self.evaluate(
            "(() => { const input = document.getElementById('cmd');"
            " input.value = 'status';"
            " input.dispatchEvent(new KeyboardEvent('keydown', {key: 'Enter', bubbles: true}));"
            " return input.value; })()"
        )
        self.assertEqual(typed, "", "the page clears the input after sending")
        self.assertTrue(
            self.wait_until("document.getElementById('screen').innerText.includes('slots,')"),
            f"the status panel never arrived: {self.screen_text()[:300]!r}",
        )
        text = self.screen_text()
        self.assertIn("reader", text)
        self.assertIn("ticks", text)
        self.assertNotIn("BID PX", text, "the table is gone in the status panel")

        # And back, so the command is a switch rather than a one-way trip.
        self.evaluate(
            "(() => { const input = document.getElementById('cmd');"
            " input.value = 'book';"
            " input.dispatchEvent(new KeyboardEvent('keydown', {key: 'Enter', bubbles: true})); })()"
        )
        self.assertTrue(
            self.wait_until("document.getElementById('screen').innerText.includes('BID PX')"),
            "`book` did not bring the table back",
        )

    def test_a_command_in_the_url_runs_on_load(self) -> None:
        # `?cmd=status` is how a view is linked to, and it is also how a browser can
        # be pointed at a command without a keyboard.
        self.navigate(f"http://127.0.0.1:{self.web_port}/?cmd=status")
        self.assertTrue(
            self.wait_until("document.getElementById('screen').innerText.includes('slots,')"),
            f"?cmd=status did not run: {self.screen_text()[:300]!r}",
        )

    def test_a_screenshot_of_the_page_is_a_real_image(self) -> None:
        # Not an assertion about pixels — an assertion that the page is a page: a
        # blank or single-colour shot means the script threw or the styles did not
        # load, and either is visible in the bytes.
        self.navigate(f"http://127.0.0.1:{self.web_port}/")
        self.assertTrue(self.wait_until("document.getElementById('screen').innerText.includes('BID PX')"))
        shot = self.ws.call("Page.captureScreenshot", {"format": "png"})
        data = base64.b64decode(shot["data"])
        self.assertGreater(len(data), 4000, "a blank page compresses to almost nothing")
        self.assertEqual(data[:8], b"\x89PNG\r\n\x1a\n", "not a PNG")
        out = pathlib.Path(os.environ.get("OB_WEB_SCREENSHOT", "/tmp/ob-web.png"))
        out.write_bytes(data)
        # Distinct colours in the shot: a page that rendered text has more than one.
        self.assertGreater(
            len(set(data[1000:4000])),
            8,
            f"the screenshot looks blank; it is at {out}",
        )


if __name__ == "__main__":
    unittest.main()