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
| OPT-010 | Functions that were inlined into their callers have no `define` block in the IR, so a per-function grep reports them as missing. | An IR audit must distinguish "no vector ops" from "no function to look at". Report the two differently. |

### Vectorization

| id | Finding | Lesson |
|----|---------|--------|
| OPT-002 | `if priced { mid } else { 0 }` inside a loop compiled to a **branch**, splitting the body into two basic blocks. The loop vectorizer skipped it. A mask (`-i64::from(cond)` and an `and`) vectorized it: 40 vector operations. | A branch in a loop body is a vectorization blocker even when both arms are cheap. Reach for a mask before reaching for a rewrite. |
| OPT-003 | The same loop's `sdiv` was suspected as a second blocker and replaced with a shift. **No effect**: 40 vector operations either way. | Test one suspected blocker at a time. A plausible-sounding cause that is not the cause is worth a ledger line, because otherwise the next person removes the division for no reason. |
| OPT-004 | A blocked three-pass prefix sum was **5.2x slower** than the sequential loop at every size from 8 to 1024. | A loop-carried dependency is not automatically worth removing. If the sequential form already runs at one instruction's latency per element, there is no slack for a wider unit to win back. |
| OPT-005 | Giving the vectorizer contiguous arrays is necessary and not sufficient. The per-symbol accessor version (pointer chase through a thousand books) and the flat-array version (three contiguous `i64` arrays) both needed the branch removed before either vectorized. | Data layout gets the loop to the door; the loop body gets it through. Check both. |

### Measurement

| id | Finding | Lesson |
|----|---------|--------|
| OPT-006 | The benchmark reported a 512-element prefix sum as 33 ns because the output buffer was never read, so the optimizer deleted the writes. | Pass the kernel's output through `std::hint::black_box`. A benchmark whose output is dead code measures the loop overhead. |
| OPT-007 | `CLOCK_MONOTONIC_RAW` resolves to tens of nanoseconds here, so two kernels of 10 and 16 ns measured as the same number. Timing a batch of 64 calls and dividing put the measurement above the resolution. | Know your timer's resolution before you report a difference smaller than it. |
| OPT-011 | An early reading of "1.23x faster" for the `top` reshape turned out to be below the timer's resolution; with batched timing the same comparison is 1.01x, which is to say no difference. | A win smaller than the noise floor is not a win. Re-measure before publishing it. |

### Algorithmic wins that beat any vectorization

| id | Finding | Lesson |
|----|---------|--------|
| OPT-008 | Checking the last record first, instead of scanning every record, made the ingest torn-tail check **122x faster** (1979 ns to 16.4 ns) because the common case is one comparison. | Before vectorizing a scan, ask whether the scan is needed at all. Constant-time beats wide. |
| OPT-009 | The constant-time fast path was applied to a general-purpose function whose contract allowed a damaged record anywhere, and a test caught it returning 3 instead of 1. It was split into two functions: a scanning one for arbitrary buffers, and a precondition-carrying one for stream buffers. | An optimisation that narrows a function's contract belongs in a differently named function. Same name, new precondition is how a fast path becomes a bug. |

### Platform

| id | Finding | Lesson |
|----|---------|--------|
| OPT-012 | A socket accepted from a non-blocking listener **inherits** the flag on macOS and the other BSDs, and does not on Linux. The reader saw `WouldBlock` on its second read, treated it as a hang-up, and closed a working connection after the first buffer. | Set the mode you need explicitly on an accepted socket. A platform difference that shows up as "the peer disconnected" is worth a test per branch. |

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
