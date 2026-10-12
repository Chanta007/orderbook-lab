# Data model — orderbook-lab

> Last updated: October 2026

There is no database. The state in this lab is a fixed-size binary record, three
files, and two in-memory structures. This document is the schema of each.

## 1. The wire record

The only structure that crosses a process boundary. 72 bytes, little-endian,
produced by `python/feed_adapter.py`, read and written by both feed daemons, and
described in `include/ob/msg.hpp` and `rust/ob/src/codec.rs`.

| Offset | Size | Field | Notes |
|--------|------|-------|-------|
| 0 | 4 | `magic` | `0x4F424C42`, ASCII `OBLB`. A reader rejects anything else. |
| 4 | 2 | `nbytes` | Always 72. Guards against a short read. |
| 6 | 1 | `type` | `1` depth, `2` heartbeat, `3` reserved command, `4` depth reset. |
| 7 | 1 | `side` | `0` bid, `1` ask, `2` both. |
| 8 | 8 | `seq` | Assigned by the ring at publish time, never by the sender. |
| 16 | 8 | `ts_ns` | Filled by the daemon when the bytes were read. |
| 24 | 16 | `trace_id` | W3C width. The first eight bytes may carry the sender's stamp. |
| 40 | 8 | `span_id` | W3C width. Locally generated correlation counter. |
| 48 | 8 | `px_e8` | Price scaled by 100,000,000, signed. |
| 56 | 8 | `qty_e8` | Size, same scale. Zero on a depth record deletes that price. |
| 64 | 8 | `symbol` | Eight ASCII bytes, zero padded. Not a C string. |

Prices are integers because a price is a label, not a measurement:
`65000.10` and `65000.09999999999` must be the same key. See
`rust/ob/src/fixed.rs`.

**The one rule that keeps the two implementations compatible**: a change to this
table changes `include/ob/msg.hpp`, `MSG_FMT` in `python/feed_adapter.py`, and the
offsets in `rust/ob/src/codec.rs` in the same commit.
`docs/CONSTRAINTS.md` §1.1 states it, and `make rust-interop` proves it.

## 2. The ring

A file mapped `MAP_SHARED`, holding a 16-byte header and `capacity` slots of 72
bytes.

| Offset | Size | Field |
|--------|------|-------|
| 0 | 4 | `magic` |
| 4 | 4 | `capacity`, a power of two |
| 8 | 8 | `wseq`, how many records have been published |
| 16 | 72 x capacity | slots |

The header is the same three fields in the same order in both implementations
(`include/ob/ring.hpp`, `rust/ob/src/ring.rs`), so either daemon's ring can be
read by the other's reader.

The ring is **not durable and not meant to be**. A reader that falls more than
`capacity` records behind loses records, and the record's own `seq` field is what
tells it that happened.

## 3. The write-ahead log

The durable half of the bargain. Records appended in publish order, no index, no
commit protocol.

| Property | Value |
|----------|-------|
| Record size | 72 bytes, the same wire record |
| Order | publish order |
| Sync | every 32 records under the default policy |
| Recovery | read until a record is short or damaged; keep everything before that |

Recovery is `Wal::replay` in Rust and `Wal::replay` in C++. Both count a torn
tail rather than failing on it, because a crash mid-append is the case the log
exists for. `docs/design/deployment.md` §4 describes the sync policies.

## 4. The symbol table

Eight wire bytes become a dense `u32`, once, at the edge
(`rust/ob/src/symbol.rs`).

| Property | Value |
|----------|-------|
| Key | the eight symbol bytes |
| Value | `SymbolId`, assigned in first-seen order, never reused |
| Structure | open addressing, linear probing, power-of-two buckets |
| Load factor | at most 0.5 |
| Growth | doubling, on the first sight of a symbol, never on an update |

Ids are dense, so an id is an array index with no second lookup. That is what
lets the cache keep a thousand symbols in flat storage.

## 5. The book

One symbol's two sides, best first, in flat arrays
(`rust/ob/src/book.rs`).

```
  px:  [ 65000.10 | 64999.00 | 64990.00 | ... ]   contiguous i64
  qty: [     0.50 |     1.20 |     2.00 | ... ]   contiguous i64
```

Two arrays rather than an array of structs, because every scan over a side reads
one field at a time, and a unit-stride walk over `i64` is the shape a vector unit
can use. `docs/design/vec-optimization.md` §4.3 measures what that layout bought.

The capacity is fixed at construction. A feed that sends more levels than that
has its worst levels dropped and a counter incremented.

## 6. The cache's structures

| Structure | Key | Value | Where |
|-----------|-----|-------|-------|
| Shard | `SymbolId & mask` | a dense array of books | `rust/ob/src/cache.rs` |
| Book slot | `SymbolId >> shard_bits` | one `Book` | the same |
| Snapshot lane | `SymbolId` | best bid and best ask, or 0 | the same |
| Event | — | the message, the top of book, and the timing | the same |
| Symbol ledger | `SymbolId` | execution counts, cost sums, markouts | `rust/ob/src/slippage.rs` |

The snapshot lane exists so a market snapshot is one linear pass instead of a
pointer chase, and it is what made the kernel vectorizable.

## 7. Configuration

One schema, `config/*.json`, read by both implementations. `docs/design/rust-implementation.md`
§5 has the full key list. Keys one implementation does not know are ignored by it
rather than rejected, which is what lets one file drive either.

## 8. What is deliberately not modelled

- **No users, no accounts, no sessions.** There is no authentication anywhere
  because there is no one to authenticate. `docs/design/security.md` §1.
- **No orders.** The lab cannot place one; the adapter talks to a
  market-data-only host and `allow_orders` must be false in dev.
- **No full-depth diff feed.** The subscription is the top of book, so a book
  holds a handful of levels per side rather than a full ladder.
- **No migrations.** There is no schema to migrate; the ring is recreated on
  every start and the log is append-only.

## 9. Cross-references

- `docs/design/core-architecture.md` — the processes and their isolation
- `docs/design/rust-implementation.md` — the Rust workspace
- `docs/design/ticker-cache.md` — the cache in full
- `docs/CONSTRAINTS.md` §1.1 — the shared-format rule
