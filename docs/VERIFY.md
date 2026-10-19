# Verify Step Registry

> Last updated: October 2026

The checks that must pass before a change to this repository is committed. Each
entry is one command with an expected result, so a verify run is a list of
outcomes rather than a judgement.

Run everything with:

```bash
make test && make rust-test && make rust-vec-audit && make e2e && make rust-e2e
```

## Steps

| id | description | type | applies_when | command | expected |
|----|-------------|------|--------------|---------|----------|
| `cpp-core-tests` | The C++ book, ring and WAL checks | shell-command | always | `make test` | exit 0; prints `ok` then `OK` |
| `rust-unit-tests` | The Rust library and CLI tests, including the view layer's | shell-command | always | `cargo test --manifest-path rust/Cargo.toml` | exit 0; every suite reports `ok` |
| `rust-format` | Formatting is normalized | shell-command | rust changed | `cargo fmt --all --manifest-path rust/Cargo.toml --check` | exit 0 |
| `rust-lint` | Clippy with warnings denied | shell-command | rust changed | `cargo clippy --manifest-path rust/Cargo.toml --all-targets -- -D warnings` | exit 0 |
| `cpp-e2e` | The fixture reaches the C++ book and the log | shell-command | always | `make e2e` | exit 0; `bids=` and `asks=` non-zero |
| `rust-e2e` | The fixture reaches the Rust book and the log | shell-command | rust changed | `make rust-e2e` | exit 0; `bids=` and `asks=` non-zero |
| `interop` | Each implementation reads the other's ring and log | shell-command | wire format or bus changed | `make rust-interop` | exit 0; two `PASS:` lines |
| `vec-audit` | The kernels that should vectorize still do | shell-command | hot kernel changed | `make rust-vec-audit` | exit 0; `PASS every required kernel emitted vector operations` |
| `tui-picker` | The ticker picker completes, switches, writes the watch file, and refuses an unlisted symbol | shell-command | tickers or watch changed | `python3 -m unittest python.test_tui_input` | exit 0; every test ok |
| `tui-input` | The terminal book's input line survives a refresh, the reference is below the prompt, and the status panel is a command | shell-command | tui changed | `python3 -m unittest python.test_tui_input` | exit 0; every test ok |
| `view-cli` | The printed frame is the drawn frame, and a command from the shell is the command from the terminal | shell-command | view layer or a frontend changed | `make view-check` | exit 0; 8 tests ok |
| `frame-budget` | No frame is ever longer or wider than the terminal, and the cursor is on the prompt | shell-command | composition changed | `cargo test --manifest-path rust/Cargo.toml -p ob-view` | exit 0; `the_frame_never_exceeds_the_rows_it_was_given` passes |
| `frame-cost` | The composition cost has a command that reports it | data-check | composition changed | `rust/target/release/view config/test.json --bench 2000 --levels 5` | exit 0; prints `ns per frame` |
| `web-socket` | The web frontend's server: page, stream, commands, isolation and the connection cap | shell-command | the web frontend changed | `python3 -m unittest discover -s python -p 'test_web.py'` | exit 0; 8 tests ok |
| `web-browser` | The page in a real browser: it renders the stream, the styles apply, and typing a command changes the view | shell-command | the page or the composition changed | `python3 -m unittest discover -s python -p 'test_web_browser.py'` | exit 0; 5 tests ok, or skipped when no Chrome is installed |
| `web-check` | Both of the above, which is what CI calls | shell-command | the web frontend changed | `make web-check` | exit 0 |
| `tui-reattach` | A window re-attaches by itself when a feed restarts, and says which remedy does not close it | shell-command | tui, ring or adapter changed | `python3 -m unittest python.test_tui_input.TuiInput.test_the_window_re_attaches_when_the_feed_restarts` | exit 0; `OK`. Shown failing first with the re-attach check disabled |
| `tui-keys` | The keys typed after a bare `Escape` reach the line, `Escape` clears it, and a prefix of one listed symbol is used | shell-command | tui or tickers changed | `python3 -m unittest python.test_tui_input.TuiInput.test_escape_neither_eats_the_keys_that_follow_nor_leaves_the_line python.test_tui_input.TuiInput.test_a_prefix_of_one_listed_symbol_switches_to_it` | exit 0; `OK`. Shown failing first with the two-byte escape read restored |
| `run-isolation` | A stop on one config reaches nothing of another's, and the shipped test config shares no path or port with the live one | shell-command | obctl, config or Makefile changed | `python3 -m unittest python.test_obctl.RunDirectoryTest` | exit 0; `OK` |
| `soak` | The cache holds a thousand symbols without loss | data-check | cache changed | `make rust-soak` | exit 0; `dropped=0` and `evicted=0` |
| `wire-format` | The 72-byte layout matches the Python packer | shell-command | wire format changed | `cargo test --manifest-path rust/Cargo.toml codec::` | exit 0 |
| `prod-gate` | A prod config is refused without the environment variable | shell-command | config changed | `cargo test --manifest-path rust/Cargo.toml config::tests::a_prod_config_is_refused` | exit 0 |
| `dev-gate` | A dev config claiming orders are possible is refused | shell-command | config changed | `cargo test --manifest-path rust/Cargo.toml config::tests::a_dev_config_that_claims_orders` | exit 0 |
| `ledger-schema` | Every ledger line carries the documented fields | data-check | ledger changed | `python3 -c "import json,pathlib; [json.loads(l) for l in pathlib.Path('docs/optimization/ledger.jsonl').read_text().splitlines() if l.strip()]"` | exit 0 |
| `docs-links` | No design doc references a **full path** that does not exist | shell-command | docs changed | `grep -ohE '`(docs|rust|python|include|config|e2e)/[A-Za-z0-9_./-]+`' docs/design/*.md \| tr -d '`' \| sort -u \| while read -r f; do [ -e "$f" ] \|\| echo "MISSING: $f"; done` | no `MISSING:` line |

## A note on the link check

`docs-links` checks **full paths** only, because that is what it can check
honestly. A design doc may write a group of files the way a person reads them —
`` `rust/ob/src/ring.rs`, `wal.rs` `` — where the second name is relative to the
first. A checker that treats every backticked filename as a path reports six false
positives on this repository's own docs, which is worse than not checking: a
check that cries wolf is a check people learn to ignore.

The row above says "full path" for that reason, and its command extracts only
names that contain a directory separator.

## Rules

1. **A performance claim carries its command.** Any change that reports a number
   adds a `docs/optimization/ledger.jsonl` line whose `evidence` field is the
   command that produced it. See `docs/optimization/README.md`.
2. **The wire format is one commit.** A change to `include/ob/msg.hpp`,
   `python/feed_adapter.py`'s `MSG_FMT` or `rust/ob/src/codec.rs` changes all
   three, and `make rust-interop` proves they still agree.
3. **The audit is the oracle for vectorization.** "It should vectorize now" is not
   a result; `make rust-vec-audit` is. A kernel that is expected to stay scalar is
   listed in the audit's output with its reason.
4. **A measured regression is a valid outcome.** Two ledger entries are
   `reverted` and two are `no-effect`. Recording them is part of the check.

## Dispatch order

Steps run in the order above. A failure at `rust-e2e` does not stop `interop` from
being reported, because the two diagnose different things: one is the pipeline,
the other is the format.

## Cross-references

- `docs/CONSTRAINTS.md` §8 — testing conventions
- `docs/design/vec-optimization.md` — the kernel measurements and the oracle
- `docs/optimization/README.md` — the ledger schema
- `docs/runbooks/rust-build-failures.md` — what to do when a step fails
