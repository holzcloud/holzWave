#!/bin/sh
# The names of the tests of the desktop pair (holzwave-app and holzwave-host), so a test that gets lost while code moves
# between the two crates is a failed command and not a surprise.
#
#   sh scripts/test-names.sh list              one test name per line, sorted and unique, on standard output
#   sh scripts/test-names.sh check FILE        every name in FILE must still be listed
#
# `list` builds the test binaries of both crates (without running a test) and keeps the lines of `cargo test -- --list`
# that end in `: test`, with that suffix removed. Names carry the module path inside their crate and not the crate name,
# so a test that moves from the app to the host with its module path unchanged keeps its name. Record a list before a
# move (`list > FILE`) and run `check FILE` after every step: a name that is no longer listed is printed, the last line
# is `test-names: N missing`. The list may only grow.
#
# Exit status: 0 when the list was printed or every name is still listed (the last line of `check` is
# `test-names: ok (M names)`), 1 when `check` found a name missing, 2 for a usage error, a FILE that is missing or
# empty, a build that failed or a list with no name in it. The script installs nothing and runs no test.

here=$(cd "$(dirname "$0")" && pwd)
cd "$here/.." || exit 2
. "$here/env.sh"

usage() {
    echo "usage: sh scripts/test-names.sh list | check FILE" >&2
    exit 2
}

# The sorted unique names to the file named by the argument, or exit 2.
names_into() {
    raw=$(mktemp) || exit 2
    if ! cargo test --locked --offline -p holzwave-app -p holzwave-host -- --list >"$raw" 2>"$raw.err"; then
        cat "$raw.err" >&2
        rm -f "$raw" "$raw.err"
        echo "test-names: the test binaries did not build" >&2
        exit 2
    fi
    sed -n 's/: test$//p' "$raw" | LC_ALL=C sort -u >"$1"
    rm -f "$raw" "$raw.err"
    if [ ! -s "$1" ]; then
        echo "test-names: the list holds no name" >&2
        exit 2
    fi
}

mode=${1:-}
case "$mode" in
    list)
        [ $# -eq 1 ] || usage
        out=$(mktemp) || exit 2
        names_into "$out"
        cat "$out"
        rm -f "$out"
        ;;
    check)
        [ $# -eq 2 ] || usage
        baseline=$2
        if [ ! -s "$baseline" ]; then
            echo "test-names: $baseline is missing or empty" >&2
            exit 2
        fi
        now=$(mktemp) || exit 2
        names_into "$now"
        LC_ALL=C sort -u "$baseline" >"$now.base"
        missing=$(LC_ALL=C comm -23 "$now.base" "$now")
        total=$(wc -l <"$now.base" | tr -d ' ')
        listed=$(wc -l <"$now" | tr -d ' ')
        rm -f "$now" "$now.base"
        if [ -n "$missing" ]; then
            printf '%s\n' "$missing"
            count=$(printf '%s\n' "$missing" | wc -l | tr -d ' ')
            echo "test-names: $count missing"
            exit 1
        fi
        echo "test-names: ok ($listed names, $total recorded)"
        ;;
    *)
        usage
        ;;
esac
