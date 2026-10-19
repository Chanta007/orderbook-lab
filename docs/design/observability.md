# Observability — orderbook-lab

> Last updated: October 2026

## 1. Purpose

What this lab can tell you about itself while it runs, and what it deliberately
cannot. The organising rule is that **a measurement names its stages and its
units**, and that a number which is present means what it says.

## 2. The four surfaces

| Surface | Where it goes | Who reads it |
|---------|--------------|--------------|
| Structured log lines | stderr, redirected to `var/<env>/…` by `obctl` | the operator, and CI on failure |
| The cache report | stderr every second, and once at exit | the operator |
| The terminal book | the terminal | the operator |
| Telemetry JSONL | `docs/telemetry/runs-*.jsonl` | the harness, for its own analysis |

There is no metrics endpoint, no dashboard, and no OpenTelemetry collector. That
is a decision, not an omission; §6 says why.

## 3. Log lines

One line per event, built in a fixed 512-byte buffer with no allocation, written
by the calling thread.

```text
t=1384215194226125 level=info component=feedd op=report published=20 applied=20 ignored=0
t=1384215194226125 level=info component=feedd op=latency stage1=n=20 min=0.0us p50=0.0us …
```

`t` is nanoseconds from `CLOCK_MONOTONIC_RAW`, which is monotonic and not a
calendar time. Deliberate: a run's lines are read together, where differences
matter and dates do not.

The format and the field types are in `rust/ob/src/log.rs`. Fields are typed
rather than `Display`, so a duration is formatted by the type that produced it and
nothing on that path can allocate or panic. A line that would overflow the buffer
is truncated with a marker rather than allocating, because a log call must never
be the thing that fails under pressure.

A process this lab stops gets a line written into its own log by the thing that
stopped it, after the signal is sent:

```text
2026-10-18T19:12:04 obctl stop: feedd pid=4718 signalled by obctl pid=9982 cwd=/Users/me/other-checkout
```

SIGTERM carries no sender, so without this a stopped daemon is silent about it. A
feed that stopped because a person ran `stop` and a feed that stopped because a
harness test did, from a different directory, read the same. The caller's pid and
working directory are what tell them apart, and the third field is what makes the
second one worth having.

Each config's process logs and pid files are in `var/run/<config>/`: keyed by the
config file rather than by its `env`, because two configs can share an env (the
shipped test config is `env: dev`) and a shared pid file is one config's `stop`
reaching another's daemon. `docs/optimization/ledger.jsonl` OPT-038.

## 4. The numbers that matter

### 4.1 Latency, in stages

```text
  sender stamps   daemon reads    ring publishes   subscriber sees
       |               |               |               |
       |<- stage 1 --->|<- stage 2 --->|<- stage 3 --->|
       |<--------------- end to end ----------------->|
```

| Stage | Measured at | Covers |
|-------|-------------|--------|
| 1 | publisher | socket and kernel, the sender's stamp to the daemon's read |
| 2 | publisher | decode, ring publish, log append, cache update |
| 3 | subscriber | how long the event waited in its queue |

What is **not** measured: the exchange's own latency, the network, or anything
before the sender wrote the record. That needs a synchronised clock at the other
end and this lab does not have one.

Stage 1 is only measurable when the sender stamped the record. When it did not,
the count of unstamped records is reported and the end-to-end figure covers stages
2 and 3. A zero is never reported for an unmeasured stage.

Distributions are fixed-bucket histograms at 7.5% resolution; the mean, minimum
and maximum are exact. A percentile is reported as the upper bound of its bucket
and is never below the true value. `rust/ob/src/latency.rs`.

### 4.2 The cache report

Printed every second and once at exit:

```text
t=… op=report published=2400000 applied=2400000 ignored=0 crossed=0 evicted=0 dropped=0 symbols=1000 …
t=… op=latency stage1=… stage2=… ingress=… stamped=0 unstamped=2400000 implausible=0
subscriber 0: delivered=2400000 evicted=0 dropped=0 high_water=2041 state=nominal | stage3: n=… p99=165.5us
```

Four things to read in it:

- **`dropped` and `evicted`** — records a subscriber never saw. Non-zero means a
  consumer is not keeping up and the policy decided what to lose.
- **`crossed`** — the book had a bid at or above an ask. A property of the feed or
  of the generator, not of the cache; `obsoak` explains its own count in the
  output because a seven-figure number would otherwise look like a fault.
- **`implausible`** — a stamp that cannot be a duration on this host.
- **the pressure state** — `nominal`, `elevated` or `saturated`, derived from lag
  in *time* rather than queue depth, with hysteresis so a lag on a threshold does
  not flip the state on every sample.

`docs/design/ticker-cache.md` §4 explains the three measurements in full.

### 4.3 The terminal book's header

The book reports the reader's own view of the ring, in three different measures.
They are not the same number and each answers a different question, which is why
it shows all three:

```text
ring=4096  lag=0 records (0ns of feed time)  staleness=5.236s
consumed=20 records  resyncs=1  reattaches=1  age=4.910s
```

| Field | Unit | Question it answers |
|-------|------|---------------------|
| `lag` | records | how many published records this reader has not taken |
| the parenthesised span | time | how much **feed time** those records span |
| `staleness` | time | how old the newest published record is |
| `age` | time | how long since this reader last applied a record |
| `resyncs` | count | how many times the writer lapped this reader |
| `reattaches` | count | how many times this window attached itself to a feed that had restarted |

**Why the lag is shown as a count and a span.** A count of records means nothing
without the feed's rate: forty records behind is a disaster at a hundred per
second and irrelevant at a million. The span is the same information in a unit an
operator can compare against a threshold without knowing the rate. It is computed
from the record stamps the ring already carries, so it costs one slot read.

**Why staleness is separate.** The span is zero when a reader has caught up, even
if the feed itself stopped an hour ago. Staleness is the age of the newest record,
so it grows on a quiet feed as well as on a slow reader. On the run above the
fixture had finished five seconds earlier: `lag` was zero, the span was zero, and
staleness was 5.2 s — which is the number that says what is on screen is old.

**Why the units change.** Durations are formatted to three decimals in whichever
unit keeps them readable: `812ns`, `14.273us`, `2.501ms`, `5.236s`. A live feed's
staleness is normally tens of microseconds, and a display offering whole seconds
could not show that at all.

All three are the **reader's** view. None of them is the exchange's latency, which
this lab cannot measure; §4.1 says what is and is not measured.

### 4.4 What a stalled window is attached to

`feed=stalled` is one state with three causes, and the window distinguishes them
rather than leaving the operator to guess. The state itself is simple: no record
applied for `STALL_AFTER` (two seconds), from an `Instant` taken at the last
apply. What it cannot say on its own is *why*, because from inside the ring the
two cases that matter look identical — records stop arriving. A quiet exchange and
a daemon that has exited both leave a valid ring whose newest record is old.

The missing fact is whether anything is still trying to write. The adapter
publishes it:

```text
var/<env>/adapter.heartbeat     "<pid> <unix seconds> <note>"
```

Written on every reconnect attempt and after every connect, at up to five times a
second while frames arrive, by writing a temporary file and renaming it, so a
reader never sees half a line. It is removed on a clean exit. The notes are
`starting`, `connected; watching A,B`, `live; watching N`, `no feedd; redialing`,
and `ws error; retrying in Ns`, so a reader can tell "still trying" from "gave up"
as well as "running" from "gone".

`ob_cli::read_heartbeat` classifies it as `Live`, `Stale` or `Absent`, with a
three-second window (`HEARTBEAT_STALE_S`) — far longer than the adapter's own
interval and far shorter than a person's patience. The window then says which case
it is:

| Heartbeat | Line under the book | What it rules out |
|-----------|--------------------|-------------------|
| fresh | the adapter is alive, so the exchange has gone quiet | this host: a restart would not help |
| stale | the adapter has exited, with its pid and how long ago | the exchange |
| absent | nothing is feeding this ring at all | a process that died without a heartbeat |

When the displayed symbol has never been applied, that is a fourth case and it is
said differently: `Nothing for SYMBOL yet`, followed by the same statement about
the adapter. `age` is the window's own, so it starts counting at the attach or the
switch, and without this the first two seconds after a switch to a symbol the
exchange is not sending would read as a feed failure. Here the selection is the
suspect, not the feed.

A heartbeat is a courtesy, not a contract: a lab that cannot write one still
feeds, and the window reports it as absent rather than failing.

### 4.5 Re-attaching to a feed that restarted

A window attached to a ring does not stay attached by itself. Two things end the
attachment and neither is visible in the bytes the window reads:

- **The writer restarts.** `Ring::create` truncates the header and the write
  sequence begins again at zero. The reader's cursor is a number from the run that
  ended, so it is *ahead* of the writer, `consume` returns `Empty` on every frame,
  and the window shows the last book of a dead run for ever.
- **The file is replaced.** A mapping follows the inode it was made from. A ring
  file that is removed and created again leaves the window reading a file nothing
  writes, and the bytes of that file are a valid, empty ring.

Both look like a healthy ring with nothing new in it, which is why a window has to
check rather than infer. Every `RING_CHECK` (250 ms) the window asks two questions:
is the file at the path still the inode it mapped (`RingReader::replaced`), and has
the write sequence gone backwards below its cursor. Either one is a re-attach: the
ring is opened again, the cursor goes to zero, the book is rebuilt from the new
run and the counters restart. The header counts it in `reattaches=`, so the state
is visible rather than inferred from prices that happen to have changed.

The prices are not sufficient evidence on their own. A window that never notices
still *appears* to recover once the new run has published as many records as the
old cursor had reached, because the slot's sequence then matches and `consume`
carries on — from the middle of a stream, having silently lost its start.

`docs/runbooks/feed-stalled.md` is the operator's side of §4.4 and §4.5.

### 4.6 The ticker picker and the status line

Two parts of the frame report state rather than measurements:

**The status line** is the last thing that happened: a switch, a refusal, or a
reason a write failed. It is where a command's answer appears, because the input
line is consumed by submitting it.

**The picker** appears between the book and the prompt when there is something on
the input line. It lists at most eight of the catalog's matches, best first, with
the highlighted one marked. Ranking is by prefix before interior match, with the
catalog's own order breaking ties, so typing `ET` offers `ETCUSDT` before a pair
that merely contains those letters.

The header also carries the displayed symbol, the count of watched symbols, and
the size of the subscription, because a book that never updates has three possible
causes and they are told apart by what is on that line: the wrong symbol selected,
the symbol not watched, or the feed stalled. §4.4 is how the window says which.

**A name that is most of a listed one** is resolved rather than refused. Exactly
one listed symbol starting with what was typed is used, and the frame says which
name it stood for; several are named; none is the refusal below. Only a prefix is
completed, never an interior match, because `usdt` is inside `BTCUSDT` and
substituting there would answer a request for one symbol with a different one.

**The refusal** is the one place where a bad value would cost more than a message:
every watched symbol shares one websocket, so an unlisted stream name in the
combined URL can take the connection down for all of them. A name matching nothing
is therefore refused with `force` and its risk named, and `only SYMBOL` — which
replaces the whole subscription — is checked the same way as a bare symbol.

**The keys.** `Tab` completes the line to the best match, the arrows move the
picker's highlight, `Enter` submits, `Escape` clears what has been typed, and
backspace deletes a character. `Escape` is a key with a meaning rather than an
unhandled byte, and it is the way out of a refusal that has left a partial name on
the line. Reading it correctly is more delicate than it looks: a terminal sends
`Escape` alone as one byte and an arrow key as three, so the reader waits 30 ms for
the rest of a sequence and keeps every byte it reads either way — `OPT-039` in
`docs/optimization/ledger.jsonl` is what happened when it did not.

**The frame's height** is fixed, and with five levels and a stalled feed it is about
27 rows: three header rows, the colour key, eleven help rows, the levels line, the
status line, up to three lines of diagnosis, the price table and the prompt. A
24-row terminal therefore scrolls the header out of view, which is the line that
says which symbol is displayed and whether the feed is live. `levels 3` fits today.
Fitting the frame to the window is a deferred item
(`docs/deferred/index.md`), because dropping rows needs a rule for which ones go.

### 4.7 Durability counters

`wal_records`, `wal_rejected`, `wal_torn_bytes` and `ring_published` come out of
the headless checker, which reads both the ring and the log. A torn tail is
reported and not failed on: it is what a crash mid-append looks like, and the log
is designed to survive it.

## 5. Correlation

`trace_id[16]` and `span_id[8]`, the W3C widths, travel in the message. No
collector reads them; they are there so that a record can be followed across the
socket, the ring and the log without a timestamp comparison.

The daemon writes its own counter into `trace_id[8..16]` and `span_id`, and leaves
`trace_id[0..8]` to the sender, because that half is the one that carries a
measurement. `docs/design/security.md` §6.

## 6. What is deliberately absent

**No OpenTelemetry SDK on the hot path.** `StartSpan` is hundreds of nanoseconds
plus a heap allocation, and a `SimpleSpanProcessor` takes a mutex and exports on
the caller's thread. The lab carries the IDs and logs on a side thread instead.
This was decided in `docs/research/2026-09-21-orderbook-lab.md` and the reasoning
still holds.

**No metrics endpoint or dashboard.** A scrape endpoint would be another listener
and another dependency for a program whose whole output is one terminal and a
report line. The numbers a dashboard would plot are in the report.

**No tracing of individual records.** At 7M records/s, tracing one in a thousand
would cost more than the pipeline.

**No calendar timestamps in the hot path.** Converting a monotonic reading to a
date is a syscall and a timezone database; nothing in the pipeline needs one.

## 7. How to read a run

```bash
make start                       # the terminal book, plus the report on stderr
make rust-soak                   # the cache under load, with the full report
rust/target/release/headless config/dev.json 1500   # the e2e numbers
```

For a single run's records rather than its summary, the log is the place:
`Wal::replay` reads it back in order, and both implementations count a torn tail
rather than stopping.

## 8. Cross-references

- `docs/design/ticker-cache.md` §4 — the latency, slippage and pressure model
- `docs/design/vec-optimization.md` — the kernel measurements
- `docs/runbooks/feed-stalled.md` — what to do when the window says the feed stopped
- `docs/optimization/ledger.jsonl` — the record of what each change cost
- `docs/telemetry/SCHEMA-v1.md` — the harness's own telemetry schema
