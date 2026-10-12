# Security — orderbook-lab

> Last updated: October 2026

## 1. What this lab is, in security terms

It is a local market-data viewer. It has **no users, no accounts, no sessions, no
database, and no PII**. It cannot place an order. Its network surface is a TCP
listener on the loopback interface and an outbound websocket to a public
market-data host.

Saying that plainly matters more than a checklist, because a threat model for a
web application would be fiction here. What follows is the threat model this
program actually has, and the four controls that implement it.

## 2. The four controls

### 2.1 It cannot trade

The adapter connects to `wss://data-stream.binance.vision`, a market-data-only
host that rejects order placement. That is a property of the endpoint, not of
this code, and it is the reason the lab uses it rather than testnet.

The second layer is `allow_orders` in the config. It must be `false` in a dev
config, and the loader refuses a dev config that sets it to `true`. Both
implementations check it: `rust/ob/src/config.rs` and `python/obctl.py`.

### 2.2 Prod is a different environment, and it refuses to start by accident

`config/dev.json` and `config/prod.json` differ in their directories and their
ports, so a running instance cannot be confused with the other. Starting the
production profile requires `ORDERBOOK_ALLOW_PROD=1` in the environment; without
it the config loader refuses, in both implementations, with a message naming the
variable.

The intent: reaching production should take a deliberate act rather than a typo in
a path.

### 2.3 The listener is on the loopback interface

`listen_host` is `127.0.0.1` in both shipped configs. The daemon accepts
unauthenticated connections from anything that can reach that address, which on a
single-user machine is that user. That is the correct scope for a lab and it is
stated here rather than assumed: **if `listen_host` were changed to `0.0.0.0`, the
daemon would accept unauthenticated writes from the network and every reader
would show whatever the sender said.**

### 2.4 The bus is a file, and the reader cannot write it

The ring is a file with ordinary filesystem permissions. The Rust reader opens it
`PROT_READ`, so a terminal that has no write access to the bus cannot corrupt it
even with a bug. The C++ reader opens `O_RDWR` because one constructor serves both
roles; the roles are separate types in Rust, which is why the tightening was
possible there.

## 3. Secrets

There are none. No API keys, no tokens, no credentials, no `.env`. The lab reads
public market data and writes to local files.

If a future version needs a key, the rule is already written in
`docs/CONSTRAINTS.md` §7: per-environment, never in the repository, and read in
one place.

## 4. Input validation

Every boundary validates, and each validation has a test:

| Boundary | What is checked | Where |
|----------|----------------|-------|
| The wire record | magic, declared length, type code, side code | `rust/ob/src/codec.rs` |
| The config file | the two gates, symbol length, ring capacity, pressure ordering | `rust/ob/src/config.rs` |
| The ring file | magic, capacity is a power of two, file length matches the header | `rust/ob/src/ring.rs` |
| The log file | record boundaries; a torn tail is trimmed on open and counted on replay | `rust/ob/src/wal.rs` |
| The symbol table | a bounded capacity, reported rather than grown without limit | `rust/ob/src/symbol.rs` |

A record that fails validation is counted and skipped rather than ending the feed.
The counter is in the daemon's report, so a feed that starts sending nonsense is
visible.

## 5. What is deliberately not defended

- **A malicious sender on the loopback interface.** Anything that can open a
  socket to `127.0.0.1:9001` can write any record it likes. The lab is a lab.
- **Resource exhaustion by a hostile feed.** A symbol table that fills reports
  the loss rather than growing without bound, and the fan-out drops rather than
  blocks, but neither is a defence against an adversary with local access.
- **At-rest encryption.** The ring and the log hold public market data. There is
  nothing to encrypt.
- **Transport encryption for the bus.** It is a local file.

## 6. The two places this lab is deliberately strict

**The daemon never overwrites a sender's stamp.** `trace_id[0..8]` is left exactly
as it arrived, so a record either carries a real measurement or is reported as
unmeasured. An earlier version filled the field with its own clock when it was
empty, which made every record look stamped with a zero latency — a measurement
that looks like data and is not. See `rust/ob/src/msg.rs`.

**A rejected latency reading is reported, not clamped.** A stage-1 duration above
one second is counted as implausible rather than folded into the histogram, so a
bad input cannot quietly become a two-second spike in the maximum forever.

Both are the same principle: a number that is present must mean what it says.

## 7. Cross-references

- `docs/CONSTRAINTS.md` §7 — secrets management, §9 — environment isolation
- `docs/design/deployment.md` §3 — the two environments
- `docs/design/data-model.md` §7 — the config schema
- `docs/runbooks/port-already-in-use.md` — the failure this listener produces
