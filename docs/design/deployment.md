# Deployment — orderbook-lab

> Last updated: October 2026

## 1. There is no deployment

This lab runs on the machine it was built on. There is no server, no container,
no orchestration, and no release artifact. Saying so first, because a deployment
document that described a pipeline for a program with no deployment would be
worse than a short one that says why.

What the document covers instead is what a reader actually needs: how to build it,
how the two environments differ, what "deploy" would mean if it were ever one, and
what CI does with it.

## 2. Building

| Implementation | Command | Requirements |
|----------------|---------|--------------|
| C++ and Python | `make` | a C++17 compiler, GNU make, python3. No packages. |
| Rust | `make rust-build` | cargo and rustc. **No crates**: `cargo build` needs no network. |
| Both | `make test` | the above, plus `python3 -m unittest` for the control script |

`rust/ob/src/sys.rs` declares the six libc functions the Rust side needs, which is
what removes the dependency tree. The two implementations share the wire format,
so either can be built and run without the other.

## 3. The two environments

| | dev | prod |
|---|---|---|
| Config | `config/dev.json` | `config/prod.json` |
| State directory | `var/dev/` | `var/prod/` |
| Run directory (pid files, process logs) | `var/run/dev/` | `var/run/prod/` |
| Listener port | 9001 | 9101 |
| Start requires | nothing | `ORDERBOOK_ALLOW_PROD=1` |
| `allow_orders` | must be `false` | must be `false` |

The environments differ in their directories and their ports so that a running
instance cannot be confused with the other, and the production profile refuses to
start without an explicit environment variable. Both implementations enforce it;
`docs/design/security.md` §2 explains why it is a refusal rather than a warning.

There is a third config, `config/test.json`, which is not a third environment: it
is `env: dev` with its own directories, port and run directory, and the `Makefile`
points every test target at it. A test that starts and stops processes needs its
own handles, and a config-shared `var/run` is how `make e2e` came to stop a live
window's daemon (`docs/optimization/ledger.jsonl` OPT-038). The run directory is
keyed by the config file rather than by its `env`, precisely because this config
shares one.

There is no staging environment. There is nothing to stage.

## 4. Durability

The ring is recreated on every start. The log is appended to and survives.

| `sync_policy` | Call | What a machine crash loses | Cost |
|---------------|------|---------------------------|------|
| `none` | nothing | everything the OS has not flushed | zero |
| `data` (default) | `fdatasync` / `fsync` | at most the last 32 records | tens of microseconds |
| `full` | `F_FULLFSYNC` on macOS, `fdatasync` elsewhere | nothing the call returned for | milliseconds |

`full` exists because on macOS `fsync` returns once the data has reached the
drive's cache, not the platter, and a lab that claims point-in-time recovery
should be able to say so honestly. `docs/design/rust-implementation.md` §5.

Recovery is `Wal::replay`: read until a record is short or damaged and keep
everything before that. A torn tail is counted and reported rather than treated as
corruption.

## 5. What CI does

`.github/workflows/ci.yml`, on every pull request and push to `dev` or `main`, two
jobs on `ubuntu-latest`:

| Job | Steps |
|-----|-------|
| `test` | install `g++ make python3`; `make test`; `make e2e` |
| `rust` | install stable with `rustfmt` and `clippy`; `make rust-test`; `make rust-e2e`; `make rust-vec-audit` |

The jobs call the **Makefile**, not the individual cargo commands. That is
deliberate and was learned the hard way: the workflow used to repeat
`cargo fmt --manifest-path rust/Cargo.toml --check`, which fails with "Failed to
find targets" because `cargo fmt` needs `--all` to resolve targets from a manifest
path. Every local check was green while the Rust job failed on every run, across
three merged pull requests. Ledger OPT-027.

CI does not run the soak. A 1000-symbol run takes a few seconds locally and would
be a fine addition; it is not there because the vectorization audit already
catches the class of regression the soak would, and a longer job is a job people
route around.

## 6. Running it

```bash
make setup                       # build and create var/dev
make start                       # feed daemon, adapter, and the terminal book
make stop                        # stop the background processes
make e2e                         # the fixture through both, headless
```

`python/obctl.py` is the control surface. `--impl cpp|rust` selects the
implementation from one config file, and the Python adapter is shared: it speaks
the same wire protocol to either.

`make start` with a terminal replaces the control process with the TUI, which is
why `stop` has no tui pid to find and why the window is left with `quit`.

## 7. If this were ever deployed

The parts that would need to change, in the order they would bite:

1. **`listen_host`.** It is `127.0.0.1`. On any other interface the daemon accepts
   unauthenticated writes from the network. `docs/design/security.md` §2.3.
2. **A supervisor.** Two background processes and a pid file is a laptop
   arrangement. `obctl` already refuses to start over a running instance and
   checks a pid's command line before signalling it, which is most of what a
   supervisor would need to be told.
3. **The ring's capacity.** 4096 slots is sized for one reader polling every
   10 ms. A slower consumer needs a bigger ring or a lossier policy, and the
   counters in §4.2 are what would say which.
4. **A clock.** The latency numbers assume every process is on one host reading
   the same monotonic clock. Across hosts they would need synchronisation, and
   the honest answer is that this lab has none.

## 8. Cross-references

- `docs/design/core-architecture.md` — the processes
- `docs/design/rust-implementation.md` §5 — the config keys
- `docs/design/security.md` — the environment gates
- `docs/runbooks/port-already-in-use.md` — the failure a restart produces
- `docs/runbooks/rust-build-failures.md` — when the Rust build will not start
