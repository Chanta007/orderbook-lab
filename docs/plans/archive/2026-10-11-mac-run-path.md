## PR Metadata
- **Type**: bugfix
- **Research**: docs/research/2026-10-11-mac-run-path.md
- **Research summary**: A clean worktree off dev already compiles and runs on this Mac: make all with no warnings, make test exit 0, make e2e exit 0, a live run filling five levels a side, and build/tui drawing feed=live through a pty. The failures are in recovery. A second `start` overwrote the pid files of the instance already running, so the first feedd and adapter ran with nothing pointing at them and port 9001 stayed bound; `stop` then printed `stop ok` and could not clear it. `stop` also signalled whatever pid the file named, so a recycled pid meant killing an unrelated process. `start` now refuses an ambiguous state and `stop` proves what it did.
- **Evidence**: make clean && make all - 4 binaries, no warnings. make test - test_core ok plus 19 Python tests OK. make e2e - bids=2 asks=2 seq=19. Live headless - bids=5 asks=5 seq=1103. pty window - 38 frames, feed=live, `levels 3` honoured, `quit` exited. Second start - refuses with the pid, exit 1. Stray feedd holding the port - refuses with the address, exit 1. Unrelated live pid in the pid file - left alone, process survived. Prod without ORDERBOOK_ALLOW_PROD=1 - refused. After start twice then stop - port free, no processes, no pid files.
- **Commit**: fix: refuse a second start, and prove what stop signalled
- **Files changed**: .gitignore, Makefile, README.md, python/obctl.py, python/test_obctl.py, docs/runbooks/port-already-in-use.md, docs/research/2026-10-11-mac-run-path.md
- **Design docs**: docs/design/core-architecture.md, README.md

# HARNESS Plan: Make the compile-and-run path hold on macOS

| Field | Value |
|-------|-------|
| Type | bugfix |
| Status | build-complete |
| Short slug | mac-run-path |
| Created | 2026-10-11 |
| Branch | feature/mac-build-run |
| Design docs | docs/design/core-architecture.md, README.md |
| Research | docs/research/2026-10-11-mac-run-path.md |
| Prior plans | 2026-09-21-book-snapshot.md: the same run path was fixed once before, for a stale book rather than stale process state. 2026-09-21-orderbook-lab-core.md: built the processes and the ports this plan guards. |

## Context

A clean worktree off `dev` already builds and runs on this Mac: `make all` with no warnings, `make test` exit 0, `make e2e` exit 0, a live run filling five levels a side, and `build/tui` drawing `feed=live` through a pty. What does not hold is recovery. `start` run twice overwrites its own pid files with dead pids, so the first feedd and adapter run forever with nothing pointing at them and port 9001 stays bound; `stop` then reports `stop ok` and cannot clear it. `stop` also signals whatever pid is in the pid file, even when that pid now belongs to an unrelated process.

**Research**: docs/research/2026-10-11-mac-run-path.md
**Auto-loaded research**: docs/research/2026-09-21-book-snapshot.md — depth5 is a full top-of-book picture, so each side is cleared before its prices are inserted; unrelated to process state, listed for completeness.

## Outcome Boundary

### Done when:
- `make test` and `make e2e` exit 0 on this Mac from a clean tree.
- `obctl.py start` while an instance is already running refuses, names the pid or the address, and leaves the running instance untouched.
- After any sequence of `start` and `stop`, port 9001 is free and no lab process is left running.
- `make start` opens the live window on Binance data and `quit` closes it.

### Failed when:
- A second `start` still leaves a feedd or adapter with no pid file pointing at it.
- `stop` reports `stop ok` while a lab process is still running.
- `stop` signals a process that is not part of this lab.
- The fix changes the 72-byte message, the ring, the WAL, the ports, or the feed subscription.

### Must not:
- Kill a process the lab did not start.
- Change `config/dev.json` or `config/prod.json`.
- Make `make test` depend on the network.

## Steps

### 1. Refuse `start` when an instance is already running — `done`
- **File(s):** python/obctl.py
- **Action:** Add `pid_alive`, `proc_command`, `owns`, `tracked`, and `port_in_use` helpers. `start` calls a new `preflight` before spawning: a live tracked pid for `feedd` or `adapter` raises SystemExit naming the pid and `obctl stop`; a bound `listen_host:listen_port` raises SystemExit naming the address and both ways out. The probe socket sets `SO_REUSEADDR`, the same option `src/feedd.cpp` sets, so the probe sees what feedd will see.
- **Acceptance:** `cd python && python3 -c "import obctl, tempfile, pathlib; obctl.ROOT = pathlib.Path(tempfile.mkdtemp()); obctl.preflight({'listen_host':'127.0.0.1','listen_port':0}, 'x')"` exits 0; a second call with a live tracked pid in that temp ROOT raises SystemExit.

### 2. Confirm the child started, or report its last log lines — `done`
- **File(s):** python/obctl.py
- **Action:** Add `log_offset`, `new_log_lines`, `read_pid`, and `await_started(name, offset, marker, ceiling)`. `start` records the log size before each spawn, then waits for feedd's `feedd listen` line (ceiling 2.0 s) or the adapter's continued liveness (ceiling 0.4 s). A child that died is reported with its own last five log lines and its pid file is removed.
- **Acceptance:** `grep -c 'def await_started' python/obctl.py` returns `1`; a feedd started while the port is held prints `bind failed` and `obctl.py start` exits non-zero with `did not start` in the output.

### 3. Make `stop` prove the kill and never signal a stranger — `done`
- **File(s):** python/obctl.py
- **Action:** `kill_pid` becomes `kill_pid(name) -> int | None`: it reads the pid, removes the pid file, returns None when the process is already gone, refuses to signal when `ps -p PID -o command=` does not still contain this lab's program path, and otherwise sends SIGTERM and waits up to 2 s for the process to exit, returning the pid when it did not. `stop` collects the survivors and the skips, prints them, and exits non-zero when anything it owns is still alive.
- **Acceptance:** `grep -c 'def kill_pid' python/obctl.py` returns `1`; `grep -c 'PROC_MARKERS' python/obctl.py` is at least `2`.

### 4. Regression tests for the two leaks — `done`
- **File(s):** python/test_obctl.py
- **Action:** Stdlib `unittest`, importing `obctl` the way `python/test_ws_frames.py` imports `feed_adapter`. Cover: a stale pid file is removed and nothing is signalled; a pid file pointing at an unrelated live process is left alone and that process survives; a tracked live pid makes `preflight` refuse; a bound port makes `preflight` refuse; a clean state passes; a process that ignores SIGTERM is reported by `kill_pid` rather than reported as stopped. Tests patch `obctl.ROOT` to a temporary directory so the repo's own `var/run` is never touched.
- **Acceptance:** `python3 -m unittest discover -s python -p 'test_obctl.py'` exits 0 with at least 6 tests run.

### 5. `make clean` removes prod state, and the test comment tells the truth — `done`
- **File(s):** Makefile
- **Action:** Add `var/prod` to the `clean` target. Replace the `Neither opens a socket.` line in the header comment, which the new loopback probe would make false, with a line saying the tests open no network connection.
- **Acceptance:** `make clean` leaves `var` absent or empty; `grep -c 'var/prod' Makefile` returns at least `1`; `grep -c 'opens a socket' Makefile` returns `0`.

### 6. Document the run path and the recovery — `done`
- **File(s):** README.md, docs/runbooks/port-already-in-use.md
- **Action:** In the README `Run it` section, state that `start` refuses when an instance is already running and that `stop` waits and reports. Add a runbook for `bind failed` / port 9001 already in use: what the message means, `obctl stop`, and the `lsof` command to find a feedd with no pid file.
- **Acceptance:** `grep -c 'port-already-in-use' README.md` returns at least `1`; `docs/runbooks/port-already-in-use.md` exists and contains `lsof -nP -iTCP:9001`.

### 7. Verify the whole path on this Mac — `done`
- **File(s):** Makefile, python/obctl.py, python/test_obctl.py, README.md, docs/runbooks/port-already-in-use.md
- **Action:** From a clean tree: `make clean && make all`, `make test`, `make e2e`, then the double-start sequence and the `make e2e`-while-live sequence, checking after each that port 9001 is free and no lab process remains. Then the live window through a pty against Binance, with `levels 3` and `quit`.
- **Acceptance:** `make test` and `make e2e` both exit 0; after `start` twice then `stop`, `lsof -nP -iTCP:9001 -sTCP:LISTEN` prints nothing; the pty run shows `feed=live` and exits on `quit`.

### 8. Keep bytecode out of git — `done`
- **File(s):** .gitignore
- **Action:** Added during verification. Running `make test` writes `python/__pycache__/`, which `.gitignore` does not cover even though it already covers `build/`, `var/`, `.venv/`, and `*.o`. Add `__pycache__/` and `*.pyc` so a test run does not leave the tree looking dirty.
- **Acceptance:** `git status --porcelain` prints no `__pycache__` line after `make test`.

## Deferred (NOT in this plan)

- Cross-worktree port message: when the port is held by a feedd from another checkout, the refusal names the address but not the other worktree. Revisit when two worktrees are worked at once.
- `obctl status` command printing each tracked pid, liveness, and the port holder. Revisit when a third "why is my window stalled" question shows up.

## Implementation delta

### Added during verification
- Step 8: `.gitignore` gained `__pycache__/` and `*.pyc`. Running `make test` wrote
  `python/__pycache__/`, which the file did not cover even though it already covered
  `build/`, `var/`, `.venv/`, and `*.o`.
- `.gitignore` also gained `docs/deferred/.index.lock`. The deferred-index flock opens
  that sidecar and never unlinks it, so the first reconcile leaves an untracked file in
  every worktree.

### Changed after internal review
- `spawn` no longer returns the pid; both call sites ignored it.
- `kill_pid` keeps the pid file when a process survives SIGTERM, so the next `stop` and
  the next `preflight` can still name it. It removes the file only once the process is
  gone, was already gone, or turned out not to be this lab's.
- `read_pid` rejects zero and negative pids. `os.kill` treats 0 and -1 as a process
  group, so a corrupt pid file must not reach the signal path.
- Tests: 10 in `python/test_obctl.py`, 19 with the existing frame tests.

### Review path
The isolated `claude -p` review subprocess failed to authenticate (OAuth session
expired), so the same-family fallback review ran instead. It produced the four changes
listed above.
