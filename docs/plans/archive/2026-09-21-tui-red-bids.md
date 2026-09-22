## PR Metadata
- **Type**: bugfix
- **Research**: docs/research/2026-09-21-tui-red-bids.md
- **Research summary**: The live book froze because the adapter exited. It answered Binance pings with an unmasked empty pong, which RFC 6455 says the server must close, and it did not reconnect. Bid prices are red by operator request. Command help is drawn inside the frame so the screen clear cannot erase it.
- **Evidence**: make test (test_core ok, 2 frame tests ok); make e2e exit 0 (bids=6 asks=6 seq=13)
- **Commit**: fix: keep the live book moving and paint bids red
- **Files changed**: python/feed_adapter.py, python/test_ws_frames.py, src/tui.cpp, Makefile, README.md, docs/research/2026-09-21-tui-red-bids.md
- **Design docs**: docs/design/core-architecture.md, README.md

# HARNESS Plan: Red bids, on-screen help, live book

| Field | Value |
|-------|-------|
| Type | bugfix |
| Status | build-complete |
| Short slug | tui-red-bids |
| Created | 2026-09-21 |
| Branch | feature/tui-red-bids |
| Design docs | docs/design/core-architecture.md, docs/research/2026-09-21-tui-red-bids.md, README.md |
| Prior plans | 2026-09-21-orderbook-lab-core.md: ANSI TUI and a stdlib adapter, both sides green |

Selected option: keep the stdlib adapter, mask pongs, reconnect, paint help inside the frame.
Research: docs/research/2026-09-21-tui-red-bids.md

**Codegen learnings**: none
**Auto-loaded research**: docs/research/2026-09-21-tui-red-bids.md — mask client frames, copy the ping payload, reconnect, red bids, help inside the frame.
**Second-brain route**: none

## Outcome Boundary

### Done when:
- Bid prices on the TUI are red and ask prices are green.
- The command list stays in the picture: quit, levels N, help, and that Enter submits a command.
- The picture says the feed is stalled when no new message has arrived, and the adapter reconnects after a dropped socket instead of exiting.
- `make test` and `make e2e` exit 0 with no new pip package.

### Failed when:
- Both sides are still green, or bids are green and asks are red.
- Help appears only after typing `help` and then disappears on the next redraw.
- The adapter process still exits on a websocket close or a 30 second timeout, leaving the book frozen with no stalled line.
- The fixture e2e now needs the network or a pip install.

### Must not:
- Add a third-party websocket package.
- Send orders, or set `allow_orders` true in dev.
- Change the mmap message layout.

## Steps

### 1. Mask pongs and reconnect the adapter — `done`
- **File(s):** python/feed_adapter.py
- **Short slug:** mask-and-reconnect
- **Action:** Send every client frame masked. A pong copies the ping payload. On close, timeout, or error, log to stderr and reconnect with backoff capped at 30 seconds. Do not exit the process.
- **Acceptance:** A unit test decodes a pong and gets the ping payload back. The mask bit is set. `python3 -m py_compile` succeeds.

### 2. Frame unit test on the existing test target — `done`
- **File(s):** python/test_ws_frames.py, Makefile
- **Short slug:** frame-unit-test
- **Action:** Add a stdlib unittest for the masked pong. Run it from `make test` after `test_core`. No network.
- **Acceptance:** `make test` runs the C++ test and the unittest, and both exit 0.

### 3. Red bids, green asks, help, stalled — `done`
- **File(s):** src/tui.cpp
- **Short slug:** tui-frame
- **Action:** Include `<chrono>`. Paint bid prices red (`\033[31m`) and ask prices green (`\033[32m`). Draw the command explanation inside the frame every refresh. Show `live` or `stalled` from the age of the last applied message (stalled at 2 seconds).
- **Acceptance:** The built `tui` binary contains both color sequences. The frame text names `quit`, `levels N`, and `help`.

### 4. README matches the frame — `done`
- **File(s):** README.md
- **Short slug:** readme-colors
- **Action:** Replace "bids and asks are green" with red bids, green asks, and the same three commands.
- **Acceptance:** README no longer says both sides are green.

### 5. Fixture e2e still passes — `done`
- **File(s):** Makefile
- **Short slug:** run-e2e
- **Action:** Run `make e2e`. Do not point it at Binance.
- **Acceptance:** `make e2e` exits 0.

## Implementation delta

No Fix sub-steps recorded in this plan.
