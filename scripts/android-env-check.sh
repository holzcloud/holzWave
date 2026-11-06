#!/bin/sh
# Check the Android toolchain on this machine against the pins of scripts/android-env.sh.
#
#   sh scripts/android-env-check.sh
#
# One line per item:
#   ok <what> <found>                         the item is there and as wanted
#   FAIL <what> found <found> wanted <wanted> the item is missing or wrong; the next line names the command that fixes it
#   info <what> <found>                       something worth knowing that is not a failure
# It prints the resolved paths first, exits 1 when any line is a FAIL, and ends with `android-env: ok` otherwise.
#
# The check only READS. It runs `java -version`, `rustup target list --installed`, `zig version` and `adb version`, and
# reads the `Pkg.Revision` line of the SDK packages' `source.properties` files. It never runs `sdkmanager --install`,
# `sdkmanager --licenses`, `rustup target add`, `brew` or a download: a missing item is a FAIL line that prints, as text, the
# command the owner can run. Installing the toolchain and accepting the SDK licences is the owner's own act.
#
# The pins live in scripts/android-env.sh (one place). They come from the research of 2026-11-05 and `repository2-3.xml` on
# dl.google.com; a newer stable may exist, and a pin moves only by a change of that file.

here=$(cd "$(dirname "$0")" && pwd -P)
root=$(cd "$here/.." && pwd -P)
cd "$root" || exit 1
. scripts/android-env.sh

fails=0
ok() { # <what> <found>
  echo "ok $1 $2"
}
info() { # <what> <found>
  echo "info $1 $2"
}
fail() { # <what> <found> <wanted> <the command that fixes it, as text>
  fails=$((fails + 1))
  echo "FAIL $1 found $2 wanted $3"
  if [ -n "${4:-}" ]; then echo "     fix: $4"; fi
}
# ver_ge <a> <b>: success when version a is at least version b, compared as dotted numbers.
ver_ge() {
  awk -v a="$1" -v b="$2" 'BEGIN {
    na = split(a, x, "."); nb = split(b, y, "."); n = (na > nb) ? na : nb
    for (i = 1; i <= n; i++) { p = x[i] + 0; q = y[i] + 0; if (p > q) exit 0; if (p < q) exit 1 }
    exit 0 }'
}
# revision <package dir>: the Pkg.Revision of its source.properties, empty when there is none.
revision() {
  [ -f "$1/source.properties" ] || return 0
  sed -n 's/^Pkg\.Revision[[:space:]]*=[[:space:]]*//p' "$1/source.properties" | head -n 1 | tr -d '\r '
}

echo "path ANDROID_HOME $ANDROID_HOME"
echo "path ANDROID_NDK_HOME $ANDROID_NDK_HOME"
echo "path JAVA_HOME ${JAVA_HOME:-unset}"

# JDK: the major version, exactly.
jdk_line=
if command -v java >/dev/null 2>&1; then
  jdk_line=$(java -version 2>&1 | sed -n '1s/.*version "\([0-9][0-9.]*\).*/\1/p')
fi
jdk_major=
case "$jdk_line" in
  1.*) jdk_major=$(echo "$jdk_line" | cut -d. -f2) ;;
  "") ;;
  *) jdk_major=$(echo "$jdk_line" | cut -d. -f1) ;;
esac
if [ "$jdk_major" = "$HOLZWAVE_JDK_MAJOR" ]; then
  ok jdk "$jdk_line"
else
  fail jdk "${jdk_line:-none}" "major $HOLZWAVE_JDK_MAJOR" "install a JDK $HOLZWAVE_JDK_MAJOR and point JAVA_HOME at it (docs/build-android.md)"
fi

# NDK: exact revision.
ndk_rev=$(revision "$ANDROID_NDK_HOME")
if [ "$ndk_rev" = "$HOLZWAVE_NDK_VERSION" ]; then
  ok ndk "$ndk_rev"
else
  fail ndk "${ndk_rev:-missing}" "$HOLZWAVE_NDK_VERSION" "sdkmanager \"ndk;$HOLZWAVE_NDK_VERSION\""
fi

# Platform and build-tools: exact directory names.
if [ -d "$ANDROID_HOME/platforms/$HOLZWAVE_ANDROID_PLATFORM" ]; then
  ok platform "$HOLZWAVE_ANDROID_PLATFORM"
else
  fail platform missing "$HOLZWAVE_ANDROID_PLATFORM" "sdkmanager \"platforms;$HOLZWAVE_ANDROID_PLATFORM\""
fi
if [ -d "$ANDROID_HOME/build-tools/$HOLZWAVE_BUILD_TOOLS" ]; then
  ok build-tools "$HOLZWAVE_BUILD_TOOLS"
else
  fail build-tools missing "$HOLZWAVE_BUILD_TOOLS" "sdkmanager \"build-tools;$HOLZWAVE_BUILD_TOOLS\""
fi

# platform-tools and emulator: at least the pinned minimum.
pt_rev=$(revision "$ANDROID_HOME/platform-tools")
if [ -n "$pt_rev" ] && ver_ge "$pt_rev" "$HOLZWAVE_PLATFORM_TOOLS_MIN"; then
  ok platform-tools "$pt_rev"
else
  fail platform-tools "${pt_rev:-missing}" "at least $HOLZWAVE_PLATFORM_TOOLS_MIN" "sdkmanager \"platform-tools\""
fi
em_rev=$(revision "$ANDROID_HOME/emulator")
if [ -n "$em_rev" ] && ver_ge "$em_rev" "$HOLZWAVE_EMULATOR_MIN"; then
  ok emulator "$em_rev"
else
  fail emulator "${em_rev:-missing}" "at least $HOLZWAVE_EMULATOR_MIN" "sdkmanager \"emulator\""
fi

# sdkmanager: present (the owner's tool; it is never run here).
if [ -x "$ANDROID_HOME/cmdline-tools/latest/bin/sdkmanager" ] || command -v sdkmanager >/dev/null 2>&1; then
  ok sdkmanager present
else
  fail sdkmanager missing "$ANDROID_HOME/cmdline-tools/latest/bin/sdkmanager" "unpack the Android command-line tools to $ANDROID_HOME/cmdline-tools/latest (docs/build-android.md)"
fi

# Rust Android target: only aarch64-linux-android is required; any other Android target is unused.
if command -v rustup >/dev/null 2>&1; then
  targets=$(rustup target list --installed 2>/dev/null)
  if echo "$targets" | grep -qx "$HOLZWAVE_RUST_ANDROID_TARGET"; then
    ok rust-target "$HOLZWAVE_RUST_ANDROID_TARGET"
  else
    fail rust-target missing "$HOLZWAVE_RUST_ANDROID_TARGET" "rustup target add $HOLZWAVE_RUST_ANDROID_TARGET"
  fi
  for t in $(echo "$targets" | grep -E 'android' | grep -vx "$HOLZWAVE_RUST_ANDROID_TARGET"); do
    info rust-target-unused "$t (installed, but unused: only $HOLZWAVE_RUST_ANDROID_TARGET is built)"
  done
else
  fail rust-target "rustup missing" "$HOLZWAVE_RUST_ANDROID_TARGET" "install rustup (docs/building.md), then: rustup target add $HOLZWAVE_RUST_ANDROID_TARGET"
fi

# Zig: exactly the pinned version.
zig_found=
if command -v zig >/dev/null 2>&1; then
  zig_found=$(zig version 2>/dev/null | head -n 1)
fi
if [ "$zig_found" = "$HOLZWAVE_ZIG" ]; then
  ok zig "$zig_found"
else
  fail zig "${zig_found:-missing}" "$HOLZWAVE_ZIG" "put Zig $HOLZWAVE_ZIG exactly on PATH (docs/building.md, scripts/env.sh)"
fi

# adb and the system images: information only.
if command -v adb >/dev/null 2>&1; then
  info adb "$(adb version 2>/dev/null | sed -n '1p;2p' | tr '\n' ' ' | sed 's/ *$//')"
else
  info adb "not found"
fi
images=
if [ -d "$ANDROID_HOME/system-images" ]; then
  for api in "$ANDROID_HOME"/system-images/*; do
    [ -d "$api" ] || continue
    for tag in "$api"/*; do
      [ -d "$tag" ] || continue
      for abi in "$tag"/*; do
        [ -d "$abi" ] || continue
        a=$(basename "$abi")
        case "$a" in
          arm64-v8a) images="$images $(basename "$api")/$(basename "$tag")/$a" ;;
          *) info system-image-unused "$(basename "$api")/$(basename "$tag")/$a (not arm64-v8a: the emulator on an Apple silicon Mac cannot run it)" ;;
        esac
      done
    done
  done
fi
info system-images "${images:- none}"

if [ "$fails" -ne 0 ]; then
  echo "android-env: $fails FAIL"
  exit 1
fi
echo "android-env: ok"
