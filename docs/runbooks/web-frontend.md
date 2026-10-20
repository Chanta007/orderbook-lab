# Runbook: the web frontend

## Starting it

```bash
make web                       # a feed, then the page on http://127.0.0.1:9002/
make web WEB_PORT=9003         # a second one, sharing the feed
```

`make web` starts the feed and the adapter (the same `obctl start` the terminal uses)
and then serves the page in the foreground. `ctrl-c` stops the server; the feed keeps
running until `make stop`. To serve a page for a feed that is already up:

```bash
rust/target/release/web config/dev.json --port 9002
```

| Flag | What it does |
|------|--------------|
| `--port N` | Which port to serve. Default 9002. Its own variable, not derived from `listen_port`. |
| `--panel book\|status` | Which body to open on. `status` is the dashboard. |
| `--levels N` | How deep the price table starts. |
| `--interval ms` | How often a frame is composed. Default 100, which is what the terminal uses. |
| `--act "command"` | Run a command before the first frame: `--act status`, `--act "levels 7"`. |
| `--follow` | Start at the live end of the ring, like the terminal, rather than showing the book as it is. |

In the page, the input bar at the bottom takes the same commands the terminal does:
`levels 7`, `status`, `book`, `ETHUSDT`, `watch`, `help`. `?cmd=status` in the URL runs
one command on load, which is how a view is linked to.

## Symptoms and what they mean

### The page says `connecting…` and never changes

The stream is not arriving. In order:

1. `curl -s http://127.0.0.1:9002/health` — if this does not answer, the server is not
   running or is on another port. `make web` prints the URL it bound.
2. `curl -sN http://127.0.0.1:9002/events | head -c 200` — if this prints an `id:` and a
   `data:` line, the server is streaming and the problem is in the browser. If it hangs
   with no output, the frame loop is not publishing: check that `feedd` is running
   (`make rust-feed`) and that the page's config points at the same ring.
3. A browser extension, a proxy or an antivirus can buffer or block
   `text/event-stream`. The page reconnects on its own; a buffering proxy is the case
   the `X-Accel-Buffering: no` header is for, and the fifteen-second keepalive comment
   is what stops an idle stream being closed.

### The page shows the frame but nothing moves

`feed=stalled` in the header is the feed, not the page: see
[`feed-stalled.md`](feed-stalled.md). The status line at the bottom right says how many
frames the page has taken and how many it dropped.

### `cannot bind 127.0.0.1:9002: Address already in use`

A server is already there — most likely one this repository started. `lsof -nP
-iTCP:9002 -sTCP:LISTEN` names it; `--port` serves a second one. The message says this,
because the first version of the terminal's own start refused with a bare error.

### `too many viewers: this server serves 32 at once`

More than 32 connections are being served. Each viewer is one connection, so this means
32 browser tabs or a script that is not closing its sockets. The server refuses rather
than queueing, so that a misbehaving client cannot make it allocate threads without
limit; the counters in `/health` say how many were refused.

### The dropdown does not appear

Typing in the bar asks the server (`GET /complete?line=…`). If the list never opens,
check the endpoint by hand:

```bash
curl -s 'http://127.0.0.1:9002/complete?line=eth' | head -c 200
```

An empty `{"suggestions":[]}` for a symbol that is listed means the catalog the server
loaded is not the one you think: it is `config/tickers.txt` from the directory the
server was started in, and it is read once at startup, so a symbol added to the file
needs a restart. A `404` means an older binary: the endpoint arrived with the
completion, and `make rust-build` is what puts it there.

The list is deliberately absent for an empty line, for `levels` after the space (a
number is typed, not chosen), and for anything no entry starts with.

### The counters say frames are being dropped

`dropped` in `/health`, or `N dropped` in the page's status line, means a viewer was
still writing when the next frame arrived. The queue holds one frame and replaces it,
which is deliberate: for a live view the current frame beats a delayed history, and a
slow viewer must not slow the feed. A number here on a local viewer means the machine
is busy rather than the network.

## What it is not

It binds `127.0.0.1` and cannot be made to bind anything else. There is no
authentication, no TLS, no compression and no keep-alive. It serves one person's
browser on their own machine, which is the same posture as the rest of the lab
(`docs/design/security.md` §2). Do not put it behind a port-forward.
