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
| OPT-026 | Three review attempts on one 56-file, ~14,000-line change failed for capacity reasons: an agent turn limit, a provider balance error, and a `max_tokens` truncation after 383K input tokens. A scoped review of one surface completed in 24 minutes and found seven real defects. | A review that fails for capacity reasons is not a review, and a gate that cannot tell the difference reports a clean pass on unreviewed code. Size the review to the diff, and when a reviewer cannot complete, record that rather than a pass. |
| OPT-027 | The Rust CI job ran `cargo fmt --manifest-path rust/Cargo.toml --check`, which fails with "Failed to find targets" because `cargo fmt` needs `--all` to resolve targets from a manifest path. `make rust-test` had the working form, so every local check was green while CI failed on every run, across three merged pull requests. | A command written twice will drift, and the copy that drifts is the one nobody runs by hand. Have CI call the same entry point a developer calls. Verifying locally is not verifying the pipeline: check the pipeline's own result before merging. |
| OPT-028 | The audit passed locally and failed in CI with 0 vector operations for both required kernels. Cross-emitting x86-64 IR on the same source reproduced it. The threshold is the instruction set: baseline x86-64 gives 0 and 0, x86-64-v2 gives 12 and 0, x86-64-v3 gives 12 and 8, and aarch64 at its default gives 12 and 8. | Vectorization is a property of the source **and** the target, and a machine-specific measurement is not a portable claim. An oracle must name the target it measured, and an audit that raises the target level has to say so, or it reports a code defect where the real difference is instruction-set cost modelling. |

### Correctness found by independent review

Seven findings came from a single independent review pass, run because the
harness's own review gate ran out of turns on a diff this size. They are grouped
here because the lesson is shared: **the defects that survive a green test suite
are the ones where a value is present and means something other than what the
consumer assumes.**

| id | Finding | Lesson |
|----|---------|--------|
| OPT-019 | The flow ring's consumer copied a slot and then claimed it. The `DropOldest` path advanced that same tail and overwrote the slot, so the copy raced with a write. The copy was discarded when the claim lost, so no wrong value escaped, but the race is undefined behaviour. | Discarding a torn value is not the same as not reading it. A lock-free queue needs a claim-before-copy discipline or a per-slot sequence. The sequence costs about 0.6 ns per push. |
| OPT-020 | The coordinated-omission backfill loop is proportional to the observed duration, and a caller that passed `0` where a timestamp belongs made that duration the whole monotonic clock: ten billion iterations inside one publish. | A correction whose cost scales with its input needs a ceiling, because the input comes from a clock. An unbounded loop over a caller-supplied value is a denial of service with a plausible name. |
| OPT-021 | `Publisher::intern` grew the symbol table and not the snapshot lane, so pre-registering symbols and then snapshotting panicked. Pre-registering is `intern`'s documented use. | When a method grows one structure it must grow every structure indexed by the same key. |
| OPT-022 | The event carried the raw monotonic **stamp** and the subscriber's tracker treats its argument as a **duration**. A stamp passes the plausibility ceiling after one second of uptime, so every stamped event was counted implausible and the end-to-end figure silently became stages 2 and 3. | An absolute reading and a duration are not interchangeable, and a plausibility check written for one silently rejects the other. |
| OPT-023 | `MS_SYNC` is `0x10` on Darwin and `4` on Linux. Passing Darwin's value to Linux's `msync` is `EINVAL`, so the flush silently did not happen. | Two platforms sharing a header's shape do not share its constants, and a flag whose wrong value is `EINVAL` fails silently. |
| OPT-024 | `expected_interval_ns` was read from the config, stored on the publisher, and never used. The documented correction never ran. | A configuration key that is parsed, validated and never read is worse than a missing feature, because the docs say the behaviour is on. |
| OPT-025 | The same shape as OPT-024, counted: three separate features were parsed and never applied, each failing silently with a plausible default. | Silent defaulting is the failure mode of every optional setting. A test that the option *changes an observable value* is the only thing that catches it. |

### Terminal behaviour

| id | Finding | Lesson |
|----|---------|--------|
| OPT-029 | The terminal book cleared the whole screen on every frame, which erased the terminal's own echo of the input line. The keystrokes still reached the program — the tty's line buffer is not affected by drawing over the screen — so the commands worked and were invisible. Fixed by owning the line: non-canonical mode with echo off, the buffer in the program, drawn as the last row of the frame, with the mode restored on every exit path including a signal. | A full-screen refresh destroys the terminal's echo of the input line, and no test without a terminal can see it. A test for this needs a pseudo-terminal, and it has to be shown failing against the unfixed binary before it is believed. |

### Tooling and the run path

| id | Finding | Lesson |
|----|---------|--------|
| OPT-030 | `obctl` invoked `cargo` by name. A user whose terminal predated the `~/.cargo/env` line in their profile had a working toolchain that was invisible to the process, and got a Python traceback ending `FileNotFoundError: ... 'cargo'`. It now resolves the tool on PATH and in the usual install directories, and reports clearly when it is genuinely absent. | A tool that invokes another tool must not assume PATH: rustup reaches PATH through a profile file a stale terminal has never read. When you cannot find an executable, say what was needed and where you looked — a subprocess traceback names the missing file and nothing a reader can act on. |

### Observability

| id | Finding | Lesson |
|----|---------|--------|
| OPT-031 | The terminal's lag was a count of records. It now reports the count **and** the span of feed time those records cover, plus the age of the newest published record. | A backlog in messages is not a latency. Every queue that reports a depth should also report the time that depth covers, because only the time can be compared against a threshold without knowing the arrival rate — and data staleness belongs beside it, since caught up on a dead feed is not caught up on a live one. |

### Verification habits

| id | Finding | Lesson |
|----|---------|--------|
| OPT-032 | A local sweep piped `make rust-test` through `grep -c` and reported nine passing suites while the target was exiting 2 on a formatting failure. Without `set -o pipefail` a pipeline reports the **last** command's status, so the chain continued. CI, which runs the target directly, caught it. | Appending a filter to a check turns it into a check that cannot fail. Use `set -o pipefail`, and prefer running a gate directly over parsing its output. A green local sweep beside a red pipeline is the signature of a filter standing between you and the exit code. |

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

## Reviewing a change this size

The ledger's findings came from two sources: measurements taken while writing the
code, and one independent review pass. The review is worth its own note because
the harness's own review gate did not produce it.

Three attempts failed on this change, in three different ways:

| Attempt | Failure |
|---------|---------|
| `review2fix` external reviewer | `Error: max turns reached` after 35 minutes |
| A claims-versus-code reviewer, first run | `429 Too Many Requests: Insufficient balance` on the provider account |
| The same reviewer, second run | `response truncated by max_tokens` after 383,195 input tokens across 11 model calls |

The first and third share a root cause: a 56-file, ~14,000-line diff is too large
for one agent review pass. The second is a billing problem that happens to look
like a review result.

What worked was a **scoped** review: one agent, one surface (the Rust library), a
bounded output budget, and an explicit instruction to report what it checked as
well as what it found. It completed in 24 minutes and returned ten issues, seven
of them real defects, all recorded as OPT-019 to OPT-025.

The rule this suggests, and the reason it is written down here: **a review that
fails for capacity reasons is not a review.** A gate that cannot tell the
difference reports a clean pass on unreviewed code. When a reviewer cannot
complete, the honest record says so.

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
