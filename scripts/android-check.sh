#!/bin/sh
# Compile gate for Android. It proves that the Rust of the engine, the host, the local socket and the app shell compiles
# for the one Android target with the pinned NDK. It does not link an app, build the Android project or run anything.
#
#   sh scripts/android-check.sh
#
# What it runs: `cargo check --locked --offline --target aarch64-linux-android` for holzwave-core, holzwave-host,
# holzwave-ipc and holzwave-app, with the environment of scripts/android-env.sh (the NDK compiler and linker variables
# are exported there, so the C build scripts of the engine's dependencies find a compiler). The target is the one fixed
# word below, never a list: the app is arm64 only (a 32-bit ARM or x86 build of the terminal engine stops in its own build
# script, and an x86_64 build cannot be run on an Apple silicon Mac).
#
# The NDK path is pinned by the environment script and printed first, because the engine's Zig build takes the newest NDK
# it finds when only ANDROID_HOME is set.
#
# Exit status: 0 when every check passed (the last line is "android-check: ok"), 1 when a check failed, 2 when a tool or
# the Rust target is missing. A missing tool is a failed gate, never a skip. The script installs nothing and downloads
# nothing (cargo runs --offline); the line it prints for a missing Rust target is text for the owner to run.

TARGET=aarch64-linux-android

here=$(cd "$(dirname "$0")" && pwd -P)
cd "$here/.." || exit 2
. scripts/android-env.sh

echo "android-check: target $TARGET"
echo "android-check: ANDROID_NDK_HOME $ANDROID_NDK_HOME"

if ! command -v cargo >/dev/null 2>&1; then
    echo "android-check: cargo is missing: install the stable toolchain with rustup" >&2
    exit 2
fi
if ! command -v rustup >/dev/null 2>&1; then
    echo "android-check: rustup is missing: install rustup (docs/building.md), then: rustup target add $TARGET" >&2
    exit 2
fi
if ! rustup target list --installed 2>/dev/null | grep -qx "$TARGET"; then
    echo "android-check: the Rust target $TARGET is missing; the owner installs it: rustup target add $TARGET" >&2
    exit 2
fi
if [ ! -f "$ANDROID_NDK_HOME/source.properties" ]; then
    echo "android-check: the pinned NDK is missing at $ANDROID_NDK_HOME; see docs/build-android.md for the install line" >&2
    exit 2
fi
if [ -z "${CC_aarch64_linux_android:-}" ] || [ ! -x "$CC_aarch64_linux_android" ]; then
    echo "android-check: the NDK C compiler for $TARGET was not found in $ANDROID_NDK_HOME; run: sh scripts/android-env-check.sh" >&2
    exit 2
fi
if ! command -v zig >/dev/null 2>&1; then
    echo "android-check: zig is missing: the engine is built by its Zig build (docs/building.md)" >&2
    exit 2
fi

failed=0
for package in holzwave-core holzwave-host holzwave-ipc holzwave-app; do
    echo "+ cargo check --locked --offline --target $TARGET -p $package"
    cargo check --locked --offline --target "$TARGET" -p "$package" || failed=1
done

if [ "$failed" -ne 0 ]; then
    echo "android-check: FAILED" >&2
    exit 1
fi
echo "android-check: ok"
