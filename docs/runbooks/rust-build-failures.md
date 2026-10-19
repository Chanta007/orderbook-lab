# Runbook: Rust build and startup failures

## Symptoms

- A build appears to do nothing, or a binary the docs name is not there:
  `rust/target/release/web: No such file or directory`.
- `make rust-build` prints `cargo: command not found`.
- A Rust binary exits with `bus: ring magic 0x00000000 is not 0x4f424c42`.
- A Rust binary exits with `bus: ring file is N bytes, header says M`.
- `make rust-vec-audit` fails and names a kernel that did not emit the vector
  instructions it should.
- `cargo build` stops with `error: linker not found`.

## What it means

0. **The change you are building was never pulled.** Check before anything else:
   `git log --oneline -1` against `gh api repos/Chanta007/orderbook-lab/commits/dev
   --jq '.sha[0:8]'`. A checkout that is behind builds the code it has, so a binary
   added by a newer change simply does not exist and the build looks broken when it
   is not. A `git pull` that hangs is the usual reason: this repository's `origin`
   has been an SSH URL, and the SSH key on this machine belongs to an account
   without access to the repository, so the fetch waits rather than failing. Pull
   over HTTPS with the `gh` token instead:

   ```bash
   git -c credential.helper= -c credential.helper='!gh auth git-credential' \
       pull --ff-only https://github.com/Chanta007/orderbook-lab.git dev
   ```

   To make plain `git pull` work from then on:

   ```bash
   git remote set-url origin https://github.com/Chanta007/orderbook-lab.git
   ```

   (`git remote set-url origin git@github.com:Chanta007/orderbook-lab.git` puts it
   back. Pushing over SSH never worked either; the HTTPS remote is the one the
   credential helper can authenticate.)

1. **`cargo` cannot be found.** The Rust path needs a toolchain of at least
   1.80, which is the `rust-version` in `rust/Cargo.toml`.

   Both the Makefile and `python/obctl.py` look for it on PATH and then in
   `$HOME/.cargo/bin`, `/opt/homebrew/bin`, `/usr/local/bin` and `/usr/bin`, and
   `make rust-build` says what it looked for and what to do when it finds none:

   ```text
   cargo was not found.
     Looked on PATH and in ~/.cargo/bin, /opt/homebrew/bin and /usr/local/bin.
     Install Rust from https://rustup.rs, or open a new terminal: rustup adds
     ~/.cargo/env to a shell profile, and a shell started before that cannot see it.
   ```

   The resolution matters because rustup reaches PATH through `~/.cargo/env`,
   which a shell reads only if its profile has loaded — so a terminal opened
   before that line existed, or an IDE that does not read the profile, has no
   `cargo` on PATH while having a perfectly good toolchain installed. That shell
   used to fail in the Makefile while `make rust-start` worked, because only
   `obctl` resolved the tool (OPT-030, then OPT-047 for the Makefile).

   A Python traceback ending `FileNotFoundError: ... 'cargo'` was the older
   behaviour of `obctl` and is fixed.
2. **A stale ring file.** `var/dev/bus` (or `var/prod/bus`) is a file that
   survives between runs. The reader refuses a file whose first four bytes are
   not `0x4F424C42` (ASCII `OBLB`). That happens when the path holds a file
   written by a different build, by another program, or by an older layout.
   The writer rebuilds the ring at start; the reader never creates it.
3. **The ring file length disagrees with its header.** The header records the
   capacity, and the file must be exactly `16 + capacity * 72` bytes. A short
   file, or a file left behind by a build with a different capacity, trips
   this check before any record is read.
4. **The vectorization audit failed.** `make rust-vec-audit` compiles the hot
   kernels with the release profile (`lto = "fat"`, `codegen-units = 1`) and
   fails when a kernel that should vectorize did not. A debug build, or a
   changed release profile, changes what the compiler emits.
5. **The linker is missing.** On macOS, `cargo build` needs the Xcode command
   line tools for `cc`. A machine that has a compiler from another source but
   no command line tools reports `error: linker not found` and stops before
   linking any binary.

## Fix

1. Install a toolchain with rustup, then make the shell see it:

   ```bash
   . "$HOME/.cargo/env"
   cargo --version    # 1.80 or newer
   ```

   If `cargo` is still missing, the toolchain directory is not on PATH;
   `$HOME/.cargo/bin` is where rustup puts it.

2. Stop the lab, remove the ring file, and start again so the writer rebuilds
   it:

   ```bash
   python3 python/obctl.py stop --config config/dev.json
   rm var/dev/bus
   python3 python/obctl.py start --config config/dev.json --impl rust
   ```

   Do not delete the pid files by hand. `stop` removes them itself and reports
   which one was stale.

3. Same as 2: the file on disk does not match its header, so it has to be
   rebuilt. Check `bus_path` in the config first. Two worktrees sharing one
   `bus_path` is the other way to reach this state.

4. Re-run `make rust-vec-audit` and read the kernel it names. Confirm the
   release profile in `rust/Cargo.toml` still has `lto = "fat"` and
   `codegen-units = 1`, and that the build did not run with the debug profile.
   The audit and the before/after numbers are described in
   [`docs/design/vec-optimization.md`](../design/vec-optimization.md).

5. Install the command line tools, then build again:

   ```bash
   xcode-select --install
   cargo build --release --manifest-path rust/Cargo.toml
   ```

## Prevention

- Build the Rust path through `make rust-build` and `make rust-test`; they run
  the same commands CI runs.
- `make stop` before `make start`. A ring file left by a killed run is the
  usual source of a magic or length error.
- Do not point two worktrees at the same `bus_path`.
