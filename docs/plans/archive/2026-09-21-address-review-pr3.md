## PR Metadata
- **Type**: bugfix
- **Research**: docs/research/2026-09-21-tui-red-bids.md
- **Research summary**: Address-review on PR #3. The WS reconnect loop never redialed feedd; Close frames were not answered; ping was untested at the call site; SCHEMA-v1 omitted writer enums; two parked angles were missing from the deferred index.
- **Evidence**: make test (test_core ok, 5 Python tests ok); make e2e exit 0 (bids=6 asks=6 seq=13)
- **Commit**: fix: redial feedd, close handshake, pin ping path
- **Files changed**: python/feed_adapter.py, python/test_ws_frames.py, docs/telemetry/SCHEMA-v1.md, docs/deferred/index.md, docs/telemetry/runs-2026-09.jsonl
- **Design docs**: docs/design/core-architecture.md, docs/telemetry/SCHEMA-v1.md, RFC 6455 §5.5.1

# HARNESS Plan: Address-review PR 3 adapter recovery

| Field | Value |
|-------|-------|
| Type | bugfix |
| Status | build-complete |
| Short slug | address-review-pr3 |
| Created | 2026-09-21 |
| Branch | feature/tui-red-bids |
| Design docs | docs/design/core-architecture.md, docs/telemetry/SCHEMA-v1.md, docs/research/2026-09-21-tui-red-bids.md, RFC 6455 §5.5.1 |
| Prior plans | 2026-09-21-tui-red-bids.md: masked pong and WS reconnect, but feedd TCP still created once in main() |

Provenance: `/mindcoachlabs:address-review` on PR #3, iteration 1/1, `--auto`, `--severity-floor info`.

**Codegen learnings**: none
**Auto-loaded research**: docs/research/2026-09-21-tui-red-bids.md — keep the stdlib adapter, mask pongs, reconnect, red bids, help inside the frame.
**Second-brain route**: none

## Outcome Boundary

### Done when:
- After `feedd` drops the TCP sink, the adapter redials `listen_host:listen_port` instead of only rebuilding the Binance socket.
- A unit test drives `_ws_after_connect` with a stub ping (opcode `0x9`) and fails if the pong is the old unmasked empty frame.
- On a Close frame the client sends a masked Close back and logs the status code.
- `SCHEMA-v1.md` documents the writer’s real `model_id_source` and verify-step shapes so `runs-2026-09.jsonl` is not silently divergent.
- The deferred index carries the FTXUI and non-blocking-stdin parked angles from the research file.

### Failed when:
- `send_all` failure is still logged only as `ws error` and the same dead `sock` is reused.
- `make test` stays green if the ping branch is reverted to `bytes([0x8A, 0x00])`.
- A Close frame still returns without a Close response and without a close code in the log.
- Historical JSONL rows are rewritten (telemetry is append-only).
- Only two of the four research parked angles remain in `docs/deferred/index.md`.

### Must not:
- Add a third-party websocket package.
- Change the mmap message layout or send orders.
- Bump telemetry to `v: 2` or add new required v1 fields.

## Steps

### 1. Reply to Close frames and log the code — `done`
- **File(s):** python/feed_adapter.py
- **Short slug:** ws-close-reply
- **Action:** On opcode `0x8`, send a masked Close (`client_frame(0x8, payload[:125])`) and log `ws close code=...` (RFC 6455 §5.5.1). Swallow `OSError` on the reply so a dead peer still exits the generator.
- **Acceptance:** A stub-socket test that feeds a Close with status 1000 sees a masked Close in `sendall` and does not raise.

### 2. Redial feedd inside the live retry path — `done`
- **File(s):** python/feed_adapter.py
- **Short slug:** redial-feedd
- **Action:** Extract `connect_feedd(cfg)`. `run_live` owns the sink: on `OSError` from send or the socket, close it, set `sock = None`, log `feedd error`, and reconnect on the next loop. Keep WS `Exception` as `ws error` without dropping a healthy sink. Fixture path still opens one connection in `main()`.
- **Acceptance:** A unit test that fails the first `sendall` with `BrokenPipeError` calls `connect_feedd` a second time. `python3 -m py_compile python/feed_adapter.py` succeeds.

### 3. Drive the ping branch through `_ws_after_connect` — `done`
- **File(s):** python/test_ws_frames.py
- **Short slug:** ping-path-test
- **Action:** Stub socket: HTTP 101, then an unmasked server ping. Assert the next client frame is a masked pong that copies the payload. Assert it is not `bytes([0x8A, 0x00])`.
- **Acceptance:** `python3 -m unittest discover -s python -p 'test_*.py'` exits 0. Replacing the opcode `0x9` handler with `bytes([0x8A, 0x00])` makes this test fail.

### 4. Document writer-true v1 telemetry — `done`
- **File(s):** docs/telemetry/SCHEMA-v1.md
- **Short slug:** schema-errata
- **Action:** Keep `v: 1` locked (no new required fields). Document additional `model_id_source` values the harness writer already emits (`settings`, `transcript`). Document that `verify_steps_run` objects may use harness pre-flight statuses (`clean`, `behind`, `rebased`, `offline`) and extra keys, and that `pass_rate` should treat `clean` as pass-equivalent and ignore unknown keys. Do not rewrite `runs-2026-09.jsonl` (append-only).
- **Acceptance:** SCHEMA-v1.md lists `settings` and `transcript`. The JSONL file is unchanged from HEAD.

### 5. Capture missing parked angles — `done`
- **File(s):** docs/deferred/index.md
- **Short slug:** deferred-parked
- **Action:** `triage_lib.py append` for “Move the TUI to FTXUI” and “Non-blocking stdin so Ctrl-C cannot stick”, with revisit-when text from `docs/research/2026-09-21-tui-red-bids.md` Parked Future Angles. Fold with `reconcile-pending`.
- **Acceptance:** `docs/deferred/index.md` contains both titles.

## DOCS TO UPDATE
- docs/telemetry/SCHEMA-v1.md — writer-true enums (step 4)
- docs/deferred/index.md — two parked angles (step 5)

## Implementation delta

No Fix sub-steps recorded in this plan.
