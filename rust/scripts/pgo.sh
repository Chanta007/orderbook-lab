#!/bin/sh
# pgo.sh — profile-guided optimization build.
#
# The procedure, and why it is not the default:
#
#   1. Build with instrumentation. Every function entry records a counter.
#   2. Run a workload that looks like production. For this lab that is `obsoak`,
#      which drives the cache the way the feed daemon does.
#   3. Merge the counters into one profile.
#   4. Rebuild, letting the optimizer use the profile for inlining and block
#      layout decisions it would otherwise guess at.
#
# It is parked rather than wired into the release profile because it needs two
# things a plain `cargo build` does not: `llvm-profdata`, from the
# `llvm-tools-preview` component, and a workload that is representative. A
# profile from a workload that does not resemble the real one makes the build
# worse, not better, and nothing in the build would say so.
#
# Usage:
#   sh rust/scripts/pgo.sh [soak flags]
#
# Example:
#   sh rust/scripts/pgo.sh --symbols 1000 --ticks 200
#
# The trained binary replaces rust/target/release/, so rebuild with
# `cargo build --release` when you are done measuring.

set -eu

here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
root=$(CDPATH= cd -- "$here/../.." && pwd)
cd "$root"

manifest="rust/Cargo.toml"
profdir="rust/target/pgo"
# rustc resolves `-C profile-use` relative to the crate being compiled, not to
# the working directory, so the profile path has to be absolute.
profdir_abs="$root/$profdir"
profdata="$profdir/merged.profdata"
profdata_abs="$profdir_abs/merged.profdata"

# llvm-profdata ships with the llvm-tools component, not with rustc itself.
profdata_bin=$(rustc --print sysroot)/lib/rustlib/$(rustc -vV | sed -n 's/^host: //p')/bin/llvm-profdata
if [ ! -x "$profdata_bin" ]; then
    cat >&2 <<'EOF'
pgo.sh: llvm-profdata was not found.

Install it with:

    rustup component add llvm-tools-preview

Then run this script again. Nothing has been built or changed.
EOF
    exit 1
fi

rm -rf "$profdir"
mkdir -p "$profdir"

echo "pgo: step 1/4 instrumented build"
RUSTFLAGS="-Cprofile-generate=$profdir" \
    cargo build --release --manifest-path "$manifest"

echo "pgo: step 2/4 running the workload"
# `obsoak` is the representative workload: it publishes the same shape of record
# the daemon does, for a thousand symbols, and exercises the cache, the book and
# the flow ring. Flags after the script name are passed through.
rust/target/release/obsoak "$@" >/dev/null

echo "pgo: step 3/4 merging profiles"
"$profdata_bin" merge -o "$profdata" "$profdir"
ls -l "$profdata"

echo "pgo: step 4/4 optimized build using the profile"
RUSTFLAGS="-Cprofile-use=$profdata_abs -Cllvm-args=-pgo-warn-missing-function" \
    cargo build --release --manifest-path "$manifest"

cat <<'EOF'

pgo: done. rust/target/release now holds the trained binaries.

To see whether it helped, compare against the untrained build:

    rust/target/release/vecbench --json
    cargo build --release && rust/target/release/vecbench --json

Record the comparison as a ledger entry rather than a claim:

    sh rust/scripts/opt-record.sh <kernel> "PGO moved this kernel"

If the numbers do not move, that is the finding. Delete the trained build with
`cargo clean -p ob` and say so in the ledger.
EOF
