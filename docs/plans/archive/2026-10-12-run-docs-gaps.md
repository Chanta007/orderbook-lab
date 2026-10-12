## PR Metadata
- **Type**: docs
- **Research**: docs/research/2026-10-12-run-docs-audit.md
- **Research summary**: A reader asked how to run the order book from a shell, and the answer was assembled from the code and from this machine. Checked claim by claim against README.md and docs/, six claims appear nowhere: the window needs a real terminal, what start prints without one, how to read the book without a window, the exact prod command, the scripts/ob wrapper, and where pid files and logs live. Everything else the answer covered is already documented. No behaviour changes; every line added describes what the code already does.
- **Evidence**: make test exit 0 (26 tests plus test_core); make e2e exit 0 (bids=2 asks=2 seq=19 wal=20); make start from a non-tty printed `start ok (no tui)` as documented; ./build/headless config/dev.json 1200 printed `bids=5 asks=5 seq=35 wal=56`; python3 scripts/ob stop printed `stop ok`; prod without ORDERBOOK_ALLOW_PROD=1 was refused and with it started on 9101; make clean left var and build empty. The audit grep set reports no remaining gap across all 21 claims.
- **Commit**: docs: close the run-path documentation gaps
- **Files changed**: README.md, docs/design/core-architecture.md, docs/research/2026-10-12-run-docs-audit.md, docs/plans/archive/2026-10-12-run-docs-gaps.md
- **Design docs**: docs/design/core-architecture.md, README.md

# HARNESS Plan: Close the run-path documentation gaps

| Field | Value |
|-------|-------|
| Type | docs |
| Status | build-complete |
| Short slug | run-docs-gaps |
| Created | 2026-10-12 |
| Branch | feature/run-docs-audit |
| Design docs | docs/design/core-architecture.md, README.md |
| Research | docs/research/2026-10-12-run-docs-audit.md |
| Prior plans | 2026-10-11-mac-run-path.md: added the start refusal, the stop wait, and the port runbook. This plan documents the rest of the run path. |

## Context

A reader asked how to run the order book from a shell. The answer came from the code and from this machine. Checked claim by claim against `README.md` and `docs/`, six of those claims appear nowhere: the window needs a real terminal, what `start` prints without one, how to read the book without a window, the exact prod command, the `scripts/ob` wrapper, and where pid files and logs live. Everything else the answer covered is already documented.

No behaviour changes. Every line added describes what the code already does, verified on this machine.

**Research**: docs/research/2026-10-12-run-docs-audit.md

## Steps

### 1. Document the window and the terminal — `done`
- **File(s):** README.md
- **Action:** In `Run it`, state that `--tui` opens the window only when stdin is a terminal, that `start` otherwise prints `start ok (no tui)` and leaves feedd and the adapter running, and how to read the book without a window: `./build/headless config/dev.json 1200`, which prints `bids= asks= seq= wal=`.
- **Acceptance:** `grep -c 'no tui' README.md` returns at least `1`; `grep -c 'build/headless' README.md` returns at least `1`.

### 2. Document the prod command line — `done`
- **File(s):** README.md
- **Action:** In `Dev and prod`, add the copy-pasteable start line for prod: `ORDERBOOK_ALLOW_PROD=1 python3 python/obctl.py start --config config/prod.json --tui`.
- **Acceptance:** `grep -c 'start --config config/prod.json' README.md` returns at least `1`.

### 3. Document the wrapper and where state lives — `done`
- **File(s):** README.md
- **Action:** Mention `scripts/ob` as the wrapper around `python/obctl.py`, and add one line naming the runtime state: pid files and `feedd.log` / `adapter.log` under `var/run/`, the ring and WAL under `var/dev/` (or `var/prod/`).
- **Acceptance:** `grep -c 'scripts/ob' README.md` returns at least `1`; `grep -c 'var/run' README.md` returns at least `1`.

### 4. Record why the window needs a terminal — `done`
- **File(s):** docs/design/core-architecture.md
- **Action:** In `Processes`, add one sentence: on a terminal, `start` replaces the control process with the TUI, which is why the window needs a terminal and why `stop` has no tui pid to find.
- **Acceptance:** `grep -c 'replaces the control process' docs/design/core-architecture.md` returns `1`.

### 5. Re-verify every claim against the running system — `done`
- **File(s):** README.md, docs/design/core-architecture.md
- **Action:** Run each command the documentation now states, on this machine, and confirm the output matches: `make test`, `make e2e`, `make start` from a non-tty, the manual `headless` read, `scripts/ob`, and the prod refusal without the environment variable. Then re-run the audit's grep set and confirm no claim is left undocumented.
- **Acceptance:** every command in the new README text runs and produces the stated output; the audit grep set reports no remaining gap.

## Deferred (NOT in this plan)

- A `docs/runbooks/running-it.md` when the README `Run it` section passes roughly 40 lines.
- Documenting the `obctl` exit codes (`0`, `1` on a refusal, `1` from `stop` when a process survives).

## Implementation delta

### What was verified rather than assumed
Every command the documentation now states was run on this machine before the
commit: `make test`, `make e2e`, `make start` from a non-tty, the manual
`headless` read, `scripts/ob`, the prod refusal, the prod start line, and
`make clean`'s scope.

### Method note
The first audit pass reported two false gaps because a pattern beginning with
`--` was read as a flag by `grep -F`. The research record states the method and
the correction, and every gap was re-checked with fixed quoting.

### Scope
Documentation only. No source file changed, so the ship path classifies this as
docs-only and takes the direct-merge route.
