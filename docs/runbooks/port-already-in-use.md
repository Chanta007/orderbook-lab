# Runbook: `bind failed`, or port 9001 already in use

## Symptoms

- `make start` prints `refusing to start: 127.0.0.1:9001 is already in use, so feedd cannot bind.`
- `make start` prints `feedd did not start (pid=...). Last lines:` followed by `bind failed`.
- `var/run/feedd.log` ends with `bind failed`.
- The window opens but stays on `feed=stalled`, and `seq=` does not climb.
- `make e2e` reports a book that does not match `e2e/fixture.jsonl`.

## What it means

One feedd owns `127.0.0.1:9001`. A second one cannot bind and exits. The two
ways to reach that state:

1. Something in this worktree is already running. `obctl.py start` should have
   refused. If it did not, the pid file was missing or held a stale pid.
2. A feedd from another worktree, or from a session that was never stopped,
   holds the port. `config/dev.json` uses the same port in every worktree, so
   two checkouts cannot both run dev at once.

Only one feedd may run per port. The adapter, the window, and `headless` all
connect to it, and each reader keeps its own cursor in the ring.

## Diagnosis

### 1. Ask the CLI first

```bash
python3 python/obctl.py stop --config config/dev.json
```

`stop` prints `left alone: ...` when a pid file names a process that is not
this lab, and `still running after SIGTERM: ...` when a process it owns did not
exit. Both lines are the answer to "what is holding it".

### 2. Find the holder of the port

```bash
lsof -nP -iTCP:9001 -sTCP:LISTEN
```

A `feedd` line means a feedd is running. Compare its pid with
`cat var/run/feedd.pid`. When they differ, the pid file belongs to another
worktree or to a process that has since exited.

### 3. Check for a second worktree

```bash
git worktree list
lsof -nP -iTCP:9001 -sTCP:LISTEN
```

Every worktree shares `config/dev.json`, so the port is shared too. Stop the
other worktree with its own `obctl.py stop`, or give this one a different
`listen_port`.

## Fix

```bash
# 1. Stop this worktree.
python3 python/obctl.py stop --config config/dev.json

# 2. If a feedd is still listening, it belongs to another worktree or to an
#    older session. Confirm it is a feedd before signalling it.
lsof -nP -iTCP:9001 -sTCP:LISTEN
kill <pid>

# 3. Start again. start refuses when the port is still taken, so a clean
#    refusal here means step 2 did not finish.
python3 python/obctl.py start --config config/dev.json --tui
```

Do not delete pid files by hand as the first move. `stop` removes them itself
and tells you which one was stale.

## Prevention

- Run `make stop` before `make start`, or let `start` refuse.
- `make e2e` calls `stop` first, so it is safe to run while a window is open.
  The window will show `feed=stalled` until the next `make start`.
- Do not run two worktrees in dev at the same time unless one of them has its
  own `listen_port` and `bus_path`.
