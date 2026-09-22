"""Stdlib checks for masked client frames and the live retry path. No network.

A client frame must have the mask bit set, and a pong must carry the ping
payload. A depth5 picture must start each side with DepthReset.
"""
from __future__ import annotations

import json
import unittest
from unittest import mock

import feed_adapter


HTTP_101 = b"HTTP/1.1 101 Switching Protocols\r\n\r\n"


def server_frame(opcode: int, payload: bytes) -> bytes:
    """Unmasked server frame (RFC 6455 §5.3: servers MUST NOT mask)."""
    head = bytearray([0x80 | (opcode & 0x0F)])
    ln = len(payload)
    if ln < 126:
        head.append(ln)
    elif ln < 65536:
        head.append(126)
        head += ln.to_bytes(2, "big")
    else:
        head.append(127)
        head += ln.to_bytes(8, "big")
    return bytes(head) + payload


class StubSocket:
    def __init__(self, chunks: list[bytes]) -> None:
        self._chunks = list(chunks)
        self.sent: list[bytes] = []

    def sendall(self, data: bytes) -> None:
        self.sent.append(bytes(data))

    def recv(self, n: int) -> bytes:
        if not self._chunks:
            return b""
        chunk = self._chunks.pop(0)
        return chunk[:n]

    def settimeout(self, _t: float) -> None:
        return None


def decode_client_frame(frame: bytes) -> tuple[int, bytes]:
    b1, b2 = frame[0], frame[1]
    if b2 & 0x80 == 0:
        raise AssertionError("client frame is not masked")
    opcode = b1 & 0x0F
    ln = b2 & 0x7F
    i = 2
    if ln == 126:
        ln = int.from_bytes(frame[2:4], "big")
        i = 4
    elif ln == 127:
        ln = int.from_bytes(frame[2:10], "big")
        i = 10
    mask = frame[i : i + 4]
    i += 4
    data = bytes(b ^ mask[n % 4] for n, b in enumerate(frame[i : i + ln]))
    return opcode, data


class SnapshotTest(unittest.TestCase):
    def test_each_side_starts_with_a_reset(self) -> None:
        raw = feed_adapter.parse_depth5(
            {"bids": [["100", "1"]], "asks": [["101", "2"]]},
            "BTCUSDT",
        )
        kinds = [(frame[6], frame[7]) for frame in raw]
        self.assertEqual(kinds[0], (feed_adapter.DEPTH_RESET, 0))
        self.assertEqual(kinds[1][0], feed_adapter.DEPTH)
        self.assertEqual(kinds[2], (feed_adapter.DEPTH_RESET, 1))
        self.assertEqual(kinds[3][0], feed_adapter.DEPTH)


class ClientFrameTest(unittest.TestCase):
    def test_pong_copies_ping_payload_and_is_masked(self) -> None:
        payload = b"\x01\x02ping-body"
        frame = feed_adapter.client_frame(0xA, payload)
        self.assertNotEqual(frame, bytes([0x8A, 0x00]))
        opcode, data = decode_client_frame(frame)
        self.assertEqual(opcode, 0xA)
        self.assertEqual(data, payload)

    def test_empty_pong_is_still_masked(self) -> None:
        frame = feed_adapter.client_frame(0xA, b"")
        self.assertEqual(frame[0] & 0x0F, 0xA)
        self.assertNotEqual(frame[1] & 0x80, 0)
        opcode, data = decode_client_frame(frame)
        self.assertEqual(opcode, 0xA)
        self.assertEqual(data, b"")


class AfterConnectTest(unittest.TestCase):
    def test_ping_path_sends_masked_pong_with_payload(self) -> None:
        payload = b"binance-ping"
        sock = StubSocket([HTTP_101, server_frame(0x9, payload)])
        list(feed_adapter._ws_after_connect(sock, "host", "/ws"))
        self.assertGreaterEqual(len(sock.sent), 2)
        pong = sock.sent[1]
        self.assertNotEqual(pong, bytes([0x8A, 0x00]))
        opcode, data = decode_client_frame(pong)
        self.assertEqual(opcode, 0xA)
        self.assertEqual(data, payload)

    def test_close_path_replies_with_masked_close_and_code(self) -> None:
        payload = (1000).to_bytes(2, "big") + b"bye"
        sock = StubSocket([HTTP_101, server_frame(0x8, payload)])
        list(feed_adapter._ws_after_connect(sock, "host", "/ws"))
        self.assertGreaterEqual(len(sock.sent), 2)
        close = sock.sent[1]
        opcode, data = decode_client_frame(close)
        self.assertEqual(opcode, 0x8)
        self.assertEqual(data[:2], payload[:2])

    def test_handshake_keeps_bytes_after_101(self) -> None:
        payload = b"coalesced-ping"
        sock = StubSocket([HTTP_101 + server_frame(0x9, payload)])
        list(feed_adapter._ws_after_connect(sock, "host", "/ws"))
        self.assertGreaterEqual(len(sock.sent), 2)
        opcode, data = decode_client_frame(sock.sent[1])
        self.assertEqual(opcode, 0xA)
        self.assertEqual(data, payload)


class StopTest(BaseException):
    """Breaks run_live's retry loop without being swallowed as ws error."""


class RedialFeeddTest(unittest.TestCase):
    def test_send_failure_redials_feedd(self) -> None:
        dials: list[object] = []

        class FailThenStop:
            def sendall(self, _data: bytes) -> None:
                raise BrokenPipeError("feedd gone")

            def close(self) -> None:
                return None

        def fake_connect(_cfg: dict) -> FailThenStop:
            if len(dials) >= 1:
                raise StopTest("redialed")
            sock = FailThenStop()
            dials.append(sock)
            return sock

        def fake_ws(_url: str):
            yield json.dumps({"bids": [["1", "1"]], "asks": []})

        cfg = {"listen_host": "127.0.0.1", "listen_port": 9, "symbol": "BTCUSDT", "ws_url": "ws://x"}
        with mock.patch.object(feed_adapter, "connect_feedd", fake_connect), mock.patch.object(
            feed_adapter, "ws_frames", fake_ws
        ), mock.patch.object(feed_adapter.time, "sleep") as sleep:
            sleep.side_effect = [None] * 8 + [RuntimeError("run_live did not redial")]
            with self.assertRaises(StopTest):
                feed_adapter.run_live(cfg)
        self.assertEqual(len(dials), 1)

    def test_ws_oserror_keeps_feedd_sink(self) -> None:
        dials: list[object] = []

        class KeepSink:
            def sendall(self, _data: bytes) -> None:
                return None

            def close(self) -> None:
                return None

        def fake_connect(_cfg: dict) -> KeepSink:
            sock = KeepSink()
            dials.append(sock)
            return sock

        def fake_ws(_url: str):
            raise TimeoutError("binance stall")
            yield  # generator so ws_frames is iterated

        cfg = {"listen_host": "127.0.0.1", "listen_port": 9, "symbol": "BTCUSDT", "ws_url": "ws://x"}
        with mock.patch.object(feed_adapter, "connect_feedd", fake_connect), mock.patch.object(
            feed_adapter, "ws_frames", fake_ws
        ), mock.patch.object(feed_adapter.time, "sleep") as sleep:
            sleep.side_effect = [None, StopTest("bounded")]
            with self.assertRaises(StopTest):
                feed_adapter.run_live(cfg)
        self.assertEqual(len(dials), 1)


if __name__ == "__main__":
    unittest.main()
