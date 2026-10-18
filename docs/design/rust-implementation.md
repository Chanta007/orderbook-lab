# The Rust implementation

> Last updated: October 2026

## 1. Purpose

The repository holds two implementations of the same lab. The C++ and Python one
is the original and stays maintained. This document describes the Rust one: what
it is for, how it is laid out, and the rules that keep the two agreeing on bytes.

The Rust version is not a rewrite in the sense of a replacement. It exists to be
read side by side with the C++ version, to carry the same 72-byte record over the
same ring format, and to add one capability the original does not have: an
in-process ticker cache for hundreds to thousands of symbols with latency,
slippage and backpressure as first-class reported quantities
(`docs/design/ticker-cache.md`).

## 2. Key files

| File | Responsibility |
|------|---------------|
| `rust/Cargo.toml` | The workspace, and the release profile the vectorization audit depends on. |
| `rust/ob/src/lib.rs` | The layer diagram, and the module list in reading order. |
| `rust/ob/src/sys.rs` | The only libc boundary: six functions declared by hand. |
| `rust/ob/src/msg.rs`, `codec.rs` | The 72-byte record and its byte layout. |
| `rust/ob/src/ring.rs`, `wal.rs` | The shared-memory bus and the durable log. |
| `rust/ob/src/book.rs`, `cache.rs` | One symbol's prices, and many symbols with subscribers. |
| `rust/ob/src/latency.rs`, `slippage.rs`, `pressure.rs` | The three measurements. |
| `rust/ob-cli/src/ingest.rs` | Accepting connections and reading whole records. |
| `rust/ob-cli/src/bin/` | `feedd`, `tui`, `headless`, `obsoak`, `vecbench`. |
| `python/test_tui_input.py` | The terminal test: a pseudo-terminal, keystrokes, and what came back. |
| `rust/ob-cli/src/tickers.rs` | The ticker catalog and the picker's ranking. |
| `rust/ob-cli/src/watch.rs` | The watch set, and the atomic write that tells the adapter about it. |
| `config/tickers.txt` | The symbols the picker offers. Editable; symbols over 8 bytes are refused. |

## 3. Architecture

### 3.1 Two crates, one direction of dependency

```text
  ob-cli   feedd, tui, headless, obsoak, vecbench  +  the ingest reader
    |
    v
  ob       cache, book, latency, slippage, pressure      SERVICES
           ring, wal, flow                               BUS
           msg, codec, fixed, symbol, clock, config, log  CORE
           sys                                           the libc boundary
```

A module may use the modules below it and never the ones above. `sys` is used by
`ring` and `clock` and by nothing else, which is why the crate has few `unsafe`
blocks to read and all of them are in two files.

### 3.2 No dependencies, on purpose

`cargo build` needs no network and the workspace has no `[dependencies]` section.

The standard library covers files, TCP, threads and clocks. The two things it does
not cover are mapping a file into shared memory — which is the bus — and reading a
monotonic clock as a number, which is what makes the latency measurements
comparable across processes. `sys.rs` declares those functions itself:

```
  mmap  munmap  msync  madvise  clock_gettime  fcntl (macOS only)
```

Six declarations and about eighty lines, against a dependency tree that would
carry its own versioning, its own unsafe, and its own build time. The repository's
front page already promises "no pip packages" for the Python side; this extends
the same promise to the Rust side.

### 3.3 The wire format is shared, and asserted

`rust/ob/src/codec.rs` holds the byte offsets of the 72-byte record as named
constants and asserts them at compile time:

```rust
const _: () = {
    assert!(MSG_SIZE == 72);
    assert!(OFF_MAGIC + 4 == OFF_NBYTES);
    // ... every field, in order
    assert!(OFF_SYMBOL + 8 == MSG_SIZE);
};
```

The ring header is the same sixteen bytes as the C++ `RingHeader`: `magic: u32`,
`capacity: u32`, `wseq: u64`. Either implementation can read the other's ring, and
`make rust-interop` proves it in both directions rather than asserting it.

**The rule**: a change to the wire format changes `include/ob/msg.hpp`,
`python/feed_adapter.py`'s `MSG_FMT`, and `rust/ob/src/codec.rs` in the same
commit. `docs/CONSTRAINTS.md` §1.1 states it as a constraint.

### 3.4 One deliberate difference from the C++ daemon

The C++ `feedd` accepts a connection and starts a thread per client, and each of
those threads publishes to the ring directly. With one adapter that is one writer,
which is what the ring requires. With two adapters it is two writers through one
`wseq` counter, which is a race the ring cannot detect: both would read the same
sequence number and one record would be overwritten.

The Rust daemon has one publishing thread. Connection threads decode and hand
records over a bounded channel:

```text
  accept --> connection thread --decode--> bounded channel --> publisher
                                                                  |
                                              ring <--------------+
                                              WAL  <--------------+
                                              cache <-------------+
```

That gives the single-writer rule for free and puts the ring, the log and the
cache in one place, written in one order, for every record.

### 3.5 Two kinds of backpressure, and why they differ

The channel between a connection and the publisher is bounded and a full channel
**blocks the reader**. The cache's fan-out queues are bounded and a full queue
**drops and counts**.

That is not an inconsistency. The ingest path is durable: a record dropped there
is not in the log and cannot be recovered, so blocking is correct — the reader
stops, TCP's window closes, and the sender slows down. The fan-out path is not
durable: a subscriber that is 200 ms behind wants the current price, not the
history it missed, so the right answer is to drop the oldest and record it.

## 4. Flow

One record's life, with the code that handles each step:

```text
  Python adapter                     rust/ob-cli/src/bin/feedd.rs
      | 72 bytes over TCP
      v
  ingest::serve                      read, accumulate, codec::decode_batch
      | Msg over a bounded channel (blocks when full: durable path)
      v
  publisher thread                   m.ts_ns = clock::now_ns()
      |                              m.fill_correlation(counter)
      +--> ring.publish              release store of wseq
      +--> wal.append                fsync every 32 records
      +--> cache.publish             book.apply, then fan out
             |                        best_bid/best_ask side tables updated
             +--> subscriber queues   bounded, DropOldest by default
                    |
                    v
             Subscriber::try_recv     stage 3 latency, pressure sample
```

## 5. Configuration

The same `config/*.json` files drive both implementations. Keys the C++ loader
does not know are read by the Rust one and ignored by the C++ one, so one file
serves both:

| Key | Default | Meaning |
|-----|---------|---------|
| `ring_capacity` | 4096 | Ring slots. A power of two. |
| `sync_policy` | `data` | `none`, `data` (`fdatasync`) or `full` (`F_FULLFSYNC` on macOS). |
| `cache_symbols` | 4096 | Symbol-table capacity before it refuses. |
| `cache_shards` | 4 | Shards. Rounded up to a power of two. |
| `cache_depth` | 16 | Levels kept per side, per symbol. |
| `cache_subscribers` | 2 | Subscriber queues created. |
| `cache_queue` | 8192 | Records each subscriber queue holds. |
| `cache_policy` | `drop-oldest` | `drop-oldest`, `drop-newest`, `spin`, `credit`. |
| `pressure_elevated_ns` | 1000000 | Lag at which a subscriber reports elevated. |
| `pressure_saturated_ns` | 10000000 | Lag at which it reports saturated. |
| `expected_interval_ns` | 100000 | Expected record interval, for the coordinated-omission correction. |
| `watch_path` | beside `bus_path` | The file the terminal writes to say what to subscribe to. Derived from `bus_path` in both implementations. |
| `tickers_path` | `config/tickers.txt` | The symbols the picker suggests from. |

`python/obctl.py --impl rust` drives the Rust binaries from the same commands as
the C++ ones. The Python adapter is shared: it speaks the wire protocol to either.

### 3.6 One websocket, several tickers

The adapter holds one combined Binance stream for the whole watch set rather than
a connection per symbol. Each payload names its own stream, so one connection can
carry fifty pairs and every record still says which pair it belongs to. The
adapter polls the watch file between frames and reconnects when the set changes,
which is what makes the subscription follow what is being looked at instead of
being fixed at startup.

**An unknown stream name can break that connection for every symbol**, which is
why the terminal refuses a symbol the catalog does not list and names a `force`
escape rather than trying it. Ledger OPT-036.

The eight-byte symbol field is the other limit: any pair whose name is longer —
`1000SHIBUSDT`, `1000SATSUSDT` — cannot be carried, and the catalog loader refuses
one rather than truncating it, because `1000SHIBUSDT` and `1000SATSUSDT` both
truncate to `1000SHIB` and a reader would show one pair's prices under the other's
name. Ledger OPT-035.

### 3.7 The terminal owns its input line

`tui` puts the tty into non-canonical mode with echo off, keeps the input buffer
itself, and draws it as the last row of every frame. The first version read whole
lines and cleared the screen on every frame, which erased the terminal's own echo:
the commands worked and were invisible.

The mode is restored on every exit path, including from a signal, because a
program that leaves a terminal without echo and line editing makes the shell look
broken and the only recovery is `stty sane`. `python/test_tui_input.py` asserts
all of that through a pseudo-terminal, and `OB_TUI_TEST_BINARY` points it at any
terminal binary so the test can be shown failing against an unfixed one.

The C++ terminal in `src/tui.cpp` still clears the screen on every frame and so
still has this behaviour. `make rust-start` is the window that does not.

## 6. Cross-references

- `docs/CONSTRAINTS.md` §1 and §1.1 — the stack, and the shared-format rule
- `docs/design/core-architecture.md` — the C++/Python implementation
- `docs/design/ticker-cache.md` — the cache
- `docs/design/vec-optimization.md` — the kernel measurements
- `docs/optimization/README.md` — the optimization ledger
- `docs/runbooks/rust-build-failures.md` — what to do when it will not build
