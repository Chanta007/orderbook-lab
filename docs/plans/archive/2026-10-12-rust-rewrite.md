## PR Metadata

- **Type**: feat
- **PRs**: #7 (the implementation), #8 (session artifacts and the review-gate record)
- **Merge commits**: `cdb1199` (implementation), `6955318` (follow-up)
- **Research**: docs/research/2026-10-12-rust-cache-architecture.md, docs/research/2026-10-12-latency-slippage-measurement.md, docs/research/2026-10-12-rust-vectorization.md
- **Research summary**: three parallel deep-research runs, 15 agents, cited reports. Per-symbol book structures and sharding at a thousand symbols; clock sources and the slippage/markout definitions; how to observe and shape LLVM's vectorizer.
- **Evidence**: `make test` OK; `make rust-test` 167 + 12 tests with fmt and clippy clean; `make rust-vec-audit` PASS; `make e2e` and `make rust-e2e` both `bids=2 asks=2 seq=19`; `make rust-interop` PASS in both directions; `obsoak --symbols 1000 --ticks 200` at 5.2-7.2M records/s with zero drops; OTKP `durable-term` skipped with a recorded reason.
- **Review**: the harness review gate could not complete on a diff this size (three failures recorded as OPT-026). A scoped independent review found ten issues, seven of them real defects, all fixed with regression tests (OPT-019 to OPT-025).
- **Commit**: feat: a Rust sibling implementation, and the optimization ledger that proves it
- **Files changed**: `rust/` (new), `docs/optimization/` (new), `docs/design/` (3 new docs), `docs/research/` (3 new records), `docs/VERIFY.md`, `docs/runbooks/rust-build-failures.md`, `CLAUDE.md`, `docs/CONSTRAINTS.md`, `docs/HARNESS.md`, `README.md`, `Makefile`, `.github/workflows/ci.yml`, `python/obctl.py`, `python/test_obctl.py`, `.gitignore`

# Active Plan: Rust sibling implementation and the optimization ledger

| Field | Value |
|-------|-------|
| Type | feat |
| Status | verified-and-merged |
| Short slug | rust-rewrite |
| Created | 2026-10-12 |
| Branch | feature/rust-rewrite |
| Worktree | .claude/worktrees/rust-rewrite |
| Design docs | docs/design/rust-implementation.md, docs/design/ticker-cache.md, docs/design/vec-optimization.md |
| Research | docs/research/2026-10-12-rust-cache-architecture.md, docs/research/2026-10-12-latency-slippage-measurement.md, docs/research/2026-10-12-rust-vectorization.md |
| Goal | docs/goals/.active-goal.md (smartdeploy-to-dev) |

## Context

The repository holds a C++ and Python market-data lab: a Python adapter, a C++
feed daemon, a shared-memory ring, a write-ahead log and a terminal book. The
request was a second implementation in Rust, kept alongside the original rather
than replacing it, delivered in two stages: ultra-clean design first, then
low-level optimization of the kind described in an article about getting the
compiler to vectorize safe Rust.

The Rust version also had to add a capability the original lacks: an in-process
ticker cache for hundreds to thousands of symbols that publishes to several
subscribers and tracks latency, slippage and backpressure as reported quantities.

A later request added the requirement that the optimization work be **tracked and
published** as a machine-readable ledger, because it is the starting point for a
compiler-optimization playbook for the harness.

## Steps

### 1. Worktree and rebase — `done`

Fast-forwarded `dev` to `origin/dev` (11 commits), then created
`.claude/worktrees/rust-rewrite` on `feature/rust-rewrite` from `origin/dev`.

### 2. Ultra deep research — `done`

Three parallel `deep-research` workflow runs, 15 agents, cited reports committed
under `docs/research/2026-10-12-*.md`: cache architecture for a thousand symbols,
latency/slippage/backpressure measurement, and rustc/LLVM auto-vectorization.

### 3. The Rust workspace, stage 1 — `done`

Two crates, zero external dependencies. `ob` holds the wire format, the bus, the
books, the cache and the three measurements; `ob-cli` holds five binaries and the
ingest reader. 161 unit tests plus 12 in the CLI crate.

The wire format is the same 72 bytes as the C++ implementation, with the offsets
asserted at compile time, and the ring header is the same 16 bytes, so either
implementation reads the other's ring.

### 4. The ticker cache — `done`

Sharded books with a flat snapshot lane, lock-free SPSC fan-out with four
policies for a full queue, staged latency with coordinated-omission correction,
a latency/depth slippage decomposition with markouts, and a three-state
backpressure machine. `obsoak` measures 1000 symbols at 7.1M records/s with zero
drops.

### 5. Stage 2, low-level optimization — `done`

`rust/scripts/vec-audit.sh` is the oracle: it compiles the library, reads the
emitted LLVM IR and asserts the required kernels emitted vector operations. Two
kernels now vectorize. Four measured changes were kept, two were reverted, two had
no effect, and two bugs in the measurement itself were found and fixed.

### 6. The optimization ledger — `done`

`docs/optimization/ledger.jsonl` holds 14 findings, each with the command that
produced its numbers, a before/after pair, a verdict and the general rule.
`rust/scripts/opt-record.sh` appends a measurement run from the benchmark so a
number in the ledger always has a command behind it.

### 7. Harness files — `done`

`CLAUDE.md`, `docs/CONSTRAINTS.md` §1/§1.1/§8/§14, `docs/HARNESS.md` §7,
`README.md`, the `Makefile` (six new targets), `.github/workflows/ci.yml` (a Rust
job), `python/obctl.py` (`--impl cpp|rust`), and a new runbook.

### 8. Verification — `done`

`make test`, `make e2e` (C++ path unaffected), `cargo test`, `cargo fmt --check`,
`cargo clippy -- -D warnings`, `make rust-e2e`, `make rust-interop`,
`make rust-vec-audit`, and the 1000-symbol soak.

### 9. PR, review and merge — `pending`

PR to `dev`, review2fix for the head, smartdeploy merge, then an OTKP e2e attempt.
