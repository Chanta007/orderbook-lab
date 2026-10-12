# Research: Which parts of the run path the repo does not document

**Date**: 2026-10-12
**Chain mode**: yield

## Question

A reader asked how to run the order book from a shell. The answer was assembled from the code and from this machine, not only from the docs. Which of those commands and behaviours are actually written down in `README.md` and `docs/`, and where should each missing one live?

## Thinking — How We Got Here

### Initial framing & gut intuition

The README has a `Run it` section with the three commands and a table of window commands, so the expectation was a short list of small gaps. The audit was mechanical: take each claim from the answer, search the README and `docs/` for it, and record the hits.

Two greps in the first pass reported false gaps because the pattern started with `--` and `grep -F` read it as a flag. That is worth recording: an audit whose own tooling can lie needs its method stated, and every gap below was re-checked with corrected quoting.

### Pivots during research

1. **Most of the answer is documented.** `make test`, `make e2e`, `make setup/start/stop`, running from the repository root, the window commands (`quit`/`q`, `levels N`), `quit` leaving the feed running, `ORDERBOOK_ALLOW_PROD=1`, the start refusal, the stop wait, `var/dev`, the port-9001-across-worktrees rule, `make clean`, `make` versus CMake, and the four binaries all have hits.

2. **Six claims had no hits, and they cluster.** Four are things a person needs the moment they step outside the happy path: that the window needs a real terminal, what `start` prints when it does not get one, how to read the book without a window, and the exact prod command line. Two are discovery facts: `scripts/ob` exists and is never mentioned, and nothing says where the pid files and logs live.

3. **The TTY rule is the one that actually costs time.** `obctl.py start` execs `build/tui` only when `stdin.isatty()`. From a pipe, a background job, or a CI shell it prints `start ok (no tui)` and leaves feedd and the adapter running. A reader who pipes `make start` sees no window, no error, and a running feed, and the README does not prepare them for it. This was hit during the earlier delivery on this machine.

4. **`headless` is described but never invoked.** The README explains that `headless` walks the ring and that `make e2e` runs it, and the diagram shows it. The manual form, `./build/headless config/dev.json <ms>`, appears nowhere. It is the natural way to check that the feed is alive without opening a window, and the audit used it for exactly that.

### Options we considered and rejected

- **Put every gap in the README *(REJECTED as the whole answer)*** — right for the operator commands, wrong for the structural reason behind the TTY rule, which belongs with the process description.
- **Put the operator commands in a new runbook *(REJECTED)*** — `docs/runbooks/` holds failure patterns. "How to start it" is not a failure, and a second copy of the run instructions would drift from the README.
- **Document the TTY rule in the design doc only *(REJECTED)*** — the reader hitting `start ok (no tui)` is in the README, not in the architecture doc.
- **Change the code so `--tui` opens a window regardless *(REJECTED)*** — the guard is correct. Without a terminal there is nothing to draw on, and the current behaviour keeps `make e2e` headless. This is a documentation gap, not a defect.

### Open empirical questions deliberately not answered here

- Whether a terminal multiplexer without a pty (some CI shells, `nohup`) is reported as a tty by `isatty()`. The documented rule states what the code checks, which is the honest boundary.

## Options Considered

| Option | Pros | Cons | Cost |
|--------|------|------|------|
| Operator facts in the README `Run it` and `Dev and prod`, plus one line in the design doc for the structural reason | Each fact lands where its reader already is. No new file. No duplication. | The README grows by a short block. | small |
| All six in the README | One place. | The design doc keeps a gap on why the window needs a terminal. | small |
| A new `docs/runbooks/running-it.md` | The README stays short. | Duplicates the README; runbooks are for failures. | small, wrong |

## Decision + Rationale

**Chosen: put the operator facts in the README and the structural reason in the design doc.**

The README owns "how do I run this", so the terminal requirement, the `start ok (no tui)` line, the manual `headless` form, the prod command, the `scripts/ob` wrapper, and a one-line note on where state lives all go there. `docs/design/core-architecture.md` gains one sentence: the TUI replaces the control process, which is why it needs a terminal and why `stop` has no tui pid to find. That sentence is the reason behind two separate facts, and the design doc is where the process structure already lives.

Nothing about the code changes. Every statement added is a description of behaviour verified on this machine.

## Evidence

### Method

For each claim, `grep -rF -- "<claim>" README.md docs/`. A claim with zero hits is a gap. Patterns beginning with `-` were re-run after a first pass reported two false gaps caused by `grep -F` reading the pattern as a flag.

### Audit result

| Claim | Status |
|-------|--------|
| Run from the repository root | documented |
| `make test`, `make e2e`, `make setup`, `make start`, `make stop` | documented |
| `make start` is `obctl start --config config/dev.json --tui` | documented |
| The window opens only on a real terminal | **gap** |
| `start ok (no tui)` without one | **gap** |
| Reading the book without a window (`./build/headless config/dev.json <ms>`) | **gap** |
| The exact prod start command | **gap** |
| `scripts/ob` wrapper | **gap** |
| Pid files and logs under `var/run/` | **gap** |
| Window commands `quit`/`q`, `levels N` | documented |
| `quit` leaves the feed running | documented |
| `ORDERBOOK_ALLOW_PROD=1` gate | documented |
| `start` refuses when one is already running | documented |
| `stop` waits and reports survivors | documented |
| `var/dev` as the data directory | documented |
| Port 9001 shared across worktrees | documented |
| `make clean` scope, `make` versus CMake, the four binaries | documented |

### Source tensions

`README.md` line 35 says the terminal and the `headless` checker do not steal each other's place in the stream, and the diagram shows `headless` reading the ring, so the checker's existence is documented while its command line is not. The gap is an invocation, not a concept.

### Internal patterns referenced

- `python/obctl.py` — `start()` execs `build/tui` only when `sys.stdin.isatty()`; otherwise it prints `start ok (no tui)`.
- `src/headless.cpp` — takes a config path and a millisecond budget, prints `bids= asks= seq= wal=`.
- `scripts/ob` — `runpy` wrapper around `python/obctl.py`.
- `README.md` — `Run it`, `Dev and prod`, `Layout`.
- `docs/design/core-architecture.md` — `Processes`, `Isolation`.

### Second-brain (kb packs)

kb: unavailable or no hits.

## Parked Future Angles

- A `docs/runbooks/running-it.md` would be right if the run path grows a second page of commands. *Revisit when:* the README `Run it` section passes roughly 40 lines.
- Document the exit codes of `obctl` (`0`, `1` on a refusal, `1` from `stop` when a process survives). *Revisit when:* a script needs to branch on them.

## Re-evaluation Hooks

- Reopen if a claim from this audit is found in the docs and was marked a gap, or the reverse.
- Reopen if `obctl.py` stops guarding the TUI exec on `isatty()`, which would make the documented terminal rule wrong.
