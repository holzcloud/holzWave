#!/bin/sh
# The end to end proof on an arm64 emulator: the page, the bridge, the core, a pseudo terminal and the system shell.
#
#   sh scripts/android-e2e.sh tracer
#
# What `tracer` does, in order:
#   1. starts the emulator when none runs (and stops it at the end only if it started it), builds the debug APK for
#      arm64-v8a and installs it over any earlier one, with the app's data cleared;
#   2. starts the app's activity, waits for its process and for the window of the app to have the focus, and saves a
#      screenshot to a temporary path (logged, then removed);
#   3. waits until the app has started a shell: a process named `sh` that belongs to the app, which only happens after the
#      page loaded, asked the core for the layout through the Tauri bridge and started a pane;
#   4. taps the middle of the screen to focus the pane and types three lines with the real Android key path (`adb shell input
#      text`, then `adb shell input keyevent 66`, the Enter key):
#          pwd>e2e-pwd           the file must hold the shell's own folder: the folder `home` that the app made inside its
#                                private storage, never the folder with the app's own files
#          echo hello>e2e-echo   the file must say hello
#          readlink /proc/$$/exe>e2e-sh   the file must name a program under /system/bin/ (the system shell)
#   5. reads the three files with `run-as` (the debug build is debuggable, so the app's private files can be read from the
#      shell of adb), prints one line per check, removes the three files, and ends with `e2e tracer: ok`.
#
# What it proves: those five layers work together in one real run on one emulator image. What it does NOT prove: any real
# phone, the keyboard of a phone (the soft keyboard is never opened here; `input text` sends key events), any other Android
# version or WebView than the emulator image carries (the script prints the WebView it found), or the system shell of a
# phone maker.
#
# Environment: HOLZWAVE_EMULATOR_PORT (default 5554) picks the emulator; HOLZWAVE_E2E_SKIP_BUILD=1 reuses the debug APK that
# is already built (for a repeated run).
#
# Exit status: 0 and the last line `e2e tracer: ok`; 1 a check failed; 2 a missing tool or a wrong argument. Everything it
# started is stopped and every temporary file removed on every way out.

PKG=ch.holzcloud.holzwave.debug
ACTIVITY=ch.holzcloud.holzwave.MainActivity
PORT=${HOLZWAVE_EMULATOR_PORT:-5554}

here=$(cd "$(dirname "$0")" && pwd -P)
cd "$here/.." || exit 2
. scripts/android-env.sh

SERIAL=emulator-$PORT
export ANDROID_SERIAL=$SERIAL

[ "${1:-}" = tracer ] || {
    echo "usage: sh scripts/android-e2e.sh tracer" >&2
    exit 2
}
for tool in adb emulator; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        echo "e2e tracer: $tool is missing: run . scripts/android-env.sh && sh scripts/android-env-check.sh" >&2
        exit 2
    fi
done

tmp=$(mktemp -d "${TMPDIR:-/tmp}/holzwave-e2e.XXXXXX") || exit 2
started_emulator=
cleanup() {
    # The three files and the app's own process are cleaned while the emulator is still there.
    if [ -n "$HOMEREL" ]; then
        adb -s "$SERIAL" shell run-as "$PKG" rm -f "$HOMEREL/e2e-pwd" "$HOMEREL/e2e-echo" "$HOMEREL/e2e-sh" >/dev/null 2>&1
    fi
    [ -n "$started_emulator" ] && sh scripts/android-emulator.sh stop >/dev/null 2>&1
    rm -rf "$tmp"
}
HOMEREL=
trap cleanup EXIT HUP INT TERM

fails=0
check() { # <description> <ok|bad> [detail]
    if [ "$2" = ok ]; then
        echo "e2e tracer: ok   $1"
    else
        echo "e2e tracer: FAIL $1 ${3:+($3)}" >&2
        fails=$((fails + 1))
    fi
}
die() { # <message>: a step that cannot continue
    echo "e2e tracer: FAIL $1" >&2
    exit 1
}
adbd() { adb -s "$SERIAL" "$@"; }
# on the device, as the app: run-as runs the command as the app's own user, in its data folder. `adb shell` hands one string to
# the shell of the device, so the whole command is one argument and its own quotes are written for that shell.
as_app() { adbd shell "run-as $PKG $*" | tr -d '\r'; }

start_time=$(date +%s)

# 1. the emulator, the build, the install
if adb devices 2>/dev/null | grep -q "^$SERIAL[[:space:]]*device"; then
    echo "e2e tracer: using the emulator that already runs ($SERIAL)"
else
    sh scripts/android-emulator.sh start || die "the emulator did not boot"
    started_emulator=1
fi
echo "e2e tracer: image $(adbd shell getprop ro.build.fingerprint | tr -d '\r')"
echo "e2e tracer: api $(adbd shell getprop ro.build.version.sdk | tr -d '\r'), abi $(adbd shell getprop ro.product.cpu.abi | tr -d '\r')"
abi=$(adbd shell getprop ro.product.cpu.abi | tr -d '\r')
[ "$abi" = arm64-v8a ] || die "the device is $abi, not arm64-v8a: only arm64 emulators are used"

apk=
if [ "${HOLZWAVE_E2E_SKIP_BUILD:-}" = 1 ]; then
    for candidate in app/src-tauri/gen/android/app/build/outputs/apk/universal/debug/*.apk; do
        [ -f "$candidate" ] && apk=$candidate
    done
else
    apk=$(sh scripts/android-build.sh debug | tail -n 1) || die "the debug build failed"
fi
[ -f "$apk" ] || die "no debug APK ($apk)"
echo "e2e tracer: apk $apk"
adbd uninstall "$PKG" >/dev/null 2>&1
sh scripts/android-emulator.sh install "$apk" || die "the install failed"

# 2. start the app and wait for its window
adbd logcat -c >/dev/null 2>&1
adbd shell am start -W -n "$PKG/$ACTIVITY" >"$tmp/start.txt" 2>&1 || die "the activity did not start: $(cat "$tmp/start.txt")"
waited=0
focused=
while [ "$waited" -lt 90 ]; do
    pid=$(adbd shell pidof "$PKG" | tr -d '\r')
    if [ -n "$pid" ] && adbd shell dumpsys window 2>/dev/null | grep -E 'mCurrentFocus|mFocusedApp' | grep -q "$PKG"; then
        focused=1
        break
    fi
    sleep 2
    waited=$((waited + 2))
done
[ -n "$focused" ] || die "the app has no process or no focused window after 90 s"
check "the app's process $pid exists and its window has the focus" ok
adbd exec-out screencap -p >"$tmp/first.png" 2>/dev/null
echo "e2e tracer: first screenshot $(wc -c <"$tmp/first.png" | tr -d ' ') bytes (removed at the end)"
echo "e2e tracer: webview $(adbd shell dumpsys webviewupdate | tr -d '\r' | grep -E 'Current WebView package|Current WebView provider' | head -n 1 | sed 's/^ *//')"

# 3. the page loaded and started a pane: a shell process of the app
waited=0
shell_up=
while [ "$waited" -lt 120 ]; do
    if as_app ps -A | grep -qw sh; then
        shell_up=1
        break
    fi
    sleep 2
    waited=$((waited + 2))
done
if [ -z "$shell_up" ]; then
    echo "e2e tracer: log of the app follows" >&2
    adbd logcat -d -t 80 2>/dev/null | grep -iE "holzwave|chromium|tauri|RustStdoutStderr|FATAL" | tail -n 40 >&2
    die "no shell of the app after 120 s: the page did not load or no pane started"
fi
check "the page started a pane: the app has a shell process" ok
sleep 3

# 4. focus the pane and type
size=$(adbd shell wm size | tr -d '\r' | sed -n 's/.*: *\([0-9]*\)x\([0-9]*\).*/\1 \2/p' | tail -n 1)
width=${size% *}
height=${size#* }
[ -n "$width" ] && [ -n "$height" ] || die "cannot read the screen size"
adbd shell input tap $((width / 2)) $((height / 2))
sleep 4 # the tap focuses the pane's input and opens the soft keyboard; keys sent before it is up are lost
type_line() { # <text with %s for a space>: type it, then press Enter (keyevent 66)
    adbd shell "input text '$1'"
    sleep 1
    adbd shell input keyevent 66
    sleep 2
}
type_line 'pwd>e2e-pwd'
type_line 'echo%shello>e2e-echo'
type_line 'readlink%s/proc/$$/exe>e2e-sh'

# 5. read back
HOMEREL=$(as_app 'find . -maxdepth 3 -type d -name home' | head -n 1 | sed 's|^\./||')
if [ -z "$HOMEREL" ]; then
    die "the app made no folder named home inside its private storage"
fi
# The shell prints /data/data/<id>/home where run-as resolves /data/user/0/<id>/home: two names for one folder, so the two are
# compared by device and inode number, not by text.
expected=$(as_app "sh -c 'cd $HOMEREL && pwd -P'")
expected_id=$(as_app "stat -c %d:%i $HOMEREL")
check "the shell's folder $expected is inside the app's private storage" ok

waited=0
while [ "$waited" -lt 30 ]; do
    if as_app "ls $HOMEREL" | grep -q e2e-sh; then break; fi
    sleep 2
    waited=$((waited + 2))
done
got_pwd=$(as_app "cat $HOMEREL/e2e-pwd" | head -n 1)
got_echo=$(as_app "cat $HOMEREL/e2e-echo" | head -n 1)
got_sh=$(as_app "cat $HOMEREL/e2e-sh" | head -n 1)
got_id=
case "$got_pwd" in
    /*[\'\"\;\$\&\|\ ]* | "") ;;
    /*) got_id=$(as_app "stat -c %d:%i $got_pwd") ;;
esac
[ -n "$got_id" ] && [ "$got_id" = "$expected_id" ] && check "pwd is the shell's own folder $got_pwd" ok \
    || check "pwd is the shell's own folder" bad "wanted $expected, got '$got_pwd'"
[ "$got_echo" = hello ] && check "a typed echo ran" ok || check "a typed echo ran" bad "wanted hello, got '$got_echo'"
case "$got_sh" in
    /system/bin/*) check "the program is the system shell $got_sh" ok ;;
    *) check "the program is the system shell" bad "wanted a path under /system/bin/, got '$got_sh'" ;;
esac

end_time=$(date +%s)
echo "e2e tracer: $((end_time - start_time)) s"
if [ "$fails" -ne 0 ]; then
    echo "e2e tracer: $fails check(s) failed" >&2
    exit 1
fi
echo "e2e tracer: ok"
