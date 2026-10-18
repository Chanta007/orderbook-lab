# Runbook: the window says `feed=stalled`

## Symptoms

- The header reads `feed=stalled`.
- `staleness=` climbs and keeps climbing. `age=` climbs with it.
- The prices on screen do not change.
- One of these lines appears under `Showing N levels.`:

```text
Feed stalled. The adapter is alive (live; watching 3, 0s ago), so the exchange has
gone quiet rather than this host. This window last applied a record 2.140s ago.
Feed stalled. The adapter (pid 4731) last wrote 14s ago and has not since: it has
exited, and this window cannot restart it. This window last applied a record 2.356s ago.
Feed stalled. No adapter heartbeat at all, so nothing is feeding this ring. This
window last applied a record 2.356s ago.
```

- Or, when the selected symbol has never arrived, this one instead of the above:

```text
Nothing for ETHUSDT yet. No adapter heartbeat at all, so nothing is feeding this
ring. If it never arrives, check that the adapter is watching it: `watch` lists the set.
```

## What it means

`feed=stalled` means this window has not applied a record for about two seconds.
It says nothing about why, and there are three different causes. The line under
the book is the diagnosis, and it is derived from `var/<env>/adapter.heartbeat`,
which the adapter rewrites every time it looks at the watch file and removes when
it exits.

| The line says | What is true | What to do |
|---------------|--------------|------------|
| the adapter is alive, and *N* seconds ago | the host is fine; the exchange has not sent a picture | wait. The adapter reconnects on its own. |
| the adapter last wrote *N* seconds ago | the adapter has exited | start a feed again, below. |
| no adapter heartbeat at all | nothing has ever fed this ring, or the heartbeat was removed | start a feed again, below. |
| `Nothing for SYMBOL yet` | this window has applied no record for the selected symbol | check the selection and the watch set before assuming the feed is down. |

The causes are told apart because the two that look identical from the ring alone
are the two a person cannot fix by waiting. `Nothing for SYMBOL yet` is a
different situation again, and it is separate on purpose: `age` is this window's
own, so the first two seconds after a switch would otherwise report a stall.

## Diagnosis

### 1. Read the adapter's own log

```bash
tail -20 var/dev/adapter.log
```

Lines are stamped, and the last one before the silence says what happened: a
`socket closed`, a `reconnecting`, or nothing at all if the process was
signalled. A stop from this lab writes a final line naming itself:

```text
2026-10-18T19:12:04 obctl stop: adapter pid=4731 signalled by obctl pid=9982 cwd=/Users/me/other-checkout
```

That line exists because SIGTERM carries no sender. A feed that stopped because
somebody ran `stop` — or because a test did, from another directory — used to look
exactly like a feed that stopped because the exchange went quiet.

### 2. Check what is running

```bash
python3 python/obctl.py stop --config config/dev.json   # prints the directory it reads
ps -o pid,command -p "$(cat var/run/dev/feedd.pid 2>/dev/null)" 2>/dev/null
lsof -nP -iTCP:9001 -sTCP:LISTEN
```

`stop` is the safe way to ask, and its output names the run directory it looked
in: a stop that found nothing and a stop that found the wrong thing read the same
until you know where it looked.

### 3. Check staleness against age

`staleness` is how old the newest published record is. `age` is how long since
*this window* applied one. Together they separate the two failures that look
alike:

| staleness | age | reading |
|-----------|-----|---------|
| growing | growing | nothing is being published: the feed, not the window |
| small | growing | records are being published for another symbol; this window is fine and the selection is wrong |
| `-` | growing | the ring is empty: no feedd has ever published into it |

## Fix

Start the feed and the adapter again, in a **second** terminal:

```bash
make rust-feed
```

The window that is open re-attaches by itself within a quarter of a second of the
new writer publishing, because a restarted daemon begins the ring's sequence
again at zero and the window notices. Its header counts it in `reattaches=N`.

Do not run `make stop` as the first move. It signals the `tui` as well, so the
window you are trying to rescue is the first thing it closes.

## Prevention

- Use `make rust-feed` when a feed has to be restarted under a window that is
  already open. `make rust-start` opens a second window; `make stop` closes the
  first one.
- Keep tests on `config/test.json`, which is what the `Makefile` does. Its pid
  files, logs, ring, WAL and port are its own, so a test run cannot stop a
  session's processes. `docs/optimization/ledger.jsonl` OPT-038 is what happened
  when they were shared.
- A window that never recovers after a feed restart is a bug in the window, not a
  feed problem. Check `reattaches=` in the header before restarting anything
  twice.