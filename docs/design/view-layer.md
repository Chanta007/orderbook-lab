# The view layer — one frame, three frontends

> Last updated: October 2026

## 1. Purpose

What one frame of this lab's user interface *is*, so that the terminal, a web page
and a CLI can each show the same thing without any of them owning the content. The
terminal book used to be one function in
`rust/ob-cli/src/bin/tui.rs` that read the ring, decided what a frame said, and
wrote ANSI, in that order, in one place. Nothing about it could be reused, and every
new panel grew that function.

The rule this document sets: **the composition of a frame is pure data, and every
renderer is a function from that data to bytes.**

## 2. The layers

```text
   data                      composition                 renderers
   ────                      ───────────                 ─────────
   ring ─┐
   watch ├─→ Snapshot ──→ compose(snapshot, view) ──→ Vec<Line> ─┬─→ ANSI   (tui)
   files ┘                       (pure)                          ├─→ HTML+SSE (web)
                                                                 ├─→ JSON   (view --json)
                                                                 └─→ text   (view --text)
```

| Crate | Owns | Must not |
|-------|------|----------|
| `ob` | the wire format, the ring, the book, the WAL, the cache, latency, slippage, pressure | know a view exists |
| `ob-view` (new) | `Line`/`Span`/`Style`, `Snapshot`, `compose`, the daemon-report parser, the bar/spark helper, and the four renderers | open a file, a socket or a terminal |
| `ob-cli` | reading the ring, the watch file, the heartbeat and the daemon's log; the input line; the action dispatcher; the three binaries | decide what a line says |

The dependency arrow is the enforcement: `ob-cli` depends on `ob-view`, and
`ob-view` depends only on `ob`. A terminal type cannot appear in the composition
because the crate that composes cannot name one.

## 3. The line model

```rust
pub struct Span { pub text: String, pub style: Style }
pub struct Line { pub spans: Vec<Span> }
pub enum Style { Plain, Dim, Bold, Bid, Ask, Good, Warn, Bad, Accent }
```

Nine styles, and they are **semantic rather than terminal**: `Bid` means "a bid
price", not "red". The ANSI renderer maps `Bid` to `\x1b[31m`; the HTML renderer
maps it to a CSS class; the JSON renderer emits `"bid"`. That is what lets the same
frame be red-on-black in a terminal and themeable in a page without the composition
knowing either.

Why a `Vec<Line>` and not a widget tree: a frame here is one column with a fixed
order (header, diagnostic, table or dashboard, optional panel, prompt). A layout
system that divides a rectangle by constraints — ratatui's design — is machinery
with nothing to do in one column. `docs/research/2026-10-19-view-layer-and-web-frontend.md`
§2 has the prior art and the cost numbers.

What a `Line` deliberately cannot express: regions, absolute positions, scrolling,
or a cursor. The one place a position is needed is the prompt (the terminal's real
cursor is moved to it), and the renderer gets that from the frame's structure: the
prompt is the last line of the frame's head, before any panel below it.

## 4. The snapshot

`Snapshot` is everything observed, and nothing derived:

| Field | Source |
|-------|--------|
| `env`, `config_path`, `symbol`, `symbols_watched` | the config and the watch file |
| `levels`, `ring_capacity`, `published`, `lag`, `backlog_ns`, `staleness_ns` | the ring |
| `consumed`, `resyncs`, `reattaches`, `age` | this reader's own counters |
| `bids`, `asks` | the book |
| `ticks`: per-symbol counts over the last second, and a short history of rates | the drain loop |
| `adapter`: `Live`/`Stale`/`Absent` with its note and age | `adapter.heartbeat` |
| `daemon`: the last `op=report`, `op=latency` and `subscriber N:` lines, parsed | `<log_path>`, tailed |
| `status`: the last thing that happened, as words | the action dispatcher |
| `input`, `suggestions`, `highlighted` | the input line |

Two of those are worth a note.

**Ticks per symbol.** The reader already sees every record and already compares its
symbol to the displayed one. Counting them costs one integer per watched symbol per
record. The table is small (the watch set is tens at most) and is a fixed array with
a linear probe, so no allocation happens on the drain path. Counts are folded into a
one-second rate by a fixed ring of the last N seconds, so the status view can show
a sparkline rather than a single number.

**The daemon's report.** The status view is asked for "queues, consumers, ring
content". The first two live inside the daemon, and the daemon already writes them
to its log once a second (`docs/design/observability.md` §4.2). The view tails the
log's last few kilobytes on a timer and parses `key=value` pairs out of the newest
line carrying each op. Nothing on the wire changes — which matters, because the ring
header and the 72-byte record are shared by three implementations and
`docs/VERIFY.md` rule 2 lands a wire change as one commit across all of them. A
daemon that has never written a report reads as "no report yet", not as zero.

## 5. Composition

```rust
pub fn compose(snapshot: &Snapshot, view: &View, budget: Budget) -> Frame
```

`Frame` is the lines plus where the cursor belongs, and `Budget` is the terminal's
size: the composition decides what fits, and the renderer only draws.

`View` is the small amount of UI state that is not data: which panel is open
(`help`, `status`), the level count, and where the picker's highlight is. The
composition is a sequence of blocks, each of which appends lines:

| Block | Always? | Content |
|-------|---------|---------|
| `header` | yes | env, symbol, sequence, live/stalled, how many symbols are watched |
| `times` | yes | lag in records and in feed time, staleness, consumed, resyncs, reattaches, age |
| `diagnostic` | when the feed is not live | why, from `adapter`, and the remedy |
| `table` | book view | bids and asks, `levels` rows |
| `dashboard` | status view | ring, reader, ticks, adapter, daemon report |
| `status` | when there is one | the last action's words |
| `picker` | while something is typed | up to eight matches |
| `prompt` | yes | `> ` and the line |
| `help` | when open | the command reference, **below the prompt** |

The reference used to be eleven rows between the header and the book: the most
expensive place in the frame for the least-read text, and it could not be turned
off. It is behind `help`/`?` now, drawn below the prompt so the input line stays
where the eye already is, and the same slot holds the status panel.

Pure functions, so the tests are ordinary unit tests: give a snapshot, assert the
lines. A snapshot is cheap to build in a test, which is what makes the panels
testable at all — the alternative is driving a terminal to check a number.

## 6. Fitting the terminal

A frame drawn with `\x1b[H` and per-line erase has one hard rule: **never write more
lines than the terminal has rows.** One line too many scrolls the screen, and a
scrolled screen breaks every subsequent frame's row arithmetic — the bug looks like
corruption several frames later.

So the renderer asks the terminal for its size (`ioctl(TIOCGWINSZ)`, falling back to
80×24 when stdout is not a terminal or the call fails) and composes to a budget:

```text
rows - header - times - prompt = space
space is spent in this order: diagnostic → table/dashboard → help → picker
```

Each block is told how many rows it may use:
`table` shows `min(levels, remaining)` price levels and says so when it is
truncated; `help` shows as many reference lines as fit; `picker` drops to one line
with a count when it cannot show all eight. The book is the block that can give up
rows without losing meaning, so it gives them up first.

## 7. Renderers

| Renderer | Output | Where |
|----------|--------|-------|
| `to_ansi` | the frame plus a per-line diff against the previous frame | `tui` |
| `to_text` | the same lines, no escapes | `view --text` |
| `to_json` | `{"lines":[{"spans":[{"text":…,"style":"bid"}]}]}` | `view --json`, `/events` |
| `to_html` | `<span class="bid">` inside `<pre>` | `web` (first paint) |

**The diff.** The frame is about 3 KB and mostly static: prices change, the header
does not. The renderer keeps the previous frame's lines and their hashes, and writes
only the lines whose hash changed, moving the cursor with `\x1b[{row};1H` before
each. That is the documented win — ratatui's own guidance is that the terminal write
is the bottleneck, not the composition
(`docs/research/2026-10-19-view-layer-and-web-frontend.md` §2). The diff is also
what keeps a 10 Hz frame cheap on a slow terminal.

## 8. The three frontends

### 8.1 `tui` — the terminal book

Reads the terminal size, drains the ring, builds a snapshot, composes, renders with
the diff, owns the input line. Actions (`levels N`, `SYMBOL`, `help`, `status`,
`watch`, …) go through the shared dispatcher.

### 8.2 `view` — the CLI API

```text
view <config.json> [--json|--text] [--once|--frames N] [--interval ms] [--view book|status]
view --act "levels 7" --config <config>
```

The same snapshot and the same composition, printed instead of drawn. This is the
"full CLI API" the request asked for and the proof that the layer is real: if the
terminal can show it, the CLI can emit it, because both call `compose`. It is also
what makes the panels testable from a shell, and what a future script or dashboard
would consume.

### 8.3 `web` — the Rust-native page

```bash
make web                      # starts a feed and serves the page on 127.0.0.1:9002
make web WEB_PORT=9003        # a second one, without a second config
```

The page is `docs/images/web-frontend.png`: the same frame the terminal draws, in a
browser, with an input bar where the prompt row would be.

| Route | Method | Purpose |
|-------|--------|---------|
| `/` | GET | the page: a `<pre>` region, a prompt input, and the stylesheet inline |
| `/events` | GET | the frame stream, as SSE, one event per composed frame |
| `/act?cmd=…` | POST | run a command through the shared dispatcher, reply with the next frame |
| `/complete?line=…&limit=…` | GET | what the line could become, from the same completion the terminal's picker uses |

Decisions, each argued in the research record:

- **The model crosses the wire, not ANSI.** A frame is JSON of styled spans. The
  browser builds `<span>`s; there is no emulator and no ANSI parser on either side,
  and the bytes are a fraction of the escape-laden alternative. The page needs about
  forty lines of JavaScript, fetched from nowhere.
- **SSE, not a websocket.** The traffic is one-way; SSE reconnects on its own, is
  plain HTTP through proxies, and costs ~10% more framing bytes on small messages.
  Commands go over ordinary POSTs, which is exactly what the sources describe for
  client actions.
- **A slow viewer cannot slow the feed.** Each client has a queue of **one** frame.
  A new frame replaces the old one and counts a drop. That is the ring's own
  `DropOldest` policy, for the ring's own reason: for a live view the current frame
  beats a delayed history. The counter is reported in the frame, so the page can say
  it is behind instead of pretending.
- **Cadence.** Frames are composed at the same 10 Hz the terminal uses, and the page
  flushes on `requestAnimationFrame`, which the sources give as the client-side
  rule (the perception floor for a live number is ~200 ms).
- **The completion is the server's, and it is debounced and aborted.** Typing pauses
  for 120 ms, then one `GET /complete`; the in-flight request is aborted when the line
  moves on, because debouncing alone lets a slow answer overwrite a newer one. The
  interaction is the shell's — Tab inserts the highlighted entry, the arrows move the
  highlight, Escape closes the list and then clears the line, Enter runs the line, a
  click inserts and runs — with the ARIA combobox roles the pattern calls for
  (`role="combobox"`, `aria-expanded`, `aria-activedescendant`, a listbox of options).
  Tab completing rather than moving focus is a deliberate deviation from the pattern:
  the bar is the page's only focusable control, so there is nowhere to tab to, and the
  convention for a line you type is a shell's.
- **The server binds `127.0.0.1` only** and has no authentication, matching the rest
  of the lab; `docs/design/security.md` §2 is the reason it refuses anything else.

The server is written against `std::net`, which is what `feedd` already accepts
connections with: no new libc declarations, no new `unsafe`, and the zero-crate
property holds. One thread per connection, capped at 32 and answering 503 past that;
one line of request parsing; no keep-alive, so a command is one connection. That is
enough for a viewer on loopback.

Three defects the first browser run found, all fixed and all of a kind that a
socket-level test cannot see: the frame carried a `stdin is not a terminal` prompt row
into the browser (the snapshot now says *where* the input is rather than *whether*
there is one), the page counted the frames published before it connected as frames it
had dropped, and the server's 503 was delivered as a connection reset because it closed
the socket with the request still unread. `docs/optimization/ledger.jsonl` OPT-044 and
OPT-045.

## 9. What this costs, measured

The numbers below come from commands that can be run again, on this machine
(`docs/optimization/ledger.jsonl` OPT-039 and OPT-040). The composition cost is from
`view --bench`, which composes frames and throws them away; the terminal numbers are
from running the terminal in a pty against a socket feed whose top of book moves every
100 ms, with `--stats` printing what the write path did.

| Thing | Measured | How |
|-------|----------|-----|
| `compose` | **5.3-5.4 µs per frame**, 17 rows, 822 bytes | `view config/test.json --bench 20000 --levels 5` |
| the same, with the status panel | 5.4 µs | the same command, `--panel status` |
| rows written per frame | **8 of 11**, 707 bytes | `tui --stats` in a pty, 5 levels, live feed |
| rows written, diff disabled | 11 of 11, 851 bytes | the same, `--no-diff` |
| terminal bytes over 6 s | **23.5 KB with the diff, 51.8 KB without** | the same two runs |
| a streamed frame, over the wire | **1,575 bytes** of JSON, ~1,591 with SSE framing | 40 events from `GET /events`, 5-level book at 110 columns |

Three readings from that:

- **The write is the expensive part, and the diff halves it.** 2.2× fewer bytes to
  the terminal for the same screen, and the gap grows with the frame: a deeper book
  and an open help panel add rows that do not change, and unchanged rows cost nothing
  at all.
- **Composition is 5.4 µs at 10 frames a second**, which is 0.005% of one core. It
  allocates a few hundred bytes per frame — the snapshot's small vectors and the
  spans' strings — and the design accepts that rather than caching strings in the
  model, because 5 µs is three orders of magnitude below the write it feeds.
- **The `--bench` number is a measurement, not a budget.** It is here so a future
  change that makes composition ten times slower has something to fail against.
- **The web frame costs about twice the ANSI frame** (1,591 bytes against 851), and
  buys a renderer that needs no terminal emulator and no escape parsing on either side.
  At 10 frames a second that is 16 KB/s to one viewer on loopback, and a viewer that
  cannot keep up loses frames rather than delaying them.

## 10. Cross-references

- `docs/research/2026-10-19-view-layer-and-web-frontend.md` — the sources behind §3, §7, §8
- `docs/design/observability.md` §4.2 — the daemon's report lines the status view reads
- `docs/design/core-architecture.md` — the ring, the book and the WAL the snapshot is built from
- `docs/design/security.md` §2 — why the web frontend binds loopback and refuses more
- `docs/design/testing.md` §3.7 — how the three frontends are tested
- `docs/runbooks/feed-stalled.md` — the operator's side of the diagnostic block