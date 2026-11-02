#!/bin/sh
# The performance guard (POL-02, D-14, D-15): measure this machine now and compare with the baseline that was recorded on it.
#
#   sh scripts/perf-check.sh                  # measure the core metrics, compare with perf/baseline-<os>-<arch>.json
#   sh scripts/perf-check.sh --measure-only   # only measure: target/perf.log and target/perf-run.json (perf-record.sh uses it)
#
# Exit status: 0 no metric is clearly worse than the baseline ("PERF OK"), 1 one is ("PERF WORSE"), 2 the guard cannot
# judge: no baseline for this machine, a measurement that failed, a log without a PERF line, an unreadable file. A guard
# that cannot judge never passes.
#
# Not part of `cargo test` or `npm test`, and no CI file reaches it (D-12): a person runs it, on the machine whose
# baseline it is. The tolerances are in scripts/perf-compare.mjs; the measurements are core/tests/perf.rs.
set -eu

cd "$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"

measure_only=no
group=core
for arg in "$@"; do
    case "$arg" in
        --measure-only) measure_only=yes ;;
        core) group=core ;;
        *)
            echo "usage: sh scripts/perf-check.sh [--measure-only] [core]" >&2
            exit 2
            ;;
    esac
done

os=$(uname -s | tr '[:upper:]' '[:lower:]')
arch=$(uname -m)
baseline="perf/baseline-$os-$arch.json"

# Look for the baseline before the minutes of measuring: a guard without one has nothing to say.
if [ "$measure_only" = no ] && [ ! -f "$baseline" ]; then
    echo "perf-check: there is no baseline for this machine ($baseline). Record one first: sh scripts/perf-record.sh" >&2
    exit 2
fi

. scripts/env.sh
mkdir -p target

echo "perf-check: measuring group '$group' in release mode (a few minutes; the machine should be idle)"
# The output goes to a file and not into a pipe, so that a failing test fails this script.
if ! cargo test --locked --offline --release -p holzwave-core --test perf -- --ignored --nocapture --test-threads=1 \
    > target/perf.log 2>&1; then
    tail -n 40 target/perf.log >&2
    echo "perf-check: the measurement failed (log: target/perf.log)" >&2
    exit 2
fi

node scripts/perf-collect.mjs target/perf.log target/perf-run.json

if [ "$measure_only" = yes ]; then
    exit 0
fi

node scripts/perf-compare.mjs "$baseline" target/perf-run.json --scope "$group."
