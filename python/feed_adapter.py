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
from urllib.parse import urlparse

MAGIC = 0x4F424C42
MSG_FMT = "<IHBBQQ16s8sqq8s"
MSG_SIZE = struct.calcsize(MSG_FMT)
assert MSG_SIZE == 72


def load_cfg(path: str) -> dict:
    with open(path, encoding="utf-8") as f:
        return json.load(f)


DEPTH = 1
DEPTH_RESET = 4


def pack_msg(side: int, px: float, qty: float, symbol: str, msg_type: int = DEPTH) -> bytes:
    """Float prices become e8 integers here. seq and trace id stay zero.

    feedd fills the trace id and the timestamp. The ring fills seq when it
    publishes. Do not invent those fields in Python.
    """
    sym = symbol.encode("ascii")[:8].ljust(8, b"\0")
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
    print(msg, file=sys.stderr, flush=True)


def connect_feedd(cfg: dict) -> socket.socket:
    host = cfg.get("listen_host", "127.0.0.1")
    port = int(cfg.get("listen_port", 9001))
    return socket.create_connection((host, port), timeout=5)


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
    url = cfg.get("ws_url") or "wss://data-stream.binance.vision:443/ws/btcusdt@depth5@100ms"
    symbol = cfg.get("symbol", "BTCUSDT")
    delay = 1
    try:
        while True:
            try:
                if sock is None:
                    try:
                        sock = connect_feedd(cfg)
                    except OSError as exc:
                        log(f"feedd error: {exc}; redialing")
                        close_quietly(sock)
                        sock = None
                if sock is not None:
                    sink = sock
                    sink_dead = False
                    for raw in ws_frames(url):
                        delay = 1
                        try:
                            obj = json.loads(raw)
                        except json.JSONDecodeError:
                            continue
                        data = obj.get("data", obj)
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
                    else:
                        log("ws closed; reconnecting")
            except Exception as exc:
                log(f"ws error: {exc}; reconnecting")
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
        run_live(cfg)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
