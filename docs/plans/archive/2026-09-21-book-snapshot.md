## PR Metadata
- **Type**: bugfix
- **Research**: docs/research/2026-09-21-book-snapshot.md
- **Research summary**: depth5 is a fresh top-of-book picture. Keeping old prices left bids above asks. Clear each side on the picture, and do not round a real size to 0.00.
- **Evidence**: make test ok; make e2e bids=2 asks=2, which is the last fixture picture
- **Commit**: fix: replace the top of book on each Binance picture
- **Files changed**: include/ob/msg.hpp, include/ob/book.hpp, python/feed_adapter.py, python/test_ws_frames.py, src/tui.cpp, tests/test_core.cpp, README.md, docs/research/2026-09-21-book-snapshot.md
- **Design docs**: docs/design/core-architecture.md, README.md

# HARNESS Plan: Replace stale book levels

| Field | Value |
|-------|-------|
| Type | bugfix |
| Status | build-complete |
| Short slug | book-snapshot |
| Created | 2026-09-21 |
| Branch | feature/tui-red-bids |
| Design docs | docs/design/core-architecture.md, README.md |
| Prior plans | 2026-09-21-tui-red-bids.md: red bids and reconnect; the book still kept old prices |

## Outcome Boundary

### Done when:
- A later top-of-book picture drops prices that are no longer in it, so the best bid is below the best ask.
- A non-zero size does not print as 0.00.
- `make test` and `make e2e` exit 0.

### Failed when:
- Every depth message clears the book, so the single-price unit test fails.
- The window still shows a bid above the ask after a fresh picture.

### Must not:
- Change the 72-byte message size.
- Subscribe to a different Binance stream.

## Steps

### 1. Clear a side on DepthReset — `done`
- **File(s):** include/ob/msg.hpp, include/ob/book.hpp, tests/test_core.cpp
- **Action:** Add DepthReset. apply clears that side and does not touch the other side or last_seq.
- **Acceptance:** test_core keeps one bid and one ask after a reset-and-replace, and the bid is below the ask.

### 2. Adapter emits the reset — `done`
- **File(s):** python/feed_adapter.py, python/test_ws_frames.py
- **Action:** A picture with a bids or asks array starts that side with DepthReset, then the non-zero prices.
- **Acceptance:** The unittest sees reset then depth for each side.

### 3. Size formatting — `done`
- **File(s):** src/tui.cpp, README.md
- **Action:** Print quantity with up to 8 decimals and strip trailing zeros. Say in the README that each picture replaces the previous prices.
- **Acceptance:** fmt_qty is what the rows use. README no longer implies old prices stay.

### 4. Fixture e2e — `done`
- **File(s):** Makefile
- **Action:** Run make e2e.
- **Acceptance:** Exit 0, and the book matches the last fixture picture (2 bids, 2 asks).
