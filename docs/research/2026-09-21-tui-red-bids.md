# Research: Red bids, on-screen help, and a book that stops freezing

**Date**: 2026-09-21
**Chain mode**: yield

## Question

The live dev TUI stopped showing book changes. Bid prices should be red, ask prices should stay green, and the command list should stay on screen. What is the smallest change that does that without a new Python package?

## Thinking — How We Got Here

### Initial framing & gut intuition

The TUI process was still running, so the first guess was a redraw bug: `\033[2J` every 100ms, or `std::chrono` used without its header. The book sequence staying identical across two reads pointed somewhere else. The adapter process was gone. The feed daemon was still listening.

### Pivots during research

1. `headless` printed `bids=110 asks=116 seq=8139 wal=8154` twice, two seconds apart, while `feedd` and `tui` were alive and `feed_adapter.py` was not. The screen can keep redrawing a dead book. The freeze is the adapter exiting, not the paint loop.
2. RFC 6455 says a client must mask every frame it sends, and the server must close on an unmasked frame. This client answers a ping with unmasked `bytes([0x8A, 0x00])` and does not copy the ping payload. Binance spot streams ping every 20 seconds and disconnect if no valid pong comes back within a minute. There is no reconnect loop, so that close ends the process.
3. Exchange pages (Coinbase, Bybit, Binance Academy) paint bids green and asks red. The operator asked for the opposite: bid prices red. The earlier lab decision "both green" was the previous request. This request replaces it. ECMA-48 names the color codes and does not assign them to a side of the book.
4. Help printed on stderr is erased. ECMA-48 ED parameter 2 (`\033[2J`) puts the character positions of the active page into the erased state. A DEC status line is a different terminal mode and is not what macOS Terminal is using here. The help text has to be drawn again inside the frame.

### Options we considered and rejected

- **Replace the adapter with `websocket-client`, `websockets`, `python-binance`, or `binance-sdk-spot` *(REJECTED)*** — those packages implement RFC 6455, and some reconnect. `websocket-client` 1.9.2 `run_forever` does not reconnect when the server closes gracefully, which is the close this bug hits, and it is a pip install. This lab's fixture path is stdlib only.
- **Switch the TUI to FTXUI or ncurses *(REJECTED)*** — the prior research already kept ANSI because FTXUI is an extra CMake fetch. The help line is a redraw problem, not a missing widget toolkit.
- **Paint bids green to match Binance Academy *(REJECTED)*** — that matches Coinbase, Bybit, and the Academy article. It contradicts the operator's sentence in this task: bid prices red.

### Open empirical questions deliberately not answered here

- The empty adapter log did not contain a close code. The unmasked pong plus Binance's 20-second ping is the mechanism that fits the 15-minute death. A packet capture was not taken.
- Whether macOS Terminal honors a DEC host-writable status line was not tested. The frame already redraws every 100ms, so the help line does not need that mode.
- Adversarial check: fetched https://pypi.org/project/websocket-client/ . It confirms `pip install` and that `run_forever` does not reconnect on a graceful close. That did not flip the stdlib fix. Caveat recorded below.

## Options Considered

| Option | Pros | Cons | Cost |
|--------|------|------|------|
| Mask client frames, copy the ping payload into the pong, reconnect with a short backoff, and log the drop. Draw bids in red, asks in green, and keep the command list inside the frame. Show a stalled line when no message arrives. | Matches RFC 6455 and the spot ping rule. No pip. Fixture e2e stays offline. Help survives `\033[2J` because it is painted again. | Hand-rolled frame parser remains. Opposite of the usual bid-green convention, on purpose. | small |
| Adopt `websocket-client` or `binance-sdk-spot` *(REJECTED)* | Published clients. Official SDK renews the 24-hour stream. | `pip install`. `websocket-client` still needs an `on_close` handler for the graceful close Binance sends. Adds a dependency the Makefile does not install. | small, wrong fit |
| FTXUI / ncurses *(REJECTED)* | Real widgets and a help panel. | Extra native dependency. Prior research rejected FTXUI for this lab. | medium |
| Bids green, asks red *(REJECTED)* | Coinbase, Bybit, and Binance Academy. | Not what was asked this turn. | small, wrong color |

## Decision + Rationale

**Chosen: keep the stdlib adapter and fix the frames, and paint the help inside the TUI frame.** Bid prices use red (`\033[31m`). Ask prices stay green (`\033[32m`).

The book stopped changing because the adapter process exited. `feedd` and `tui` do not restart it. The TUI keeps clearing and drawing the last book, so the window looks frozen even though the process is still scheduled. The client sends an unmasked empty pong. RFC 6455 section 5.1 says the server must close that connection. Binance's spot stream document says the server pings every 20 seconds, requires the pong to copy the ping payload, and disconnects if that pong does not arrive within a minute. `run_live` then returns. Reconnect belongs in that function.

`websocket-client` does not remove the work. Its own project page says `run_forever` does not reconnect on a graceful server close, and installing it would break the current "python3 with no pip" test run.

Deep-dive: optional. The files and the frame change are already named.

## Evidence

### External

- RFC 6455 §5.1, https://datatracker.ietf.org/doc/html/rfc6455#section-5.1 — "a client MUST mask all frames that it sends to the server. ... The server MUST close the connection upon receiving a frame that is not masked. In this case, a server MAY send a Close frame with a status code of 1002 (protocol error)."
- RFC 6455 §5.5.2, https://datatracker.ietf.org/doc/html/rfc6455#section-5.5.2 — "Upon receipt of a Ping frame, an endpoint MUST send a Pong frame in response. ... A Pong frame sent in response to a Ping frame must have identical Application data as found in the message body of the Ping frame being replied to."
- Binance spot WebSocket streams, https://github.com/binance/binance-spot-api-docs/blob/master/web-socket-streams.md — "The WebSocket server will send a ping frame every 20 seconds. If the WebSocket server does not receive a pong frame back from the connection within a minute the connection will be disconnected. When you receive a ping, you must send a pong with a copy of ping's payload as soon as possible. Unsolicited pong frames are allowed, but will not prevent disconnection."
- websocket-client 1.9.2, https://pypi.org/project/websocket-client/ — "You can use `pip install websocket-client` to install. ... `run_forever` does not automatically reconnect if the server closes the WebSocket gracefully."
- websockets 17.1, https://pypi.org/project/websockets/ — "An implementation of the WebSocket Protocol (RFC 6455 & 7692)."
- python-binance 1.0.37, https://pypi.org/project/python-binance/ — "Websocket handling with reconnection and multiplexed connections."
- binance-sdk-spot 11.3.0, https://pypi.org/project/binance-sdk-spot/ — "The WebSocket connection is automatically renewed ... before the 24 hours expiration."
- ECMA-48 SGR, https://ecma-international.org/wp-content/uploads/ECMA-48_5th_edition_june_1991.pdf — "31 red display. 32 green display."
- ECMA-48 ED, same PDF — parameter 2: "all character positions of the page are put into the erased state."
- Coinbase, https://www.coinbase.com/en-ca/learn/advanced-trading/what-is-an-order-book — "Buy orders (or bids) are represented by green numbers. ... Sell orders (or asks) are red numbers."
- Bybit, https://www.bybit.kz/en-KAZ/help-center/article/What-Is-An-Order-Book — "The Red prices in the order book represent the selling price, while the green prices represent the buying price."
- Binance Academy, https://binance.com/en/academy/articles/what-is-an-order-book-and-how-does-it-work — search extract: "bids (buy orders in green) and one for asks (sell orders in red)." The page body did not reload on a direct fetch.
- Binance USDⓈ-M futures, https://developers.binance.com/en/docs/products/derivatives-trading-usds-futures/websocket-market-streams/Connect — ping every 3 minutes. This is not the spot stream the lab uses.

### Source tensions

Spot streams say a ping every 20 seconds and a one-minute pong deadline (https://github.com/binance/binance-spot-api-docs/blob/master/web-socket-streams.md). USDⓈ-M futures and the older binance-connector 1.8.0 text say a ping every 3 minutes. The lab URL is the spot vision host, so the 20-second rule is the one that applies.

Coinbase, Bybit, and Binance Academy use green bids. This task asks for red bids. The operator's sentence wins. ECMA-48 does not pick a side.

### Internal patterns referenced

- `src/tui.cpp:55-77` — one green SGR for both sides, clear-and-redraw every 100ms, help only on stderr, cursor starts at `ring.wseq()`, `std::chrono` used with no direct include.
- `python/feed_adapter.py:96-146` — `settimeout(30)`, unmasked empty pong, return on close, `run_live` has no reconnect, process then exits.
- `python/obctl.py:55-68` — spawns feedd and the adapter once. It does not restart a dead adapter.
- Live check 2026-09-21 22:11: tui pid 47141 state `S+`, feedd pid 47143 listening on 127.0.0.1:9001, adapter absent, seq stayed 8139.
- `docs/research/2026-09-21-orderbook-lab.md` — "Bids and asks both green as requested." ANSI chosen over FTXUI. That color sentence is superseded by this request.
- `README.md` — still says both sides are green.
- `tests/test_core.cpp` and `make e2e` never open a websocket and never read ANSI.
- kb: unavailable or no hits.

### Second-brain (kb packs)

kb: unavailable or no hits.

## Parked Future Angles

- Paint bids green and asks red to match Coinbase, Bybit, and Binance Academy. *Revisit when:* the operator says bid prices should be green.
- Replace the stdlib client with `websockets` or `binance-sdk-spot`. *Revisit when:* a live session shows a fragmented or compressed data frame that `ws_frames` drops, or the 24-hour disconnect is not followed by a successful reconnect.
- Move the TUI to FTXUI. *Revisit when:* one macOS Terminal window cannot show the book and the command list with ANSI alone.
- Make stdin non-blocking so Ctrl-C cannot block on `input.join()`. *Revisit when:* a SIGINT leaves the `tui` process alive after the feed has stopped.

## Re-evaluation Hooks

- Reopen if a masked pong that copies the ping payload is still followed by a Binance close within one minute on `wss://data-stream.binance.vision`.
- Reopen if `make test` or `make e2e` starts requiring pip.
