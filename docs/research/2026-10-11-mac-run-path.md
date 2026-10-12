# Research: The macOS compile-and-run path, and what wedges it

**Date**: 2026-10-11
**Chain mode**: yield

## Question

Can a person clone this repo on a Mac, compile it, and run the live window end to end? Where does that path break, and what change makes the break recoverable from the CLI the README already documents?

## Thinking — How We Got Here

### Initial framing & gut intuition

The expectation was a toolchain problem: a missing compiler, a C++17 gap, a Python version gap, or a `make` that only works on Ubuntu. This machine is macOS 13.5.1 on arm64 with Apple clang 14.0.3, GNU Make 3.81, and Python 3.12.6 from Homebrew.

The first pass proved that expectation wrong. A clean worktree off `origin/dev` built all four binaries with `make all` and printed no warnings, `make test` exited 0, `make e2e` exited 0 with `bids=2 asks=2 seq=19 wal=20`, a live run against `data-stream.binance.vision` filled five levels a side, and `build/tui` drawn through a pty showed `seq=971 feed=live` with the best bid below the best ask.

The compile path is fine. The run path is where the failures are.

### Pivots during research

1. **The pty check moved the target.** Driving `build/tui` through a pseudo-terminal showed the window is correct: 38 frames in four seconds, five price rows, `levels 3` cut it to three rows, `quit` exited. So the window itself needs no change. The interesting surface moved to `python/obctl.py`, which is what the README tells a user to run.

2. **`start` twice leaks the running instance permanently.** Reproduced: with feedd pid 58620 and adapter pid 58621 running, a second `start` spawned feedd 58629 (which failed to bind and exited) and adapter 58630, and **overwrote both pid files**. `stop` then signalled 58629 (already dead) and 58630, printed `stop ok`, and left 58620 holding port 9001 with no pid file pointing at it. Every later `make start` and `make e2e` fails with `bind failed` and the CLI has no way back. The trigger is ordinary: run `make start` twice, or run `make e2e` while the window is open.

3. **A first read of this was wrong, and the correction matters.** An earlier probe showed a feedd still listening right after `make e2e` finished, which looked like a second leak. Re-running with a two-second settle showed the port free and no processes left: that was normal shutdown latency (`stop` returns immediately after `os.kill`, feedd exits within ~250 ms). Test A below is clean. Only the double-start path leaks. Recording this so the fix does not chase a phantom.

4. **`stop` can signal a process that is not ours.** Reproduced: writing an unrelated live pid into `var/run/feedd.pid` and running `stop` killed that process. `kill_pid` reads the number and calls `os.kill` with no check that the pid still belongs to this lab. macOS reuses pids, and a pid file survives a crash, so `make stop` can kill an unrelated program on the user's machine.

### Options we considered and rejected

- **Auto-stop then start *(REJECTED)*** — convenient, but it would silently kill the feed behind a window the user still has open, and it would not help when the port is held by a feedd this repo never recorded (another worktree, an older session). A refusal that names the pid and the command is more honest, and matches the repo's existing voice: prod refuses to start, dev refuses `allow_orders`.
- **Port probe alone *(REJECTED)*** — catches an untracked feedd but not a second adapter, which connects happily to the running feedd and doubles the publish rate. Both checks are needed.
- **Pid file alone *(REJECTED)*** — misses the untracked case above, which is exactly the state the current bug leaves behind.
- **Record the pid inside the ring or the WAL *(REJECTED)*** — the WAL is a market-data record. Process bookkeeping does not belong in it.
- **Rewrite the process control in C++ *(REJECTED)*** — the lab already says Python owns setup/start/stop. The defect is four functions in that file, not the language.

### Open empirical questions deliberately not answered here

- Whether `SO_REUSEADDR` on the probe socket reproduces feedd's own bind outcome in every macOS kernel state. The probe uses the same option feedd uses, and the spawn check is the backstop when the probe is wrong.
- Whether an unrelated process can hold 9001 in a way that a bind probe reports as free. A listening socket reports busy; that is the case that matters.

## Options Considered

| Option | Pros | Cons | Cost |
|--------|------|------|------|
| `start` refuses when a tracked process is alive, refuses when the port is bound, and confirms the child survived. `stop` confirms the kill and signals only a process whose command line still looks like this lab's. | Recovers from both leak shapes. Keeps the CLI the README already documents. Fails loudly where the user is looking. | `stop` now waits for the process to exit, so it takes ~250 ms instead of returning instantly. | small |
| Auto-stop then start | One command always works. | Kills a feed behind an open window. Does not cover an untracked feedd. | small |
| Port probe only | Catches the untracked feedd. | Misses the second adapter. | small |
| Leave `obctl.py` alone, document `lsof` recovery | No code change. | The README would document a workaround for a bug the CLI creates. | small, wrong |

## Decision + Rationale

**Chosen: make `start` refuse an ambiguous state and make `stop` prove what it did.**

`start` gains three checks before it spawns anything: a live tracked pid for `feedd` or `adapter` refuses with the pid and the fix, a bound `listen_host:listen_port` refuses with the address and the two ways out, and the child is watched for its own ready line so a feedd that cannot bind reports its last log lines instead of leaving a dead pid file. `stop` signals only a process whose `ps` command line still contains this lab's program path, then waits up to two seconds for each process to actually exit and reports anything still alive.

The reason this shape and not a friendlier one: the failure the user hits is not "start did not work", it is "start said it worked and then nothing works again". A refusal that names the pid, the port, and the command to run turns an unrecoverable state into a one-line fix, and the pid-ownership check removes the only path where this CLI touches a process that is not its own.

## Evidence

### External

- Nothing external was needed. The behavior is local process and socket semantics: `bind(2)` fails with `EADDRINUSE` while another socket is listening on the same address, and `SO_REUSEADDR` does not permit two active listeners on the same address and port.
- Binance's public market-data host was reachable from this machine (`tcp connect ok 0.17 s`), which is what made the live half of the check possible.

### Source tensions

`docs/design/core-architecture.md` describes the processes and the ports, and the README's `Run it` section documents `setup`/`start`/`stop` as the operator path. Neither says what `start` does when one is already running. That gap is the bug: the code silently overwrote its own bookkeeping.

### Internal patterns referenced

- `python/obctl.py` — `spawn`, `kill_pid`, `start`, `stop`, `pid_file`.
- `src/feedd.cpp` — `bind failed` on stderr, exit 1; `SO_REUSEADDR` set before bind.
- `Makefile` — `e2e` calls `stop` then `start`, which is why `make e2e` while the window is open is a normal sequence and not an exotic one.
- `.gitignore` — `var/` is ignored, so pid files are never committed.

### Measured on this machine

| Check | Result |
|-------|--------|
| `make clean && make all` | 4 binaries, no warnings |
| `make test` | exit 0, 8 Python tests plus `test_core` |
| `make e2e` | exit 0, `bids=2 asks=2 seq=19 wal=20` |
| Live headless against Binance | `bids=5 asks=5 seq=1103 wal=1124` |
| `build/tui` through a pty | 38 frames, `feed=live`, `levels 3` honoured, `quit` exited |
| Prod without `ORDERBOOK_ALLOW_PROD=1` | refused, exit 1 |
| Dev with `allow_orders` true | refused, exit 1 |
| `make e2e` while the window is open, 2 s settle | clean, port free |
| `start` twice, then `stop` | feedd 58620 and adapter 58621 still alive, port 9001 held |
| Unrelated live pid in `var/run/feedd.pid`, then `stop` | unrelated process killed |

### Second-brain (kb packs)

kb: unavailable or no hits.

## Parked Future Angles

- Run the same double-start check from a second worktree on the same port, where the pid file lives in the other checkout. *Revisit when:* someone works two worktrees at once and hits `bind failed` with no pid file in the tree they are standing in. The port refusal already names the address; a cross-worktree message would be nicer.
- Give `obctl` a `status` command that prints each tracked pid, whether it is alive, and who holds the port. *Revisit when:* a third "why is my window stalled" question shows up.

## Re-evaluation Hooks

- Reopen if a feedd is observed holding the port with no pid file in any worktree after a `stop` that printed `stop ok`.
- Reopen if `ps -p PID -o command=` is ever unavailable on a supported platform, since the pid-ownership check depends on it.
