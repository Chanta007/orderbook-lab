#!/bin/sh
# vec-audit.sh — compile the hot kernels and check what LLVM actually emitted.
#
# This is the oracle. "The loop vectorizes now" is not a measurement; the IR the
# compiler produced is. This script compiles the library with `--emit=llvm-ir`,
# finds each named kernel's function body, counts the vector operations in it,
# and exits non-zero when a kernel that is supposed to vectorize did not.
#
# It is deliberately a shell script over `grep` rather than a Rust test: the
# input is the compiler's own output, and the check has to be able to run on a
# build that does not otherwise link.
#
# Usage:  sh rust/scripts/vec-audit.sh [--verbose]
#
# Exit codes: 0 all required kernels vectorized, 1 at least one did not.

set -eu

verbose=0
if [ "${1:-}" = "--verbose" ]; then
    verbose=1
fi

here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
root=$(CDPATH= cd -- "$here/.." && pwd)
cd "$root"

echo "vec-audit: emitting LLVM IR for the ob library"

# `cargo rustc` passes the extra flags only to the final crate, which is what is
# wanted: the audit is about this crate's code, not its dependencies (there are
# none).
#
# `lto` is turned off **for the audit only**, and this is the single most
# important line in the script. With the release profile's `lto = "fat"`,
# `--emit=llvm-ir` writes the module *before* the LTO pipeline runs, so the file
# is unoptimized and contains no vector operations for any kernel. The first
# version of this audit read that file, found zero vector ops everywhere, and
# reported that the compiler had refused to vectorize code it had in fact
# vectorized. The shipped binaries are still built with LTO; what is being
# checked here is the vectorizer's decision on each kernel, and that decision is
# made per-crate before LTO merges anything.
# The target is named, and on x86-64 it is raised, because vectorization here is
# target-dependent and the difference is large. Measured on the same source:
#
#   aarch64 (default)      mids_into 12 vector ops   top 8
#   x86-64  (baseline)     mids_into  0              top 0
#   x86-64-v2 (SSE4.2)     mids_into 12              top 0
#   x86-64-v3 (AVX2)       mids_into 12              top 8
#
# So the code has the shape the vectorizer accepts, and the baseline x86-64 cost
# model declines to use it. Auditing at the baseline would report a failure of
# the code when what failed is the target, so the audit raises the level on
# x86-64 and says which level it used. Ledger OPT-028 has the measurement.
target_triple=$(rustc -vV | sed -n 's/^host: //p')
case "$target_triple" in
    x86_64-*) audit_cpu="-Ctarget-cpu=x86-64-v3" ;;
    *)        audit_cpu="" ;;
esac
echo "vec-audit: target $target_triple ${audit_cpu:-at its default cpu level}"

# shellcheck disable=SC2086  # audit_cpu is a deliberate single-flag word split
RUSTFLAGS="$audit_cpu" \
    cargo rustc --release --lib -p ob --config 'profile.release.lto="off"' -- --emit=llvm-ir >/dev/null

# Newest IR file for the library. `cargo rustc` writes one per invocation, and a
# raised `-Ctarget-cpu` writes under a different path, so both are searched.
ir=$(ls -t target/release/deps/ob-*.ll 2>/dev/null | head -1)
if [ -z "$ir" ] || [ ! -f "$ir" ]; then
    echo "vec-audit: FAIL no LLVM IR found under target/release/deps" >&2
    exit 1
fi
echo "vec-audit: reading $ir"

# Extract one function body by its mangled name fragment.
extract() {
    # awk exits at the first `}` at column 0, which ends a `define` block.
    awk -v pat="$1" '
        $0 ~ "^define .*" pat { inside = 1 }
        inside { print }
        inside && /^}/ { exit }
    ' "$ir"
}

# Count vector-typed operations in a body. `<4 x i64>` and friends appear in
# both the load/store and the arithmetic, on both aarch64 and x86-64.
count_vectors() {
    printf '%s' "$1" | grep -c -E '<[0-9]+ x i[0-9]+>' || true
}

# Count the calls LLVM left to a named library function, which is how a memcpy
# shows up when it did not get inlined.
count_calls() {
    printf '%s' "$1" | grep -c -E "call .*@${2}" || true
}

fail=0

# Each row: kernel name, mangled fragment, minimum vector ops, why.
# The minimum is what the reshaped code actually produces, minus slack for
# instruction selection differences between targets.
check() {
    name=$1
    frag=$2
    want=$3
    body=$(extract "$frag")
    if [ -z "$body" ]; then
        printf '  %-24s %s\n' "$name" "MISSING (no function matched '$frag')"
        fail=1
        return
    fi
    got=$(count_vectors "$body")
    if [ "$got" -ge "$want" ]; then
        printf '  %-24s ok    %s vector ops (want >= %s)\n' "$name" "$got" "$want"
    else
        printf '  %-24s FAIL  %s vector ops (want >= %s)\n' "$name" "$got" "$want"
        fail=1
    fi
    if [ "$verbose" -eq 1 ]; then
        printf '%s\n' "$body" | grep -E '<[0-9]+ x i[0-9]+>' | head -5 | sed 's/^/      /'
    fi
}

# Kernels that must vectorize. Each one is a loop over contiguous memory with no
# loop-carried dependency and no branch in the body. The minimum is what the
# reshaped code actually emits, less slack for instruction selection differences
# between aarch64 and x86-64.
echo "vec-audit: kernels that must vectorize"
check "cache::Publisher::mids_into" "mids_into" 8
check "book::Levels::top" "Levels3top" 4

# Kernels that are allowed to stay scalar, listed so that the audit's silence
# about them is a decision rather than an oversight. The reasons and the
# measurements are in docs/design/vec-optimization.md §5.
echo "vec-audit: kernels documented as scalar (not checked)"
printf '  %-28s %s\n' "book::Levels::cumulative" "loop-carried dependency; 5.2x faster than the blocked form"
printf '  %-28s %s\n' "book::Levels::fill" "early exit on a filled order"
printf '  %-28s %s\n' "codec::stream_records" "one comparison on the common path; 122x faster than the scan"
printf '  %-28s %s\n' "codec::decode_batch" "strided 72-byte records"

if [ "$fail" -ne 0 ]; then
    echo "vec-audit: FAIL at least one required kernel did not vectorize" >&2
    echo "  see docs/design/vec-optimization.md for the shapes that work" >&2
    exit 1
fi

echo "vec-audit: PASS every required kernel emitted vector operations"
