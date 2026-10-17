#!/bin/sh
# Launch check of the Apple on-device model bridge, run on a Mac. It builds the probe example of holzwave-apple-ai, signs
# it ad hoc the way scripts/release-macos.sh signs the app, shows how it is linked, runs it and checks what it prints.
#
#   sh scripts/apple-ai-launch-check.sh
#
# The probe is linked with the arguments the app's build script passes (the weak FoundationModels framework and the
# Swift run path), so a process that contains the bridge must start on a Mac whose macOS has no such framework. The
# file it prints at the end is the one to copy to a Mac below macOS 26 (with scp or a USB stick; AirDrop adds a
# quarantine flag, `xattr -cr availability` removes it) and run there: it must print `availability: os_too_old` and
# `selftest: ok`, with no dyld error and no crash.
#
# Exit status: 0 when every check passed (the last line is "launch-check: ok"), 1 when a check failed, 2 when the
# platform or a tool is missing. A missing tool is a failed gate, never a skip. The script installs nothing.

here=$(cd "$(dirname "$0")" && pwd)
cd "$here/.." || exit 2

if [ "$(uname -s)" != "Darwin" ]; then
    echo "launch-check: this check needs a Mac (uname -s is $(uname -s))" >&2
    exit 2
fi
. "$here/env.sh"

for tool in cargo xcrun otool codesign sw_vers; do
    if ! command -v "$tool" > /dev/null 2>&1; then
        echo "launch-check: $tool is missing" >&2
        exit 2
    fi
done

echo "== system"
sw_vers
xcodebuild -version 2> /dev/null | head -n 1
swift --version 2>&1 | head -n 1

echo "== build"
if ! cargo build --locked --offline --release -p holzwave-apple-ai --example availability; then
    echo "launch-check: the build failed" >&2
    exit 1
fi
binary="$PWD/target/release/examples/availability"
if [ ! -x "$binary" ]; then
    echo "launch-check: $binary was not built" >&2
    exit 1
fi

echo "== signature"
if ! codesign --force --sign - "$binary"; then
    echo "launch-check: signing failed" >&2
    exit 1
fi
if ! codesign --verify --strict "$binary"; then
    echo "launch-check: the signature does not verify" >&2
    exit 1
fi
details=$(codesign -dv "$binary" 2>&1)
echo "$details" | grep -E 'Signature|Identifier'
if ! echo "$details" | grep -q 'Signature=adhoc'; then
    echo "launch-check: the signature is not ad hoc" >&2
    exit 1
fi

echo "== link"
otool -L "$binary"
echo "-- minimum macOS the binary declares"
otool -l "$binary" | grep -A5 LC_BUILD_VERSION
echo "-- dylib load commands that name FoundationModels"
framework=$(otool -l "$binary" | awk '
    /^Load command/ { cmd = "" }
    $1 == "cmd" { cmd = $2 }
    $1 == "name" && cmd ~ /DYLIB/ && $2 ~ /FoundationModels/ { print cmd, $2 }')
if [ -n "$framework" ]; then
    echo "$framework"
fi

echo "== run"
output=$("$binary")
status=$?
echo "$output"
first=$(echo "$output" | sed -n 1p)
second=$(echo "$output" | sed -n 2p)
if [ "$status" -ne 0 ]; then
    echo "launch-check: the probe exited with status $status" >&2
    exit 1
fi
case "$first" in
    "availability: "*) ;;
    *)
        echo "launch-check: the first line is not an availability line" >&2
        exit 1
        ;;
esac
if [ "$second" != "selftest: ok" ]; then
    echo "launch-check: the self test did not report ok" >&2
    exit 1
fi

# FoundationModels must be linked weakly, and only weakly. A strong entry would stop the process on a Mac without it.
if [ -n "$framework" ]; then
    if echo "$framework" | grep -v '^LC_LOAD_WEAK_DYLIB ' > /dev/null; then
        echo "launch-check: FoundationModels is linked strongly" >&2
        exit 1
    fi
elif [ "$first" != "availability: sdk_lacks_framework" ]; then
    echo "launch-check: the binary names no FoundationModels framework although the SDK has it" >&2
    exit 1
fi

echo "signed probe: $binary"
echo "copy it to a Mac below macOS 26 (scp or a USB stick, then xattr -cr availability) and run ./availability"
echo "launch-check: ok"
