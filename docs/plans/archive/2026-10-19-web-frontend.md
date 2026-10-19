## PR Metadata

- **Type**: feat
- **PRs**: (filled in after the merge)
- **Merge commits**: (filled in after the merge)
- **Research**: docs/research/2026-10-19-view-layer-and-web-frontend.md (the SSE-against-WebSocket and slow-viewer sections, written before this change and used as its design)
- **Research summary**: no new research. The previous change's record already answered the two questions this one turns on — SSE rather than a websocket for one-way frames, and a bounded per-viewer queue with drop-oldest so a slow browser cannot slow the feed — and this change is the implementation of §8.3 of the design doc.
- **Evidence**: `make test`, `make rust-test`, `make rust-vec-audit`, `make e2e`, `make rust-e2e`, `make rust-interop`, `make rust-soak`, `make view-check` and `make web-check` all PASS under `set -e -o pipefail`. `make web-check` is 8 socket tests plus 5 browser tests driving Chrome over the DevTools protocol. Measured: a streamed frame is 1,575 bytes of JSON (~1,591 with SSE framing) against 851 bytes as ANSI (OPT-043).
- **Review**: three defects found by looking at the rendered page and fixed with tests (OPT-044, OPT-045, OPT-046).
- **Commit**: feat(web): the same frame, served to a browser
- **Files changed**: `rust/ob-cli/src/web.rs` (new), `rust/ob-cli/src/bin/web.rs` (new), `rust/ob-view/src/{snapshot,compose,lib}.rs`, `rust/ob-cli/src/{window.rs,lib.rs,Cargo.toml}`, `rust/ob-cli/src/bin/{tui,view}.rs`, `python/test_web.py` (new), `python/test_web_browser.py` (new), `Makefile`, `.github/workflows/ci.yml`, `docs/design/{view-layer,testing,rust-implementation}.md`, `docs/runbooks/web-frontend.md` (new), `docs/optimization/{ledger.jsonl,README.md}`, `docs/VERIFY.md`, `README.md`, `docs/images/web-frontend.png` (new)

# Active Plan: the web frontend

| Field | Value |
|-------|-------|
| Type | feat |
| Status | verified-and-merged |
| Short slug | web-frontend |
| Created | 2026-10-19 |
| Branch | feature/web-frontend |
| Worktree | .claude/worktrees/web-frontend |
| Design docs | docs/design/view-layer.md §8.3 |
| Research | docs/research/2026-10-19-view-layer-and-web-frontend.md |
| PR | (filled in after the merge) |
| Goal | smartdeploy-to-dev (ship to dev directly approved by the user this turn) |

## Context

The previous change extracted a view layer and left the web frontend specified but not
built, because no browser was available to verify it with. The user has Chrome and
Vivaldi, and Chrome can be run headless: this change builds the frontend, tests the
server from a socket and the page in a real browser, and rolls it out.

## Decisions

- **`std::net`, not new libc declarations.** `feedd` already accepts connections with
  `std::net`, so the server uses the same: no new `unsafe`, no new declarations, and
  the zero-crate property holds.
- **SSE, and the model rather than ANSI.** One-way frames, a browser that reconnects on
  its own, no terminal emulator and no escape parsing on either side. ~1,591 bytes per
  frame against 851 as ANSI.
- **A queue of one frame per viewer.** The newest replaces whatever was waiting, and the
  difference is counted. The ring's own `DropOldest` policy, for the ring's own reason.
- **A third state for the input.** `Local | Remote | None`, because a page has an input
  bar and a frame that says `stdin is not a terminal` in a browser is wrong.
- **Loopback only, no authentication.** The same posture as the rest of the lab.
- **A real browser in the test suite**, driven over the DevTools protocol from the
  standard library, with a profile directory of its own so nothing is shared with a
  browser a person has open.

## Scope

| Step | Deliverable | Done when |
|------|-------------|-----------|
| 1 | `web.rs`: routes, the frame stream, the per-viewer queue, the connection cap, the page | Unit tests pass; `curl` shows the page, the stream and a command |
| 2 | `bin/web.rs`: the frame loop over the same `Window`, commands through the shared dispatcher | `make web` serves a live page |
| 3 | `python/test_web.py`: the server from a socket | 8 tests pass without a browser |
| 4 | `python/test_web_browser.py`: the page in Chrome, including typing a command | 5 tests pass; a screenshot is saved and looked at |
| 5 | `make web`, `make web-check`, CI wiring, runbook, design doc, ledger | The sweep is green |

## Non-goals

- TLS, authentication, compression, keep-alive, or binding anything but loopback.
- A JavaScript framework, or any asset fetched from a CDN.
- A second implementation of any command: the page's commands go through the same
  dispatcher a keystroke does.
- Changing the wire format, or the C++/Python implementations.

## Verification

`make test`, `make rust-test`, `make rust-vec-audit`, `make e2e`, `make rust-e2e`,
`make rust-interop`, `make rust-soak`, `make view-check`, `make web-check` — each run
directly under `set -e -o pipefail`, all PASS. The browser half of `web-check` runs
Chrome headless over the DevTools protocol; on a machine without Chrome it skips and
says so.
