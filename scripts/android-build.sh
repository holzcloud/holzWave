#!/bin/sh
# Build the Android app (an APK) for the one ABI of the project, arm64-v8a.
#
#   sh scripts/android-build.sh [debug|release]      (default: debug)
#
# It sources scripts/android-env.sh and runs the pinned Tauri command line:
#
#   npm --prefix app run tauri -- android build --ci --target aarch64 --apk [--debug]
#
# The target is the one fixed word `aarch64` (Rust target aarch64-linux-android, ABI arm64-v8a) and is passed on every run.
# No other target is ever named: the terminal engine does not build for 32-bit ARM or x86, and an x86_64 build cannot be run
# on an Apple silicon Mac.
#
#   debug    the debuggable build, application id ch.holzcloud.holzwave.debug, that scripts/android-e2e.sh installs; the APK is
#            app/src-tauri/gen/android/app/build/outputs/apk/universal/debug/app-universal-debug.apk and holds arm64-v8a only
#   release  the release build WITHOUT a signature. Signing is the owner's own step with the owner's own key; this script
#            never signs, never creates a key and never calls keytool or apksigner. It says it is unsigned.
#
# The Gradle build downloads the Android Gradle plugin, Kotlin and the AndroidX libraries that the generated project names
# from Google's and Maven Central's repositories (that is Gradle's own job; this script installs nothing itself, and no SDK
# package, no Rust target and nothing with brew).
#
# Exit status: 0 and the path of the APK as the last line on success, 1 when the build failed, 2 when a tool is missing or
# the argument is wrong.

here=$(cd "$(dirname "$0")" && pwd -P)
cd "$here/.." || exit 2
. scripts/android-env.sh

mode=${1:-debug}
case "$mode" in
    debug | release) ;;
    *)
        echo "usage: sh scripts/android-build.sh [debug|release]" >&2
        exit 2
        ;;
esac

for tool in npm java; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        echo "android-build: $tool is missing: run . scripts/android-env.sh && sh scripts/android-env-check.sh" >&2
        exit 2
    fi
done
if [ ! -f "$ANDROID_NDK_HOME/source.properties" ]; then
    echo "android-build: the pinned NDK is missing at $ANDROID_NDK_HOME: run sh scripts/android-env-check.sh" >&2
    exit 2
fi
if [ ! -d app/src-tauri/gen/android ]; then
    echo "android-build: app/src-tauri/gen/android is missing: it is part of the repository" >&2
    exit 2
fi

echo "android-build: $mode build, target aarch64 (arm64-v8a)"
echo "android-build: ANDROID_NDK_HOME $ANDROID_NDK_HOME"
if [ "$mode" = debug ]; then
    npm --prefix app run tauri -- android build --ci --target aarch64 --apk --debug || {
        echo "android-build: the build failed" >&2
        exit 1
    }
else
    npm --prefix app run tauri -- android build --ci --target aarch64 --apk || {
        echo "android-build: the build failed" >&2
        exit 1
    }
fi

# The Tauri command line builds the Gradle flavour `universal` when it is not asked to split per ABI. In this project that
# flavour holds exactly one ABI, arm64-v8a (the plugin fixes the list), so the APK is arm64 only whatever it is called. The
# `arm64` folder is what a split build would make.
out=app/src-tauri/gen/android/app/build/outputs/apk
apk=
for candidate in "$out/universal/$mode"/*.apk "$out/arm64/$mode"/*.apk; do
    [ -f "$candidate" ] && apk=$candidate
done
if [ -z "$apk" ]; then
    echo "android-build: the build said it was done but there is no APK under $out/universal/$mode" >&2
    exit 1
fi
if [ "$mode" = release ]; then
    case "$apk" in
        *unsigned*) echo "android-build: this APK is UNSIGNED: it cannot be installed until the owner signs it with the owner's own key" ;;
        *) echo "android-build: this APK is named like a signed one: a signing configuration of the owner's is in the project; check it" ;;
    esac
fi
echo "$apk"
