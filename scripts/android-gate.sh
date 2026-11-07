#!/bin/sh
# The Android phase gate in one command: every check of the phase that needs no emulator, and with --emulator the whole
# emulator matrix as well.
#
#   sh scripts/android-gate.sh              # the checks that need no emulator
#   sh scripts/android-gate.sh --emulator   # the same, then `sh scripts/android-e2e.sh matrix` (three arm64 images)
#
# The steps, in this order. Each prints `gate <name>: ok` or `gate <name>: FAILED <command>`, and the gate stops at the first
# failure (exit status 1) with the failing command named:
#
#   env            sh scripts/android-env-check.sh                   the toolchain against the pins
#   compile        sh scripts/android-check.sh                       the Rust code compiles for aarch64-linux-android
#   script-tests   the stand-in tests of the Android scripts         test-android-env.sh, test-android-scripts.sh,
#                                                                    test-android-version-code.sh, test-android-apk-check.sh
#   workspace      cargo test --workspace --locked --offline         every Rust test of the repository (run without the
#                                                                    HOLZWAVE_NDK_BIN variable of the Android environment)
#   page-build     npm --prefix app run build                        the page compiles
#   page-unit      npm --prefix app test                             the page's unit tests
#   page-browser   npm --prefix app run test:browser                 the page in WebKit and Chromium
#   design         node docs/design/render.cjs --check               every drawn view renders (the design render check)
#   release-build  sh scripts/android-build.sh release               an UNSIGNED release APK
#   release-apk    sh scripts/android-apk-check.sh <apk> release     the APK against the rules of the release
#   lock           git diff --quiet -- Cargo.lock                    no dependency moved (judged only when the tree was clean at
#                                                                    the start; otherwise a note is printed and nothing is judged)
#   emulator       sh scripts/android-e2e.sh matrix                  only with --emulator
#
# What it cannot cover: anything on a phone or a tablet, and the checks that an emulator cannot make (the matrix prints them as
# `not run`; a `not run` is not a pass). It does not sign, install or download anything, and it never addresses a physical
# device: the emulator step is the script that only talks to an attached emulator. It writes nothing outside the folders that
# its steps write to (target/, app/dist and the Android project's build folder) and one private temporary folder that it removes.
#
# With --emulator the three system images are checked first: a missing one is exit status 2 with the one `sdkmanager` line as
# text for the owner to run; the gate never installs a package.
#
# Exit status: 0 every step passed, 1 a step failed, 2 a tool or an image is missing or the call is wrong. A missing tool is
# never a skip.

here=$(cd "$(dirname "$0")" && pwd -P)
cd "$here/.." || exit 2

usage() {
    echo "usage: sh scripts/android-gate.sh [--emulator]" >&2
    exit 2
}

EMULATOR=0
case "$#:${1:-}" in
    0:) ;;
    1:--emulator) EMULATOR=1 ;;
    *) usage ;;
esac

. scripts/android-env.sh

for tool in cargo npm node git; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        echo "android-gate: $tool is missing: run . scripts/android-env.sh && sh scripts/android-env-check.sh, and npm --prefix app ci (docs/building.md)" >&2
        exit 2
    fi
done

if [ "$EMULATOR" -eq 1 ]; then
    missing=
    for image in 'android-37.0/default/arm64-v8a' 'android-31/default/arm64-v8a' 'android-37.0/google_apis_ps16k/arm64-v8a'; do
        [ -d "$ANDROID_HOME/system-images/$image" ] || missing="$missing $image"
    done
    if [ -n "$missing" ]; then
        echo "android-gate: a system image is missing for the emulator matrix:$missing" >&2
        echo "android-gate: the owner installs them (the gate installs nothing): sdkmanager \"system-images;android-31;default;arm64-v8a\" \"system-images;android-37.0;default;arm64-v8a\" \"system-images;android-37.0;google_apis_ps16k;arm64-v8a\"" >&2
        exit 2
    fi
fi

tmp=$(mktemp -d "${TMPDIR:-/tmp}/holzwave-android-gate.XXXXXX") || exit 2
trap 'rm -rf "$tmp"' EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

# Whether the tree was clean when the gate started: only then may a changed Cargo.lock be blamed on the steps.
if [ -z "$(git status --porcelain 2>/dev/null)" ]; then
    clean=1
else
    clean=0
fi

steps=0
STEP=

step() { # <name>: the steps that follow belong to it
    STEP=$1
}

# run <command...>: run it, show its output as it comes, and stop the gate at the first failure with the command named.
run() {
    echo "gate $STEP: + $*"
    { "$@"; echo "$?" > "$tmp/rc"; } 2>&1 | tee "$tmp/out"
    rc=1
    read -r rc < "$tmp/rc" || rc=1
    if [ "$rc" -ne 0 ]; then
        echo "gate $STEP: FAILED $*" >&2
        exit 1
    fi
}

done_step() {
    steps=$((steps + 1))
    echo "gate $STEP: ok"
}

step env
run sh scripts/android-env-check.sh
done_step

step compile
run sh scripts/android-check.sh
done_step

step script-tests
run sh scripts/test-android-env.sh
run sh scripts/test-android-scripts.sh
run sh scripts/test-android-version-code.sh
run sh scripts/test-android-apk-check.sh
done_step

step workspace
# HOLZWAVE_NDK_BIN is exported by scripts/android-env.sh for the Android steps. A test of the host counts the HOLZWAVE_ variables
# that a shell inherits, so the host tests run without it, as they do in a plain shell.
run env -u HOLZWAVE_NDK_BIN cargo test --workspace --locked --offline
done_step

step page-build
run npm --prefix app run build
done_step

step page-unit
run npm --prefix app test
done_step

step page-browser
run npm --prefix app run test:browser
done_step

step design
run node docs/design/render.cjs --check
done_step

step release-build
run sh scripts/android-build.sh release
apk=$(tail -n 1 "$tmp/out")
if [ -z "$apk" ] || [ ! -f "$apk" ]; then
    echo "gate release-build: FAILED the build printed no path of an APK as its last line (got '$apk')" >&2
    exit 1
fi
done_step

step release-apk
run sh scripts/android-apk-check.sh "$apk" release
done_step

step lock
if [ "$clean" -eq 1 ]; then
    run git diff --quiet -- Cargo.lock
    done_step
else
    steps=$((steps + 1))
    echo "gate lock: ok (not judged: the tree was not clean when the gate started, so a change of Cargo.lock cannot be blamed on the steps)"
fi

if [ "$EMULATOR" -eq 1 ]; then
    step emulator
    run sh scripts/android-e2e.sh matrix
    done_step
fi

echo "android-gate: ok ($steps steps)"
