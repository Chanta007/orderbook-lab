## PR Metadata
- **Type**: feat
- **Research**: docs/research/2026-09-21-orderbook-lab.md
- **Research summary**: mmap ring + WAL; Binance MD-only WS; OTel IDs off hot path; file-driven setup/start/stop; no Kafka/Redis.
- **Evidence**: make test; make e2e (bids=6 asks=6); prod start refused
- **Commit**: feat: mmap-bus order-book lab with TUI and fixture e2e
- **Files changed**: include/ob, src, python, config, Makefile, CI, README
- **Design docs**: docs/design/core-architecture.md

# HARNESS Plan: orderbook-lab core prototype

| Field | Value |
|-------|-------|
| Type | feat |
| Status | build-complete |
| Short slug | orderbook-lab-core |
| Created | 2026-09-21 |
| Branch | feature/core |
| Design docs | docs/design/core-architecture.md |
| Research | docs/research/2026-09-21-orderbook-lab.md |

## Steps

### 1. Bus, WAL, book, config — `done`
### 2. Processes + Python ctl — `done`
### 3. CI + docs — `done`
