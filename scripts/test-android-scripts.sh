#!/bin/sh
# Prove the refusals and the clean-up of the Android scripts without any Android software.
#
#   sh scripts/test-android-scripts.sh
#
# The scripts under test are scripts/android-check.sh, android-build.sh, android-emulator.sh, android-e2e.sh (with android-adb.sh, the
# one door to adb), android-e2e-summary.sh, android-ssh-server.sh and android-gate.sh (the phase gate, run here with stand-ins for every
# step command). Each case
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
    "$sdk/platform-tools" "$sdk/emulator" "$sdk/cmdline-tools/latest/bin" "$c/state" "$c/home/.android/avd"
  # The three virtual devices exist (their configuration says hw.keyboard=yes, which `create` must turn into no).
  for avd in holzwave-arm64 holzwave-arm64-api31 holzwave-arm64-16k; do
    mkdir -p "$c/home/.android/avd/$avd.avd"
    printf 'hw.keyboard=yes\nhw.ramSize=2G\n' > "$c/home/.android/avd/$avd.avd/config.ini"
  done
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
  shim "$c/bin/adb" "${C_ADB:-case \"\$*\" in *devices*) echo \"List of devices attached\" ;; *) exit 1 ;; esac}"
  shim "$c/bin/emulator" "${C_EMU:-exit 1}"
  # avdmanager makes the device's configuration the way the real one does (with a hardware keyboard), for the profile's name.
  shim "$c/bin/avdmanager" 'cat > /dev/null; n=; while [ $# -gt 0 ]; do [ "$1" = --name ] && n=$2; shift; done
mkdir -p "$HOME/.android/avd/$n.avd"; printf "hw.keyboard=yes\nhw.ramSize=2G\n" > "$HOME/.android/avd/$n.avd/config.ini"'
  for forbidden in sdkmanager brew curl wget keytool jarsigner apksigner; do
    shim "$c/bin/$forbidden" "exit 9"
  done
  # uname: -m answers what the case wants, everything else is the real one.
  printf '#!/bin/sh\ncase "$1" in -m) echo %s ;; *) exec /usr/bin/uname "$@" ;; esac\n' "${C_UNAME:-arm64}" > "$c/bin/uname"
  chmod +x "$c/bin/uname"
  cp "$here/android-env.sh" "$here/android-check.sh" "$here/android-build.sh" "$here/android-emulator.sh" "$here/android-e2e.sh" \
    "$here/android-adb.sh" "$here/android-e2e-summary.sh" "$here/android-ssh-server.sh" "$here/android-gate.sh" "$c/repo/scripts/"
}
# run <script> [args...]: run a script of the fake machine; output in $out, status in $rc. $EXTRA holds more
# NAME=value pairs for the environment (the script's environment is otherwise empty). The calls of the shims go to $ALL too.
run() {
  s=$1
  shift
  out=$(cd "$c/repo" && env -i HOME="$c/home" HOMEBREW_PREFIX="$c/brew" HW_LOG="$LOG" TMPDIR="$c/tmpdir" \
    PATH="$c/bin:/usr/bin:/bin" HOLZWAVE_ANDROID_HOME="$sdk" HOLZWAVE_JAVA_HOME="$c/jdk" \
    HW_STATE="$c/state" HW_DEVICES="$c/devices" HW_ORDER="$c/order.log" HW_QEMU="${C_QEMU:-1}" \
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
[ "$rc" -eq 2 ] && [ ! -e "$c/tmpdir/out.png" ] && [ "$(ls -A "$c/tmpdir" | wc -l | tr -d ' ')" -eq 0 ] \
  && ok "a screenshot with no emulator attached is exit 2 and leaves no temporary file and no output" || bad "shot without an emulator (rc $rc, left: $(ls -A "$c/tmpdir"))"
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
[ "$rc" -eq 2 ] && echo "$out" | grep -q 'tracer|service|vault|tls|handover|clipboard' && ok "an unknown section is a usage error that names the six sections, exit 2" \
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

mkcase
rm -f "$c/bin/adb"
run android-e2e.sh tls
[ "$rc" -eq 2 ] && nothing_left && ok "the tls section without adb is exit 2" || bad "e2e tls without adb (rc $rc): $out"
mkcase
: > "$LOG"
run android-e2e.sh tls
if [ "$rc" -eq 1 ] && echo "$out" | grep -q 'the emulator did not boot' && nothing_left && [ "$(calls npm)" -eq 0 ]; then
  ok "the tls section with an emulator that does not boot is exit 1, nothing is built, no server is started, no screen lock is touched"
else
  bad "e2e tls with a dead emulator (rc $rc, left: $(ls -A "$c/tmpdir")): $out"
fi
if grep -q 'locksettings' "$LOG" || grep -q 's_server' "$LOG"; then
  bad "the tls section touched the screen lock or started a server before the emulator was up"
else
  ok "the tls section calls neither locksettings nor a TLS server before the emulator is up"
fi


mkcase
rm -f "$c/bin/adb"
run android-e2e.sh handover
[ "$rc" -eq 2 ] && nothing_left && ok "the handover section without adb is exit 2" || bad "e2e handover without adb (rc $rc): $out"
mkcase
rm -f "$c/bin/adb"
run android-e2e.sh clipboard
[ "$rc" -eq 2 ] && nothing_left && ok "the clipboard section without adb is exit 2" || bad "e2e clipboard without adb (rc $rc): $out"
for section in handover clipboard; do
  mkcase
  : > "$LOG"
  run android-e2e.sh "$section"
  if [ "$rc" -eq 1 ] && echo "$out" | grep -q 'the emulator did not boot' && nothing_left && [ "$(calls npm)" -eq 0 ]; then
    ok "the $section section with an emulator that does not boot is exit 1, nothing is built, no temporary file is left"
  else
    bad "e2e $section with a dead emulator (rc $rc, left: $(ls -A "$c/tmpdir")): $out"
  fi
  if grep -q 'locksettings' "$LOG"; then
    bad "the $section section touched the screen lock"
  else
    ok "the $section section never calls locksettings"
  fi
done

echo "test 5b: the parser of the dialog driver finds a node in a uiautomator dump"
# The function under test is the one in the script, cut out by its first and last line, so what is proved is what runs.
sed -n '/^ui_center() {/,/^}/p' "$here/android-e2e.sh" > "$tmp/ui_center.sh"
if [ "$(wc -l < "$tmp/ui_center.sh" | tr -d ' ')" -gt 10 ]; then
  . "$tmp/ui_center.sh"
  # A dump in the format `uiautomator dump` writes: one line, one node per element, attributes in double quotes.
  cat > "$tmp/dump-a.xml" <<'XML'
<?xml version='1.0' encoding='UTF-8' standalone='yes' ?><hierarchy rotation="0"><node index="0" text="" resource-id="" class="android.widget.FrameLayout" package="com.android.documentsui" content-desc="" bounds="[0,0][1080,2400]"><node index="1" text="Downloads" resource-id="" class="android.widget.TextView" content-desc="" bounds="[157,163][421,234]" /><node index="2" text="" resource-id="" class="android.widget.ImageButton" content-desc="Show roots" bounds="[0,136][147,262]" /><node index="3" text="SAVE" resource-id="android:id/button1" class="android.widget.Button" content-desc="" bounds="[812,2148][1043,2274]" /><node index="4" text="hw späce &amp; &lt;x&gt; &quot;q&quot; it&apos;s.bin" resource-id="" class="android.widget.TextView" content-desc="" bounds="[196,1043][497,1094]" /><node index="5" text="hw-big.bin" resource-id="android:id/title" class="android.widget.TextView" content-desc="" bounds="[100,300][300,400]" /><node index="6" text="hw-big.bin" resource-id="android:id/title" class="android.widget.TextView" content-desc="" bounds="[500,900][700,1000]" /><node index="7" text="Huge" resource-id="" class="android.widget.TextView" content-desc="" bounds="[2147483000,10][2147483600,50]" /><node index="8" text="No bounds" resource-id="" class="android.widget.TextView" content-desc="" /><node index="9" text="Odd bounds" resource-id="" class="android.widget.TextView" content-desc="" bounds="[a,b][c,d]" /><node index="10" text="Downloads folder" resource-id="com.android.documentsui:id/breadcrumb_text" class="android.widget.TextView" content-desc="" bounds="[0,262][288,388]" /></node></hierarchy>
XML
  # The same screen in the layout of an older dump: one node per line.
  cat > "$tmp/dump-b.xml" <<'XML'
<?xml version='1.0' encoding='UTF-8' standalone='yes' ?>
<hierarchy rotation="0">
  <node index="0" text="Recent files" content-desc="" resource-id="x" bounds="[63,388][890,546]" />
  <node index="1" text="" content-desc="Preview the file report.pdf" resource-id="" bounds="[891,546][1080,735]" />
  <node index="2" text="report.pdf" content-desc="" resource-id="android:id/title" bounds="[189,585][581,642]" />
</hierarchy>
XML
  expect_xy() { # <file> <what> <wanted "x y"> <description>
    got=$(ui_center "$1" "$2"); status=$?
    if [ "$status" -eq 0 ] && [ "$got" = "$3" ]; then ok "ui_center: $4 ($got)"; else bad "ui_center: $4 (status $status, got '$got', wanted '$3')"; fi
  }
  expect_none() { # <file> <what> <description>
    got=$(ui_center "$1" "$2"); status=$?
    if [ "$status" -eq 1 ] && [ -z "$got" ]; then ok "ui_center: $3 (status 1, nothing printed)"; else bad "ui_center: $3 (status $status, got '$got')"; fi
  }
  expect_xy "$tmp/dump-a.xml" Downloads "289 198" "a node found by its text has its centre computed from its bounds"
  expect_xy "$tmp/dump-a.xml" "Show roots" "73 199" "a node found by its content description"
  expect_xy "$tmp/dump-a.xml" "id:android:id/button1" "927 2211" "a node found by its resource id (the Save button of the create dialog)"
  expect_xy "$tmp/dump-a.xml" 'hw späce & <x> "q" it'"'"'s.bin' "346 1068" "a text with a letter outside ASCII and escaped characters (&amp; &lt; &gt; &quot; &apos;) is compared after it is unescaped"
  expect_none "$tmp/dump-a.xml" 'hw späce &amp; <x>' "a text is compared after unescaping, not as written in the dump"
  expect_xy "$tmp/dump-a.xml" hw-big.bin "200 350" "a node that is found twice: the first match wins"
  expect_xy "$tmp/dump-a.xml" Huge "2147483300 30" "bounds with a large number"
  expect_xy "$tmp/dump-b.xml" report.pdf "385 613" "the layout with one node per line"
  expect_xy "$tmp/dump-b.xml" "Preview the file report.pdf" "985 640" "a content description that holds the name of a file is not mistaken for the file's own node"
  expect_none "$tmp/dump-a.xml" "Nothing like this" "a node that is not there"
  expect_none "$tmp/dump-a.xml" Down "a text that is only the start of another text is not a match"
  expect_none "$tmp/dump-a.xml" "id:button1" "a resource id is matched whole"
  expect_none "$tmp/dump-a.xml" "No bounds" "a node without bounds is not tapped"
  expect_none "$tmp/dump-a.xml" "Odd bounds" "bounds that are not numbers are not tapped"
  expect_none "$tmp/dump-b.xml" "id:" "an empty resource id names no node"
  : > "$tmp/dump-empty.xml"
  expect_none "$tmp/dump-empty.xml" Downloads "an empty dump"
  expect_none "$tmp/dump-missing.xml" Downloads "a dump that does not exist"
else
  bad "the function ui_center was not found in android-e2e.sh"
fi

echo "test 7: the result table counts what ran, names what did not run, and an empty input is not a pass"
tab=$(printf '\t')
summary() { # <lines...>: feed the lines to the summary script; output in $out, status in $rc
  out=$(printf '%s\n' "$@" | sh "$here/android-e2e-summary.sh" 2>&1)
  rc=$?
}
summary "result${tab}tracer${tab}a${tab}ok${tab}" "result${tab}vault${tab}b${tab}ok${tab}"
if [ "$rc" -eq 0 ] && echo "$out" | tail -n 1 | grep -qx 'e2e summary: ok, not run: 0' && echo "$out" | grep -q '^total  *2  *0  *0$'; then
  ok "all ok is exit 0, the counts are 2, 0, 0 and the last line says not run: 0"
else
  bad "all ok (rc $rc): $out"
fi
summary "result${tab}tracer${tab}a${tab}ok${tab}" "result${tab}touch${tab}pinch${tab}not-run${tab}no two-finger gesture on this image"
if [ "$rc" -eq 0 ] && echo "$out" | grep -q 'touch: pinch -- no two-finger gesture on this image' && echo "$out" | tail -n 1 | grep -qx 'e2e summary: ok, not run: 1'; then
  ok "a not run is exit 0 but is named with its reason and counted on the last line, never as ok"
else
  bad "one not run (rc $rc): $out"
fi
if echo "$out" | grep -q '^total  *1  *1  *0$'; then ok "a not run is not counted among the ok"; else bad "not run counted wrong: $out"; fi
summary "result${tab}tracer${tab}a${tab}ok${tab}" "result${tab}ssh${tab}b${tab}FAIL${tab}the server logged a login"
if [ "$rc" -eq 1 ] && echo "$out" | grep -q 'ssh: b -- the server logged a login' && echo "$out" | tail -n 1 | grep -qx 'e2e summary: FAIL: 1, not run: 0'; then
  ok "one FAIL is exit 1 and is named with its reason"
else
  bad "one FAIL (rc $rc): $out"
fi
summary
if [ "$rc" -eq 2 ] && echo "$out" | grep -q 'nothing ran'; then ok "an empty input is exit 2 (nothing ran is not a pass)"; else bad "empty input (rc $rc): $out"; fi
summary "some other line" "another one"
[ "$rc" -eq 2 ] && ok "input without a single result line is exit 2" || bad "no result lines (rc $rc): $out"
summary "result${tab}vault${tab}a \"quoted\" | piped 'check'${tab}not-run${tab}it's | a \"reason\" with a pipe" "result${tab}vault${tab}c${tab}ok${tab}"
if [ "$rc" -eq 0 ] && echo "$out" | grep -qF "vault: a \"quoted\" | piped 'check' -- it's | a \"reason\" with a pipe" && echo "$out" | grep -q '^vault  *1  *1  *0$'; then
  ok "a check and a reason with pipes and quotes do not break the table"
else
  bad "pipes and quotes (rc $rc): $out"
fi
summary "result${tab}vault${tab}c${tab}maybe${tab}" "result${tab}tracer${tab}d${tab}ok${tab}"
if [ "$rc" -eq 1 ] && echo "$out" | grep -q 'unreadable status "maybe"'; then ok "a status nobody can read is a FAIL, not a pass"; else bad "unknown status (rc $rc): $out"; fi

echo "test 8: every adb call names an emulator, and a physical phone is never addressed"
APK_DUMMY=app-debug.apk
PHYS="ABC123XYZ"
devices_both() { printf 'List of devices attached\n%s\tdevice\nemulator-5554\tdevice\n' "$PHYS" > "$c/devices"; }
RICH_ADB='case "$1" in
  devices) cat "$HW_DEVICES" 2>/dev/null || { echo "List of devices attached"; [ -f "$HW_STATE/up" ] && printf "emulator-5554\tdevice\n"; } ;;
  -s) serial=$2; shift 2
      case "$*" in
        "shell getprop ro.kernel.qemu" | "shell getprop ro.boot.qemu") [ "$serial" = emulator-5554 ] && echo "$HW_QEMU" ;;
        "shell getprop sys.boot_completed") [ -f "$HW_STATE/up" ] && echo 1 ;;
        "shell getprop ro.build.fingerprint") cat "$HW_STATE/avd" 2>/dev/null ;;
        "emu kill") rm -f "$HW_STATE/up" ;;
        *) exit 0 ;;
      esac ;;
  *) exit 0 ;;
esac'
C_ADB=$RICH_ADB; mkcase; unset C_ADB
devices_both
: > "$c/repo/$APK_DUMMY"
: > "$LOG"
run android-emulator.sh install "$APK_DUMMY"
if [ "$rc" -eq 0 ] && grep -q '^adb -s emulator-5554 install -r ' "$LOG" && ! grep -q "$PHYS" "$LOG"; then
  ok "with a phone and an emulator attached the install goes to the emulator by its serial and the phone's serial is in no call"
else
  bad "install with both attached (rc $rc): $out / $(grep '^adb' "$LOG")"
fi
if grep '^adb ' "$LOG" | grep -v '^adb devices' | grep -qv '^adb -s emulator-5554 '; then
  bad "an adb call without the emulator's serial: $(grep '^adb ' "$LOG" | grep -v '^adb devices' | grep -v '^adb -s emulator-5554 ' | head -n 2)"
else
  ok "every adb call but the list of devices carries -s emulator-5554"
fi
printf 'List of devices attached\n%s\tdevice\n' "$PHYS" > "$c/devices"
: > "$LOG"
run android-emulator.sh install "$APK_DUMMY"
if [ "$rc" -eq 2 ] && ! grep -Eq '^adb .*( shell| install| push| wm )|locksettings' "$LOG" && ! grep -q "$PHYS" "$LOG"; then
  ok "with only a phone attached the install is exit 2 and no shell, install, push, wm or locksettings call was made"
else
  bad "install with only a phone (rc $rc): $out / $(grep '^adb' "$LOG")"
fi
: > "$LOG"
run android-emulator.sh shot "$c/tmpdir/x.png"
[ "$rc" -eq 2 ] && ! grep -Eq '^adb .*( shell| exec-out)' "$LOG" && ok "with only a phone attached a screenshot is exit 2 and no command reached it" || bad "shot with only a phone (rc $rc): $(grep '^adb' "$LOG")"
C_QEMU=0; C_ADB=$RICH_ADB; mkcase; unset C_ADB
devices_both
: > "$LOG"
run android-emulator.sh install "$APK_DUMMY"
if [ "$rc" -eq 2 ] && echo "$out" | grep -q 'emulator property' && ! grep -q '^adb -s emulator-5554 install' "$LOG"; then
  ok "a target that does not report the emulator property is refused (exit 2) before anything is installed"
else
  bad "a non-emulator behind an emulator serial (rc $rc): $out"
fi
: > "$LOG"
run android-e2e.sh tracer
if [ "$rc" -eq 2 ] && ! grep -Eq '^adb .*( install| push| wm )|locksettings|input' "$LOG" && ! grep -q "$PHYS" "$LOG"; then
  ok "the end to end script stops at the same door: exit 2, nothing installed, nothing typed, the phone not addressed"
else
  bad "e2e with a non-emulator target (rc $rc): $out / $(grep '^adb' "$LOG" | head -n 4)"
fi
unset C_QEMU
C_ADB=$RICH_ADB; mkcase; unset C_ADB
printf 'List of devices attached\n%s\tdevice\n' "$PHYS" > "$c/devices"
: > "$LOG"
run android-e2e.sh tracer
if ! grep -q "$PHYS" "$LOG" && ! grep '^adb ' "$LOG" | grep -v '^adb devices' | grep -qv '^adb -s emulator-5554 ' && ! grep -q locksettings "$LOG"; then
  ok "with only a phone attached the end to end script does not address it (no call carries its serial, every call is for the emulator)"
else
  bad "e2e with only a phone: $(grep '^adb' "$LOG" | head -n 4)"
fi
mkcase
EXTRA="HOLZWAVE_EMULATOR_PORT=notaport"
run android-emulator.sh start
rc1=$rc
run android-e2e.sh tracer
EXTRA=
[ "$rc1" -eq 2 ] && [ "$rc" -eq 2 ] && ok "a port that is not a number is refused by both scripts (exit 2), so no text becomes a serial" || bad "bad port (rc $rc1, $rc)"

echo "test 8b: an emulator that is attached but offline for a moment is waited for, not started a second time"
C_ADB=$RICH_ADB; mkcase; unset C_ADB
printf 'List of devices attached\nemulator-5554\toffline\n' > "$c/devices"
( sleep 2; printf 'List of devices attached\nemulator-5554\tdevice\n' > "$c/devices" ) &
flip=$!
: > "$LOG"
run android-emulator.sh start
wait "$flip" 2>/dev/null
if [ "$rc" -eq 0 ] && echo "$out" | grep -q 'already running' && [ "$(calls emulator)" -eq 0 ]; then
  ok "an offline emulator that becomes a device within seconds is reported as running and no second emulator is started"
else
  bad "offline then device (rc $rc, emulator calls $(calls emulator)): $out"
fi

echo "test 9: the throwaway ssh server"
mkcase
cat > "$c/bin/fake-sshd" <<'SH'
#!/bin/sh
exec sleep 4713
SH
chmod +x "$c/bin/fake-sshd"
shim "$c/bin/ssh-keyscan" 'echo "[127.0.0.1]:1 ssh-ed25519 AAAA"'
EXTRA="HOLZWAVE_SSHD=$c/bin/fake-sshd"
run android-ssh-server.sh start
EXTRA=
if [ "$rc" -eq 0 ]; then
  ssh_dir=$(echo "$out" | sed -n 's/^dir=//p')
  cfg="$ssh_dir/sshd_config"
  listen=$(grep -c '^ListenAddress ' "$cfg")
  if [ -d "$ssh_dir" ] && [ "$listen" -eq 1 ] && grep -qx 'ListenAddress 127.0.0.1' "$cfg"; then
    ok "start writes a config with ListenAddress 127.0.0.1 and no other ListenAddress"
  else
    bad "ListenAddress lines: $listen in $cfg"
  fi
  if grep -qx 'PasswordAuthentication no' "$cfg" && grep -qx 'KbdInteractiveAuthentication no' "$cfg" && grep -qx 'AllowTcpForwarding no' "$cfg" \
    && grep -qx 'X11Forwarding no' "$cfg" && grep -qx "AllowUsers $(id -un)" "$cfg" && grep -qx 'LogLevel VERBOSE' "$cfg"; then
    ok "the config allows only the current user, no password, no forwarding, and logs every login"
  else
    bad "the config: $(cat "$cfg")"
  fi
  for want in dir port user host_key_type host_key_blob fingerprint client_key client_pass proof log; do
    echo "$out" | grep -q "^$want=." || bad "start printed no $want="
  done
  ok "start printed its key=value lines"
  mode=$(ls -ld "$ssh_dir" | cut -c1-10)
  [ "$mode" = "drwx------" ] && ok "the folder is private (mode 0700)" || bad "folder mode $mode"
  if ssh-keygen -y -P '' -f "$ssh_dir/client_key" >/dev/null 2>&1; then
    bad "the client key opens with an empty passphrase (it is not sealed)"
  else
    ok "the throwaway client key is sealed with a passphrase"
  fi
  if grep -rq -F "$(cat "$ssh_dir/client_pass")" "$LOG" "$ssh_dir/sshd_config" 2>/dev/null; then bad "the passphrase is in the log or the config"; else ok "the passphrase is in no command log and not in the config"; fi
  run android-ssh-server.sh stop "$ssh_dir"
  first=$rc
  run android-ssh-server.sh stop "$ssh_dir"
  if [ "$first" -eq 0 ] && [ "$rc" -eq 0 ] && [ ! -e "$ssh_dir" ] && ! pgrep -f 'sleep 4713' >/dev/null 2>&1; then
    ok "stop removes the folder and the server, and is safe to call twice"
  else
    bad "stop (rc $first, $rc), folder $(ls -d "$ssh_dir" 2>&1)"
    pkill -f 'sleep 4713' 2>/dev/null
  fi
else
  bad "start with a stand-in server (rc $rc): $out"
fi
mkcase
mkdir -p "$c/tmpdir/not-ours"
run android-ssh-server.sh stop "$c/tmpdir/not-ours"
[ "$rc" -eq 2 ] && [ -d "$c/tmpdir/not-ours" ] && ok "stop refuses a folder this script did not make and leaves it" || bad "stop of a foreign folder (rc $rc)"
mkcase
EXTRA="HOLZWAVE_SSHD=$c/nowhere/sshd"
run android-ssh-server.sh start
EXTRA=
[ "$rc" -eq 2 ] && nothing_left && ok "start without sshd is exit 2 and leaves nothing" || bad "no sshd (rc $rc, left: $(ls -A "$c/tmpdir"))"
mkcase
mkdir "$c/empty"
out=$(cd "$c/repo" && env -i HOME="$c/home" TMPDIR="$c/tmpdir" PATH="$c/empty" /bin/sh scripts/android-ssh-server.sh start 2>&1)
rc=$?
[ "$rc" -eq 2 ] && echo "$out" | grep -q 'ssh-keygen is missing' && nothing_left && ok "start without ssh-keygen is exit 2 and leaves nothing" || bad "no ssh-keygen (rc $rc): $out"
mkcase
printf '#!/bin/sh\nexit 1\n' > "$c/bin/fake-sshd"
chmod +x "$c/bin/fake-sshd"
shim "$c/bin/ssh-keyscan" 'exit 1'
EXTRA="HOLZWAVE_SSHD=$c/bin/fake-sshd"
run android-ssh-server.sh start
EXTRA=
[ "$rc" -eq 1 ] && nothing_left && ok "a server that stops at start is exit 1 and the folder is removed" || bad "dying server (rc $rc, left: $(ls -A "$c/tmpdir")): $out"

echo "test 10: the three profiles of the emulator"
add_image() { mkdir -p "$sdk/system-images/$1/$2/arm64-v8a"; }
for entry in 'api37:holzwave-arm64:android-37.0:default' 'api31:holzwave-arm64-api31:android-31:default' '16k:holzwave-arm64-16k:android-37.0:google_apis_ps16k'; do
  prof=${entry%%:*}; rest=${entry#*:}; name=${rest%%:*}; rest=${rest#*:}; level=${rest%%:*}; flavour=${rest#*:}
  image="system-images;$level;$flavour;arm64-v8a"
  mkcase
  add_image "$level" "$flavour"
  : > "$LOG"
  run android-emulator.sh create "$prof"
  cfg="$c/home/.android/avd/$name.avd/config.ini"
  if [ "$rc" -eq 0 ] && grep -q "^avdmanager create avd --force --name $name --package $image " "$LOG" && [ "$(grep -c '^hw.keyboard=' "$cfg")" -eq 1 ] && grep -qx 'hw.keyboard=no' "$cfg" && grep -qx 'hw.ramSize=2G' "$cfg"; then
    ok "profile $prof: create makes $name from $image with hw.keyboard=no and keeps the other settings"
  else
    bad "profile $prof create (rc $rc): $out / $(grep '^avdmanager' "$LOG") / $(cat "$cfg")"
  fi
  for bad_image in 'system-images;android-37.0;default;x86_64' 'system-images;android-31;default;armeabi-v7a' 'system-images;android-37.0;google_apis_ps16k;x86'; do
    : > "$LOG"
    EXTRA="HOLZWAVE_ANDROID_IMAGE=$bad_image"
    run android-emulator.sh create "$prof"
    EXTRA=
    if [ "$rc" -eq 2 ] && echo "$out" | grep -q 'arm64' && [ "$(calls avdmanager)" -eq 0 ]; then :; else bad "profile $prof accepted '$bad_image' (rc $rc): $out"; fi
  done
  ok "profile $prof: three images that are not arm64-v8a are refused before avdmanager runs"
  C_IMAGE=no; mkcase; unset C_IMAGE
  rm -rf "$sdk/system-images/$level/$flavour"
  : > "$LOG"
  run android-emulator.sh create "$prof"
  if [ "$rc" -eq 2 ] && echo "$out" | grep -qF "sdkmanager \"$image\"" && [ "$(calls sdkmanager)" -eq 0 ] && [ "$(calls avdmanager)" -eq 0 ]; then
    ok "profile $prof: a missing image is exit 2, the sdkmanager line for $image is printed as text and nothing is installed"
  else
    bad "profile $prof missing image (rc $rc): $out"
  fi
done
mkcase
run android-emulator.sh create sideways
[ "$rc" -eq 2 ] && echo "$out" | grep -q 'unknown profile' && ok "an unknown profile is exit 2" || bad "unknown profile (rc $rc): $out"
mkcase
rm -rf "$c/home/.android/avd/holzwave-arm64-16k.avd"
run android-emulator.sh start 16k
[ "$rc" -eq 2 ] && echo "$out" | grep -q 'create 16k' && ok "start of a device that was never created is exit 2 and names the create command" || bad "start without the device (rc $rc): $out"

echo "test 11: the all and matrix runners"
FAKE_SECTION='#!/bin/sh
avd=$(sed -n "s/.*-avd \([^ ]*\).*/\1/p" "$HW_STATE/avd")
echo "$avd $1" >> "$HW_ORDER"
r="$HOLZWAVE_E2E_RESULTS"
if [ "$FAKE_KILL" = "$avd:$1" ]; then kill -TERM $PPID; sleep 1; exit 1; fi
if [ "$FAKE_FAIL" = "$avd:$1" ]; then printf "result\t%s\t%s\t%s\t%s\n" "$1" "a check" FAIL "broke" >> "$r"; exit 1; fi
if [ "$FAKE_SILENT" = "$avd:$1" ]; then exit 1; fi
if [ "$1" = vault ]; then printf "result\t%s\t%s\t%s\t%s\n" vault "a check" not-run "no | pipe" >> "$r"; fi
printf "result\t%s\t%s\t%s\t%s\n" "$1" "a check" ok "" >> "$r"
exit 0'
RUNNER_ENV='HOLZWAVE_E2E_SKIP_BUILD=1'
runner_case() { # the fake machine with a fake section command
  C_EMU='echo "$*" > "$HW_STATE/avd"; touch "$HW_STATE/up"; exec sleep 4714'; C_ADB=$RICH_ADB; mkcase; unset C_EMU C_ADB
  printf '%s\n' "$FAKE_SECTION" > "$c/repo/fake-section.sh"
  chmod +x "$c/repo/fake-section.sh"
  : > "$c/order.log"
  : > "$LOG"
  rm -f "$c/devices"
}
runner_run() { # <extra env> <args...>
  e=$1
  shift
  EXTRA="$RUNNER_ENV HOLZWAVE_E2E_SECTION_CMD=$c/repo/fake-section.sh $e"
  run android-e2e.sh "$@"
  EXTRA=
}
none_running() { [ ! -f "$c/state/up" ] && ! pgrep -f 'sleep 4714' >/dev/null 2>&1; }
ALL9="tracer service vault tls handover clipboard touch ssh pagesize"
runner_case
runner_run "" all
order=$(awk '{ printf "%s ", $2 }' "$c/order.log")
if [ "$rc" -eq 0 ] && [ "$order" = "$ALL9 " ] && echo "$out" | tail -n 1 | grep -qx 'e2e all: ok'; then
  ok "all runs tracer, service, vault, tls, handover, clipboard, touch, ssh, pagesize in that order and ends ok"
else
  bad "all (rc $rc, order '$order'): $(echo "$out" | tail -n 8)"
fi
if echo "$out" | grep -q '^total  *9  *1  *0$' && echo "$out" | grep -q 'vault: a check -- no | pipe' && echo "$out" | grep -q 'e2e summary: ok, not run: 1'; then
  ok "all prints one table: nine sections, the not run named with its reason (and counted)"
else
  bad "the table of all: $(echo "$out" | grep -A14 'table')"
fi
none_running && ok "the emulator all started is stopped again" || { bad "an emulator is left after all"; pkill -f 'sleep 4714' 2>/dev/null; }
runner_case
runner_run "" matrix
order=$(awk '{ printf "%s:%s ", $1, $2 }' "$c/order.log")
want=""
for sec in $ALL9; do want="${want}holzwave-arm64:$sec "; done
for sec in tracer service vault pagesize; do want="${want}holzwave-arm64-api31:$sec "; done
for sec in tracer service pagesize; do want="${want}holzwave-arm64-16k:$sec "; done
if [ "$rc" -eq 0 ] && [ "$order" = "$want" ]; then
  ok "matrix runs all on the newest image, then tracer service vault pagesize on Android 12, then tracer service pagesize on the 16 KB image"
else
  bad "matrix (rc $rc): order '$order'"
fi
[ "$(echo "$out" | grep -c '^section  *ok  *not run  *FAIL$')" -eq 3 ] && ok "matrix prints one table per image" || bad "tables: $(echo "$out" | grep -c '^section')"
if echo "$out" | grep -q '^e2e matrix: holzwave-arm64\|^e2e matrix: api37: ok, not run: 1' && echo "$out" | grep -q '^e2e matrix: api31: ok, not run: 1' && echo "$out" | grep -q '^e2e matrix: 16k: ok, not run: 0'; then
  ok "matrix ends with one result line per image"
else
  bad "the final lines: $(echo "$out" | tail -n 6)"
fi
[ "$(grep -c '^adb -s emulator-5554 emu kill' "$LOG")" -eq 3 ] && none_running && ok "matrix starts three emulators, one at a time, and stops each of them" \
  || { bad "emulators started/stopped: kills $(grep -c 'emu kill' "$LOG")"; pkill -f 'sleep 4714' 2>/dev/null; }
runner_case
runner_run "FAKE_FAIL=holzwave-arm64-api31:service" matrix
order=$(awk '{ printf "%s:%s ", $1, $2 }' "$c/order.log")
case "$order" in *"holzwave-arm64-16k:pagesize "*) later=yes ;; *) later=no ;; esac
if [ "$rc" -eq 1 ] && [ "$later" = yes ] && echo "$out" | grep -q '^e2e matrix: api31: FAIL: 1' && echo "$out" | grep -q '^e2e matrix: 16k: ok' && none_running; then
  ok "a FAIL on one image does not stop the later images from running, the final status is 1, and the emulator is stopped"
else
  bad "a FAIL on the middle image (rc $rc, later $later): $(echo "$out" | tail -n 5)"
fi
runner_case
runner_run "FAKE_SILENT=holzwave-arm64:touch" all
if [ "$rc" -eq 1 ] && echo "$out" | grep -q 'touch: the section ended well -- exit status 1 and no failed check of its own'; then
  ok "a section that ends with a failure status and no result line of its own is a FAIL in the table"
else
  bad "a silent failure (rc $rc): $(echo "$out" | tail -n 8)"
fi
runner_case
runner_run "FAKE_KILL=holzwave-arm64:service" all
if [ "$rc" -ne 0 ] && none_running; then
  ok "an interrupt in the middle of all stops the emulator the runner started"
else
  bad "interrupted all (rc $rc): emulator state $(ls "$c/state")"
  pkill -f 'sleep 4714' 2>/dev/null
fi
runner_case
runner_run "" all sideways
[ "$rc" -eq 2 ] && echo "$out" | grep -q 'unknown profile' && [ ! -s "$c/order.log" ] && ok "all with an unknown profile is exit 2 and runs nothing" || bad "all sideways (rc $rc): $out"
runner_case
printf 'List of devices attached\nemulator-5554\tdevice\n' > "$c/devices"
runner_run "" matrix
[ "$rc" -eq 2 ] && echo "$out" | grep -q 'already running' && [ ! -s "$c/order.log" ] && ok "matrix refuses to start while an emulator already runs (one at a time)" || bad "matrix with a running emulator (rc $rc): $out"

echo "test 12: the names the touch checks look for are the names the page gives"
for pair in 'app/src/keyrow.ts:aria: "Escape"' 'app/src/keyrow.ts:aria: "Tab"' 'app/src/keyrow.ts:aria: "Control"' 'app/src/keyrow.ts:aria: "Alt"' 'app/src/keyrow.ts:aria: "Arrow up"' \
  'app/src/keyrow.ts:aria: "Arrow down"' 'app/src/keyrow.ts:root.id = "keyrow"' 'app/src/phone.ts:nav.id = "phone-nav"' 'app/src/touchstate.ts:copy: "Copy", paste: "Paste"' \
  'app/src/chrome.ts:"New workspace"' 'app/src/chrome.ts:"Close workspace"'; do
  file=${pair%%:*}
  text=${pair#*:}
  if grep -qF "$text" "$root/$file"; then :; else bad "$file does not contain $text any more: the touch section looks for it"; fi
done
ok "the key row, the phone's bars, the tablet's bar and the selection menu still carry the names the touch section looks for"
for text in 'Escape' 'Control' '"Arrow up"' '"id:keyrow"' '"id:phone-nav"' '"id:bar"' '"New workspace"' '"Copy"' '"Paste"'; do
  grep -qF -- "$text" "$here/android-e2e.sh" || bad "the touch section does not name $text"
done
ok "the touch section names each of them"

echo "test 13: the phase gate runs its steps in order, stops at the first failure and names it, and calls the matrix only with --emulator"
# gatecase: the fake machine with stand-ins for every command the gate runs. The scripts it calls log their call and fail when
# FAKE_FAIL (a comma list of script names) names them; cargo, npm and node fail when HW_FAIL (a comma list of tool:last-word, such
# as npm:test) names the call; git answers `status --porcelain` with nothing (a clean tree) and `diff --quiet` with 0 (unchanged);
# the lock cases give it another shim.
gatecase() {
  mkcase
  for s in android-env-check.sh android-check.sh test-android-env.sh test-android-scripts.sh test-android-version-code.sh \
    test-android-apk-check.sh android-apk-check.sh android-e2e.sh; do
    printf '#!/bin/sh\necho "%s $*" >> "$HW_LOG"\ncase ",${FAKE_FAIL:-}," in *",%s,"*) exit 1 ;; esac\nexit 0\n' "$s" "$s" > "$c/repo/scripts/$s"
  done
  printf '#!/bin/sh\necho "android-build.sh $*" >> "$HW_LOG"\ncase ",${FAKE_FAIL:-}," in *",android-build.sh,"*) exit 1 ;; esac\nmkdir -p out\n: > out/app-universal-release-unsigned.apk\necho "building"\necho out/app-universal-release-unsigned.apk\n' > "$c/repo/scripts/android-build.sh"
  for tool in cargo npm node; do
    shim "$c/bin/$tool" 'echo "ndkbin-for-$(basename "$0")=${HOLZWAVE_NDK_BIN:-unset}" >> "$HW_LOG"
for last in "$@"; do :; done
case ",${HW_FAIL:-}," in *",'"$tool"':$last,"*) exit 1 ;; esac
exit 0'
  done
  shim "$c/bin/git" 'case "$*" in
  "status --porcelain") exit 0 ;;
  "diff --quiet -- Cargo.lock") exit 0 ;;
  *) exit 1 ;;
esac'
  for dir in android-31/default/arm64-v8a android-37.0/google_apis_ps16k/arm64-v8a; do mkdir -p "$sdk/system-images/$dir"; done
}
gate_order() { # the calls of the log that the gate makes, one word each, in order
  grep -E '^(android-env-check|android-check|test-android-env|test-android-scripts|test-android-version-code|test-android-apk-check|android-build|android-apk-check|android-e2e)\.sh |^(cargo|npm|node|git) ' "$LOG" \
    | awk '{ if ($1 == "npm") print "npm:" $NF; else if ($1 == "cargo" || $1 == "node") print $1; else if ($1 == "git") print "git:" $2; else print $1 " " $2 }'
}

gatecase
: > "$LOG"
run android-gate.sh
expected='git:status
android-env-check.sh 
android-check.sh 
test-android-env.sh 
test-android-scripts.sh 
test-android-version-code.sh 
test-android-apk-check.sh 
cargo
npm:build
npm:test
npm:test:browser
node
android-build.sh release
android-apk-check.sh out/app-universal-release-unsigned.apk
git:diff'
got=$(gate_order | sed 's/ $/ /')
if [ "$rc" -eq 0 ] && [ "$got" = "$expected" ]; then
  ok "the gate runs the environment check, the compile gate, the four stand-in tests, the workspace, the page build, unit and browser tests, the design check, the release build, the APK check and the lock check in that order"
else
  bad "the gate's order (rc $rc): $(printf '%s' "$got" | tr '\n' ',')"
fi
for name in env compile script-tests workspace page-build page-unit page-browser design release-build release-apk lock; do
  echo "$out" | grep -qx "gate $name: ok" || bad "no line 'gate $name: ok'"
done
ok "every step printed its 'gate <name>: ok' line"
echo "$out" | tail -n 1 | grep -qx 'android-gate: ok (11 steps)' && ok "the gate ends with its own ok line and the count of steps" || bad "the last line: $(echo "$out" | tail -n 1)"
[ "$(calls android-e2e.sh)" -eq 0 ] && ok "without --emulator the matrix is never called" || bad "the matrix was called without --emulator"
grep -q '^android-apk-check.sh .* release$' "$LOG" && ok "the APK check is given the path that the release build printed last" || bad "the APK check call: $(grep '^android-apk-check' "$LOG")"
nothing_left && ok "the gate leaves nothing in its temporary folder" || bad "left in the temporary folder: $(ls "$c/tmpdir")"
[ "$(grep -c '^ndkbin-for-cargo=unset$' "$LOG")" -eq 1 ] && grep -q '^ndkbin-for-npm=' "$LOG" && ! grep -q '^ndkbin-for-npm=unset$' "$LOG" \
  && ok "the workspace tests run without HOLZWAVE_NDK_BIN, which the Android environment exports (a host test counts the HOLZWAVE_ variables of a shell), and the other steps keep it" \
  || bad "HOLZWAVE_NDK_BIN in the steps: $(grep '^ndkbin-for' "$LOG" | tr '\n' ' ')"

gatecase
: > "$LOG"
EXTRA="FAKE_FAIL=android-check.sh"
run android-gate.sh
EXTRA=
if [ "$rc" -eq 1 ] && echo "$out" | grep -q '^gate compile: FAILED sh scripts/android-check.sh$' && [ "$(calls cargo)" -eq 0 ] && [ "$(calls npm)" -eq 0 ] \
  && ! echo "$out" | grep -q '^gate compile: ok'; then
  ok "a failing compile gate stops the gate at once (exit 1), names the command, and nothing after it runs"
else
  bad "the compile failure (rc $rc, cargo $(calls cargo), npm $(calls npm)): $(echo "$out" | tail -n 4)"
fi
gatecase
: > "$LOG"
EXTRA="FAKE_FAIL=test-android-version-code.sh"
run android-gate.sh
EXTRA=
if [ "$rc" -eq 1 ] && echo "$out" | grep -q '^gate script-tests: FAILED sh scripts/test-android-version-code.sh$' && [ "$(calls test-android-apk-check.sh)" -eq 0 ] \
  && [ "$(calls cargo)" -eq 0 ]; then
  ok "a failing stand-in test names its own command, and the next one does not run"
else
  bad "the stand-in failure (rc $rc): $(echo "$out" | tail -n 4)"
fi
gatecase
: > "$LOG"
EXTRA="HW_FAIL=npm:test"
run android-gate.sh
EXTRA=
if [ "$rc" -eq 1 ] && echo "$out" | grep -q '^gate page-unit: FAILED npm --prefix app test$' && [ "$(calls node)" -eq 0 ] && [ "$(calls android-build.sh)" -eq 0 ]; then
  ok "a failing page test stops the gate before the design check and the release build"
else
  bad "the page test failure (rc $rc, node $(calls node)): $(echo "$out" | tail -n 4)"
fi
gatecase
: > "$LOG"
EXTRA="FAKE_FAIL=android-apk-check.sh"
run android-gate.sh
EXTRA=
if [ "$rc" -eq 1 ] && echo "$out" | grep -q '^gate release-apk: FAILED sh scripts/android-apk-check.sh out/app-universal-release-unsigned.apk release$'; then
  ok "a failing APK check names the command with the path of the APK"
else
  bad "the APK check failure (rc $rc): $(echo "$out" | tail -n 3)"
fi

gatecase
rm -f "$c/bin/node"
: > "$LOG"
run android-gate.sh
if [ "$rc" -eq 2 ] && echo "$out" | grep -q 'node is missing' && [ ! -s "$LOG" ]; then
  ok "a missing tool is exit 2 before any step runs, never a skip"
else
  bad "the missing node (rc $rc, log $(wc -l < "$LOG")): $out"
fi
gatecase
: > "$LOG"
run android-gate.sh --everything
[ "$rc" -eq 2 ] && echo "$out" | grep -q '^usage:' && [ ! -s "$LOG" ] && ok "an unknown argument is exit 2 and runs nothing" || bad "the unknown argument (rc $rc): $out"

gatecase
: > "$LOG"
run android-gate.sh --emulator
if [ "$rc" -eq 0 ] && [ "$(calls android-e2e.sh)" -eq 1 ] && grep -qx 'android-e2e.sh matrix' "$LOG" && [ "$(tail -n 1 "$LOG")" = "android-e2e.sh matrix" ] \
  && echo "$out" | grep -qx 'gate emulator: ok' && echo "$out" | tail -n 1 | grep -qx 'android-gate: ok (12 steps)'; then
  ok "with --emulator the matrix is the last step and is called once"
else
  bad "--emulator (rc $rc): $(grep -c '' "$LOG") calls, last '$(tail -n 1 "$LOG")'"
fi
gatecase
rm -rf "$sdk/system-images/android-31"
: > "$LOG"
run android-gate.sh --emulator
if [ "$rc" -eq 2 ] && echo "$out" | grep -q 'android-31/default/arm64-v8a' && echo "$out" | grep -q 'sdkmanager "system-images;android-31;default;arm64-v8a"' && [ ! -s "$LOG" ]; then
  ok "a missing image is exit 2 with the one sdkmanager line as text, before any step, and nothing is installed"
else
  bad "the missing image (rc $rc): $out"
fi
gatecase
: > "$LOG"
EXTRA="FAKE_FAIL=android-e2e.sh"
run android-gate.sh --emulator
EXTRA=
[ "$rc" -eq 1 ] && echo "$out" | grep -q '^gate emulator: FAILED sh scripts/android-e2e.sh matrix$' && ok "a failing matrix is the gate's failure and names the matrix command" || bad "the matrix failure (rc $rc): $(echo "$out" | tail -n 3)"

gatecase
shim "$c/bin/git" 'case "$*" in "status --porcelain") exit 0 ;; "diff --quiet -- Cargo.lock") exit 1 ;; *) exit 1 ;; esac'
: > "$LOG"
run android-gate.sh
if [ "$rc" -eq 1 ] && echo "$out" | grep -q '^gate lock: FAILED git diff --quiet -- Cargo.lock$'; then
  ok "a changed Cargo.lock on a tree that was clean at the start fails the gate"
else
  bad "the changed lock on a clean tree (rc $rc): $(echo "$out" | tail -n 3)"
fi
gatecase
shim "$c/bin/git" 'case "$*" in "status --porcelain") echo " M README.md" ;; "diff --quiet -- Cargo.lock") exit 1 ;; *) exit 1 ;; esac'
: > "$LOG"
run android-gate.sh
if [ "$rc" -eq 0 ] && echo "$out" | grep -q '^gate lock: ok (not judged' && ! grep -q '^git diff' "$LOG"; then
  ok "on a tree that was not clean at the start the lock is not judged, and the gate says so"
else
  bad "the lock on a dirty tree (rc $rc): $(echo "$out" | tail -n 3)"
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
for script in android-check.sh android-build.sh android-emulator.sh android-e2e.sh android-adb.sh android-e2e-summary.sh android-ssh-server.sh android-gate.sh; do
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
