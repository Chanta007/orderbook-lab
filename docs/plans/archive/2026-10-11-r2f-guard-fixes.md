## PR Metadata
- **Type**: bugfix
- **Research**: docs/research/2026-10-11-mac-run-path.md
- **Research summary**: The internal review of PR #5 found that the spawn check shipped by the first commit did not work. `spawn` discarded the `Popen`, a child that exits during the wait becomes an unreaped zombie, and `os.kill(pid, 0)` succeeds for a zombie, so a feedd that died at its bind was reported as started and left a pid file pointing at nothing. Three WARN findings and two NITs from the same review are fixed here too.
- **Evidence**: Reproduced before the fix at unit level (a child that writes `bind failed` and exits is reported as started, pid file left behind) and end to end (`start ok` exit 0 with `bind failed` in feedd's log and a dead `var/run/feedd.pid`). After the fix: the real `build/feedd` spawned while the port is held is reported with its own `bind failed` line and leaves no pid file; `make test` 26 tests OK; `make e2e` bids=2 asks=2 seq=19; live window 39 frames feed=live seq=995, `levels 3` honoured, `quit` exited; after every sequence port 9001 free, no processes, no pid files.
- **Commit**: fix: watch the child, not the pid, when a start is checked
- **Files changed**: python/obctl.py, python/feed_adapter.py, python/test_obctl.py, .gitignore, docs/deferred/index.md, docs/plans/archive/2026-10-11-r2f-guard-fixes.md
- **Design docs**: docs/design/core-architecture.md, README.md

# HARNESS Plan: Fix the review findings on the start and stop guards

| Field | Value |
|-------|-------|
| Type | bugfix |
| Status | build-complete |
| Short slug | r2f-guard-fixes |
| Created | 2026-10-11 |
| Branch | feature/mac-build-run |
| Design docs | docs/design/core-architecture.md, README.md |
| Research | docs/research/2026-10-11-mac-run-path.md |
| Prior plans | 2026-10-11-mac-run-path.md: added the guards this plan corrects; its spawn check does not work against a child that dies at once. |

## Context

The internal review of PR #5 found a CRITICAL defect in the change that PR shipped. `spawn` discards the `Popen`, so a child that exits during the wait becomes an unreaped zombie, and `os.kill(pid, 0)` succeeds for a zombie. `await_started` therefore reads a dead feedd as alive, returns success, and leaves a pid file pointing at nothing.

Reproduced twice on this machine:

- Unit level: `spawn` a child that writes `bind failed` and exits, then call `await_started` with no intervening `subprocess` call. It returns instead of raising, and the pid file is left behind.
- End to end through the CLI: `obctl.py start --config <cfg with listen_host "localhost">` prints `start ok` and exits 0. feedd exits 1 with `bind failed` because `src/feedd.cpp` parses the host with `inet_addr`, which takes a numeric IPv4 address only, while the probe's `socket.bind` resolves the name. `var/run/feedd.pid` is left pointing at a dead pid.

The first verification missed it because the port probe fired before the spawn check in every scenario tested, and because the instrumentation reaped the zombie before the check ran.

Four more findings from the same review are in scope: a failed `ps` reads as "not ours" and abandons a live process (WARN), a failed second spawn leaves the first child running (WARN), the probe's docstring claims parity with feedd's bind that it does not have for a named host (WARN), and the test for a dead child uses a pid that was never a child, so it cannot catch any of this (NIT).

**Research**: docs/research/2026-10-11-mac-run-path.md

## Outcome Boundary

### Done when:
- A feedd or adapter that exits during `start` is reported, with its own log lines, and `start` exits non-zero.
- `make test` and `make e2e` still exit 0, including the fixture run where the adapter exits after about 0.29 s.
- A `ps` that cannot be run leaves the pid file in place and makes `stop` exit non-zero.
- A `start` that fails after feedd is up does not leave feedd running.
- A `listen_host` that feedd cannot parse is refused before anything is spawned.

### Failed when:
- `start` reports success while feedd or the adapter is dead.
- `make e2e` fails because the adapter's normal exit is read as a failure.
- `stop` deletes a pid file for a process it could not identify.

### Must not:
- Change the 72-byte message, the ring, the WAL, the ports, or the feed subscription.
- Change `config/dev.json` or `config/prod.json`.
- Make `make test` depend on the network.

## Steps

### 1. Watch the child, not the pid — `done`
- **File(s):** python/obctl.py
- **Action:** `spawn` returns the `Popen`. `await_started(name, proc, offset, marker, ceiling)` waits for the marker, breaks when `proc.poll()` reports the child gone, and re-checks the marker once at the deadline before failing. Drop `ADAPTER_CEILING`; both children now have a marker and share `START_CEILING`.
- **Acceptance:** `grep -c 'proc.poll()' python/obctl.py` returns at least `1`; a child that exits at once makes `await_started` raise and removes its pid file.

### 2. Give the adapter a ready line — `done`
- **File(s):** python/feed_adapter.py
- **Action:** Log `adapter connected` in `connect_feedd` once the socket is up. The adapter otherwise has no positive signal, and in fixture mode it exits after about 0.29 s, which is below any liveness ceiling, so a liveness check alone would fail a healthy `make e2e`.
- **Acceptance:** `grep -c 'adapter connected' python/feed_adapter.py` returns `1`; `make e2e` exits 0 and `var/run/adapter.log` ends with `adapter connected`.

### 3. Do not leave half a pair running — `done`
- **File(s):** python/obctl.py
- **Action:** Wrap the two spawn-and-await blocks in `try`/`except SystemExit`, and on failure stop the adapter and feedd this invocation started before re-raising. Without this, a failed adapter leaves feedd up and the next `start` is refused with `already running`.
- **Acceptance:** `grep -c 'except SystemExit' python/obctl.py` returns `1`; an injected adapter failure leaves no lab process alive and no pid file.

### 4. Separate "gone" from "cannot tell" — `done`
- **File(s):** python/obctl.py
- **Action:** `proc_command` returns `None` when `ps` cannot be asked, `""` when the process is gone. `owns` treats `None` as unknown. `kill_pid` keeps the pid file and returns the pid when the command line cannot be read, so `stop` reports it and exits non-zero instead of abandoning a live process.
- **Acceptance:** `grep -c 'def proc_command' python/obctl.py` returns `1`; with `proc_command` forced to `None`, `kill_pid` returns the pid and the pid file still exists.

### 5. Refuse a host feedd cannot parse, and report the real bind error — `done`
- **File(s):** python/obctl.py
- **Action:** `preflight` refuses a `listen_host` that is not a numeric IPv4 address, naming `inet_addr` as the reason, and refuses a non-numeric `listen_port`. Replace `port_in_use` with `port_refusal(host, port, cfg_path) -> str`: `EADDRINUSE` reports another feedd, any other `OSError` reports the actual bind error rather than blaming a process that is not there.
- **Acceptance:** `grep -c 'inet_addr' python/obctl.py` returns `1`; `preflight` with `listen_host "localhost"` raises SystemExit; `preflight` with `listen_port "nine"` raises SystemExit.

### 6. Tests that can catch this — `done`
- **File(s):** python/test_obctl.py
- **Action:** Replace the fake-pid test with a real short-lived child: assert `await_started` raises for a child that exits at once, and returns for a child that prints the marker. Add tests for the `ps`-failure path, the half-pair cleanup, and the two new preflight refusals. Capture the code's stdout in the tests so `make test` output stays readable.
- **Acceptance:** `python3 -m unittest discover -s python -p 'test_obctl.py'` exits 0 with at least 15 tests; `make test` prints no `stop ok` or `left alone` lines.

### 7. Re-verify the whole path on this Mac — `done`
- **File(s):** Makefile, python/obctl.py, python/feed_adapter.py, python/test_obctl.py
- **Action:** From a clean tree: `make clean && make all`, `make test`, `make e2e`, the double-start sequence, the e2e-while-live sequence, the `listen_host "localhost"` config, the stray-feedd case, and the live window through a pty.
- **Acceptance:** `make test` and `make e2e` exit 0; the `localhost` config is refused before spawning; after every sequence `lsof -nP -iTCP:9001 -sTCP:LISTEN` prints nothing.

## Implementation delta

### The defect and how it was found
The first commit's `await_started` asked `os.kill(pid, 0)` whether the child was
alive. A child that has exited but not been reaped is a zombie, and signal 0
succeeds for a zombie, so the check reported a dead feedd as started. The first
verification missed it because the port probe refused before the spawn check in
every scenario tested, and because the instrumentation called `ps`, which
collects exited children and made the zombie disappear.

### Fixes
- `spawn` returns the `Popen`; `await_started` watches `proc.poll()` and waits for
  a ready line. Both children have one: feedd prints `feedd listen`, and the
  adapter now prints `adapter connected` from `connect_feedd`.
- `kill_pid` asks `ps` for liveness instead of `os.kill`. A zombie answers
  `<defunct>`, which is not running. Without this, every successful `stop` waited
  the full grace period and then reported a failure that was not one.
- A `ps` that cannot be asked is `None`, not `""`. `kill_pid` keeps the pid file
  and reports the pid, so `stop` exits non-zero instead of abandoning a live
  process it could not identify.
- A failed second spawn stops what this run started, so a refused start does not
  leave feedd holding the port.
- `preflight` refuses a `listen_host` that is not a numeric IPv4 address, because
  `src/feedd.cpp` parses it with `inet_addr` while Python's bind resolves names.
  `port_refusal` reports the real bind error: only `EADDRINUSE` blames another
  feedd.
- `spawn` closes the log handle when `Popen` raises.

### Tests
18 in `python/test_obctl.py`, up from 10, and 26 with the frame tests. The two
tests that asserted a SIGTERM-ignoring process survives were passing for the
wrong reason before: the zombie made a dead child look alive. They now wait for
the child to install its handler, which is what they meant to test.

### Review path
review2fix returned `skipped-unprovisioned` (no review provider is configured on
this machine, exit 3, ship-OK degrade). The same-model self-review ran instead,
in a fresh-context subagent, and found the CRITICAL defect plus three WARN and
five NIT findings. All of the CRITICAL and WARN findings and four of the five
NITs are fixed here; the fifth (the unreachable `tui` marker) is kept on purpose
so a future `tui` pid file would still be ownership-checked before a signal.
