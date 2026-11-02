#!/bin/sh
# The performance guard (POL-02, D-14, D-15): measure this machine now and compare with the baseline that was recorded on it.
#
#   sh scripts/perf-check.sh [group]                 # measure the group, compare with perf/baseline-<os>-<arch>.json
#   sh scripts/perf-check.sh --measure-only [group]  # only measure: target/perf.log and target/perf-run.json (perf-record.sh uses it)
#
# Groups (the default is core):
#   core       the engine and the pane actor, no socket (core/tests/perf.rs)          scope core.
#   server     a real socket to the real server: key to frame, reattach, memory      scope server.   (server/tests/perf.rs)
#   web        a real browser against the real server: key to paint, reload          scope web.      (app/tests/web/perf.mjs)
#   container  the core and server tests in the Linux container, the reference for the server (D-16)   scope core.,server.
#   all        core, server and web in turn; the exit status is the worst one
#
# A group is judged only on the metrics it measured (its scope), so a run of one group is never failed for lacking the metrics
# of another. Inside the scope a metric the run lacks fails.
#
# Exit status: 0 no metric is clearly worse than the baseline ("PERF OK"), 1 one is ("PERF WORSE"), 2 the guard cannot
# judge: an unknown group, no baseline for this machine, a measurement that failed, a log without a PERF line, an unreadable
# file. A guard that cannot judge never passes.
#
# Not part of `cargo test` or `npm test`, and no CI file reaches it (D-12): a person runs it, on the machine whose
# baseline it is. The tolerances are in scripts/perf-compare.mjs; the measurements are core/tests/perf.rs,
# server/tests/perf.rs and app/tests/web/perf.mjs.
set -eu

cd "$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"

GROUPS_LIST="core, server, web, container, all"

measure_only=no
group=core
for arg in "$@"; do
    case "$arg" in
        --measure-only) measure_only=yes ;;
        core | server | web | container | all) group=$arg ;;
        *)
            echo "usage: sh scripts/perf-check.sh [--measure-only] [group]   groups: $GROUPS_LIST" >&2
            exit 2
            ;;
    esac
done

# `all` runs every group of this machine on its own, so each is judged on its own scope; the worst status is the status.
if [ "$group" = all ]; then
    if [ "$measure_only" = yes ]; then
        echo "perf-check: --measure-only takes one group, not all ($GROUPS_LIST)" >&2
        exit 2
    fi
    worst=0
    for g in core server web; do
        status=0
        sh scripts/perf-check.sh "$g" || status=$?
        if [ "$status" -gt "$worst" ]; then worst=$status; fi
    done
    if [ "$worst" -ne 0 ]; then
        echo "perf-check: all: at least one group did not pass (exit status $worst)" >&2
    fi
    exit "$worst"
fi

os=$(uname -s | tr '[:upper:]' '[:lower:]')
arch=$(uname -m)
baseline="perf/baseline-$os-$arch.json"

log=target/perf.log
case "$group" in
    core | server | web) scope="$group." ;;
    container)
        # The container is another machine: its baseline is named by its own PERF-MACHINE line (perf/baseline-linux-aarch64.json
        # on an Apple Silicon Mac), so it can only be looked for after the measurement.
        scope="core.,server."
        log=target/perf-container.log
        ;;
esac

# Look for the baseline before the minutes of measuring: a guard without one has nothing to say.
if [ "$group" != container ] && [ "$measure_only" = no ] && [ ! -f "$baseline" ]; then
    echo "perf-check: there is no baseline for this machine ($baseline). Record one first: sh scripts/perf-record.sh" >&2
    exit 2
fi

. scripts/env.sh
mkdir -p target

# The server tests embed the page and the web file loads it, so it must be built, and from the current sources.
build_page() {
    if ! npm --prefix app run build > target/build.log 2>&1; then
        tail -n 40 target/build.log >&2
        echo "perf-check: the page did not build (log: target/build.log)" >&2
        exit 2
    fi
}

echo "perf-check: measuring group '$group' in release mode (a few minutes; the machine should be idle)"
# The output goes to a file and not into a pipe, so that a failing test fails this script.
status=0
case "$group" in
    core)
        cargo test --locked --offline --release -p holzwave-core --test perf -- --ignored --nocapture --test-threads=1 \
            > target/perf.log 2>&1 || status=$?
        ;;
    server)
        build_page
        cargo test --locked --offline --release -p holzwave-server --features in-process-workers --test perf -- \
            --ignored --nocapture --test-threads=1 > target/perf.log 2>&1 || status=$?
        ;;
    web)
        build_page
        node app/tests/web/perf.mjs > target/perf.log 2>&1 || status=$?
        ;;
    container)
        # The mode builds the page, runs the two perf binaries in the container, prints what they print and keeps it in the log.
        sh scripts/linux-container-test.sh perf || status=$?
        ;;
esac
if [ "$status" -ne 0 ]; then
    tail -n 40 "$log" >&2
    echo "perf-check: the measurement failed (log: $log)" >&2
    exit 2
fi

node scripts/perf-collect.mjs "$log" target/perf-run.json

if [ "$measure_only" = yes ]; then
    exit 0
fi

if [ "$group" = container ]; then
    # The machine the log names, not this one.
    baseline=$(node scripts/perf-baseline.mjs --name "$log") || {
        echo "perf-check: the container log names no machine (PERF-MACHINE line)" >&2
        exit 2
    }
    if [ ! -f "$baseline" ]; then
        echo "perf-check: there is no baseline for the container ($baseline). Record one first: sh scripts/perf-record.sh container" >&2
        exit 2
    fi
fi

node scripts/perf-compare.mjs "$baseline" target/perf-run.json --scope "$scope"
