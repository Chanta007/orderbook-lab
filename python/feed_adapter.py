"""Market-data adapter: Binance public WS or fixture → binary Msg on TCP.

Never talks to TRADE endpoints. Dev-only public market data.

depth5 is a fresh top-of-book picture. parse_depth5 sends DepthReset for a
side before that side's prices, or old prices stay in the book forever.
Client websocket frames are masked. A pong copies the ping payload. Binance
closes the socket if that pong does not come back. run_live reconnects.
"""
from __future__ import annotations

import argparse
import json
import os
import socket
import ssl
import struct
import sys
import threading
import time
from pathlib import Path
from urllib.parse import urlparse, urlunsplit

MAGIC = 0x4F424C42
MSG_FMT = "<IHBBQQ16s8sqq8s"
MSG_SIZE = struct.calcsize(MSG_FMT)
assert MSG_SIZE == 72


def load_cfg(path: str) -> dict:
    with open(path, encoding="utf-8") as f:
        return json.load(f)


DEPTH = 1
DEPTH_RESET = 4


# The default market-data host and its depth stream. One string, so a config that
# does not set `ws_url` and the combined URL built from it cannot disagree.
DEFAULT_WS_URL = "wss://data-stream.binance.vision:443/ws/btcusdt@depth5@100ms"

# The wire record carries a symbol in a fixed eight-byte field, so this is the
# most a symbol can be. Anything longer cannot be carried, and truncating it would
# map distinct pairs onto the same bytes: a reader would show one pair's prices
# under another's name.
SYMBOL_BYTES = 8

# The depth stream every watched symbol is subscribed to. Used both to build the
# URL and to read the symbol back out of a combined payload, so the two cannot
# disagree.
STREAM_SUFFIX = "@depth5@100ms"

# The file the terminal writes to say what it wants watched.
#
# A file rather than a socket, because the two processes already share a directory
# and a file needs no protocol: the terminal replaces it atomically, the adapter
# stats it, and either side can restart without the other noticing. It is also
# readable by hand, which makes the subscription something you can inspect rather
# than something you have to infer.
WATCH_NAME = "watch.txt"


def symbol_fits(symbol: str) -> bool:
    """Whether a symbol can be carried in the record's eight bytes."""
    return 0 < len(symbol) <= SYMBOL_BYTES and symbol.isascii()


def watch_path(cfg: dict) -> Path:
    """Where the watch list lives, defaulting to the bus's directory.

    Derived from `bus_path` so there is no second path to keep in step: whatever
    environment the bus is in, the watch list sits beside it.
    """
    explicit = cfg.get("watch_path")
    if explicit:
        return Path(explicit)
    return Path(cfg.get("bus_path", "var/dev/bus")).parent / WATCH_NAME


def read_watch(cfg: dict) -> list[str]:
    """The symbols to subscribe to, uppercase, in file order, without duplicates.

    A missing or empty file means the configured default symbol, so the lab works
    before the terminal has ever been opened. A symbol that does not fit the wire
    field is refused with its line number rather than truncated.
    """
    default = [str(cfg.get("symbol", "BTCUSDT")).upper()]
    path = watch_path(cfg)
    try:
        text = path.read_text()
    except OSError:
        return default
    out: list[str] = []
    seen: set[str] = set()
    for number, line in enumerate(text.splitlines(), 1):
        symbol = line.strip().upper()
        if not symbol or symbol.startswith("#"):
            continue
        if not symbol_fits(symbol):
            log(
                f"watch {path} line {number}: {symbol!r} does not fit "
                f"{SYMBOL_BYTES} bytes; ignoring it"
            )
            continue
        if symbol in seen:
            continue
        seen.add(symbol)
        out.append(symbol)
    return out or default


def combined_url(cfg: dict, symbols: list[str]) -> str:
    """One websocket carrying every symbol in `symbols`.

    Binance's combined form wraps each payload as
    `{"stream": "btcusdt@depth5@100ms", "data": {...}}`, which is what lets one
    connection carry several symbols and still say which is which. A connection
    per symbol would work too and would be a thread per symbol; one connection is
    less to go wrong.
    """
    base = urlparse(cfg.get("ws_url") or DEFAULT_WS_URL)
    streams = "/".join(f"{s.lower()}{STREAM_SUFFIX}" for s in symbols)
    return urlunsplit((base.scheme, base.netloc, "/stream", f"streams={streams}", ""))


def symbol_of(obj: dict, fallback: str) -> str:
    """Which symbol a payload belongs to.

    The stream name carries it: `btcusdt@depth5@100ms` is `BTCUSDT`. A payload with
    no stream name came from a single-stream connection, which is what a config's
    own `ws_url` is, and belongs to the symbol the caller expects.
    """
    stream = obj.get("stream")
    if isinstance(stream, str) and "@" in stream:
        return stream.split("@", 1)[0].upper()
    return fallback


def pack_msg(side: int, px: float, qty: float, symbol: str, msg_type: int = DEPTH) -> bytes:
    """Float prices become e8 integers here. seq and trace id stay zero.

    feedd fills the trace id and the timestamp. The ring fills seq when it
    publishes. Do not invent those fields in Python.
    """
    # Eight bytes exactly, zero padded. Not a C string: a symbol that fills the
    # field has no terminator, and a reader takes the bytes it is given. The Rust
    # side truncated to seven to keep one, which mangled every eight-character
    # symbol; both use all eight now.
    sym = symbol.encode("ascii")[:SYMBOL_BYTES].ljust(SYMBOL_BYTES, b"\0")
    return struct.pack(
        MSG_FMT,
        MAGIC,
        MSG_SIZE,
        msg_type,
        side,
        0,
        0,
        b"\0" * 16,
        b"\0" * 8,
        int(px * 1e8),
        int(qty * 1e8),
        sym,
    )


def send_all(sock: socket.socket, data: bytes) -> None:
    sock.sendall(data)


def log(msg: str) -> None:
    """One line, stamped.

    The stamp used to be absent, which made the log useless for the question it
    exists to answer. A window showing `feed stalled` cannot be matched to a line
    saying why without one, and neither can two runs in the same file: the log is
    append-only and shared, so an earlier session's lines sit above this one's.
    """
    print(f"{_stamp()} {msg}", file=sys.stderr, flush=True)


def _stamp() -> str:
    """Monotonic seconds since this process started, and the wall clock.

    Monotonic for the differences, which is what a stall is measured in, and the
    wall clock so a line can be matched against a window someone was watching.
    """
    up = time.monotonic() - _STARTED
    return f"[+{up:9.3f}s {time.strftime('%H:%M:%S')}]"


# When this process started, for the log's monotonic stamp.
_STARTED = time.monotonic()

# The file the adapter touches to say it is alive.
#
# The terminal reads it, because otherwise it cannot tell the two causes of a
# stalled book apart: an exchange that has gone quiet, and no adapter at all.
# Both look identical from the ring, and the second one is what a stopped session
# leaves behind. A file rather than a socket, for the same reasons the watch list
# is a file: the processes share a directory, and either can restart alone.
HEARTBEAT_NAME = "adapter.heartbeat"


def heartbeat_path(cfg: dict) -> Path:
    """Where the liveness file lives, beside the bus like the watch list."""
    explicit = cfg.get("heartbeat_path")
    if explicit:
        return Path(explicit)
    return watch_path(cfg).parent / HEARTBEAT_NAME


def beat(cfg: dict, note: str) -> None:
    """Say we are alive, and what we are doing.

    Written on every reconnect attempt and after every connect, so a reader can
    tell "still trying" from "gave up" as well as "running" from "gone". Failure
    is reported and not fatal: a heartbeat is a courtesy, and a lab that cannot
    write one should still feed.
    """
    path = heartbeat_path(cfg)
    try:
        path.parent.mkdir(parents=True, exist_ok=True)
        # Same trick as the watch file: write beside the target and rename, so a
        # reader never sees a half-written line.
        temp = path.with_suffix(".tmp")
        temp.write_text(f"{os.getpid()} {int(time.time())} {note}\n")
        os.replace(temp, path)
    except OSError as exc:
        log(f"cannot write the heartbeat {path}: {exc}")


def connect_feedd(cfg: dict) -> socket.socket:
    """Connect to feedd, and say so on stderr.

    obctl waits for this line before it calls the start good. The adapter has
    no other ready signal: with no feedd it can sit in the redial loop for a
    long time without failing, and a start that cannot tell the difference
    reports success over a book that will never fill.
    """
    host = cfg.get("listen_host", "127.0.0.1")
    port = int(cfg.get("listen_port", 9001))
    sock = socket.create_connection((host, port), timeout=5)
    log("adapter connected")
    return sock


def close_quietly(sock: socket.socket | None) -> None:
    if sock is None:
        return
    try:
        sock.close()
    except OSError:
        pass


def close_code_text(payload: bytes) -> str:
    if len(payload) < 2:
        return "none"
    code = int.from_bytes(payload[:2], "big")
    reason = payload[2:].decode("utf-8", "replace")
    if reason:
        return f"{code} reason={reason!r}"
    return str(code)


def client_frame(opcode: int, payload: bytes) -> bytes:
    """One client-to-server websocket frame.

    The high bit of the first byte is FIN. The low 4 bits are the opcode.
    Opcode 0xA is pong. Opcode 0x8 is close. The high bit of the second
    byte is the mask bit. RFC 6455 says a client must set it on every
    frame, and the server must close the connection if it is missing.
    The next 4 bytes are the mask key. Each payload byte is XORed with
    that key. A pong must use the ping's payload, not an empty body.
    """
    mask = os.urandom(4)
    ln = len(payload)
    head = bytearray([0x80 | (opcode & 0x0F)])
    if ln < 126:
        head.append(0x80 | ln)
    elif ln < 65536:
        head.append(0x80 | 126)
        head += ln.to_bytes(2, "big")
    else:
        head.append(0x80 | 127)
        head += ln.to_bytes(8, "big")
    head += mask
    masked = bytes(b ^ mask[i % 4] for i, b in enumerate(payload))
    return bytes(head) + masked


def _side_levels(obj: dict, long_key: str, short_key: str) -> list | None:
    if long_key in obj:
        return obj.get(long_key) or []
    if short_key in obj:
        return obj.get(short_key) or []
    return None


def parse_depth5(obj: dict, symbol: str) -> list[bytes]:
    """Turn one JSON picture into the bytes feedd expects.

    Binance sends bids and asks, or the short keys b and a. If a side is
    present, even as an empty list, the first message is DepthReset so the
    book drops prices from the previous picture. A zero size inside the
    picture is skipped. The reset already cleared the side.
    """
    out: list[bytes] = []
    for side, levels in (
        (0, _side_levels(obj, "bids", "b")),
        (1, _side_levels(obj, "asks", "a")),
    ):
        if levels is None:
            continue
        out.append(pack_msg(side, 0.0, 0.0, symbol, DEPTH_RESET))
        for px, qty in levels:
            if float(qty) == 0.0:
                continue
            out.append(pack_msg(side, float(px), float(qty), symbol))
    return out


def ws_frames(url: str):
    """Connect, yield text payloads, and always close the socket.

    Server frames are not masked. Opcode 9 is a ping: answer with a masked
    pong that copies the payload, then keep reading. Opcode 8 is close:
    answer, log the code, and return so run_live can reconnect. Bytes that
    arrived in the same TCP read as the HTTP 101 headers are kept. Dropping
    them loses the first book update.
    """
    u = urlparse(url)
    host = u.hostname or "data-stream.binance.vision"
    port = u.port or (443 if u.scheme == "wss" else 80)
    path = u.path or "/ws"
    if u.query:
        path += "?" + u.query
    raw = socket.create_connection((host, port), timeout=15)
    if u.scheme == "wss":
        ctx = ssl.create_default_context()
        sock = ctx.wrap_socket(raw, server_hostname=host)
    else:
        sock = raw
    try:
        yield from _ws_after_connect(sock, host, path)
    finally:
        try:
            sock.close()
        except OSError:
            pass


def _ws_after_connect(sock: socket.socket, host: str, path: str):
    key = os.urandom(16).hex()[:24]
    req = (
        f"GET {path} HTTP/1.1\r\n"
        f"Host: {host}\r\n"
        "Upgrade: websocket\r\n"
        "Connection: Upgrade\r\n"
        f"Sec-WebSocket-Key: {key}==\r\n"
        "Sec-WebSocket-Version: 13\r\n"
        "\r\n"
    )
    sock.sendall(req.encode())
    hdr = b""
    while b"\r\n\r\n" not in hdr:
        chunk = sock.recv(4096)
        if not chunk:
            raise RuntimeError("ws handshake closed")
        hdr += chunk
    if b"101" not in hdr.split(b"\r\n", 1)[0]:
        raise RuntimeError("ws handshake failed: " + hdr[:200].decode("latin1", "replace"))
    sock.settimeout(30)
    buf = hdr.split(b"\r\n\r\n", 1)[1]
    while True:
        while True:
            if len(buf) < 2:
                break
            b1, b2 = buf[0], buf[1]
            opcode = b1 & 0x0F
            masked = b2 & 0x80
            ln = b2 & 0x7F
            i = 2
            if ln == 126:
                if len(buf) < 4:
                    break
                ln = int.from_bytes(buf[2:4], "big")
                i = 4
            elif ln == 127:
                if len(buf) < 10:
                    break
                ln = int.from_bytes(buf[2:10], "big")
                i = 10
            if masked:
                i += 4
            if len(buf) < i + ln:
                break
            payload = buf[i : i + ln]
            buf = buf[i + ln :]
            if opcode == 0x8:
                try:
                    sock.sendall(client_frame(0x8, payload[:125]))
                except OSError:
                    pass
                log(f"ws close code={close_code_text(payload)}")
                return
            if opcode == 0x9:
                sock.sendall(client_frame(0xA, payload))
                continue
            if opcode in (0x1, 0x0):
                yield payload.decode("utf-8", "replace")
        data = sock.recv(65536)
        if not data:
            break
        buf += data


def run_live(cfg: dict, sock: socket.socket | None = None) -> None:
    """Stay up. The websocket and the TCP connection to feedd fail separately.

    A dead feedd is redialed. A closed websocket sleeps and tries again.
    The sleep doubles up to 30 seconds so a down exchange is not a tight loop.
    This function does not return on a normal close. That is what stopped
    the old adapter from exiting and leaving the TUI on a frozen book.
    """
    symbols = read_watch(cfg)
    beat(cfg, "starting")
    log(f"adapter starting, pid {os.getpid()}, watching {len(symbols)}")
    delay = 1
    # How often to look at the watch file while frames are arriving. Frames come
    # in every 100 ms per symbol, so this is checked thousands of times a second
    # at fifty symbols; a timestamp keeps it to five stats a second.
    check_every = 0.2
    next_check = time.monotonic() + check_every
    try:
        while True:
            try:
                if sock is None:
                    try:
                        sock = connect_feedd(cfg)
                    except OSError as exc:
                        log(f"feedd error: {exc}; redialing")
                        beat(cfg, "no feedd; redialing")
                        close_quietly(sock)
                        sock = None
                if sock is not None:
                    sink = sock
                    sink_dead = False
                    url = combined_url(cfg, symbols)
                    log(f"watching {len(symbols)}: {' '.join(symbols)}")
                    beat(cfg, f"connected; watching {','.join(symbols)}")
                    for raw in ws_frames(url):
                        delay = 1
                        try:
                            obj = json.loads(raw)
                        except json.JSONDecodeError:
                            continue
                        # The combined payload names its own symbol, so one
                        # connection can carry fifty pairs and each record still
                        # says which one it belongs to.
                        data = obj.get("data", obj)
                        symbol = symbol_of(obj, symbols[0])
                        for msg in parse_depth5(data, symbol):
                            try:
                                send_all(sink, msg)
                            except OSError as exc:
                                log(f"feedd error: {exc}; redialing")
                                close_quietly(sink)
                                sock = None
                                sink_dead = True
                                break
                        if sink_dead:
                            break
                        # Has the terminal asked for a different set? Answered
                        # between frames rather than on a timer thread, because
                        # the frames are the clock and this is where the answer
                        # matters: a reconnect is only safe between frames.
                        now = time.monotonic()
                        if now >= next_check:
                            next_check = now + check_every
                            # A beat per frame would be a write per frame; this is
                            # the same timestamp, so the file says "alive" about
                            # five times a second while frames are arriving, which
                            # is what lets a reader tell a quiet feed from a dead
                            # process within a second.
                            beat(cfg, f"live; watching {len(symbols)}")
                            wanted = read_watch(cfg)
                            if wanted != symbols:
                                log(
                                    f"watch changed: {len(symbols)} -> "
                                    f"{len(wanted)}; reconnecting"
                                )
                                symbols = wanted
                                break
                    else:
                        log("ws closed; reconnecting")
            except Exception as exc:
                log(f"ws error: {type(exc).__name__}: {exc}; reconnecting")
                beat(cfg, f"ws error; retrying in {delay}s")
            # The wait is logged, because a log that shows a failure and then
            # nothing for thirty seconds reads like a hang. The number is the one
            # that matters: a reader can see how long the gap will be.
            if delay > 1:
                log(f"waiting {delay}s before reconnecting")
            time.sleep(delay)
            delay = min(delay * 2, 30)
    finally:
        close_quietly(sock)


def run_fixture(path: str, sock: socket.socket, symbol: str) -> None:
    """Same parse_depth5 path as the live feed, from a file. make e2e uses this.

    A short sleep between lines gives headless time to be scheduled. The
    function returns when the file ends. It does not reconnect.
    """
    with open(path, encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            obj = json.loads(line)
            for msg in parse_depth5(obj, symbol):
                send_all(sock, msg)
            time.sleep(0.01)


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--config", required=True)
    ap.add_argument("--fixture", default="")
    args = ap.parse_args()
    cfg = load_cfg(args.config)
    if cfg.get("env") == "prod" and os.environ.get("ORDERBOOK_ALLOW_PROD") != "1":
        print("prod adapter refused", file=sys.stderr)
        return 2
    if cfg.get("allow_orders"):
        print("adapter refuses allow_orders", file=sys.stderr)
        return 2
    if args.fixture:
        sock = connect_feedd(cfg)
        try:
            run_fixture(args.fixture, sock, cfg.get("symbol", "BTCUSDT"))
            time.sleep(0.2)
        finally:
            close_quietly(sock)
    else:
        # Anything that escapes run_live ends the process, and a process that
        # ended without saying why is what this change is about: the window showed
        # a stalled book and the log showed nothing at all.
        try:
            run_live(cfg)
        except BaseException as exc:
            log(f"fatal: {type(exc).__name__}: {exc}")
            raise
        finally:
            try:
                heartbeat_path(cfg).unlink(missing_ok=True)
            except OSError:
                pass
            log("adapter exiting")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
