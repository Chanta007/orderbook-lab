# Research: Low-latency C++ order-book lab (Mac + Ubuntu)

**Type**: wide + implementation
**Date**: 2026-09-21

## Question

How should we build a tidy, multi-process C++ lab (threads + network) with a generic in-memory bus, WAL PITR, TUI, OpenTelemetry IDs, file-driven setup/start/stop, and a live crypto **test/public** feed — without putting Kafka/Redis or the OTel SDK on the hot path, and without accidental prod orders?

## Thinking — How We Got Here

### Initial framing & gut intuition

Aeron/Iceoryx would be “real” IPC. Full OTel C++ SDK would be “real” observability. Binance testnet would be “real” exchange. All three are too heavy or too easy to mis-wire for a student lab that must actually compile on this Mac and Ubuntu and pass e2e today.

### Pivots during research

1. Iceoryx/Aeron need extra daemons. Portable `mmap(MAP_SHARED)` of a file (POSIX) is enough and works on Darwin + Linux. `memfd_create` is Linux-only.
2. Binance **market-data-only** host `wss://data-stream.binance.vision` cannot place orders. Safer than testnet keys.
3. OTel SDK `StartSpan` is hundreds of ns plus heap; `SimpleSpanProcessor` mutex + Export on the caller. Carry 16-byte trace_id in the message; log on a side thread.

### Options we considered and rejected

- **Kafka or Redis as the bus *(REJECTED)*** — Kafka is a TCP broker cluster; Redis Unix-socket RTT is tens of µs plus a single thread. Not a matching hot path.
- **Full opentelemetry-cpp on feed/match threads *(REJECTED)*** — allocation, mutex, gRPC.
- **Iceoryx2 as required runtime *(REJECTED)*** — extra runtime; overkill for one-host lab. Revisit if we outgrow mmap.
- **Prod Binance TRADE API from dev *(REJECTED)*** — accidental real orders.

### Open empirical questions deliberately not answered here

- Sub-microsecond calibration on this laptop (not a gate for the prototype).
- Whether FTXUI FetchContent is available if CMake/network fail — ANSI TUI is the portable fallback.

## Options Considered

| Option | Pros | Cons | Cost |
|--------|------|------|------|
| A. File-backed mmap ring + WAL + Python WS adapter + C++ feedd/tui | Portable, no broker, C++ threads+TCP, MD-only feed | Not Iceoryx | small |
| B. Iceoryx2 + OTel SDK + Boost.Beast | Official packages | Heavy deps, SDK on hot path risk | large |
| C. Single process, no WAL | Fast to demo | Fails PITR, multi-process, resume GitHub bar | small, wrong |

## Decision + Rationale

**Chosen: A.** Generic mmap ring is the backend for every process. Feed handler is a **separate process** that accepts normalized ticks over TCP (from a Python adapter on Binance public `btcusdt@depth5@100ms` or a fixture). C++ feedd publishes to the ring and WAL. TUI and tests consume the ring. Trace IDs (16+8 bytes) travel in the message; async logger only. `config/dev.json` vs `config/prod.json`; start refuses prod unless `ORDERBOOK_ALLOW_PROD=1`. Bids and asks both green as requested.

Evidence: Binance MD-only WS (https://developers.binance.com/en/docs/products/spot/web-socket-streams), POSIX mmap, PostgreSQL WAL/PITR model, OTel W3C 16/8-byte IDs, iceoryx/Aeron inventory (not selected).

## Evidence

### External

- https://developers.binance.com/en/docs/products/spot/web-socket-streams — `wss://stream.binance.com:9443`, `data-stream.binance.vision` MD-only
- https://github.com/eclipse-iceoryx/iceoryx — shm IPC inventory (not used)
- https://www.w3.org/TR/trace-context/ — 16-byte trace-id, 8-byte parent-id
- https://github.com/open-telemetry/opentelemetry-cpp — SDK not on hot path
- https://github.com/ArthurSonzogni/FTXUI — TUI inventory; ANSI used if CMake missing
- https://man7.org/linux/man-pages/man3/shm_open.3p.html and POSIX mmap

### Internal patterns referenced

- Factory brief in CLAUDE.md; CONSTRAINTS §12 (dev/main), §9.3 env isolation, §5 correlation IDs

## Parked Future Angles

- Iceoryx2 zero-copy. *Revisit when:* more than three processes on one box need typed loans.
- Full OTLP sidecar. *Revisit when:* an OTel collector is running locally and the async logger is proven not to stall.

## Re-evaluation Hooks

- Live Binance WS handshake fails from CI: keep fixture replay as the required e2e; live is optional.
- Need Windows: out of scope (Mac + Ubuntu only).
