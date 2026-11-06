#!/bin/sh
# Prove the contract of scripts/android-env.sh and scripts/android-env-check.sh on a fake SDK tree.
#
#   sh scripts/test-android-env.sh
#
# Needs no JDK, no Android SDK, no network and installs nothing. Each case builds a throwaway directory with a fake SDK tree
# (a source.properties with the Pkg.Revision the pins want in every package directory), a copy of the two scripts (and no
# scripts/env.sh, so the real Rust and Zig on the machine cannot get ahead of the stand-ins) and a private PATH of tiny shims
# for java, rustup, zig, adb, sdkmanager, avdmanager, brew and curl. Every shim logs its call to one file; the last
# checks assert that no call installed, added, updated or accepted anything and that brew and curl were never called.
#
# Exit status: 0 everything held, 1 a check failed.

here=$(cd "$(dirname "$0")" && pwd -P)
root=$(cd "$here/.." && pwd -P)

checks=0
failures=0
ok() { # <what held>
  checks=$((checks + 1))
  echo "  ok   $1"
}
bad() { # <what did not hold>
  checks=$((checks + 1))
  failures=$((failures + 1))
  echo "  FAIL $1" >&2
}

tmp=$(mktemp -d "${TMPDIR:-/tmp}/holzwave-android-env.XXXXXX") || exit 1
trap 'rm -rf "$tmp"' EXIT HUP INT TERM
LOG="$tmp/calls.log"
: > "$LOG"

# The pins, read from the script under test.
pin() { (cd "$root" && . scripts/android-env.sh >/dev/null 2>&1 && eval "printf %s \"\$$1\""); }
NDK=$(pin HOLZWAVE_NDK_VERSION)
PLATFORM=$(pin HOLZWAVE_ANDROID_PLATFORM)
BUILD_TOOLS=$(pin HOLZWAVE_BUILD_TOOLS)
PT_MIN=$(pin HOLZWAVE_PLATFORM_TOOLS_MIN)
EM_MIN=$(pin HOLZWAVE_EMULATOR_MIN)
ZIG=$(pin HOLZWAVE_ZIG)
TARGET=$(pin HOLZWAVE_RUST_ANDROID_TARGET)
JDK=$(pin HOLZWAVE_JDK_MAJOR)

n=0
# mkcase: build the fake machine in $c. Variables (defaults in brackets) shape it:
#   C_NDK_REV [pin] C_PLATFORM [yes] C_JAVA [JDK 21 line] C_ZIG [pin] C_RUST [the one target]
mkcase() {
  n=$((n + 1))
  c="$tmp/case$n"
  sdk="$c/sdk"
  mkdir -p "$c/bin" "$c/home" "$c/brew" "$c/jdk/bin" "$c/repo/scripts" "$sdk/ndk/$NDK" "$sdk/build-tools/$BUILD_TOOLS" \
    "$sdk/platform-tools" "$sdk/emulator" "$sdk/cmdline-tools/latest/bin"
  [ "${C_PLATFORM:-yes}" = yes ] && mkdir -p "$sdk/platforms/$PLATFORM"
  printf 'Pkg.Revision = %s\n' "${C_NDK_REV:-$NDK}" > "$sdk/ndk/$NDK/source.properties"
  printf 'Pkg.Revision=%s\n' "$PT_MIN" > "$sdk/platform-tools/source.properties"
  printf 'Pkg.Revision=%s\n' "$EM_MIN" > "$sdk/emulator/source.properties"
  printf 'Pkg.Revision=%s\n' "$BUILD_TOOLS" > "$sdk/build-tools/$BUILD_TOOLS/source.properties"
  printf 'Pkg.Revision=22.0\n' > "$sdk/cmdline-tools/latest/source.properties"
  shim "$c/jdk/bin/java" "echo 'openjdk version \"${C_JAVA:-$JDK.0.5}\" 2025-01-21' >&2"
  shim "$c/bin/zig" "echo ${C_ZIG:-$ZIG}"
  shim "$sdk/platform-tools/adb" "echo 'Android Debug Bridge version 1.0.41'; echo 'Version $PT_MIN-1'"
  shim "$sdk/cmdline-tools/latest/bin/sdkmanager" "echo 'sdkmanager must not be run' >&2; exit 9"
  shim "$c/bin/avdmanager" "exit 9"
  shim "$c/bin/brew" "exit 9"
  shim "$c/bin/curl" "exit 9"
  rust_list=$(echo ${C_RUST:-$TARGET})
  shim "$c/bin/rustup" "case \"\$*\" in 'target list --installed') printf '%s\n' aarch64-apple-darwin $rust_list ;; *) exit 9 ;; esac"
  cp "$here/android-env.sh" "$here/android-env-check.sh" "$c/repo/scripts/"
}
shim() { # <path> <body>: a script that logs its call, then runs the body
  printf '#!/bin/sh\necho "%s $*" >> "$HW_LOG"\n%s\n' "$(basename "$1")" "$2" > "$1"
  chmod +x "$1"
}
# run_check: run the check script of the fake machine; output in $out, status in $rc
run_check() {
  out=$(cd "$c/repo" && env -i HOME="$c/home" HOMEBREW_PREFIX="$c/brew" HW_LOG="$LOG" PATH="$c/bin:/usr/bin:/bin" \
    HOLZWAVE_ANDROID_HOME="$sdk" HOLZWAVE_JAVA_HOME="$c/jdk" sh scripts/android-env-check.sh 2>&1)
  rc=$?
}
fail_lines() { echo "$out" | grep -c '^FAIL '; }
expect_fail() { # <description> <tool> <found> <wanted>: exit 1, one FAIL line naming all three, no ok line
  run_check
  line=$(echo "$out" | grep '^FAIL ')
  if [ "$rc" -eq 1 ] && [ "$(fail_lines)" -eq 1 ] && ! echo "$out" | grep -q '^android-env: ok$' \
    && echo "$line" | grep -q "^FAIL $2 found $3 wanted $4"; then
    ok "$1"
  else
    bad "$1 (rc $rc: $line)"
  fi
}

echo "test 1: a complete fake machine"
mkcase
run_check
oks=$(echo "$out" | grep -c '^ok ')
if [ "$rc" -eq 0 ] && [ "$(echo "$out" | tail -n 1)" = "android-env: ok" ] && ! echo "$out" | grep -q '^FAIL ' && [ "$oks" -ge 9 ]; then
  ok "every tool ok ($oks ok lines), last line android-env: ok, exit 0"
else
  bad "complete machine (rc $rc): $out"
fi
for what in jdk ndk platform build-tools platform-tools emulator sdkmanager rust-target zig; do
  echo "$out" | grep -q "^ok $what " || bad "no ok line for $what"
done
echo "$out" | grep -q "^path ANDROID_HOME $sdk\$" && ok "the resolved paths are printed first" || bad "paths not printed"
[ "$(echo "$out" | head -n 1 | cut -d' ' -f1)" = path ] || bad "paths are not the first lines"

echo "test 2: each wrong or missing item fails once, with found and wanted"
C_NDK_REV=30.0.1; mkcase; unset C_NDK_REV; expect_fail "a wrong NDK revision" ndk 30.0.1 "$NDK"
C_PLATFORM=no; mkcase; unset C_PLATFORM; expect_fail "a missing platform directory" platform missing "$PLATFORM"
C_JAVA=17.0.9; mkcase; unset C_JAVA; expect_fail "a java of major 17" jdk 17.0.9 "major $JDK"
C_ZIG=0.17.1; mkcase; unset C_ZIG; expect_fail "a zig of 0.17.1" zig 0.17.1 "$ZIG"
mkcase; rm -f "$sdk/ndk/$NDK/source.properties"; expect_fail "an NDK with no source.properties" ndk missing "$NDK"
C_RUST=x86_64-linux-android; mkcase; unset C_RUST
run_check
line=$(echo "$out" | grep '^FAIL ')
if [ "$rc" -eq 1 ] && [ "$(fail_lines)" -eq 1 ] && echo "$line" | grep -q "^FAIL rust-target found missing wanted $TARGET\$"; then
  ok "a rustup that does not list $TARGET"
else
  bad "missing rust target (rc $rc: $line)"
fi
printf 'Pkg.Revision=1.0.0\n' > "$sdk/emulator/source.properties"
run_check
echo "$out" | grep -q "^FAIL emulator found 1.0.0 wanted at least $EM_MIN" && [ "$rc" -eq 1 ] \
  && ok "an emulator below the minimum (compared as version numbers)" || bad "emulator minimum"
mkcase
printf 'Pkg.Revision=99.0.0\n' > "$sdk/emulator/source.properties"
run_check
[ "$rc" -eq 0 ] && ok "an emulator above the minimum passes" || bad "emulator above minimum (rc $rc): $out"

echo "test 3: other installed Android Rust targets are information, not a failure"
C_RUST="$TARGET
x86_64-linux-android"; mkcase; unset C_RUST
run_check
if [ "$rc" -eq 0 ] && echo "$out" | grep -q '^info rust-target-unused x86_64-linux-android .*unused'; then
  ok "x86_64-linux-android gives an info line saying it is unused, exit 0"
else
  bad "unused target (rc $rc): $out"
fi

echo "test 5: overrides, default places, nothing written"
mkcase
before=$(cd "$c" && find . -type f -exec cksum {} + | sort | cksum; find "$c" | wc -l)
run_check
after=$(cd "$c" && find . -type f -exec cksum {} + | sort | cksum; find "$c" | wc -l)
[ "$before" = "$after" ] && ok "the check wrote nothing to disk" || bad "the check changed files"
echo "$out" | grep -q "^path JAVA_HOME $c/jdk\$" && ok "HOLZWAVE_JAVA_HOME and HOLZWAVE_ANDROID_HOME override the defaults" || bad "overrides"
env_in() { # <env assignments...>: print "ANDROID_HOME|JAVA_HOME|NDK_HOME" as the script computes them in the fake repo
  (cd "$c/repo" && env -i HOME="$c/home" HOMEBREW_PREFIX="$c/brew" PATH="/usr/bin:/bin" "$@" \
    sh -c '. scripts/android-env.sh; printf "%s|%s|%s" "$ANDROID_HOME" "${JAVA_HOME:-}" "$NDK_HOME"')
}
case "$(uname -s)" in Darwin) def="$c/home/Library/Android/sdk" ;; *) def="$c/home/Android/Sdk" ;; esac
got=$(env_in)
[ "$got" = "$def||$def/ndk/$NDK" ] && ok "default SDK place: $(echo "$def" | sed "s|$c/|<case>/|")" || bad "default place: $got"
mkdir -p "$c/brew/share/android-commandlinetools"
got=$(env_in)
[ "$got" = "$c/brew/share/android-commandlinetools||$c/brew/share/android-commandlinetools/ndk/$NDK" ] \
  && ok "Homebrew's android-commandlinetools is used when it is the one that exists" || bad "brew place: $got"
mkdir -p "$def"
got=$(env_in)
[ "$(echo "$got" | cut -d'|' -f1)" = "$def" ] && ok "the Android Studio place wins when it exists" || bad "studio place: $got"
got=$(env_in HOLZWAVE_ANDROID_HOME="$c/custom")
[ "$got" = "$c/custom||$c/custom/ndk/$NDK" ] && ok "HOLZWAVE_ANDROID_HOME wins over every default; NDK_HOME is pinned by path" || bad "override: $got"
mkdir -p "$c/brew/opt/openjdk@$JDK/bin"
got=$(env_in)
[ "$(echo "$got" | cut -d'|' -f2)" = "$c/brew/opt/openjdk@$JDK" ] && ok "Homebrew's openjdk@$JDK is the JDK default" || bad "brew jdk: $got"

echo "test 6: sourcing twice, no exit, no -e"
mkcase
once=$(cd "$c/repo" && env -i HOME="$c/home" HOMEBREW_PREFIX="$c/brew" PATH="/usr/bin:/bin" HOLZWAVE_ANDROID_HOME="$sdk" HOLZWAVE_JAVA_HOME="$c/jdk" \
  sh -c '. scripts/android-env.sh; printf %s "$PATH"')
twice=$(cd "$c/repo" && env -i HOME="$c/home" HOMEBREW_PREFIX="$c/brew" PATH="/usr/bin:/bin" HOLZWAVE_ANDROID_HOME="$sdk" HOLZWAVE_JAVA_HOME="$c/jdk" \
  sh -c '. scripts/android-env.sh; . scripts/android-env.sh; printf %s "$PATH"')
[ "$once" = "$twice" ] && ok "sourcing twice leaves PATH the same as once" || bad "PATH changed by the second source"
case "$once" in "$c/jdk/bin:"*"$sdk/platform-tools:"*) ok "the JDK is first on PATH, platform-tools is on it" ;; *) bad "PATH order: $once" ;; esac
alive=$(cd "$c/repo" && env -i HOME="$c/home" PATH="/usr/bin:/bin" sh -c '. scripts/android-env.sh; case $- in *e*) echo set-e ;; *) echo alive ;; esac')
[ "$alive" = alive ] && ok "sourcing leaves the shell running and sets no -e" || bad "shell state after sourcing: $alive"
if grep -v '^[[:space:]]*#' "$here/android-env.sh" | grep -Eq '(^|[^[:alnum:]_])(exit|set[[:space:]]+-[a-z]*e)([^[:alnum:]_]|$)'; then
  bad "android-env.sh contains exit or set -e"
else
  ok "android-env.sh contains no exit and no set -e"
fi
real_once=$(cd "$root" && env -i HOME="$HOME" PATH="/usr/bin:/bin" sh -c '. scripts/android-env.sh; printf %s "$PATH"')
real_twice=$(cd "$root" && env -i HOME="$HOME" PATH="/usr/bin:/bin" sh -c '. scripts/android-env.sh; . scripts/android-env.sh; printf %s "$PATH"')
[ "$real_once" = "$real_twice" ] && ok "the same in the repository root (with scripts/env.sh)" || bad "PATH changed in the repository root"

echo "test 4: nothing was installed, added, updated or downloaded"
if grep -Eq '^(brew|curl|avdmanager|sdkmanager) ' "$LOG"; then
  bad "an install tool was called: $(grep -E '^(brew|curl|avdmanager|sdkmanager) ' "$LOG" | head -n 3)"
else
  ok "sdkmanager, avdmanager, brew and curl were never called"
fi
verbs='(^| )(--?install|--?update|--?licenses|install|add|update|upgrade|licenses)( |$)'
if grep -Eq "$verbs" "$LOG"; then
  bad "a call carried an install verb: $(grep -E "$verbs" "$LOG" | head -n 3)"
else
  ok "no call carried an install, add, update or licenses verb"
fi
if grep '^rustup ' "$LOG" | grep -vqx 'rustup target list --installed'; then
  bad "rustup was called with something else than target list --installed"
else
  ok "rustup was only asked for its installed targets"
fi
[ -s "$LOG" ] && ok "the shims were reached ($(wc -l < "$LOG" | tr -d ' ') logged calls)" || bad "no shim was ever called"

if [ "$failures" -ne 0 ]; then
  echo "android-env-test: $failures of $checks checks FAILED" >&2
  exit 1
fi
echo "android-env-test: ok $checks checks"
