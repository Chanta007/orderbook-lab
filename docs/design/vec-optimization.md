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

| Kernel | What changed | Before | After | Verdict | Ledger |
|--------|--------------|--------|-------|---------|--------|
| `codec::stream_records` | check the last record first instead of scanning | 1979 ns | 16.4 ns | kept, **121x** | OPT-008 |
| `cache::Publisher::mids_into` | flat side tables + a mask instead of an `if` | 3676.7 ns | 260.7 ns | kept, **14.1x** | OPT-002, OPT-013 |
| `book::Levels::cumulative` | blocked three-pass prefix sum | 6.9 ns | 38.3 ns | **reverted**, the block was 5.5x slower | OPT-004 |
| `book::Levels::top` | `zip` instead of an index loop | 13.3 ns | 13.2 ns | no effect, kept for readability | OPT-011 |
| `codec::decode_batch` | none | 14586 ns / 4096 records | — | scalar, memory-bound | §5.1 |
| `flow::Producer::push` | none | 3.6 ns | — | scalar, one record per call | §5.3 |

### 4.1 The constant-time win that beat every vector version

`codec::stream_records` answers "how many whole records does this buffer hold".
The scan version walked all 4096 records of a 64 KiB read: 1979 ns. A buffer read
from a socket cannot have a hole in the middle, so the only defect possible is a
torn tail, and checking the **last** record answers the question in the common
case. 16.4 ns.

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
   to 260.7 ns.

The lesson is in the ordering: measured after the layout change alone, the result
looked like a 6% win and a wasted afternoon. Measured after both, it is 14.1x.

### 4.4 The suspected blocker that was not

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
record, plus the compare-and-exchange on the drop path.

Measured cost: **3.6 ns** per push.

### 5.4 `book::Levels::cumulative` — a loop-carried dependency

See §4.2. Measured cost of the shipped sequential form: **6.9 ns**.

## 6. Build settings that were considered

| Setting | Status | Why |
|---------|--------|-----|
| `lto = "fat"`, `codegen-units = 1` | **on** | Cross-crate inlining across the workspace; costs build time, buys nothing measurable in these kernels but nothing else either. |
| `-C target-cpu=native` | **not used** | The binaries are meant to run on the machine that built them, but a benchmark that only means something on one CPU is not a benchmark. Revisit if a deployment target is pinned. |
| Profile-guided optimization | **not used** | `rust/scripts/pgo.sh` exists and documents the procedure. It needs `llvm-profdata` from `llvm-tools-preview` and a representative workload; `obsoak` is the obvious profile source. Parked until a kernel is identified that PGO would move. |
| BOLT | **not used** | Not available for macOS. Relevant only if the Linux path becomes a deployment target. |
| `std::simd` | **not used** | Nightly only. The one kernel where it would pay is the prefix sum in §4.2, and the sequential form already runs at one instruction per element. |

## 7. How to re-run everything

```bash
make rust-vec-audit     # the oracle: which kernels emitted vector ops
make rust-bench         # the kernel measurements
sh rust/scripts/vec-audit.sh --verbose   # and show the vector instructions
```

## 8. Cross-references

- `docs/optimization/README.md` — the ledger schema and the findings grouped by lesson
- `docs/optimization/ledger.jsonl` — the machine-readable record
- `docs/design/rust-implementation.md` — the workspace and its layers
- `docs/design/ticker-cache.md` — the cache these kernels serve
- `docs/CONSTRAINTS.md` §14 — the performance targets and the measurement rule
