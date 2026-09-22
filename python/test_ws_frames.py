"""Stdlib checks for the masked client pong. No network."""
from __future__ import annotations

import unittest

import feed_adapter


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


if __name__ == "__main__":
    unittest.main()
