# orderbook-lab

Public C++ lab: multithreaded market-data path, mmap ring bus, WAL, green TUI.

**GitHub:** https://github.com/Chanta007/orderbook-lab  
**Default branch:** `dev` (integration). **`main`** is production. Work on `feature/*` worktrees off `dev`.

This is a sample project (threads + TCP + shared-memory ring). It is **not** a production matching engine.

## One-liners (config-driven)

```bash
python3 python/obctl.py setup --config config/dev.json
python3 python/obctl.py start --config config/dev.json --tui
python3 python/obctl.py stop  --config config/dev.json
```

Or `make setup`, `make start`, `make stop`.

`start --tui` shows a live book. Bids **and** asks are green. Commands: `quit`, `levels N`, `help`.

Live feed uses Binance **market-data-only**  
`wss://data-stream.binance.vision/ws/btcusdt@depth5@100ms`  
(no API key, cannot place orders). Fixture e2e does not need the network:

```bash
make test
make e2e
```

## Dev vs prod

| | dev | prod |
|--|-----|------|
| config | `config/dev.json` | `config/prod.json` |
| orders | `allow_orders` must be false | still false unless you change it **and** set `ORDERBOOK_ALLOW_PROD=1` |
| ports / data dir | `var/dev`, `:9001` | `var/prod`, `:9101` |

Start **refuses** prod unless `ORDERBOOK_ALLOW_PROD=1`.

## Components

| Process | Role |
|---------|------|
| `python/feed_adapter.py` | WS or fixture → TCP |
| `feedd` | TCP → mmap ring + WAL + async log |
| `tui` / `headless` | consume ring, book, display or assert |

Trace IDs (16+8 bytes) travel in every `Msg`. Logging is a side thread.

## Layout

```
include/ob/   msg, ring, wal, book, config, trace
src/          feedd, tui, headless
config/       dev.json, prod.json
python/       obctl.py, feed_adapter.py
e2e/          fixture.jsonl
```
