# Testing — orderbook-lab

> Last updated: October 2026

## 1. Purpose

What is tested, what each layer is for, and the one rule that keeps the numbers in
this repository honest.

## 2. The layers

| Layer | Command | Count | What it covers |
|-------|---------|-------|----------------|
| Rust unit tests | `make rust-test` | 175 + 38 + 9 | the library, the CLI, and the ingest reader |
| C++ unit tests | `make test` | 1 binary | the book, the ring and the log |
| Python tests | `make test` | 53 | the control script's process handling, the run directory, and the adapter's frames |
| Terminal behaviour | `make rust-test` | 5 | the book's input line, its terminal mode, the picker, two books in one ring, and re-attaching to a restarted feed, through a real pty |
| Fixture e2e, C++ | `make e2e` | — | the fixture to a book and a log, through the C++ daemon |
| Fixture e2e, Rust | `make rust-e2e` | — | the same fixture through the Rust daemon |
| Interop | `make rust-interop` | — | each implementation's daemon with the other's reader |
| Vectorization audit | `make rust-vec-audit` | 2 kernels | the compiler emitted vector operations |
| Soak | `make rust-soak` | — | 1000 symbols through the cache, with loss counters |

`docs/VERIFY.md` is the registry: one row per check, with the command and the
expected result, so a verify run is a list of outcomes rather than a judgement.

## 3. What each layer is for, and what it is not

### 3.1 Unit tests, 175 of them

Concentrated where the reasoning is, not spread for coverage. The four areas that
carry the most:

- **The wire format.** Offsets asserted at compile time, a round trip, and a
  rejection case for each way a record can be damaged.
- **The ring and the queue.** Ordering, a lapped reader, a damaged slot, an
  eviction, and a two-thread run. The eviction test asserts that every value
  returned was one that was pushed *and* that the values are non-decreasing,
  which is what catches a torn read.
- **The book.** A depth5 picture replacing the previous one, a zero size deleting
  a level, a crossed book, and a reset not moving the sequence.
- **The measurements.** Exact count/sum/min/max, bucket boundaries, the
  coordinated-omission correction, the backpressure hysteresis, and the slippage
  decomposition.

A test that would have passed before the fix is not a regression test. Five of the
tests added after the independent review fail without their fix, and each names
the defect it covers in its comment.

### 3.2 The fixture e2e

`e2e/fixture.jsonl` is three top-of-book pictures. Both implementations replay it
and both must produce a book with levels on each side and a log with records:

```
bids=2 asks=2 seq=19 wal=20
```

The same fixture, the same checker, two implementations. That is the point: the
e2e is a statement about the path, not about one implementation of it.

### 3.3 Interop, which is the strongest test here

`make rust-interop` runs the C++ daemon and has the **Rust** reader read the ring
and log it wrote, then the reverse. It proves the shared-format claim rather than
asserting it, and it is the test that would fail if someone changed an offset in
one implementation and not the other.

`docs/CONSTRAINTS.md` §1.1 makes that a rule; this target is what enforces it.

### 3.4 The vectorization audit

`rust/scripts/vec-audit.sh` compiles the library, reads the emitted LLVM IR, and
counts the vector operations in each named kernel. It exits non-zero when a kernel
that should vectorize did not.

It also prints the kernels **documented as scalar**, with the reason, so silence
about them is a decision rather than an oversight.

Two things it had to learn, both recorded in the ledger:

- Emit the IR with `lto` off. Under fat LTO, `--emit=llvm-ir` writes pre-LTO IR,
  and an audit of that file reported zero vector operations for the whole crate
  (OPT-001).
- Name the target. The same source vectorizes on aarch64 and not on baseline
  x86-64; `mids_into` needs SSE4.2 and `top` needs AVX2 (OPT-028).

### 3.5 The terminal test

`python/test_tui_input.py` opens a pseudo-terminal, runs the book on it, types
into it, and reads back what the book drew. It exists because the bug it covers
cannot be seen without a terminal: the first version cleared the whole screen on
every frame, which erased the terminal's own echo of the input line, so commands
worked and were invisible.

It asserts four things: what is typed appears in a frame, the terminal is in
non-canonical mode with echo off while the book runs, the command takes effect,
and the terminal is restored when the book exits — including when it exits on a
signal.

It also refuses to test a stale artifact. The test drives a compiled binary, so it
checks the binary is newer than the newest file under `rust/*/src` and skips with a
message if it is not. A stale binary is worse than a missing one: missing skips,
and stale fails or passes for a reason that has nothing to do with the source. That
happened — the frame assertions were added, a checkout had not been rebuilt, and
the new test failed against code that predated the change, which looked like
flakiness until the build date was checked. `make rust-test` depends on
`rust-build`, so it always tests what is in the tree; `make test` does not, so it
skips these two on a stale tree and says so.

`OB_TUI_TEST_BINARY=<path>` points it at any terminal binary. That is how it was
checked against one that still has the old behaviour: pointed at `build/tui` it
fails on `ICANON is still set`, and pointed at the Rust one it passes. **A test for
a bug that was fixed has to be shown failing before it is believed**, and the
override is what makes that repeatable rather than a claim.

One of the five cases is a recovery, and it is worth saying why the assertion is
the counter rather than the prices. `test_the_window_re_attaches_when_the_feed_restarts`
feeds a window, stops the daemon, starts a second daemon over the same ring, and
requires the window to come back on its own with `reattaches=1`. A window that did
*not* re-attach still appears to recover once the new run has published as many
records as the old cursor had reached, because the slot's sequence then matches
and `consume` carries on from the middle of the stream. The prices come back; only
the counter shows that the start of the run was lost. Disabling the check and
re-running is how that was established, and the same run is what makes the pricing
assertions worth keeping.

## Session isolation

Every config keeps its pid files and process logs in its own `var/run/<config>/`,
because a test that stops processes must not be able to reach a session somebody
has open. `make e2e`, `make rust-e2e` and `make rust-interop` use
`config/test.json`, which has its own ring, WAL, log, watch file and port; the live
targets use `$(CONFIG)`, which is `config/dev.json` by default and can be
overridden per window.

This is a test-suite rule rather than a convenience. A harness run of `make e2e`
from a second worktree stopped a live window's feedd and adapter and left it on
`feed=stalled` with a 39-second staleness, and the window could not recover even
after the feed was started again. Both halves are fixed and both have a test:
`RunDirectoryTest` in `python/test_obctl.py` asserts that two configs share no pid
file, that a stop on one leaves the other's process running, and that the two
shipped configs share no path or port; the terminal test above asserts the
recovery. `docs/optimization/ledger.jsonl` OPT-038 has the evidence.

### 3.6 The soak

`obsoak` drives the cache with a thousand symbols and reports what it cost:
latency by stage, slippage split into its latency and depth causes, and the
backpressure state. It is a test in the sense that it fails loudly — non-zero
`dropped` or `evicted` means a consumer is losing data — and a measurement in the
sense that its numbers are the ones quoted in the design docs.

## 4. The rule

**A performance claim carries the command that produced it.**

Every number in `docs/design/vec-optimization.md` and
`docs/design/ticker-cache.md` comes from a line in
`docs/optimization/ledger.jsonl`, and every line has an `evidence` field naming
the command. `rust/scripts/opt-record.sh` appends a measurement from the benchmark
itself, so a number cannot be typed in by hand.

The corollary, learned twice: **a benchmark whose output is never read measures
nothing.** The first version of `vecbench` reported a 512-element prefix sum as
33 nanoseconds because the writes had been deleted as dead code, and a
profile-guided build reported a 1000-symbol snapshot at 16 nanoseconds because
batching made the call loop-invariant and the optimiser hoisted it out. Both are
ledger entries (OPT-006, OPT-017).

## 5. What is not tested

- **A live exchange connection.** The fixture is the required path; a live run
  needs the network and is not in CI. `docs/research/2026-09-21-orderbook-lab.md`
  records that decision.
- **The terminal's appearance.** `tui` is checked by running it, not by a
  snapshot test. A frame is a string; asserting its bytes would break on every
  layout tweak and catch nothing.
- **Cross-host latency.** The measurement assumes one host and one clock, and the
  design docs say so rather than testing a claim that cannot hold.
- **Fuzzing.** The codec's rejection cases are enumerated by hand. A fuzzer would
  be a real addition to a parser that reads a socket; it is not here because the
  framing is fixed-width and the input space is small enough to enumerate.

## 6. Running everything

```bash
make test && make rust-test && make rust-vec-audit && make e2e && make rust-e2e && make rust-interop
```

On a clean checkout that is the whole suite. `make rust-soak` is separate because
it is a measurement rather than a pass/fail check.

## 7. Cross-references

- `docs/VERIFY.md` — the registry of checks
- `docs/design/vec-optimization.md` — the kernel measurements and the oracle
- `docs/optimization/README.md` — the ledger schema
- `docs/CONSTRAINTS.md` §8 — testing conventions
