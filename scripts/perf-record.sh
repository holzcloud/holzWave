#!/bin/sh
# Record the performance baseline of this machine (POL-02, D-15, D-16).
#
#   sh scripts/perf-record.sh [core|server|web|container|all] [--replace]
#
# It measures exactly as `perf-check.sh` does (it runs that script with --measure-only: one test command, one collector per
# group), then writes or extends perf/baseline-<os>-<arch>.json. A metric that is already in the file is replaced only with
# --replace, and a baseline is replaced on purpose, with the reason in the commit message. New metrics are added, so a later
# group extends the file of a machine without touching the first. `all` is core, server and web in turn.
#
# The file is a record of THIS machine only: the date, the operating system and architecture, the CPU model string, the core
# count, the commit and the metrics. It holds no host name, user name, path or address.
set -eu

cd "$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"

group=core
replace=
for arg in "$@"; do
    case "$arg" in
        --replace) replace=--replace ;;
        core | server | web | container | all) group=$arg ;;
        *)
            echo "usage: sh scripts/perf-record.sh [core|server|web|container|all] [--replace]" >&2
            exit 2
            ;;
    esac
done

os=$(uname -s | tr '[:upper:]' '[:lower:]')
arch=$(uname -m)
baseline="perf/baseline-$os-$arch.json"

if [ "$group" = all ]; then
    groups="core server web"
else
    groups=$group
fi

for g in $groups; do
    sh scripts/perf-check.sh --measure-only "$g"
    mkdir -p perf
    if [ "$g" = container ]; then
        # The container is another machine: the file name and the header (os, arch, cpu, cores) come from its own PERF-MACHINE
        # line, never from this Mac.
        baseline=$(node scripts/perf-baseline.mjs --name target/perf-container.log)
        node scripts/perf-baseline.mjs target/perf-run.json "$baseline" --machine target/perf-container.log ${replace:+"$replace"}
    else
        node scripts/perf-baseline.mjs target/perf-run.json "$baseline" ${replace:+"$replace"}
    fi
done

echo "perf-record: $baseline"
echo "perf-record: this file is a record of this machine only (cpu, cores, date, commit); do not copy it to another one"
