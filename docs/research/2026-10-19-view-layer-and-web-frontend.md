# The view layer, the status view, and a second frontend — research

> 2026-10-19. Written before the implementation, from four searches plus what this
> repository already knows. Every claim that came from outside is cited; the
> decisions at the end are mine and are argued rather than cited.

## 1. What was asked

Move the command reference out of the middle of the frame (below the input line, or
behind a command), add a live system-status view (queues, consumers, ring content,
tickers, tick rate) with some graphical treatment, and explore a framework split so
that the same content and input handling can drive a Rust-native web page and a CLI
API without giving up performance.

## 2. Prior art for the split: ratatui's Buffer/Widget, and TEA

Ratatui is the reference Rust TUI library. Its architecture is worth reading even
though this repository will not use it, because it answers "where does the work
go".

- **Immediate mode.** The application calls `Terminal::draw(|frame| …)` in a loop.
  There is no retained widget tree: the whole frame is rebuilt every time.
- **Buffer.** A `Buffer` is a flat `Vec<Cell>` of `width * height`; each cell holds
  a symbol and a style. Widgets write into it, and nothing talks to the terminal
  directly.
- **Double buffering with a diff.** Two buffers exist; after a draw they are
  diffed and only the changed cells are written out as ANSI. Typical terminal sizes
  are "low tens of microseconds" for the diff; 255×255 (~65k cells) is ~0.6-0.8 ms
  for a full change.
- **The backend write is the real bottleneck**, not the diff: "Only changed cells
  are written … This is often the real bottleneck … Static UIs can skip `draw`
  entirely."
- **The application pattern is the Elm architecture**: model (all state), update
  (message → new model), view (model → widgets). Stateful widgets keep their own
  small state (selection, scroll offset) passed in each frame.

Source: [ratatui ARCHITECTURE.md](https://github.com/ratatui/ratatui/blob/main/ARCHITECTURE.md),
[rendering under the hood](https://ratatui.rs/concepts/rendering/under-the-hood/),
[The Elm architecture](https://ratatui.rs/concepts/application-patterns/the-elm-architecture/),
[per-frame cost discussion](https://github.com/ratatui/ratatui/discussions/579).

Two things follow for this repository.

1. **The layer to extract is model → view, not a widget system.** This frame is one
   column of text with a fixed order: a header, an optional diagnostic, a table, an
   optional panel, a prompt. A layout solver that divides a rectangle by constraints
   would be machinery with nothing to do here.
2. **The place to spend effort is the write path**, and "skip unchanged output" is
   the documented win. A line-hash diff gets most of that for a frame that is mostly
   static, without the cell grid.

## 3. Prior art for the web frontend: streaming the model, not ANSI

There are two established ways to put a terminal-shaped view in a browser.

- **A terminal emulator in the browser.** xterm.js interprets VT/ANSI and is used by
  VS Code, Hyper, ttyd and JupyterLab. The server sends raw ANSI. It is exact and it
  brings a large JavaScript dependency into a repository whose selling point is that
  the Rust side has none.
- **Convert ANSI to HTML.** `ansi_up.js` (zero deps), `fancy-ansi`, `ansi-to-html`,
  or xterm's serialize addon `serializeAsHTML()`. These wrap SGR sequences in
  `<span style="…">` inside a `<pre>`, and they ignore everything that is not a
  style (cursor moves, erases) — which is most of what this frame's writer emits
  (`\x1b[H`, `\x1b[K`, `\x1b[J`).

Sources: [xterm.js](https://xtermjs.org/), [serialize addon](https://github.com/xtermjs/xterm.js/blob/master/addons/addon-serialize/README.md),
[ansi_up](https://github.com/drudru/ansi_up), [ANSI→HTML conversion notes](https://dzx.fr/blog/how-to-convert-ansi-terminal-content-to-html).

Both are the wrong shape for this program: it already knows the structure it wants
to draw (a price, a size, a colour) and throws that knowledge away by encoding ANSI.
The alternative is to stream **the styled lines themselves** and let a few lines of
JavaScript build spans. That is the design here.

## 4. SSE against WebSocket, for a stream of frames

- **Small-message framing**: a websocket frame costs 2-14 bytes; SSE costs ~8-14
  bytes of `data: ` plus newlines. A 1,000-event benchmark with 117-byte JSON
  payloads: 119,692 bytes over a websocket, 131,596 over SSE — about 10% more.
- **Latency**: at 50 ms simulated RTT, 26.16 ms (WS) against 26.29 ms (SSE), a
  difference under 0.3 ms; both are ~1 RTT after the connection is open.
- **Reconnection**: SSE has it built in (`EventSource`, `Last-Event-ID`); a
  websocket needs heartbeats, backoff and state recovery written by hand.
- **Infrastructure**: SSE is plain HTTP, so it passes proxies, firewalls and load
  balancers once buffering is disabled; a websocket needs an `Upgrade` path and
  often special configuration.
- **Direction**: SSE is server→client only. Client actions go over ordinary HTTP.

Sources: [SSE vs WebSocket cost of 1,000 events](https://daily.dev/posts/websocket-vs-sse-vs-long-polling-the-real-cost-of-1-000-events-wip1lzawd),
[streaming architecture 2026](https://jetbi.com/blog/streaming-architecture-2026-beyond-websockets),
[websockets won the benchmark, use SSE anyway](https://blog.authon.dev/websockets-vs-sse-websockets-won-the-benchmark-use-sse-anyway).

For a one-way stream of frames to a viewer, SSE wins on everything except ~10% of
framing bytes on small messages, and the actions this UI accepts (a command line, a
switch) are exactly the "separate HTTP request" the sources describe.

## 5. Backpressure to a slow viewer — the same problem the ring already solves

The SSE literature is unambiguous, and it is the ring's policy restated:

- "Treat slow clients as a first-class concern: apply per-client backpressure,
  prefer dropping/coalescing old updates ('drop frames') to stay current, and never
  let one lagging consumer starve others."
- Bounded per-client queue (64-256 items or KB) with a policy when it fills.
- For live dashboards, freshness beats completeness: **drop oldest** or keep only
  the latest value, and count what was dropped.
- Do not block the producer or other clients; disconnect or warn after a grace
  period.
- Heartbeats (`: keepalive`) stop proxies killing an idle stream, and must not
  count as data.
- On the client, buffer into a ref and flush on `requestAnimationFrame` or every
  100-200 ms — the perception floor for a KPI card is ~200 ms.

Sources: [realtime backpressure](https://www.thenodebook.com/realtime/realtime-backpressure),
[backpressure with slow clients](https://codingeasypeasy.com/blog/backpressure-in-webflux-handling-slow-clients-in-spring-boot/),
[SSE in production](https://blog.stackademic.com/net-10-sse-in-production-the-proxy-buffering-default-that-turns-real-time-into-batches-cbe49c45c3ad),
[realtime dashboard that doesn't melt the browser](https://medium.com/@sahilsamimsiddiqui/frontend-system-design-building-a-real-time-dashboard-that-doesnt-melt-the-browser-950717490901).

This repository has already made this decision twice: the SPSC ring has a
`DropOldest` policy as its default, and backpressure is a state derived from lag in
time. A web viewer is one more consumer that may be slow, and it gets the same
treatment — a capacity of one frame and a drop counter — because "one slow viewer
must not slow the feed" is the same sentence as "one slow subscriber must not slow
the producer".

## 6. Drawing a graph without a graphics library

The Unicode block elements U+2581 to U+2588 (`▁▂▃▄▅▆▇█`) give eight vertical levels
per cell and are the standard way to draw a bar or a sparkline in a terminal:
normalise to 0-7 and index the string. Left-aligned counterparts
(`▏▎▍▌▋▊▉█`, U+258F-U+2588) give eight horizontal levels for progress bars, and
Braille patterns (U+2800-U+28FF) give 2×4 dots per cell when more resolution is
wanted. Rendering caveats: they assume a monospace font, and a few of them
(`▁ ▂ █`) can look a pixel off on some renderers.

Sources: [Block Elements](https://en.wikipedia.org/wiki/Block_Elements),
[the tao of unicode sparklines](https://blog.jonudell.net/2021/08/05/the-tao-of-unicode-sparklines/),
[cli::spark_bar](https://rdrr.io/cran/cli/man/spark_bar.html).

So the status view needs no dependency: a bar is one character, and the ring's fill
is one of eight horizontal glyphs.

## 7. What the repository already has, which shapes the status view

The daemon already reports everything the status view is asked to show, on its own
log lines, once a second:

```text
t=… level=info component=feedd op=report published=2400000 applied=… ignored=0 crossed=0 evicted=0 dropped=0 symbols=1000 connections=1 invalid=0 bytes=… rate=7
t=… level=info component=feedd op=latency stage1=… stage2=… ingress=… stamped=0 unstamped=… implausible=0
subscriber 0: delivered=2400000 evicted=0 dropped=0 high_water=2041 state=nominal | stage3: n=… p99=165.5us
```

`docs/design/observability.md` §4.2 calls those the four things to read in a run:
`dropped`/`evicted` (data a consumer never saw), `crossed`, `implausible`, and the
pressure state. The status view can display them by tailing the file the daemon
already writes. That needs no wire-format change — which matters because the ring
header and the 72-byte record are shared by three implementations and `docs/VERIFY.md`
rule 2 says a wire change lands as one commit across all of them.

The ring itself gives the rest: capacity, the published sequence, this reader's lag
in records and in feed time, staleness, and the per-symbol tick counts the reader
can accumulate as it drains.

## 8. Decisions

| Question | Decision | Why |
|----------|----------|-----|
| Widget toolkit or line model? | Line model: `Vec<Line>`, each `Line` a `Vec<Span>` | The frame is one column with fixed order; a layout solver would have nothing to do. §2 |
| Where does the pure code live? | A new crate `ob-view` | The dependency graph then enforces the split: nothing in it can reach a terminal or a file. §2 |
| ANSI to the browser, or the model? | The model, as JSON | It keeps the structure the program already has, drops the ANSI escapes that are most of the bytes, and needs no emulator or converter. §3 |
| SSE or WebSocket? | SSE, with actions over ordinary HTTP | One-way traffic, built-in reconnect, plain HTTP through proxies; the cost is ~10% framing on small messages. §4 |
| Slow viewer? | One-frame queue with drop-oldest and a drop counter | The ring's own policy, for the same reason; §5 is the external version of the same argument |
| Graphs? | Unicode blocks | Eight levels per cell, no dependency, one character each. §6 |
| Where does the status data come from? | The daemon's existing log lines, tailed | No wire-format change, and both implementations already write them. §7 |

## 9. What this research did not settle

- **Frame size and cadence on the web.** The sources give client-side guidance
  (flush on `requestAnimationFrame`, ~200 ms perception floor) but no measured cost
  for a 3 KB text frame at 10 Hz. The implementation measures its own bytes and
  composition cost instead of assuming.
- **Whether the browser needs a diff.** The terminal renderer can skip unchanged
  lines; for the web the design sends coalesced whole frames and lets the client
  replace the panel, which is simpler and bounded. That is a decision, not a
  measurement, and the implementation records the bytes.
- **No browser was available in this environment**, so the web frontend's visual
  result is verified with `curl` and by rendering the same model through the CLI.
  That limit is recorded in the design doc and in the runbook.