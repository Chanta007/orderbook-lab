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
consumed=20 records  resyncs=0  age=4.910s
```

| Field | Unit | Question it answers |
|-------|------|---------------------|
| `lag` | records | how many published records this reader has not taken |
| the parenthesised span | time | how much **feed time** those records span |
| `staleness` | time | how old the newest published record is |
| `age` | time | how long since this reader last applied a record |
| `resyncs` | count | how many times the writer lapped this reader |

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

### 4.4 Durability counters

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
- `docs/optimization/ledger.jsonl` — the record of what each change cost
- `docs/telemetry/SCHEMA-v1.md` — the harness's own telemetry schema
