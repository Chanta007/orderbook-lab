# Vectorization and low-level optimization

> Last updated: October 2026

## 1. Purpose

This document is the reference for how the Rust implementation's hot kernels were
measured and reshaped, and what the compiler actually emitted for each one. It is
the prose companion to `docs/optimization/ledger.jsonl`, which is the
line-by-line history of attempts including the ones that failed.

The rule this document exists to enforce: **a performance claim carries the
command that produced it.** Where a number appears here, the command is in the
ledger entry it came from.

## 2. Key files

| File | Responsibility |
|------|---------------|
| `rust/scripts/vec-audit.sh` | The oracle. Compiles the library and asserts the required kernels emitted vector operations. |
| `rust/scripts/opt-record.sh` | Runs the benchmark and appends a `measure` entry to the ledger, carrying the command. |
| `rust/ob-cli/src/bin/vecbench.rs` | The kernel benchmark, including the rejected shapes kept for comparison. |
| `docs/optimization/ledger.jsonl` | One JSON object per finding. |
| `docs/optimization/README.md` | The ledger schema and the findings grouped by lesson. |

## 3. The method

The loop is the one from the article that prompted this work: write idiomatic safe
Rust, compile, **read what the compiler emitted**, profile, reshape where it
failed, repeat. Reading the emitted code is the step that is usually skipped, and
in this codebase it was the step that mattered most — twice it was the only thing
that revealed what was really happening.

```text
   write the obvious loop
        |
        v
   emit LLVM IR and count vector ops        <-- the oracle, not the eyeball
        |
        +--> vector ops present? --- yes --> measure, record, keep or revert
        |
        +--> none ------------------> find the blocker in the IR
                                          |
                                          v
                                      reshape, go back to the top
```

### 3.1 Emitting IR you can trust

The first version of the audit read the IR emitted under the release profile,
which sets `lto = "fat"`. Under fat LTO, `--emit=llvm-ir` writes the module
**before** the LTO pipeline runs, so the file is unoptimized. The audit reported
zero vector operations for every kernel in the crate — including one whose source
shape was known to vectorize.

The check that exposed it was a control: the same loop compiled standalone, with
`rustc -O --emit=llvm-ir`, produced forty vector operations. That discrepancy
means the measurement is wrong, not the compiler.

The audit now emits with `lto` off:

```sh
cargo rustc --release --lib -p ob --config 'profile.release.lto="off"' -- --emit=llvm-ir
```

With that one change the same crate reports **181** vector operations. The shipped
binaries are still built with LTO; what is being inspected is the vectorizer's
per-kernel decision, which is made before LTO merges anything. Ledger: OPT-001.

## 4. The kernels

Each row is a kernel that was reshaped, the shape it had, and what the change
measured. Numbers are from `vecbench` on an Apple silicon machine with a warm
cache, `--iters 4000 --levels 32` unless noted.

| Kernel | What changed | Shipped | Compared with | Ratio | Verdict | Ledger |
|--------|--------------|---------|---------------|-------|---------|--------|
| `codec::stream_records` | check the last record first instead of scanning | 16.3 ns | 2018 ns | **124x faster** | kept | OPT-008 |
| `cache::Publisher::mids_into` | flat side tables **and** a mask instead of an `if` | 407.8 ns | 3315 ns | **8.1x faster** | kept | OPT-002, OPT-005, OPT-013 |
| `book::Levels::cumulative` | sequential loop, not a blocked three-pass prefix sum | 7.6 ns | 36.7 ns | **4.8x faster than the blocked form** | reverted | OPT-004 |
| `book::Levels::top` | `zip` instead of an index loop | 13.2 ns | 13.9 ns | 1.04x, i.e. no difference | no effect | OPT-011 |
| `codec::decode_batch` | none | 17412 ns / 4096 records | — | — | scalar, memory-bound | §5.1 |
| `flow::Producer::push` | a per-slot sequence, for soundness | 4.7 ns | — | — | scalar; the sequence cost about 0.6 ns | §5.3 |

**Read the ratio, not the nanosecond value.** Absolute timings on this machine
drift by tens of percent with system load — `decode` has been measured at 14.6 µs
and 20.3 µs for the same input on the same machine. A comparison taken in one
`vecbench` run is meaningful; a nanosecond value quoted from a different run is
not. The ratios above come from a single run of
`rust/target/release/vecbench --iters 4000 --levels 32`.

Reproduce any row with:

```bash
cargo build --release --manifest-path rust/Cargo.toml
rust/target/release/vecbench --iters 4000 --levels 32
```

### 4.1 The constant-time win that beat every vector version

`codec::stream_records` answers "how many whole records does this buffer hold".
The scan version walked all 4096 records of a 64 KiB read: 2018 ns. A buffer read
from a socket cannot have a hole in the middle, so the only defect possible is a
torn tail, and checking the **last** record answers the question in the common
case. 19.8 ns.

No amount of SIMD gets a scan to 121x. This is the finding worth carrying into any
codebase: **before vectorizing a scan, ask whether the scan is needed.**

The fast path was first applied *inside* the existing function, which a test
caught: the general contract allows a damaged record anywhere, and the test's
holed buffer returned 3 instead of 1. It is now two functions with different
names and different documented preconditions. Ledger: OPT-009.

### 4.2 The reshape that was rejected

The standard way to make a prefix sum vectorizable is to block it so the block
totals and the block-offset adds are independent passes. That was implemented and
measured at every size from 8 to 1024 levels.

It was **5.5x slower at every size**. The sequential loop runs at one `add` per
element, which is the latency of a single add instruction, so there is no slack
for a wider unit to win back, and the blocked form pays for three passes to reach
the same answer.

A prefix sum that is genuinely faster needs the within-block prefix computed with
lane shuffles, which in Rust means `std::simd` on nightly or hand-written
intrinsics. This crate takes neither. The rejected form stays in `vecbench` as
`experiment_cumulative_blocked` so the measurement stays reproducible.

### 4.3 The reshape that worked, and needed two changes

`mids_into` writes every symbol's midpoint in one pass. Two changes were needed
and neither was sufficient alone:

1. **The layout.** The original walked symbol ids and called a per-symbol
   accessor: shard arithmetic, two bounds-checked lookups and two `Option` chains
   per symbol. The publisher now keeps `best_bid` and `best_ask` in two flat
   arrays written once per book change, so the loop is three contiguous `i64`
   arrays. Measured alone: 3676.7 → 3260 ns, about 6%.
2. **The body.** `if priced { mid } else { 0 }` compiled to a **branch**, which
   put the two arms in different basic blocks. The loop vectorizer skipped it
   entirely. A mask — `-i64::from(bid != 0 && ask != 0)` combined with `and` —
   keeps the body in one block. Forty vector operations, and the measurement went
   from 3676.7 ns to 260.7 ns.

The lesson is in the ordering: measured after the layout change alone, the result
looked like a 6% win and a wasted afternoon. Measured after both, it is 14.1x.

### 4.4 Vectorization is target-dependent

The audit passed on Apple silicon and failed in CI with **0** vector operations for
both required kernels. Cross-emitting x86-64 IR on the same source reproduced it
locally, so it was not a CI quirk. Sweeping the x86-64 baseline level shows where
the threshold is:

| Target | `mids_into` | `top` |
|--------|------------|-------|
| aarch64, default | 12 | 8 |
| x86-64, baseline | **0** | **0** |
| x86-64-v2 (SSE4.2) | 12 | **0** |
| x86-64-v3 (AVX2) | 12 | 8 |

The code is identical in every row. What differs is the instruction set the cost
model is allowed to use, and on baseline x86-64 it declines. `mids_into` needs
SSE4.2; `top` needs AVX2.

The audit now prints the target triple it compiled for and raises the level to
`x86-64-v3` on x86-64, saying so, because auditing at the baseline would report a
defect in the code when the difference is in the target. The shipped build stays
at the default, so a reader on any x86-64 machine gets a binary that runs.

The lesson generalises past this repository: **a machine-specific measurement is
not a portable claim.** An oracle has to name what it measured.

### 4.5 The suspected blocker that was not

The `sdiv` in that same loop was the obvious second suspect, since vectorized
signed division has a high cost-model price. Replacing `/ 2` with `>> 1` changed
nothing: forty vector operations either way. The division came back, because
truncating division is the clearer statement of a midpoint and a shift would have
differed from it for a crossed book.

Ledger: OPT-003. A plausible cause that is not the cause is worth a line, or the
next person removes the division for no reason.

## 5. Kernels documented as scalar

Silence about a kernel is ambiguous, so each one that is *not* vectorized is named
here with its reason and its measurement. The audit prints this list too, so a
reader of its output sees a decision rather than an omission.

### 5.1 `codec::decode_batch` — strided

Records are 72 bytes apart, so the header of record `i` is at `i * 72`. A strided
walk does not vectorize; the loads a vector unit wants adjacent are 72 bytes
apart. Restructuring the wire format to a structure-of-arrays layout would make it
vectorizable and would break the shared format that the C++ and Python
implementations read. That trade was not taken.

Measured cost: **3.56 ns per record** (14586 ns for 4096 records). The loop is
memory-bound on 294 KiB of input, which is the real limit, not the arithmetic.

### 5.2 `book::Levels::fill` — early exit

The loop walks levels until the order is filled and stops. A data-dependent exit
is incompatible with vectorization, and it is the right shape here: the common
case is a fill at the touch, which exits on the first level.

Measured cost: **16.5 ns** for a one-unit fill over a 32-level side.

### 5.3 `flow::Producer::push` — one record per call

There is no loop to vectorize. The cost is the release store that publishes the
record, the per-slot sequence store, and the compare-and-exchange on the drop
path.

Measured cost: **4.7 ns** per push. The slot sequence is about 0.6 ns of that, and
it is the price of removing a data race that an independent review found: the
consumer used to copy a slot before claiming it, and the `DropOldest` path could
overwrite the slot mid-copy. Ledger OPT-019.

### 5.4 `book::Levels::cumulative` — a loop-carried dependency

See §4.2. Measured cost of the shipped sequential form: **7.5 ns**.

## 6. Build settings that were considered

| Setting | Status | Why |
|---------|--------|-----|
| `lto = "fat"`, `codegen-units = 1` | **on** | Cross-crate inlining across the workspace; costs build time, buys nothing measurable in these kernels but nothing else either. |
| `-C target-cpu=native` | **not used in the shipped build** | The binaries are meant to run on the machine that built them, but a benchmark that only means something on one CPU is not a benchmark. The audit raises the level on x86-64 to `x86-64-v3`, and says so, because the baseline cost model declines to vectorize both kernels. |
| Baseline `x86-64` as the shipped default | **kept, with the cost understood** | On a baseline x86-64 target neither required kernel vectorizes: `mids_into` needs SSE4.2 and `top` needs AVX2. That is a property of the target, not of the code — the same source vectorizes on aarch64 at its default. Ledger OPT-028 has the sweep. A deployment that cares can raise the level; the lab does not, because it has to run on whatever the reader has. |
| Profile-guided optimization | **measured, no effect** | `rust/scripts/pgo.sh` runs the full procedure: instrument, drive with `obsoak`, merge with `llvm-profdata`, rebuild with `-Cprofile-use`. On the kernels that matter it moved nothing measurable (ledger OPT-018). What it did do was expose a bug in the benchmark: the trained build reported a 1000-symbol snapshot at 16 ns, because batching made the call loop-invariant and the optimizer hoisted it. The script stays so the test is repeatable when a kernel is added that PGO might help. |
| BOLT | **not used** | Not available for macOS. Relevant only if the Linux path becomes a deployment target. |
| `std::simd` | **not used** | Nightly only. The one kernel where it would pay is the prefix sum in §4.2, and the sequential form already runs at one instruction per element. |

## 7. How to re-run everything

```bash
make rust-vec-audit                      # the oracle: which kernels emitted vector ops
sh rust/scripts/vec-audit.sh --verbose   # and show the vector instructions

cargo build --release --manifest-path rust/Cargo.toml
rust/target/release/vecbench --iters 4000 --levels 32   # the kernel measurements
rust/target/release/vecbench --json                     # the same, machine-readable

sh rust/scripts/opt-record.sh mids "a hypothesis"       # append a measurement to the ledger
sh rust/scripts/pgo.sh --symbols 500 --ticks 50         # the profile-guided build
```

## 8. What an independent review found

The harness's own review gate ran out of turns on a diff of this size, so a
separate independent review was run over the Rust library. It found ten issues,
seven of which were real defects in shipped code. Five of them were the same kind
of mistake: **a value that is present and means something other than what the
consumer assumes.**

| Finding | What it was |
|---------|-------------|
| OPT-019 | A lock-free copy that raced with a write. No wrong value escaped, but the race was undefined behaviour. Fixed with a per-slot sequence, at 0.6 ns per push. |
| OPT-020 | A correction loop whose cost scaled with a caller-supplied clock reading, which a benchmark turned into ten billion iterations. Bounded now, and the truncation is counted. |
| OPT-021 | A snapshot that read past a side table the symbol table had outgrown. A panic on the documented path. |
| OPT-022 | A subscriber handed a raw timestamp where it expected a duration, so every stamped event was counted implausible and the end-to-end figure was silently wrong. |
| OPT-023 | A platform constant that differs between Darwin and Linux, where the wrong value fails silently. |
| OPT-024, OPT-025 | Configuration and CLI options that were parsed and never applied. Four of them, each failing silently with a plausible default. |

Each fix has a regression test that fails without it. The lesson that generalises:
**the defects that survive a green test suite are the ones where a value is present
and means something other than what the consumer assumes.** A test that exercises
the happy path cannot see them; a test that asserts an option changes an observable
number can.

## 9. Cross-references

- `docs/optimization/README.md` — the ledger schema and the findings grouped by lesson
- `docs/optimization/ledger.jsonl` — the machine-readable record
- `docs/design/rust-implementation.md` — the workspace and its layers
- `docs/design/ticker-cache.md` — the cache these kernels serve
- `docs/CONSTRAINTS.md` §14 — the performance targets and the measurement rule
