# Optimization ledger

A record of every performance change made to this repository: what was tried, what
the compiler emitted, what the measurement said, and what was reverted. It exists
so that a claim about performance can be checked rather than believed, and so that
the reusable part of each finding — the rule that generalises past this codebase —
can be lifted out later into a compiler-optimization playbook.

## Why a ledger and not a design doc

A design doc describes the shape of the system. This file describes the *history
of attempts*, including the ones that failed, and it has to be diffable and
machine-readable so that a future tool can read it. Design docs say what the code
is; the ledger says how it got there and what it cost.

The distinction matters most for the negative results. "We tried the standard
vectorization reshape and it was 5.2 times slower" is the single most useful line
in this file for anyone who reads it next, and a design doc has nowhere to put it.

## Files

| File | What it is |
|------|-----------|
| `ledger.jsonl` | One JSON object per line, one line per finding. The machine-readable record. |
| `README.md` | This file: the schema, the rules for adding a line, and the findings grouped by lesson. |
| `../../rust/scripts/vec-audit.sh` | The oracle. Compiles the library and asserts the required kernels emitted vector operations. |
| `../../rust/scripts/opt-record.sh` | Appends a measurement run to `ledger.jsonl` from the benchmark itself, so a number in the ledger has a command behind it. |

## Schema

Every line in `ledger.jsonl` is one JSON object with these fields:

| Field | Type | Meaning |
|-------|------|---------|
| `id` | string | `OPT-NNN`, assigned in order, never reused. |
| `date` | string | `YYYY-MM-DD`. |
| `kind` | string | One of `change`, `reject`, `measure`, `bug`, `tooling`. |
| `kernel` | string | The function or module under discussion, `module::Type::method`. |
| `hypothesis` | string | What was expected to be true before the change. |
| `change` | string | What was actually done, in one sentence. |
| `evidence` | string | The exact command that produced the numbers. |
| `before` | object or null | `{ "metric": string, "value": number, "unit": string }` or null when the finding has no before/after. |
| `after` | object or null | Same shape. |
| `verdict` | string | One of `kept`, `reverted`, `no-effect`, `inconclusive`. |
| `lesson` | string | The general rule. This is the field a playbook would be built from. |
| `tags` | array of string | Free-form, for grouping: `vectorization`, `codegen`, `measurement`, `platform`, `contract`. |

### Rules for adding a line

1. **A number needs a command.** `evidence` names the command that produced
   `before` and `after`. If there is no command, the finding is `inconclusive` and
   says so.
2. **`reject` and `no-effect` are as important as `kept`.** A ledger that only
   records successes is a marketing document.
3. **Never rewrite a line.** A later finding that contradicts an earlier one gets
   its own line and references the earlier `id`. The history of a wrong belief is
   part of the record.
4. **One finding per line.** If a change did two things, it is two lines.
5. **`lesson` is written for someone who has not read this repository.** Name the
   tool and the flag, not the local situation.

## The findings, grouped by lesson

### Codegen inspection

| id | Finding | Lesson |
|----|---------|--------|
| OPT-001 | `--emit=llvm-ir` under `lto = "fat"` writes **pre-LTO** IR, so an audit of it sees unoptimized code. The crate reported **0** vector operations; with `lto = "off"` the same crate reports **181**. | When you inspect generated code, first prove the file you are reading is the output you think it is. A control function with a known-good shape is the cheapest way to do that. |
| OPT-018 | Profile-guided optimization, built and measured properly, moved nothing measurable in the kernels that matter (247.6 ns to 242.5 ns). | PGO is worth testing and not worth assuming. What it did produce was the discovery of OPT-017, which was worth more than the optimization. |
| OPT-010 | Functions that were inlined into their callers have no `define` block in the IR, so a per-function grep reports them as missing. | An IR audit must distinguish "no vector ops" from "no function to look at". Report the two differently. |

### Vectorization

| id | Finding | Lesson |
|----|---------|--------|
| OPT-002 | `if priced { mid } else { 0 }` inside a loop compiled to a **branch**, splitting the body into two basic blocks. The loop vectorizer skipped it. A mask (`-i64::from(cond)` and an `and`) vectorized it: 40 vector operations. | A branch in a loop body is a vectorization blocker even when both arms are cheap. Reach for a mask before reaching for a rewrite. |
| OPT-003 | The same loop's `sdiv` was suspected as a second blocker and replaced with a shift. **No effect**: 40 vector operations either way. | Test one suspected blocker at a time. A plausible-sounding cause that is not the cause is worth a ledger line, because otherwise the next person removes the division for no reason. |
| OPT-004 | A blocked three-pass prefix sum was **5.2x slower** than the sequential loop at every size from 8 to 1024. | A loop-carried dependency is not automatically worth removing. If the sequential form already runs at one instruction's latency per element, there is no slack for a wider unit to win back. |
| OPT-005 | Giving the vectorizer contiguous arrays is necessary and not sufficient. The per-symbol accessor version (pointer chase through a thousand books) and the flat-array version (three contiguous `i64` arrays) both needed the branch removed before either vectorized. | Data layout gets the loop to the door; the loop body gets it through. Check both. |
| OPT-013 | The layout change and the body change together are **13.7x** (3370 ns to 246.6 ns for a 1000-symbol snapshot). Measured after the layout change alone the result looked like a 6% win. | Two changes that are individually useless can be jointly decisive. Measure after both, not after each. |

### Measurement

| id | Finding | Lesson |
|----|---------|--------|
| OPT-006 | The benchmark reported a 512-element prefix sum as 33 ns because the output buffer was never read, so the optimizer deleted the writes. | Pass the kernel's output through `std::hint::black_box`. A benchmark whose output is dead code measures the loop overhead. |
| OPT-007 | `CLOCK_MONOTONIC_RAW` resolves to tens of nanoseconds here, so two kernels of 10 and 16 ns measured as the same number. Timing a batch of 64 calls and dividing put the measurement above the resolution. | Know your timer's resolution before you report a difference smaller than it. |
| OPT-011 | An early reading of "1.23x faster" for the `top` reshape turned out to be below the timer's resolution; with batched timing the same comparison is 1.04x, which is to say no difference. | A win smaller than the noise floor is not a win. Re-measure before publishing it. |
| OPT-017 | Batching introduced a failure the single-call timer could not have. A kernel whose input does not change within a batch is loop-invariant, so the optimizer hoisted the call out and reused the result: a profile-guided build reported a 1000-symbol snapshot at **16 ns**, which is 0.016 ns per symbol. | Black-boxing the output stops a call being deleted; it does not stop it being hoisted. A batched timer needs the **input** marked opaque too. Sanity-check a batched number against the work it claims to do. |

### Algorithmic wins that beat any vectorization

| id | Finding | Lesson |
|----|---------|--------|
| OPT-008 | Checking the last record first, instead of scanning every record, made the ingest torn-tail check about **100x faster** (2018 ns to 19.8 ns) because the common case is one comparison. | Before vectorizing a scan, ask whether the scan is needed at all. Constant-time beats wide. |
| OPT-009 | The constant-time fast path was applied to a general-purpose function whose contract allowed a damaged record anywhere, and a test caught it returning 3 instead of 1. It was split into two functions: a scanning one for arbitrary buffers, and a precondition-carrying one for stream buffers. | An optimisation that narrows a function's contract belongs in a differently named function. Same name, new precondition is how a fast path becomes a bug. |
| OPT-014 | The CLI stored flag names with the leading dashes stripped and every call site looked them up with the dashes still on, so `--json`, `--levels`, `--symbols` and `--ticks` were silently ignored and every binary ran on its defaults. The tool ran, the numbers were plausible, and the option was never applied. | A lookup returning `Option` makes a name mismatch indistinguishable from an absent flag, and an optional flag's default hides it. Test that an option *changes the behaviour*, not that it parses. |

### Platform

| id | Finding | Lesson |
|----|---------|--------|
| OPT-012 | A socket accepted from a non-blocking listener **inherits** the flag on macOS and the other BSDs, and does not on Linux. The reader saw `WouldBlock` on its second read, treated it as a hang-up, and closed a working connection after the first buffer. | Set the mode you need explicitly on an accepted socket. A platform difference that shows up as "the peer disconnected" is worth a test per branch. |
| OPT-015 | `Ring::create` did a synchronous `msync` of the header. The header is written through a `MAP_SHARED` mapping, so a reader sees it through the page cache; the flush only bought crash durability for a file whose contents are deliberately not durable. It cost 2.25 s of ready-line latency under I/O load. | A synchronous flush of a mapping nobody needs durability from buys nothing. Ask what a barrier is protecting before paying for it. |
| OPT-016 | The first execution of a freshly linked binary on macOS pays for code signature validation: a hello-world Rust binary took **5.1 s** on its first run and 0.01 s after. A two-second ready-line ceiling turned that into "feedd did not start". | A ready-line handshake with a tight timeout is a bet on process start-up cost, and that cost is not constant. Make the failure message say whether the child died or is merely slow. |

## Reading the numbers

Absolute timings on this machine drift by tens of percent with system load: the
same `decode` kernel has measured 14.6 µs and 20.3 µs for the same input. A ratio
taken inside one `vecbench` run is meaningful; a nanosecond value quoted from a
different run is not. Where a `before` and an `after` were measured in separate
runs, the entry says so.

## The oracle

`rust/scripts/vec-audit.sh` is the check that keeps this file honest. It compiles
the library with LTO off, extracts each named kernel's function body from the
emitted IR, counts the vector operations in it, and exits non-zero when a kernel
that is supposed to vectorize did not.

It also prints the kernels that are **documented as scalar**, with the reason, so
that silence about them is a decision rather than an oversight.

Two rules learned the hard way and now encoded in the script:

- Emit the IR with `lto` off (OPT-001).
- Distinguish "no vector ops" from "no function matched" (OPT-010).
