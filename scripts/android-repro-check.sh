#!/bin/sh
# Is the unsigned release APK reproducible on this machine? Build it twice from scratch and compare the two files byte for byte.
#
#   . scripts/android-env.sh && sh scripts/android-repro-check.sh
#
# What it does, in order:
#   1. fixes SOURCE_DATE_EPOCH once for both builds (the value already in the environment, else the date of the last commit), so that
#      both builds see the same date even when a commit lands in between;
#   2. clears the outputs a build would reuse: app/src-tauri/gen/android/app/build, app/dist and target/aarch64-linux-android/release,
#      so that the Rust code, the terminal engine, the page and the Gradle project are all built again, not taken from a cache;
#   3. runs `sh scripts/android-build.sh release`, keeps a copy of the APK and its SHA-256 in a private temporary folder;
#   4. clears the same outputs again and builds a second time;
#   5. compares the two APKs with cmp. When they differ it names the entries of the archive whose CRC or size differ (unzip -v), so the
#      cause can be found, and fails.
#   6. clears the outputs once more at the end (the last release APK is left in place only with HOLZWAVE_REPRO_KEEP=1) and removes the
#      temporary folder on every way out.
#
# What it proves: two builds in the same folder, on the same machine, with the same tools and the same date give the same bytes. What
# it does NOT prove: a rebuild in another folder, under another user, on another machine or operating system, with other tool
# versions, or by F-Droid's build server.
#
# It never signs, never creates a key and installs nothing.
#
# Exit status: 0 and the last line `android-repro: ok <sha256>`; 1 a build failed or the two APKs differ; 2 a missing tool or a wrong
# argument.

here=$(cd "$(dirname "$0")" && pwd -P)
cd "$here/.." || exit 2

if [ $# -ne 0 ]; then
    echo "usage: sh scripts/android-repro-check.sh" >&2
    exit 2
fi
. scripts/android-env.sh
for tool in cmp unzip shasum; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        echo "android-repro: $tool is missing" >&2
        exit 2
    fi
done

if [ -z "${SOURCE_DATE_EPOCH:-}" ]; then
    SOURCE_DATE_EPOCH=$(git log -1 --format=%ct 2>/dev/null)
fi
case "$SOURCE_DATE_EPOCH" in
    "" | *[!0-9]*)
        echo "android-repro: no commit date to build from: run inside the git checkout or set SOURCE_DATE_EPOCH to a number" >&2
        exit 2
        ;;
esac
export SOURCE_DATE_EPOCH

tmp=$(mktemp -d "${TMPDIR:-/tmp}/holzwave-repro.XXXXXX") || exit 2
clear_outputs() {
    rm -rf app/src-tauri/gen/android/app/build app/dist target/aarch64-linux-android/release
}
finish() {
    [ "${HOLZWAVE_REPRO_KEEP:-}" = 1 ] || clear_outputs
    rm -rf "$tmp"
}
trap finish EXIT
trap 'exit 1' HUP INT TERM

sha() { shasum -a 256 "$1" | awk '{ print $1 }'; }

build_once() { # <n>: a clean build, the APK copied to $tmp/<n>.apk
    clear_outputs
    echo "android-repro: build $1 of 2 (SOURCE_DATE_EPOCH $SOURCE_DATE_EPOCH, outputs cleared)"
    if ! sh scripts/android-build.sh release >"$tmp/build-$1.log" 2>&1; then
        tail -n 15 "$tmp/build-$1.log" >&2
        echo "android-repro: FAIL build $1 failed" >&2
        exit 1
    fi
    apk=$(tail -n 1 "$tmp/build-$1.log")
    if [ ! -f "$apk" ]; then
        echo "android-repro: FAIL build $1 named no APK ('$apk')" >&2
        exit 1
    fi
    cp "$apk" "$tmp/$1.apk" || exit 1
    echo "android-repro: build $1: $apk $(wc -c <"$tmp/$1.apk" | tr -d ' ') bytes, sha256 $(sha "$tmp/$1.apk")"
}

build_once 1
build_once 2

if cmp -s "$tmp/1.apk" "$tmp/2.apk"; then
    echo "android-repro: ok $(sha "$tmp/1.apk")"
    exit 0
fi
echo "android-repro: FAIL the two release APKs differ" >&2
# The entries of the archive whose size or CRC differ: `unzip -v` lists length, method, size, ratio, date, time, CRC and name.
unzip -v "$tmp/1.apk" 2>/dev/null | awk 'NF >= 8 && $7 ~ /^[0-9a-f]{8}$/ { print $8, $1, $7 }' | sort >"$tmp/1.list"
unzip -v "$tmp/2.apk" 2>/dev/null | awk 'NF >= 8 && $7 ~ /^[0-9a-f]{8}$/ { print $8, $1, $7 }' | sort >"$tmp/2.list"
diffs=$(comm -3 "$tmp/1.list" "$tmp/2.list" | awk '{ print $1 }' | sort -u)
if [ -n "$diffs" ]; then
    echo "android-repro: entries that differ:" >&2
    printf '%s\n' "$diffs" | sed 's/^/  /' >&2
else
    echo "android-repro: every entry has the same size and CRC: the difference is in the archive's own bytes (order, dates, alignment)" >&2
fi
exit 1
