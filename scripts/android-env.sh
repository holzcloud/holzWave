# Android toolchain environment for local builds and tests of holzWave. Source it, do not run it:
#
#   . scripts/android-env.sh
#
# What it does: sources scripts/env.sh (Rust and Zig) when that file exists, names the Android toolchain pins in one
# place, and exports JAVA_HOME, ANDROID_HOME, ANDROID_NDK_HOME and NDK_HOME, the NDK compiler, archiver and linker of the one
# Rust target (so that a plain `cargo check --target aarch64-linux-android` finds a C compiler for build scripts) and extends
# PATH. Each directory is added to
# PATH only when it exists and is not on PATH yet, so sourcing the file twice changes nothing. It installs nothing and
# writes nothing: the JDK, the SDK packages, the NDK and the Rust target are installed by the owner (docs/build-android.md),
# and `sh scripts/android-env-check.sh` reads the machine against the pins below.
#
# The pins. They are named here and nowhere else; the check script reads them from this file. The versions come from the
# research of 2026-11-05 and from `repository2-3.xml` on dl.google.com (the SDK manager's own repository index). A newer
# stable release may exist by the time you read this: a pin moves only by a change of this file.
#
#   JDK 21               the current long-term-support release; any of 17 to 26 runs the Gradle that the Tauri template pins
#   NDK 30.0.16248370    NDK r30; it is pinned by path below because the engine's Zig build takes the NEWEST NDK it finds
#                        when only ANDROID_HOME is set, and a second NDK on the machine would silently change the build
#   platform android-37.0 and build-tools 37.0.0
#   platform-tools, emulator: a minimum, not an exact version (what the SDK manager offered on 2026-11-06)
#   Zig 0.17.1           exactly, as in docs/building.md
#   one Rust Android target: aarch64-linux-android (ABI arm64-v8a); nothing else is built, listed or documented
#   API level 31        the level of the NDK compiler wrapper (aarch64-linux-android31-clang): the app's minSdk
#
# Where the SDK is looked for, the first one winning: $HOLZWAVE_ANDROID_HOME; $HOME/Library/Android/sdk on macOS or
# $HOME/Android/Sdk elsewhere (the Android Studio place) when that directory exists; Homebrew's
# share/android-commandlinetools (the `android-commandlinetools` cask) when it exists; else the per-OS place above (which
# then does not exist, and the check says so).
# Where the JDK is looked for, the first one winning: $HOLZWAVE_JAVA_HOME; Homebrew's opt/openjdk@21 when it exists;
# `/usr/libexec/java_home -v 21` when that program finds one; else the JAVA_HOME the shell already has.
# $HOMEBREW_PREFIX is used when set, else /opt/homebrew.
#
# This file is sourced into the interactive shell: it never calls `exit` and never sets `set -e`. It is plain POSIX
# sh and also works when sourced from bash or zsh. Source it from the repository root.

HOLZWAVE_JDK_MAJOR=21
HOLZWAVE_NDK_VERSION=30.0.16248370
HOLZWAVE_ANDROID_PLATFORM=android-37.0
HOLZWAVE_BUILD_TOOLS=37.0.0
HOLZWAVE_PLATFORM_TOOLS_MIN=37.0.1
HOLZWAVE_EMULATOR_MIN=37.2.13
HOLZWAVE_ZIG=0.17.1
HOLZWAVE_RUST_ANDROID_TARGET=aarch64-linux-android
HOLZWAVE_ANDROID_API=31

if [ -f scripts/env.sh ]; then
    . scripts/env.sh
fi

_hw_prepend() {
    [ -d "$1" ] || return 0
    case ":$PATH:" in
        *":$1:"*) ;;
        *) PATH="$1:$PATH" ;;
    esac
}

_hw_brew="${HOMEBREW_PREFIX:-/opt/homebrew}"

# The Android SDK.
if [ -n "${HOLZWAVE_ANDROID_HOME:-}" ]; then
    ANDROID_HOME="$HOLZWAVE_ANDROID_HOME"
else
    case "$(uname -s 2>/dev/null)" in
        Darwin) _hw_default="$HOME/Library/Android/sdk" ;;
        *) _hw_default="$HOME/Android/Sdk" ;;
    esac
    ANDROID_HOME="$_hw_default"
    for _hw_candidate in "$_hw_default" "$_hw_brew/share/android-commandlinetools"; do
        if [ -d "$_hw_candidate" ]; then
            ANDROID_HOME="$_hw_candidate"
            break
        fi
    done
    unset _hw_default _hw_candidate
fi

# The NDK is set explicitly, by path, for the reason in the header.
ANDROID_NDK_HOME="$ANDROID_HOME/ndk/$HOLZWAVE_NDK_VERSION"
NDK_HOME="$ANDROID_NDK_HOME"

# The NDK's own LLVM toolchain, for the one Rust target. Cargo's `cc` crate and the linker setting look the compiler up by
# these names; without them a build script that compiles C stops with `ToolNotFound: aarch64-linux-android-clang`. The
# `tauri android build` command sets the same variables itself for its own runs, so these only serve a direct `cargo`. The
# prebuilt directory is named after the host (darwin-x86_64 also runs on Apple silicon); the first one that exists is used.
# The variables are exported only when the compiler is there, so a machine without the NDK gets none and the check says why.
unset HOLZWAVE_NDK_BIN
for _hw_host in darwin-x86_64 linux-x86_64 windows-x86_64; do
    if [ -d "$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/$_hw_host/bin" ]; then
        HOLZWAVE_NDK_BIN="$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/$_hw_host/bin"
        break
    fi
done
unset _hw_host
if [ -n "${HOLZWAVE_NDK_BIN:-}" ] && [ -x "$HOLZWAVE_NDK_BIN/aarch64-linux-android${HOLZWAVE_ANDROID_API}-clang" ]; then
    CC_aarch64_linux_android="$HOLZWAVE_NDK_BIN/aarch64-linux-android${HOLZWAVE_ANDROID_API}-clang"
    CXX_aarch64_linux_android="$HOLZWAVE_NDK_BIN/aarch64-linux-android${HOLZWAVE_ANDROID_API}-clang++"
    AR_aarch64_linux_android="$HOLZWAVE_NDK_BIN/llvm-ar"
    RANLIB_aarch64_linux_android="$HOLZWAVE_NDK_BIN/llvm-ranlib"
    CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER="$CC_aarch64_linux_android"
    CARGO_TARGET_AARCH64_LINUX_ANDROID_AR="$AR_aarch64_linux_android"
    export HOLZWAVE_NDK_BIN CC_aarch64_linux_android CXX_aarch64_linux_android AR_aarch64_linux_android \
        RANLIB_aarch64_linux_android CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER CARGO_TARGET_AARCH64_LINUX_ANDROID_AR
else
    unset HOLZWAVE_NDK_BIN
fi

# The JDK.
if [ -n "${HOLZWAVE_JAVA_HOME:-}" ]; then
    JAVA_HOME="$HOLZWAVE_JAVA_HOME"
elif [ -d "$_hw_brew/opt/openjdk@$HOLZWAVE_JDK_MAJOR" ]; then
    JAVA_HOME="$_hw_brew/opt/openjdk@$HOLZWAVE_JDK_MAJOR"
elif [ -x /usr/libexec/java_home ]; then
    _hw_jh=$(/usr/libexec/java_home -v "$HOLZWAVE_JDK_MAJOR" 2>/dev/null) || _hw_jh=
    if [ -n "$_hw_jh" ] && [ -d "$_hw_jh" ]; then
        JAVA_HOME="$_hw_jh"
    fi
    unset _hw_jh
fi
unset _hw_brew

# Order matters: the last directory prepended wins, so the JDK comes first on PATH.
_hw_prepend "$ANDROID_HOME/platform-tools"
_hw_prepend "$ANDROID_HOME/cmdline-tools/latest/bin"
_hw_prepend "$ANDROID_HOME/emulator"
if [ -n "${JAVA_HOME:-}" ]; then
    _hw_prepend "$JAVA_HOME/bin"
    export JAVA_HOME
fi

unset -f _hw_prepend 2>/dev/null || unset _hw_prepend
export ANDROID_HOME ANDROID_NDK_HOME NDK_HOME PATH
