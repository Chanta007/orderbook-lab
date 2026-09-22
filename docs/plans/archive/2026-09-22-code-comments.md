## PR Metadata
- **Type**: docs
- **Research**: none. Comments only. No behavior change.
- **Research summary**: File headers and a few lines where the reason is not obvious: ring overrun, WAL sync, DepthReset, prod guard, TUI cursor, masked pong.
- **Evidence**: make test ok (test_core and 8 Python tests)
- **Commit**: docs: explain the non-obvious parts of the lab
- **Files changed**: include/ob/*.hpp, src/*.cpp, python/feed_adapter.py, python/obctl.py, python/test_ws_frames.py, tests/test_core.cpp, Makefile, CMakeLists.txt
- **Design docs**: README.md, docs/design/core-architecture.md

# HARNESS Plan: Code comments

| Field | Value |
|-------|-------|
| Type | docs |
| Status | build-complete |
| Short slug | code-comments |
| Created | 2026-09-22 |
| Branch | feature/tui-red-bids |

## Steps

### 1. Comment only where the reason is not the code — `done`
- **File(s):** headers, src, python, Makefile, CMakeLists.txt
- **Action:** Add a short note on the fixed 72-byte message, the ring lap, the WAL fsync, the prod guard, the TUI cursor, and the depth5 reset. Do not restate obvious lines. JSON configs cannot hold comments.
- **Acceptance:** make test exits 0. No control flow changed.
