# Research: Replace the top-of-book picture instead of keeping old prices

**Date**: 2026-09-21
**Chain mode**: yield

## Question

The live window says `feed=live` and the sequence climbs, but the red bids sit above the green asks and some sizes print as `0.00`. What change makes the picture match the current Binance top of book?

## Thinking — How We Got Here

### Initial framing & gut intuition

A frozen sequence would mean the adapter died again. The screenshot shows `seq=50429` and `feed=live`, and a later read climbed from 69549 to 69799. The feed is moving. The book contents are wrong.

### Pivots during research

1. `btcusdt@depth5@100ms` is a partial book depth stream. Each message is the current top 5 bids and top 5 asks, not a diff. Binance's diff stream is the one that deletes a level by sending quantity 0.
2. `Book::apply` only inserts or erases the one price in the message. Prices that fall out of the top 5 stay forever. After the market moves down, the highest old bids are still the ones the window prints, so they sit above the new asks.
3. Sizes use `%.2f`. A quantity below 0.005 becomes `0.00` even though the level was kept because its quantity is not zero.

### Options we considered and rejected

- **Hide crossed prices in the window only *(REJECTED)*** — the stored book would still be wrong, and headless would still count the stale levels.
- **Switch the config to the diff stream *(REJECTED)*** — that stream is the right shape for quantity-0 deletes, but it is a different subscription and a larger change than this lab needs. The stream we already use is a full top-of-book picture.

### Open empirical questions deliberately not answered here

- The screenshot was not compared to a simultaneous Binance top-of-book dump. The crossed prices match the keep-every-level bug without that dump.
- Adversarial check: not run as a separate fetch. The stream name in `config/dev.json` is `btcusdt@depth5@100ms`, which Binance documents as partial book depth, not a diff.

## Options Considered

| Option | Pros | Cons | Cost |
|--------|------|------|------|
| On each picture, clear that side and then insert only the prices in the picture. Print sizes with enough decimals that a non-zero size is not `0.00`. | Matches the stream we already subscribe to. Fixture e2e ends on the last picture. Individual test messages that are not pictures still insert one price. | A message that is not a picture must not clear the book. | small |
| Subscribe to diff depth *(REJECTED)* | Quantity 0 is a real delete. | New stream, new sequence rules, more code. | medium |
| Hide crossed rows in the TUI *(REJECTED)* | The window would look ordered. | The book underneath stays stale. | small, wrong |

## Decision + Rationale

**Chosen: clear each side at the start of a top-of-book picture, then insert that picture's prices.** A new message type `DepthReset` does the clear. Ordinary `Depth` messages, including `tests/test_core.cpp`, still update one price. Sizes print up to 8 decimals with trailing zeros removed.

Deep-dive: optional. The message type and the two call sites are already named.

## Evidence

### External

- Binance spot WebSocket streams, partial book depth, https://github.com/binance/binance-spot-api-docs/blob/master/web-socket-streams.md — the `<symbol>@depth<levels>` and `@depth<levels>@100ms` streams push the top 5, 10, or 20 bids and asks. The separate diff stream is `<symbol>@depth`. The lab config uses `btcusdt@depth5@100ms`.

### Source tensions

None between the partial-depth description and this config. The earlier lab code treated those pictures as diffs. That is the bug, not a conflict between two Binance pages.

### Internal patterns referenced

- `config/dev.json` stream `btcusdt@depth5@100ms`.
- `include/ob/book.hpp` `apply` inserts or erases one price.
- Screenshot 2026-09-21: `feed=live`, `seq=50429`, best bid about 85845, best ask about 85644, several sizes `0.00`.
- Live check after that shot: sequence 69549 then 69799 two seconds later.
- `e2e/fixture.jsonl` second line drops a bid that the first line had. After this change the final book is the last picture.

### Second-brain (kb packs)

kb: unavailable or no hits.

## Parked Future Angles

- Subscribe to diff depth. *Revisit when:* the lab needs updates below the top 20, or a level must change without a full picture.

## Re-evaluation Hooks

- Reopen if a `depth5` message from `data-stream.binance.vision` is observed to omit a still-live price that the previous picture contained.
