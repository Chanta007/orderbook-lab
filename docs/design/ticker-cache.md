# The ticker cache

> Last updated: October 2026

## 1. Purpose

The cache is the capability the Rust implementation adds. It holds order books for
hundreds to thousands of symbols, publishes every change to several in-process
subscribers without locks, and reports what the pipeline cost in **latency**,
**slippage** and **backpressure** as first-class quantities rather than as
adjectives.

It is a bus between *threads*. The shared-memory ring
(`docs/design/rust-implementation.md` §3.3) is the bus between *processes*. A
strategy co-located with the feed reads the cache; the terminal and the headless
checker read the ring.

## 2. Key files

| File | Responsibility |
|------|---------------|
| `rust/ob/src/cache.rs` | The publisher, the subscribers, the symbol routing, the snapshot lane. |
| `rust/ob/src/book.rs` | One symbol's two sides, best first, in flat arrays. |
| `rust/ob/src/flow.rs` | The bounded SPSC queue and the four policies for a full one. |
| `rust/ob/src/latency.rs` | Stage measurement, histograms, coordinated-omission correction. |
| `rust/ob/src/slippage.rs` | The latency/depth decomposition, and markouts. |
| `rust/ob/src/pressure.rs` | Lag, and the three-state backpressure machine. |
| `rust/ob/src/symbol.rs` | Interning eight wire bytes into a dense `u32`. |
| `rust/ob-cli/src/bin/obsoak.rs` | The load generator and the report. |

## 3. Architecture

### 3.1 One writer, N readers, no locks

```text
                       +----------- Publisher -----------+
   records ----------> |  symbol table -> shard -> book   |
                       |  best_bid / best_ask side tables |
                       |  latency stages 1 and 2          |
                       |  slippage ledger                 |
                       +--------------+-------------------+
                                      | bounded queue, one per subscriber
                         +------------+------------+
                         v            v            v
                    Subscriber 1  Subscriber 2  Subscriber 3
                    stage 3       stage 3       stage 3
                    pressure      pressure      pressure
```

`cache::build` returns the publisher and every subscriber from one call, which is
how the single-writer rule is expressed in the type system: the publisher is the
only value that can touch the books, and each subscriber owns the read side of
exactly one queue. The producer and consumer halves are `Send` and not `Sync`, so
two threads cannot share one end.

Nothing in the fan-out takes a mutex. A subscriber that cannot keep up loses
records according to its policy, and the loss is counted in three places: the
subscriber's own counters, the publisher's totals, and the pressure state.

### 3.2 Sharding, and what it is for

Symbols are assigned to shards by their id's low bits; each shard holds a dense
array of books indexed by the id's high bits. A lookup is a mask and a shift with
no hashing on the update path.

Shards exist so that a future version can give each shard to a different
publishing thread without changing anything else. Today there is one publisher,
because a market-data connection is one thread, and adding locking for threads
that do not exist would be complexity without a measurement behind it. The shard
count is still configurable, and `obsoak --shards` measures the routing cost at
1, 2, 8 and 32.

### 3.3 The snapshot lane

The publisher keeps `best_bid` and `best_ask` in two flat arrays indexed by global
symbol id, written once per book change. A market snapshot is then one linear pass
over three contiguous arrays instead of a pointer chase through a thousand `Book`
structs.

This is both a data-layout decision and the thing that made the kernel
vectorizable; `docs/design/vec-optimization.md` §4.3 has the measurement (3676.7 ns
to 260.7 ns for a 1000-symbol snapshot).

### 3.4 Nothing allocates on the update path

Every structure is sized at `build`. The two allocating calls left are the symbol
table growing on a new symbol and a shard's book array growing on a new symbol, and
both are on the *first sight* of a symbol, which is a moment when the process is
not yet at full rate for it.

## 4. The three measurements

### 4.1 Latency, in stages

Every process here reads the same monotonic clock on one host, so a stamp taken by
the sender and a stamp taken by the daemon can be subtracted:

```text
  sender stamps   daemon reads    ring publishes   subscriber sees
       |               |               |               |
       |<- stage 1 --->|<- stage 2 --->|<- stage 3 --->|
       |<--------------- end to end ----------------->|
```

| Stage | Measured at | Covers |
|-------|-------------|--------|
| 1 | publisher | socket and kernel, sender's stamp to the daemon's read |
| 2 | publisher | decode, ring publish, log append, cache update |
| 3 | subscriber | how long the event waited in its queue |
| total | subscriber | the whole path, from the event's carried ingress figure |

What is **not** measured: the exchange's own latency, the network, or anything
before the sender wrote the record. That would need a synchronised clock at the
other end, and this lab does not have one.

Stage 1 is only measurable when the sender stamped the record. The C++ daemon
overwrites `trace_id[0..8]` with its own reading; this one leaves the field alone
and writes its correlation counter into `trace_id[8..16]` and `span_id`. So a
record that carries a sender stamp keeps a measurable stage 1, and a record that
does not is reported as **unmeasured** rather than as a zero-latency sample. An
earlier version filled the field when it was empty, which made every record look
stamped with a stage-1 latency of zero — a measurement that looks like data and is
not.

Distributions are fixed-bucket histograms with a 7.5% resolution, so a percentile
is reported as the upper bound of its bucket and is never below the true value.
The mean, minimum and maximum are exact. A percentile beyond the tracked range is
reported as a lower bound through the overflow count.

`LatencyTracker::record_with_expected_interval` applies Tene's coordinated-omission
correction: the missing samples are charged `observed - k * interval`, walking down
from just under the observed value. They are *not* charged a constant interval,
which is the version that looks right and understates the tail.

### 4.2 Slippage, split into its causes

```text
  decision mid          mid when the order         price actually
  (t0)                  reached the book (t1)      paid (t1)
      |                        |                        |
      |<--- latency move ----->|<---- depth cost ------>|
      |<------------------- implementation shortfall -->|
```

Both components are reported in hundredths of a basis point of the reference mid,
signed so that **positive is a cost**, for a buy or a sell. A system that reports
only the total cannot tell an operator whether to make the feed faster or change
the order size, which is the whole reason for the split.

**Markouts** answer a different question: after the trade, did the price move in
the position's favour? Observations are queued when the trade happens and settled
by `Publisher::resolve_markouts` on the publisher's timer, at 10 ms, 100 ms and 1 s.
A pending queue that overflows drops the oldest and counts it, because an
unsettled markout is better dropped than settled against a stale mid.

The lab's demonstration is `obsoak`: it decides on the **previous** picture's
midpoint and acts on the current book, so the latency component is one picture of
drift and the depth component is the cost of walking the levels for the order size.

### 4.3 Backpressure, as a state rather than a number

A queue that is 60% full is fine for a consumer that runs every microsecond and
alarming for one that runs every second. Depth alone says nothing. What matters is
**lag**: how far behind the producer the consumer is, in time.

| State | Lag |
|-------|-----|
| `Nominal` | below `pressure_elevated_ns` |
| `Elevated` | at or above it |
| `Saturated` | at or above `pressure_saturated_ns` |

Leaving a state requires the lag to fall to **half** the threshold that was
crossed. Without that, a lag sitting on a threshold flips the state on every
sample and the transition counter measures jitter rather than incidents.

Reported: the state, transitions, saturation events, the fraction of time spent
saturated, the peak lag, the peak queue occupancy, and the lag distribution. The
saturated percentage is the number worth alerting on: dimensionless, comparable
between runs, and non-zero only when records were at risk.

## 5. The four policies for a full queue

| Policy | Behaviour when full | Use it when |
|--------|--------------------|-------------|
| `DropOldest` (default) | evict the oldest, keep the newest | the newest price is what matters: a display, a signal |
| `DropNewest` | refuse the new record, keep the queue | every record must be seen in order: an audit sink |
| `Spin { spins }` | spin briefly, then refuse | the consumer is known to be microseconds behind |
| `Credit` | refuse unless the consumer has granted credit | the consumer wants to control its own inflow |

`DropOldest` costs a compare-and-exchange on the drop path and nothing on the
normal path: the producer advances the read position *before* overwriting the
slot, so a consumer that loses the race retries and never reads a slot being
written.

`Credit` is the one policy where the consumer decides its own inflow, which turns
the queue into a window rather than a buffer.

## 6. Shard count

`obsoak --shards N` measures the routing cost. The default of 4 was chosen because
the routing is a mask and a shift either way and the difference between 1 and 32
shards is below the measurement's resolution for a single publisher. A future
multi-publisher version would change that, and the knob is there for it.

## 7. Depth snapshots versus a subscriber-owned book

A subscriber that needs more than the top of book has two options:

- `Subscriber::apply_to(&mut book, n)` keeps its own `Book` from the events. The
  events still pass through `try_recv`, so the measurement is identical to a
  subscriber that only reads the top.
- `Publisher::levels(symbol, side, out)` takes a snapshot from the publishing
  thread.

The events carry the top of book and the message that produced it, not a full
depth snapshot. Copying the whole book into every event would cost more than the
fan-out it saves, and most consumers want the touch.

## 8. What the strategy costs the feed

`obsoak` runs the feed and a simulated taker on the same thread, which is the
honest model of a co-located strategy: it reads the book without a round trip, and
the price of that is that its work is on the feed's thread. The stage-2 latency
distribution in the soak's report is what that costs.

## 9. Cross-references

- `docs/design/rust-implementation.md` — the workspace and the shared wire format
- `docs/design/vec-optimization.md` — the kernel measurements
- `docs/optimization/README.md` — the ledger
- `docs/CONSTRAINTS.md` §14 — the performance targets
