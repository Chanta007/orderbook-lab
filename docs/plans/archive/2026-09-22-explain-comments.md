## PR Metadata
- **Type**: docs
- **Research**: none. Comments only, written so a person can walk through each file out loud.
- **Research summary**: No behavior change. Each lab source file now has a walkthrough of what to say, plus a note on the functions that are easy to mis-explain.
- **Evidence**: make test ok
- **Commit**: docs: walkthrough comments for explaining the lab
- **Files changed**: include/ob, src, python/feed_adapter.py, python/obctl.py, tests/test_core.cpp, Makefile, CMakeLists.txt
- **Design docs**: README.md

# HARNESS Plan: Explain-the-code comments

| Field | Value |
|-------|-------|
| Type | docs |
| Status | build-complete |
| Short slug | explain-comments |
| Created | 2026-09-22 |
| Branch | feature/tui-red-bids |

## Steps

### 1. Walkthrough comments — `done`
- **File(s):** lab sources listed above
- **Action:** Add a spoken walkthrough at the top of each file and a note before each function whose reason is easy to get wrong. Do not change control flow. JSON configs cannot hold comments.
- **Acceptance:** make test exits 0.
