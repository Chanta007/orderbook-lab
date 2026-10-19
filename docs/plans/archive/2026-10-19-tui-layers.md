## PR Metadata

- **Type**: feat
- **PRs**: #22
- **Merge commits**: `d6b41939`
- **Research**: docs/research/2026-10-19-view-layer-and-web-frontend.md
- **Research summary**: four searches — ratatui's Buffer/Widget/diff architecture and its per-frame cost numbers, ANSI-to-HTML conversion against streaming the model, SSE against WebSocket for a frame stream, and the backpressure rules for a slow viewer. Plus the block-element technique for text-mode graphs, which is what the status panel draws with.
- **Evidence**: `make test`, `make rust-test`, `make rust-vec-audit`, `make e2e`, `make rust-e2e`, `make rust-interop`, `make rust-soak` and `make view-check` all PASS, each run directly under `set -e -o pipefail`. 47 tests in `ob-view`, 67 in `ob-cli`, 176 in `ob`, 8 pty tests and 8 `view` tests. Measured: 8 of 11 rows and 707 bytes per frame with the line diff against 11 of 11 and 851 bytes without, and 23.5 KB against 51.8 KB to the terminal over six seconds (OPT-039); composition at 5.3-5.4 microseconds per frame (OPT-040).
- **Review**: the harness review gate; the diff is small enough to review whole.
- **Commit**: feat(view): a frame as a layer, a status panel, and the reference below the prompt
- **Files changed**: `rust/ob-view/` (new crate), `rust/ob-cli/src/{actions,window,stats,daemon,screen}.rs` (new), `rust/ob-cli/src/bin/{tui,view}.rs`, `rust/ob-cli/src/lib.rs`, `rust/ob/src/clock.rs`, `python/test_tui_input.py`, `python/test_view.py` (new), `Makefile`, `.github/workflows/ci.yml`, `docs/design/{view-layer,testing,rust-implementation}.md`, `docs/research/2026-10-19-view-layer-and-web-frontend.md` (new), `docs/optimization/{ledger.jsonl,README.md}`, `docs/VERIFY.md`, `README.md`

# Active Plan: a view layer for the terminal book, a status view, and a second frontend

| Field | Value |
|-------|-------|
| Type | feat |
| Status | verified-and-merged |
| Short slug | tui-layers |
| Created | 2026-10-19 |
| Branch | feature/tui-layers |
| Worktree | .claude/worktrees/tui-layers |
| PR | #22, merged as `d6b41939` |
| Design docs | docs/design/view-layer.md |
| Research | docs/research/2026-10-19-view-layer-and-web-frontend.md |
| Goal | smartdeploy-to-dev (ship to dev directly approved by the user this turn) |

## Context

Three requests in one turn:

1. The command reference is eleven lines in the middle of the frame, between the
   header and the book. Move it below the input line, or behind a command.
2. A "system status" view that shows, graphically and in real time, the queues, the
   consumers, the ring's content, the tickers and the tick rate.
3. Explore moving the terminal book onto a framework/library split so features are
   easier to add, and so the same content and the same input handling can drive a
   **Rust-native web page** and a **CLI API that exposes the whole feature set** —
   with everything kept as cheap as it is now.

Requests 1 and 2 are shape changes inside one binary. Request 3 is an
architectural change: today `rust/ob-cli/src/bin/tui.rs` is 1,573 lines that
acquire data, decide what a frame says, and render ANSI in one function, so
nothing about it can be reused by a second frontend.

## Decisions

- **The view model is a list of styled lines, not a widget tree.** A frame is
  `Vec<Line>`, each `Line` a `Vec<Span>` with a style. That is enough to drive a
  terminal (ANSI), a web page (spans as DOM nodes) and a CLI (`--json`), and it
  costs one small allocation per line per frame. A widget tree with a layout solver
  (ratatui's design) buys region composition this program does not need: the frame
  has one column and a fixed top-to-bottom order.
- **A new crate `ob-view` owns the pure part**: the line types and their
  renderers, the snapshot a frame is composed from, the composition itself, and the
  parser for the daemon's own report lines. It has no I/O and no terminal type in
  it, so its tests are ordinary unit tests.
- **`ob-cli` keeps the I/O**: the ring reader, the watch file, the heartbeat, the
  log tail. It builds a `Snapshot` and hands it to `ob-view::compose`.
- **The terminal renderer writes only the lines that changed.** The frame is
  ~3 KB and mostly static; hashing each line and skipping the unchanged ones is
  cheaper than the write, and the write was the accepted bottleneck.
- **The web frontend streams the structured model over SSE, not ANSI.** No
  terminal emulator in the browser, no ANSI parser on either side, and the bytes
  are a fraction of the ANSI stream. Server-sent events rather than a websocket:
  the traffic is one-way, and the loss is ~10% of framing on small messages against
  a handshake, a custom reconnect and proxy configuration for a websocket.
- **A slow browser must not slow the feed.** Each client gets a bounded queue of
  one frame with a drop-oldest policy and a counter of what it dropped, which is
  the same policy and the same reason as the ring's `DropOldest`: for a live view,
  the current frame beats a delayed history.

## Scope, in order

| Step | Deliverable | Done when |
|------|-------------|-----------|
| 1 | The command reference moves behind `help`/`?`, drawn **below** the prompt, off by default | A pty test sees no reference in the default frame and sees it after `help`, below the input line |
| 2 | The frame never writes more lines than the terminal has rows (ioctl `TIOCGWINSZ`, bounded book rows, bounded help panel) | A unit test pins the budget; a pty test on a 24-row pty sees no scroll |
| 3 | `status` / `book` switch the main area; the status view draws the ring, this reader's rate, per-symbol tick rates as bars, the adapter's liveness, and the daemon's own queues/consumers/pressure | A pty test switches to the status view and back; unit tests cover the composition and the report parser |
| 4 | `ob-view` owns the line types, snapshot, composition and the report parser; `tui` renders it | `cargo test -p ob-view` passes; `tui` behaviour is unchanged except for steps 1-3 |
| 5 | A `view` binary: the same composition as text or JSON, once or streaming | `make view-check` prints a frame as JSON with the fields the terminal shows |
| 6 | A `web` binary: `/` serves the page, `/events` streams frames over SSE, `/act` runs a command; bounded per-client queues with drop-oldest and a drop counter | `curl` proves the page, three frames, an action that changes the view, and a counted drop for a client that reads slowly |

Steps 1-5 ship in this change. Step 6 ships if it lands green in the same
worktree; the design covers it either way.

## Non-goals

- Replacing the C++ terminal or the C++/Python implementations.
- A general widget toolkit, a layout solver, or theming.
- WebSockets, or any JavaScript dependency that is fetched from a CDN.
- Changing the wire format. The status view reads the daemon's existing log lines.
- Authentication on the web frontend beyond binding to `127.0.0.1`.

## Verification

- `cargo test`, `cargo fmt --check`, `cargo clippy -D warnings`.
- `python3 -m unittest python.test_tui_input` (pty), extended for the help panel,
  the line budget and the status view.
- `make view-check` and `make web-check` (new, both offline).
- The full existing sweep: `make test`, `make rust-test`, `make rust-vec-audit`,
  `make e2e`, `make rust-e2e`, `make rust-interop`, `make rust-soak`.
- No browser is available in this environment: the web frontend is verified with
  `curl` (page, stream, action, slow client) and that limit is recorded.