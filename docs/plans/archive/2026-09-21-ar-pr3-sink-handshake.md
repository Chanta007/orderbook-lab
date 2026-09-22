## PR Metadata
- **Type**: bugfix
- **Research**: docs/research/2026-09-21-tui-red-bids.md
- **Research summary**: Address-review on PR #3. `except OSError` wrapped `ws_frames` so Binance stalls looked like a dead feedd sink; HTTP 101 leftover bytes were dropped; the redial test could hang.
- **Evidence**: make test (test_core ok, 7 Python tests ok)
- **Commit**: fix: keep feedd sink on WS OSError, parse 101 remainder
- **Files changed**: python/feed_adapter.py, python/test_ws_frames.py, docs/telemetry/runs-2026-09.jsonl
- **Design docs**: docs/design/core-architecture.md, RFC 6455 §4.1

# HARNESS Plan: Address-review PR 3 WS sink vs handshake

| Field | Value |
|-------|-------|
| Type | bugfix |
| Status | build-complete |
| Short slug | ar-pr3-sink-handshake |
| Created | 2026-09-21 |
| Branch | feature/tui-red-bids |
| Design docs | docs/design/core-architecture.md, docs/research/2026-09-21-tui-red-bids.md, RFC 6455 §4.1 |
| Prior plans | 2026-09-21-address-review-pr3.md: redial feedd on OSError, but the catch wrapped ws_frames so Binance TLS/timeout looked like a dead sink |

Provenance: `/mindcoachlabs:address-review` on PR #3, iteration 1/1, `--auto`, `--severity-floor info`.

**Codegen learnings**: none
**Auto-loaded research**: docs/research/2026-09-21-tui-red-bids.md — keep the stdlib adapter, mask pongs, reconnect, red bids, help inside the frame.
**Second-brain route**: none

## Outcome Boundary

### Done when:
- A Binance stall or TLS error (`TimeoutError` / `ssl.SSLError`) logs `ws error` and keeps a healthy `feedd` TCP sink.
- A `BrokenPipeError` from `send_all` still logs `feedd error`, closes the sink, and redials.
- Bytes after the HTTP 101 terminator in the same `recv` are parsed as WebSocket frames (coalesced ping gets a masked pong).
- `test_send_failure_redials_feedd` cannot hang `make test` if the redial assignment is removed.

### Failed when:
- `except OSError` around `ws_frames` still treats a WS timeout as `feedd error` and sets `sock = None`.
- `_ws_after_connect` still starts the frame loop with `buf = b""` after a coalesced 101+ping recv.
- The redial test still patches `time.sleep` to a no-op with no bound, so a missing `sock = None` spins forever.

### Must not:
- Add a third-party websocket package.
- Change the mmap message layout or send orders.
- Drop the existing ping/close unit tests.

## Steps

### 1. Catch OSError only around connect_feedd and send_all — `done`
- **File(s):** python/feed_adapter.py
- **Short slug:** narrow-oserror
- **Action:** In `run_live`, catch `OSError` only around `connect_feedd` and `send_all`. On those paths log `feedd error`, `close_quietly`, set `sock = None`. Let `OSError` from `ws_frames` / `_ws_after_connect` (TimeoutError, ConnectionResetError, ssl.SSLError) fall through to `except Exception` as `ws error` without dropping a healthy sink. Keep the existing backoff sleep.
- **Acceptance:** A unit test that raises `TimeoutError` from `ws_frames` does not call `connect_feedd` a second time. A `BrokenPipeError` from `send_all` still redials. `python3 -m py_compile python/feed_adapter.py` succeeds.

### 2. Keep handshake remainder as the WS buffer — `done`
- **File(s):** python/feed_adapter.py
- **Short slug:** handshake-remainder
- **Action:** After the 101 check in `_ws_after_connect`, set `buf = hdr.split(b"\r\n\r\n", 1)[1]` instead of `buf = b""` so bytes after the HTTP terminator in the same `recv` are the start of the WebSocket stream (RFC 6455 §4.1: once the connection is established, data frames follow on the same TCP stream).
- **Acceptance:** A stub socket that returns HTTP 101 concatenated with an unmasked ping in one `recv` still sends a masked pong copying the ping payload.

#### Fix 2a — drain leftover frames before next recv (MINOR, auto-fixed)
- **Trigger:** `test_handshake_keeps_bytes_after_101` failed: `len(sock.sent) == 1` because the frame loop recvd EOF before parsing `buf`
- **Action:** Parse complete frames from `buf` first; only `recv` when the buffer is incomplete
- **Files changed:** python/feed_adapter.py (`_ws_after_connect`)

### 3. Bound the redial test and cover coalesced handshake plus WS OSError — `done`
- **File(s):** python/test_ws_frames.py
- **Short slug:** bound-redial-tests
- **Action:** Patch `time.sleep` with a `side_effect` that raises after a small N so a missing redial is `assertRaises(StopTest)` failure (or a distinct RuntimeError), not a hung `make test`. Add `test_handshake_keeps_bytes_after_101` (coalesced 101+ping). Add `test_ws_oserror_keeps_feedd_sink` (TimeoutError from `ws_frames` must not increment `connect_feedd` calls across a second loop iteration).
- **Acceptance:** `python3 -m unittest discover -s python -p 'test_*.py'` exits 0. Replacing the remainder assignment with `buf = b""` fails the coalesced test. Removing `sock = None` after send failure fails the redial test instead of hanging.

#### Fix 3a — mypy sink narrowing (MINOR, auto-fixed)
- **Trigger:** mypy arg-type on `send_all(sock, msg)` after `sock` is annotated `socket | None`
- **Action:** Bind a local `sink` after the `if sock is not None` guard and pass that to `send_all` / `close_quietly`
- **Files changed:** python/feed_adapter.py (`run_live`)

## DOCS TO UPDATE
- n/a — recovery-path bugfix; core-architecture process diagram unchanged

## Implementation delta

- Fix 2a — drain leftover frames before next recv (MINOR, auto-fixed): `test_handshake_keeps_bytes_after_101` failed: `len(sock.sent) == 1` because the frame loop recvd EOF before parsing `buf`
- Fix 3a — mypy sink narrowing (MINOR, auto-fixed): mypy arg-type on `send_all(sock, msg)` after `sock` is annotated `socket | None`
