#!/bin/sh
# opt-record.sh — append a measurement run to docs/optimization/ledger.jsonl.
#
# The ledger's first rule is that a number needs a command behind it. This script
# is how a number gets one: it runs the benchmark, captures the result as JSON,
# and appends a `measure` entry that carries the exact command in its `evidence`
# field. Nobody has to type a number into the ledger by hand, which is the only
# way the ledger stays true.
#
# Usage:
#   sh rust/scripts/opt-record.sh <kernel> "<hypothesis>" [--levels N] [--records R]
#
# Example:
#   sh rust/scripts/opt-record.sh mids "the flat side table is faster than the pointer chase"
#
# It prints the entry it appended. Review it, edit the `lesson` field by hand if
# the finding deserves one, and commit it with the change it describes.

set -eu

if [ $# -lt 2 ]; then
    echo "usage: sh rust/scripts/opt-record.sh <kernel> \"<hypothesis>\" [bench flags]" >&2
    exit 2
fi

kernel=$1
hypothesis=$2
shift 2

here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
root=$(CDPATH= cd -- "$here/../.." && pwd)
cd "$root"

ledger="docs/optimization/ledger.jsonl"
bench="rust/target/release/vecbench"

echo "opt-record: building the benchmark" >&2
cargo build --release --manifest-path rust/Cargo.toml >/dev/null

if [ ! -x "$bench" ]; then
    echo "opt-record: $bench is missing after a successful build" >&2
    exit 1
fi

# The command that produced the numbers, recorded verbatim. A reader has to be
# able to paste it and get the same row.
command="$bench --kernels $kernel $*"
echo "opt-record: running $command" >&2
result=$("$bench" --kernels "$kernel" "$@" --json)

# The last `measure` id plus one. Ids are never reused, even after a rejection.
next=$(python3 - "$ledger" <<'PY'
import json, pathlib, sys
path = pathlib.Path(sys.argv[1])
highest = 0
if path.is_file():
    for line in path.read_text(encoding="utf-8").splitlines():
        if not line.strip():
            continue
        try:
            entry = json.loads(line)
        except json.JSONDecodeError:
            continue
        ident = str(entry.get("id", ""))
        if ident.startswith("OPT-") and ident[4:].isdigit():
            highest = max(highest, int(ident[4:]))
print(f"OPT-{highest + 1:03d}")
PY
)

python3 - "$ledger" "$next" "$kernel" "$hypothesis" "$command" "$result" <<'PY'
import datetime, json, pathlib, sys

ledger, ident, kernel, hypothesis, command, result = sys.argv[1:7]
data = json.loads(result)
row = data["kernels"][0]

entry = {
    "id": ident,
    "date": datetime.date.today().isoformat(),
    "kind": "measure",
    "kernel": kernel,
    "hypothesis": hypothesis,
    "change": f"measured the shipped kernel with {command}",
    "evidence": command,
    "before": {
        "metric": "ns_per_op_baseline",
        "value": round(row["baseline_mean_ns"], 1),
        "unit": "ns",
    },
    "after": {
        "metric": "ns_per_op_shipped",
        "value": round(row["mean_ns"], 1),
        "unit": "ns",
    },
    "verdict": "inconclusive",
    "lesson": "TODO: write the general rule, or delete this field if the run taught nothing.",
    "tags": ["measurement", kernel],
}

path = pathlib.Path(ledger)
path.parent.mkdir(parents=True, exist_ok=True)
with path.open("a", encoding="utf-8") as fh:
    fh.write(json.dumps(entry, sort_keys=False) + "\n")

print(json.dumps(entry, indent=2))
print(f"\nappended {ident} to {ledger}")
print("Edit `lesson` and `verdict` before committing: a measurement is not a finding.")
PY
