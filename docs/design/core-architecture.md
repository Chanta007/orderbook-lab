# Core architecture — orderbook-lab

## Processes

```
[Binance MD-only WS or fixture]
        |
  feed_adapter.py  (Python, not on C++ hot path)
        | TCP 127.0.0.1:9001  packed Msg (72 bytes)
     feedd  (C++, threads: accept + per-client + async log)
        | mmap ring  +  WAL append
     tui / headless / tests
```

The mmap ring is the generic backend. Every consumer has its own cursor.

## Isolation

- `config/dev.json` vs `config/prod.json` (different dirs and ports).
- `allow_orders` must stay false in dev; loader throws if set.
- Prod start requires `ORDERBOOK_ALLOW_PROD=1`.
- Adapter never calls TRADE URLs.

## Observability

`Msg.trace_id[16]` + `span_id[8]` (W3C sizes). `AsyncLog` formats hex on a side thread. No OTel SDK on feed/match.

## PITR

`Wal::replay(path)` reads the append-only file. fsync/fdatasync every 32 records.
