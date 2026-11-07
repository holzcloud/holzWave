#!/bin/sh
# Prove the refusals and the clean-up of the Android scripts without any Android software.
#
#   sh scripts/test-android-scripts.sh
#
# The scripts under test are scripts/android-check.sh, android-build.sh, android-emulator.sh and android-e2e.sh. Each case
# builds a throwaway directory with a copy of those scripts and of android-env.sh, a fake SDK tree and a private PATH of tiny
# shims for adb, emulator, avdmanager, npm, cargo, rustup, java, zig, uname and for the tools that must never be called
# (sdkmanager, brew, curl, wget, keytool, jarsigner, apksigner). Every shim logs its call to one file; the last checks assert
# that none of the forbidden tools was reached. It needs no JDK, no SDK, no network, no emulator, and installs nothing.
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

tmp=$(mktemp -d "${TMPDIR:-/tmp}/holzwave-android-scripts.XXXXXX") || exit 1
trap 'rm -rf "$tmp"' EXIT HUP INT TERM
LOG="$tmp/calls.log"
ALL="$tmp/all-calls.log"
: > "$LOG"
: > "$ALL"
EXTRA=

pin() { (cd "$root" && . scripts/android-env.sh >/dev/null 2>&1 && eval "printf %s \"\$$1\""); }
NDK=$(pin HOLZWAVE_NDK_VERSION)
API=$(pin HOLZWAVE_ANDROID_API)
TARGET=$(pin HOLZWAVE_RUST_ANDROID_TARGET)
IMAGE='system-images;android-37.0;default;arm64-v8a'

shim() { # <path> <body>: a program that logs its call, then runs the body
  printf '#!/bin/sh\necho "%s $*" >> "$HW_LOG"\n%s\n' "$(basename "$1")" "$2" > "$1"
  chmod +x "$1"
}

n=0
# mkcase: the fake machine in $c. Variables (defaults in brackets):
#   C_UNAME [arm64]  what `uname -m` says
#   C_RUST [yes]     whether rustup lists the Android target
#   C_IMAGE [yes]    whether the arm64 system image directory exists
#   C_CARGO_RC [0]   the exit status of the cargo shim
#   C_EMU            the body of the emulator shim [exit 1: it never boots]
mkcase() {
  n=$((n + 1))
  c="$tmp/case$n"
  sdk="$c/sdk"
  bin="$sdk/ndk/$NDK/toolchains/llvm/prebuilt/darwin-x86_64/bin"
  mkdir -p "$c/bin" "$c/home" "$c/brew" "$c/tmpdir" "$c/jdk/bin" "$c/repo/scripts" "$c/repo/app/src-tauri/gen/android" "$bin" \
    "$sdk/platform-tools" "$sdk/emulator" "$sdk/cmdline-tools/latest/bin"
  [ "${C_IMAGE:-yes}" = yes ] && mkdir -p "$sdk/system-images/android-37.0/default/arm64-v8a"
  printf 'Pkg.Revision = %s\n' "$NDK" > "$sdk/ndk/$NDK/source.properties"
  : > "$bin/aarch64-linux-android$API-clang"; chmod +x "$bin/aarch64-linux-android$API-clang"
  : > "$bin/aarch64-linux-android$API-clang++"; chmod +x "$bin/aarch64-linux-android$API-clang++"
  : > "$bin/llvm-ar"; : > "$bin/llvm-ranlib"
  shim "$c/jdk/bin/java" "echo 'openjdk version \"21.0.5\"' >&2"
  shim "$c/bin/npm" 'echo "npm-env SOURCE_DATE_EPOCH=$SOURCE_DATE_EPOCH RUSTFLAGS=[$RUSTFLAGS] FLAGS=$(printf %s "$CARGO_ENCODED_RUSTFLAGS" | tr "\037" "|")" >> "$HW_LOG"
out=app/src-tauri/gen/android/app/build/outputs/apk/universal
case " $* " in
  *" --debug "*) mkdir -p "$out/debug"; : > "$out/debug/app-universal-debug.apk" ;;
  *) mkdir -p "$out/release"; : > "$out/release/app-universal-release-unsigned.apk" ;;
esac'
  shim "$c/bin/cargo" 'echo "cargo-env CC=$CC_aarch64_linux_android LINKER=$CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER NDK=$ANDROID_NDK_HOME" >> "$HW_LOG"; exit '"${C_CARGO_RC:-0}"
  shim "$c/bin/zig" "echo 0.17.1"
  shim "$c/bin/rustc" "echo 'rustc 1.99.0 (stand-in)'"
  shim "$c/bin/git" "case \"\$*\" in 'log -1 --format=%ct') echo 1700000000 ;; *) exit 1 ;; esac"
  rust_list=
  [ "${C_RUST:-yes}" = yes ] && rust_list=$TARGET
  shim "$c/bin/rustup" "case \"\$*\" in 'target list --installed') printf '%s\n' aarch64-apple-darwin $rust_list ;; *) exit 9 ;; esac"
  shim "$c/bin/adb" 'case "$*" in *devices*) echo "List of devices attached" ;; *) exit 1 ;; esac'
  shim "$c/bin/emulator" "${C_EMU:-exit 1}"
  shim "$c/bin/avdmanager" "cat > /dev/null"
  for forbidden in sdkmanager brew curl wget keytool jarsigner apksigner; do
    shim "$c/bin/$forbidden" "exit 9"
  done
  # uname: -m answers what the case wants, everything else is the real one.
  printf '#!/bin/sh\ncase "$1" in -m) echo %s ;; *) exec /usr/bin/uname "$@" ;; esac\n' "${C_UNAME:-arm64}" > "$c/bin/uname"
  chmod +x "$c/bin/uname"
  cp "$here/android-env.sh" "$here/android-check.sh" "$here/android-build.sh" "$here/android-emulator.sh" "$here/android-e2e.sh" \
    "$c/repo/scripts/"
}
# run <script> [args...]: run a script of the fake machine; output in $out, status in $rc. $EXTRA holds more
# NAME=value pairs for the environment (the script's environment is otherwise empty). The calls of the shims go to $ALL too.
run() {
  s=$1
  shift
  out=$(cd "$c/repo" && env -i HOME="$c/home" HOMEBREW_PREFIX="$c/brew" HW_LOG="$LOG" TMPDIR="$c/tmpdir" \
    PATH="$c/bin:/usr/bin:/bin" HOLZWAVE_ANDROID_HOME="$sdk" HOLZWAVE_JAVA_HOME="$c/jdk" \
    HOLZWAVE_EMULATOR_BOOT_TIMEOUT=6 $EXTRA sh "scripts/$s" "$@" 2>&1)
  rc=$?
  cat "$LOG" >> "$ALL"
}
calls() { grep -c "^$1 " "$LOG"; }
nothing_left() { [ -z "$(ls -A "$c/tmpdir")" ]; }
: > "$LOG"

echo "test 1: the emulator is arm64 only"
for image in 'system-images;android-37.0;default;x86_64' 'system-images;android-31;google_apis;x86' 'system-images;android-30;default;armeabi-v7a'; do
  mkcase
  : > "$LOG"
  EXTRA="HOLZWAVE_ANDROID_IMAGE=$image"
  run android-emulator.sh create
  EXTRA=
  if [ "$rc" -ne 0 ] && echo "$out" | grep -q 'arm64' && [ "$(calls avdmanager)" -eq 0 ]; then
    ok "create refuses '$image', names the arm64 rule and calls no avdmanager (exit $rc)"
  else
    bad "create with '$image' (rc $rc, avdmanager calls $(calls avdmanager)): $out"
  fi
done
mkcase
: > "$LOG"
run android-emulator.sh create
if [ "$rc" -eq 0 ] && grep -q "^avdmanager create avd --force --name holzwave-arm64 --package $IMAGE " "$LOG"; then
  ok "create with the arm64 image passes the id on to avdmanager"
else
  bad "create with an arm64 image (rc $rc): $out / $(grep '^avdmanager' "$LOG")"
fi
C_UNAME=x86_64; mkcase; unset C_UNAME
: > "$LOG"
run android-emulator.sh create
if [ "$rc" -eq 2 ] && echo "$out" | grep -q 'needs an arm64 host' && [ "$(calls avdmanager)" -eq 0 ]; then
  ok "create refuses a host that is not arm64, with the reason, and calls no avdmanager"
else
  bad "non-arm64 host (rc $rc): $out"
fi
C_IMAGE=no; mkcase; unset C_IMAGE
: > "$LOG"
run android-emulator.sh create
if [ "$rc" -eq 2 ] && echo "$out" | grep -q "sdkmanager \"$IMAGE\"" && [ "$(calls sdkmanager)" -eq 0 ] && [ "$(calls avdmanager)" -eq 0 ]; then
  ok "a missing image is exit 2 with the sdkmanager line as text, and nothing is installed"
else
  bad "missing image (rc $rc): $out"
fi

echo "test 2: the build always passes --target aarch64 and never signs"
mkcase
: > "$LOG"
run android-build.sh debug
npm_line=$(grep '^npm ' "$LOG")
targets=$(echo "$npm_line" | grep -o -- '--target [^ ]*' | tr '\n' ',')
if [ "$rc" -eq 0 ] && [ "$targets" = "--target aarch64," ] && echo "$npm_line" | grep -q -- '--debug' \
  && ! echo "$npm_line" | grep -Eq 'x86_64|armv7|i686|--target[^ ]* all|--split-per-abi' \
  && echo "$out" | tail -n 1 | grep -q 'app-universal-debug.apk$'; then
  ok "debug: exactly one --target aarch64, --debug, the APK path is the last line"
else
  bad "debug build (rc $rc, npm: $npm_line): $out"
fi
mkcase
: > "$LOG"
run android-build.sh release
env_line=$(grep '^npm-env ' "$LOG")
repo_real=$(cd "$c/repo" && pwd -P)
flags="--remap-path-prefix=$repo_real=/holzwave-src|--remap-path-prefix=$c/home/.cargo=/cargo-home|--remap-path-prefix=$sdk/ndk/$NDK=/android-ndk"
if [ "$rc" -eq 0 ] && echo "$env_line" | grep -q 'SOURCE_DATE_EPOCH=1700000000 ' && echo "$env_line" | grep -q 'RUSTFLAGS=\[\] ' \
  && [ "$env_line" = "npm-env SOURCE_DATE_EPOCH=1700000000 RUSTFLAGS=[] FLAGS=$flags" ] \
  && grep '^npm ' "$LOG" | grep -q -- ' -- --locked$' && echo "$out" | grep -q '^android-build: rustc 1.99.0' \
  && echo "$out" | grep -q "^android-build: ANDROID_NDK_HOME $sdk/ndk/$NDK\$" && echo "$out" | grep -q '^android-build: SOURCE_DATE_EPOCH 1700000000$'; then
  ok "the build runs with SOURCE_DATE_EPOCH from the last commit, the three path remaps, --locked, and prints the Rust version and the NDK"
else
  bad "reproducibility environment (rc $rc): $env_line (wanted FLAGS=$flags) / $(grep '^npm ' "$LOG")"
fi
mkcase
: > "$LOG"
EXTRA="SOURCE_DATE_EPOCH=1600000000 RUSTFLAGS=-Cdebuginfo=2"
run android-build.sh debug
EXTRA=
env_line=$(grep '^npm-env ' "$LOG")
if [ "$rc" -eq 0 ] && echo "$env_line" | grep -q 'SOURCE_DATE_EPOCH=1600000000 RUSTFLAGS=\[\] ' \
  && echo "$out" | grep -q 'RUSTFLAGS of the environment are replaced'; then
  ok "a SOURCE_DATE_EPOCH of the caller is kept, and a RUSTFLAGS of the caller is replaced by the pinned flags and says so"
else
  bad "caller environment (rc $rc): $env_line / $out"
fi
mkcase
rm -f "$c/bin/git"
printf '#!/bin/sh\nexit 1\n' > "$c/bin/git"; chmod +x "$c/bin/git"
: > "$LOG"
run android-build.sh debug
[ "$rc" -eq 2 ] && echo "$out" | grep -q 'SOURCE_DATE_EPOCH' && [ "$(calls npm)" -eq 0 ] \
  && ok "no commit date and no SOURCE_DATE_EPOCH is exit 2 and builds nothing" || bad "no commit date (rc $rc): $out"
mkcase
: > "$LOG"
run android-build.sh
if [ "$rc" -eq 0 ] && grep '^npm ' "$LOG" | grep -q -- '--debug' && grep '^npm ' "$LOG" | grep -q -- '--target aarch64'; then
  ok "the default is the debug build, with the target"
else
  bad "default build (rc $rc): $out"
fi
mkcase
: > "$LOG"
run android-build.sh release
npm_line=$(grep '^npm ' "$LOG")
targets=$(echo "$npm_line" | grep -o -- '--target [^ ]*' | tr '\n' ',')
if [ "$rc" -eq 0 ] && [ "$targets" = "--target aarch64," ] && ! echo "$npm_line" | grep -q -- '--debug' \
  && echo "$out" | grep -q 'UNSIGNED' && [ "$(calls keytool)" -eq 0 ] && [ "$(calls jarsigner)" -eq 0 ] \
  && [ "$(calls apksigner)" -eq 0 ]; then
  ok "release: the target again, no --debug, the output says UNSIGNED, no signing tool is called"
else
  bad "release build (rc $rc, npm: $npm_line): $out"
fi
mkcase
: > "$LOG"
run android-build.sh sideways
[ "$rc" -eq 2 ] && [ "$(calls npm)" -eq 0 ] && ok "an unknown mode is exit 2 and runs nothing" || bad "unknown mode (rc $rc)"
mkcase
shim "$c/bin/npm" "exit 1"
run android-build.sh debug
[ "$rc" -eq 1 ] && nothing_left && ok "a failed build is exit 1 and leaves nothing behind" || bad "failed build (rc $rc, left: $(ls -A "$c/tmpdir"))"

echo "test 3: the compile gate has one target, installs nothing, and says what is missing"
C_RUST=no; mkcase; unset C_RUST
: > "$LOG"
run android-check.sh
if [ "$rc" -eq 2 ] && echo "$out" | grep -q "rustup target add $TARGET" && [ "$(calls cargo)" -eq 0 ] \
  && ! grep '^rustup ' "$LOG" | grep -q 'target add'; then
  ok "a missing rustup target is exit 2, the add command is printed as text and not run"
else
  bad "missing target (rc $rc): $out"
fi
mkcase
: > "$LOG"
run android-check.sh
cargo_lines=$(grep '^cargo check ' "$LOG")
if [ "$rc" -eq 0 ] && [ "$(echo "$out" | tail -n 1)" = "android-check: ok" ] && [ "$(echo "$cargo_lines" | wc -l | tr -d ' ')" -eq 4 ] \
  && ! echo "$cargo_lines" | grep -v -- "--locked --offline --target $TARGET " | grep -q . \
  && echo "$cargo_lines" | grep -q -- '-p holzwave-core$' && echo "$cargo_lines" | grep -q -- '-p holzwave-host$' \
  && echo "$cargo_lines" | grep -q -- '-p holzwave-ipc$' && echo "$cargo_lines" | grep -q -- '-p holzwave-app$'; then
  ok "four cargo checks, all --locked --offline for $TARGET and for nothing else, last line android-check: ok"
else
  bad "the gate (rc $rc): $out / $cargo_lines"
fi
if grep -q "^cargo-env CC=$sdk/ndk/$NDK/toolchains/llvm/prebuilt/darwin-x86_64/bin/aarch64-linux-android$API-clang LINKER=$sdk/ndk/$NDK/toolchains/llvm/prebuilt/darwin-x86_64/bin/aarch64-linux-android$API-clang NDK=$sdk/ndk/$NDK\$" "$LOG" \
  && echo "$out" | grep -q "^android-check: ANDROID_NDK_HOME $sdk/ndk/$NDK\$"; then
  ok "cargo gets the NDK compiler and linker of the pinned NDK, and the script prints the NDK path"
else
  bad "the compiler environment of cargo: $(grep '^cargo-env' "$LOG" | head -n 1)"
fi
C_CARGO_RC=101; mkcase; unset C_CARGO_RC
run android-check.sh
[ "$rc" -eq 1 ] && ! echo "$out" | grep -q '^android-check: ok$' && nothing_left && ok "a failed cargo check is exit 1, never ok, and leaves nothing" || bad "failed gate (rc $rc)"
mkcase
rm -f "$c/bin/cargo"
run android-check.sh
[ "$rc" -eq 2 ] && ok "a missing cargo is exit 2, not a skip" || bad "missing cargo (rc $rc): $out"
mkcase
rm -rf "$sdk/ndk/$NDK/toolchains"
run android-check.sh
[ "$rc" -eq 2 ] && echo "$out" | grep -q 'NDK C compiler' && ok "an NDK without the compiler is exit 2" || bad "missing compiler (rc $rc): $out"

echo "test 4: the emulator script cleans up on every way out"
mkcase
run android-emulator.sh start
if [ "$rc" -eq 1 ] && echo "$out" | grep -q 'ended early' && nothing_left; then
  ok "a start whose emulator dies is exit 1 and leaves no pid file and no log"
else
  bad "dying emulator (rc $rc, left: $(ls -A "$c/tmpdir")): $out"
fi
C_EMU='exec sleep 4711'; mkcase; unset C_EMU
run android-emulator.sh start
if [ "$rc" -eq 1 ] && echo "$out" | grep -q 'no boot within' && nothing_left; then
  ok "a start that never boots times out as exit 1, kills what it started and leaves nothing"
else
  bad "timeout (rc $rc, left: $(ls -A "$c/tmpdir")): $out"
fi
if pgrep -f 'sleep 4711' >/dev/null 2>&1; then
  bad "the stand-in emulator still runs"
  pkill -f 'sleep 4711' 2>/dev/null
else
  ok "the emulator the script started is not left running"
fi
mkcase
run android-emulator.sh shot "$c/tmpdir/out.png"
[ "$rc" -eq 1 ] && [ ! -e "$c/tmpdir/out.png" ] && [ "$(ls -A "$c/tmpdir" | wc -l | tr -d ' ')" -eq 0 ] \
  && ok "a failed screenshot is exit 1 and leaves no temporary file and no output" || bad "failed shot (rc $rc, left: $(ls -A "$c/tmpdir"))"
mkcase
run android-emulator.sh
[ "$rc" -eq 2 ] && ok "no subcommand is a usage error, exit 2" || bad "usage (rc $rc)"
mkcase
rm -f "$c/bin/adb"
run android-emulator.sh install
[ "$rc" -eq 2 ] && ok "a missing adb is exit 2" || bad "missing adb (rc $rc): $out"

echo "test 5: the end to end script refuses and cleans up"
mkcase
rm -f "$c/bin/adb"
run android-e2e.sh tracer
[ "$rc" -eq 2 ] && nothing_left && ok "a missing adb is exit 2" || bad "e2e without adb (rc $rc): $out"
mkcase
run android-e2e.sh
[ "$rc" -eq 2 ] && ok "no argument is a usage error, exit 2" || bad "e2e usage (rc $rc)"
mkcase
run android-e2e.sh nonsense
[ "$rc" -eq 2 ] && echo "$out" | grep -q 'tracer|service|vault' && ok "an unknown section is a usage error that names the three sections, exit 2" \
  || bad "e2e unknown section (rc $rc): $out"
mkcase
rm -f "$c/bin/adb"
run android-e2e.sh service
[ "$rc" -eq 2 ] && nothing_left && ok "the service section without adb is exit 2" || bad "e2e service without adb (rc $rc): $out"
mkcase
: > "$LOG"
run android-e2e.sh tracer
if [ "$rc" -eq 1 ] && echo "$out" | grep -q 'the emulator did not boot' && nothing_left && [ "$(calls npm)" -eq 0 ]; then
  ok "an emulator that does not boot is exit 1, nothing is built, no temporary file is left"
else
  bad "e2e with a dead emulator (rc $rc, left: $(ls -A "$c/tmpdir")): $out"
fi

mkcase
: > "$LOG"
run android-e2e.sh service
if [ "$rc" -eq 1 ] && echo "$out" | grep -q 'the emulator did not boot' && nothing_left && [ "$(calls npm)" -eq 0 ]; then
  ok "the service section with an emulator that does not boot is exit 1, nothing is built, no temporary file is left"
else
  bad "e2e service with a dead emulator (rc $rc, left: $(ls -A "$c/tmpdir")): $out"
fi

mkcase
rm -f "$c/bin/adb"
run android-e2e.sh vault
[ "$rc" -eq 2 ] && nothing_left && ok "the vault section without adb is exit 2" || bad "e2e vault without adb (rc $rc): $out"
mkcase
: > "$LOG"
run android-e2e.sh vault
if [ "$rc" -eq 1 ] && echo "$out" | grep -q 'the emulator did not boot' && nothing_left && [ "$(calls npm)" -eq 0 ]; then
  ok "the vault section with an emulator that does not boot is exit 1, nothing is built, no screen lock is touched"
else
  bad "e2e vault with a dead emulator (rc $rc, left: $(ls -A "$c/tmpdir")): $out"
fi
if grep -q 'locksettings' "$LOG"; then
  bad "the vault section touched the screen lock before it had set one"
else
  ok "the vault section never calls locksettings before the emulator is up"
fi

echo "test 6: nothing was installed, downloaded or signed"
if grep -Eq '^(sdkmanager|brew|curl|wget|keytool|jarsigner|apksigner) ' "$ALL"; then
  bad "a forbidden tool was called: $(grep -E '^(sdkmanager|brew|curl|wget|keytool|jarsigner|apksigner) ' "$ALL" | head -n 3)"
else
  ok "over every run above, sdkmanager, brew, curl, wget, keytool, jarsigner and apksigner were never called ($(wc -l < "$ALL" | tr -d ' ') calls logged)"
fi
if grep '^rustup ' "$ALL" | grep -vqx 'rustup target list --installed'; then
  bad "rustup was called with something else than target list --installed"
else
  ok "rustup was only asked for its installed targets"
fi
for script in android-check.sh android-build.sh android-emulator.sh android-e2e.sh; do
  # The commands of a script: no comment lines, no lines that only print a message.
  code=$(grep -v '^[[:space:]]*#' "$here/$script" | grep -Ev '^[[:space:]]*(echo|printf) |^[[:space:]]*"|^[[:space:]]*\*\)' || true)
  if echo "$code" | grep -Eq '(^|[[:space:];&|(])(sdkmanager|brew|curl|wget|keytool|jarsigner|apksigner)([[:space:]]|$)'; then
    bad "$script runs an installer or signing tool: $(echo "$code" | grep -E 'sdkmanager|brew|curl|wget|keytool|jarsigner|apksigner' | head -n 2)"
  else
    ok "$script runs no installer, downloader or signing tool (they appear in messages only)"
  fi
  if grep -v '^[[:space:]]*#' "$here/$script" | grep -Eq -- '--target[= ]+(armv7|i686|x86_64|all)|armeabi|(x86_64|i686)-linux-android|armv7-linux'; then
    bad "$script names a target other than aarch64"
  else
    ok "$script names no target other than aarch64"
  fi
done

if [ "$failures" -ne 0 ]; then
  echo "android-scripts-test: $failures of $checks checks FAILED" >&2
  exit 1
fi
echo "android-scripts-test: ok"
