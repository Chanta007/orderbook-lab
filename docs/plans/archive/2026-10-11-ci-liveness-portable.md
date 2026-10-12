## PR Metadata
- **Type**: bugfix
- **Research**: docs/research/2026-10-11-mac-run-path.md
- **Research summary**: CI on Ubuntu failed where macOS passed. The stop path decided a zombie by comparing the `ps -o command=` output to the exact string `<defunct>`, which is what macOS prints; procps prints the command with a marker in front of it. The zombie then read as running, the wait loop ran out its grace period, and `stop` kept the pid file. The portable signal is the `ps` state letter, which both platforms report as `Z` for a zombie.
- **Evidence**: `make test` 26 tests OK with no ResourceWarning on macOS; `make e2e` bids=2 asks=2 seq=19; `ps -p PID -o state=` returns `Z` for a killed child on macOS and `proc_running` returns False; two real start/stop cycles leave port 9001 free with no processes and no pid files; Ubuntu CI `test` job pass.
- **Commit**: fix: read the process state, not the ps command text
- **Files changed**: python/obctl.py, python/test_obctl.py, docs/plans/archive/2026-10-11-ci-liveness-portable.md
- **Design docs**: docs/design/core-architecture.md

# HARNESS Plan: Make the stop-path liveness check portable

| Field | Value |
|-------|-------|
| Type | bugfix |
| Status | build-complete |
| Short slug | ci-liveness-portable |
| Created | 2026-10-11 |
| Branch | feature/mac-build-run |
| Design docs | docs/design/core-architecture.md |
| Research | docs/research/2026-10-11-mac-run-path.md |
| Prior plans | 2026-10-11-r2f-guard-fixes.md: introduced the ps-based liveness check this plan makes portable. |

## Context

CI on PR #5 fails on Ubuntu at `test_a_failed_adapter_check_stops_feedd`: a pid file is left behind. The same test passes on macOS.

Cause: `proc_running` decides a zombie by comparing the `ps -o command=` output to the exact string `<defunct>`. macOS prints exactly that. procps on Linux prints the command with the marker, for example `[python3] <defunct>`, so the equality fails, the process reads as running, the wait loop runs out its grace period, and `stop` keeps the pid file.

The portable signal is the state letter. procps and BSD `ps` both report `Z` for a zombie, and `ps -p PID -o state=` is supported by both.

Also fixed here: two tests in `AwaitStartedTest` kill their children without waiting, which prints `ResourceWarning: subprocess N is still running` in CI.

**Research**: docs/research/2026-10-11-mac-run-path.md

## Outcome Boundary

### Done when:
- `make test` exits 0 on macOS and on the Ubuntu runner.
- A zombie is reported as not running on both platforms.
- `make test` prints no `ResourceWarning`.

### Failed when:
- A stop that killed its process still reports it as running.
- The check depends on the localized text of `ps` output.

### Must not:
- Change the 72-byte message, the ring, the WAL, the ports, or the feed subscription.
- Make `make test` depend on the network.

## Steps

### 1. Read the process state, not the command text — `done`
- **File(s):** python/obctl.py
- **Action:** Add `proc_state(pid)` returning the `ps -p PID -o state=` letter, `""` when the pid is gone, `None` when ps cannot be asked. `proc_running` returns False when the state is empty or starts with `Z`, and falls back to the `<defunct>` substring only when the state could not be read. `kill_pid` uses `proc_running` for its first check and keeps its pid file when the state cannot be read.
- **Acceptance:** `grep -c 'def proc_state' python/obctl.py` returns `1`; `grep -c 'startswith("Z")' python/obctl.py` returns `1`.

### 2. Reap the test children — `done`
- **File(s):** python/test_obctl.py
- **Action:** Replace the `proc.kill` cleanups in `AwaitStartedTest` with a helper that kills and waits, so no child is left unreaped and no `ResourceWarning` reaches CI output.
- **Acceptance:** `make test` prints no `ResourceWarning`; `make test` exits 0.

### 3. Re-verify locally and on CI — `done`
- **File(s):** python/obctl.py, python/test_obctl.py
- **Action:** Run `make test` and `make e2e` locally, push, and wait for the Ubuntu CI job to report.
- **Acceptance:** `make test` and `make e2e` exit 0 locally; `gh pr checks 5` reports `test pass`.

## Implementation delta

### Why CI caught what local runs did not
The macOS verification could not see this: the check matched macOS's exact
`<defunct>` spelling. The Ubuntu job runs the same tests on the same Python
version and failed on the first run.

### Fixes
- `proc_state` reads `ps -p PID -o state=` and returns the letter. `proc_running`
  treats an empty state or a `Z` as not running, and falls back to the
  `<defunct>` substring only when the state could not be read.
- `kill_pid` uses `proc_running` for its first check, and keeps the pid file when
  the state cannot be read, the same way it does when the command cannot be read.
- The two `AwaitStartedTest` cleanups kill and wait instead of killing alone, so
  no child is left unreaped and CI no longer prints `ResourceWarning`.
