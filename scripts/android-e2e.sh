#!/bin/sh
# The end to end proof on an arm64 emulator: the page, the bridge, the core, a pseudo terminal and the system shell.
#
#   sh scripts/android-e2e.sh tracer
#   sh scripts/android-e2e.sh service
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
#      shell of adb), prints one line per check, removes the three files, and ends with `e2e $SECTION: ok`.
#
# What it proves: those five layers work together in one real run on one emulator image. What it does NOT prove: any real
# phone, the keyboard of a phone (the soft keyboard is never opened here; `input text` sends key events), any other Android
# version or WebView than the emulator image carries (the script prints the WebView it found), or the system shell of a
# phone maker.
#
# What `service` does (it starts like `tracer`: emulator, debug APK, fresh install, the app's window, the first shell) and then
# proves the foreground service that keeps shells alive in the background, one line per check:
#   1. dumpsys lists HolzwaveService as a foreground service of the type specialUse (printed as types=0x40000000);
#   2. the notification of the app has the title `holzWave is running`, the text `1 shell`, the action `Stop all`, the channel
#      `Running sessions` (low importance, no sound, no badge), and shows `2 shells` after a second pane is opened by a tap;
#   3. the notification permission: the run grants it with `pm grant`; a run with the permission revoked and the ask noted as made
#      shows that the service is still foreground while its notification is not posted and no dialog asks again; and a run from
#      a cleared app (nothing granted, nothing asked) shows the system's dialog at the first start of a session, taps Allow
#      (found by the resource id of the system's button) and sees the notification show;
#   4. the app goes to the background (HOME), 20 s pass, both shells are still alive and the service is still foreground; the
#      app comes back and the pane still answers (a marker file written by a typed command);
#   5. after one shell exits the text says `1 shell`; the action `Stop all` is tapped in the notification shade (found with
#      uiautomator, never assumed: a shade that cannot be parsed makes the check `not run`, never ok), after which no shell of
#      the app lives, the service is gone, the notification is gone and the app is still running with its window; a new shell
#      then starts the service again.
# What it does NOT prove: the system's permission dialog of another Android version, skin or locale, Android's phantom-process limit (the emulator does
# not apply it as phones do), the skins of phone makers, doze and app standby over hours, a swipe of the app out of the recents
# list, and any real phone.
#
# Environment: HOLZWAVE_EMULATOR_PORT (default 5554) picks the emulator; HOLZWAVE_E2E_SKIP_BUILD=1 reuses the debug APK that
# is already built (for a repeated run).
#
# Exit status: 0 and the last line `e2e $SECTION: ok`; 1 a check failed; 2 a missing tool or a wrong argument. Everything it
# started is stopped and every temporary file removed on every way out.

SECTION=${1:-}
PKG=ch.holzcloud.holzwave.debug
ACTIVITY=ch.holzcloud.holzwave.MainActivity
PORT=${HOLZWAVE_EMULATOR_PORT:-5554}

here=$(cd "$(dirname "$0")" && pwd -P)
cd "$here/.." || exit 2
. scripts/android-env.sh

SERIAL=emulator-$PORT
export ANDROID_SERIAL=$SERIAL

case "$SECTION" in
    tracer | service) ;;
    *)
        echo "usage: sh scripts/android-e2e.sh tracer|service" >&2
        exit 2
        ;;
esac
for tool in adb emulator; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        echo "e2e $SECTION: $tool is missing: run . scripts/android-env.sh && sh scripts/android-env-check.sh" >&2
        exit 2
    fi
done

tmp=$(mktemp -d "${TMPDIR:-/tmp}/holzwave-e2e.XXXXXX") || exit 2
started_emulator=
cleanup() {
    # The three files and the app's own process are cleaned while the emulator is still there.
    if [ -n "$HOMEREL" ]; then
        adb -s "$SERIAL" shell run-as "$PKG" rm -f "$HOMEREL/e2e-pwd" "$HOMEREL/e2e-echo" "$HOMEREL/e2e-sh" "$HOMEREL/e2e-bg" >/dev/null 2>&1
    fi
    [ -n "$started_emulator" ] && sh scripts/android-emulator.sh stop >/dev/null 2>&1
    rm -rf "$tmp"
}
HOMEREL=
trap cleanup EXIT HUP INT TERM

fails=0
check() { # <description> <ok|bad> [detail]
    if [ "$2" = ok ]; then
        echo "e2e $SECTION: ok   $1"
    else
        echo "e2e $SECTION: FAIL $1 ${3:+($3)}" >&2
        fails=$((fails + 1))
    fi
}
die() { # <message>: a step that cannot continue
    echo "e2e $SECTION: FAIL $1" >&2
    exit 1
}
adbd() { adb -s "$SERIAL" "$@"; }
# on the device, as the app: run-as runs the command as the app's own user, in its data folder. `adb shell` hands one string to
# the shell of the device, so the whole command is one argument and its own quotes are written for that shell.
as_app() { adbd shell "run-as $PKG $*" | tr -d '\r'; }

start_time=$(date +%s)

# 1. the emulator, the build, the install
if adb devices 2>/dev/null | grep -q "^$SERIAL[[:space:]]*device"; then
    echo "e2e $SECTION: using the emulator that already runs ($SERIAL)"
else
    sh scripts/android-emulator.sh start || die "the emulator did not boot"
    started_emulator=1
fi
echo "e2e $SECTION: image $(adbd shell getprop ro.build.fingerprint | tr -d '\r')"
echo "e2e $SECTION: api $(adbd shell getprop ro.build.version.sdk | tr -d '\r'), abi $(adbd shell getprop ro.product.cpu.abi | tr -d '\r')"
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
echo "e2e $SECTION: apk $apk"
adbd uninstall "$PKG" >/dev/null 2>&1
sh scripts/android-emulator.sh install "$apk" || die "the install failed"
# The permission of the notification is granted for the run (Android 13 and later), in both sections: without it the app asks
# at the first start of a session, and the system's dialog would take the keys of the tracer. The service section drives the
# dialog itself at its end.
adbd shell pm grant "$PKG" android.permission.POST_NOTIFICATIONS || die "the notification permission could not be granted"

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
echo "e2e $SECTION: first screenshot $(wc -c <"$tmp/first.png" | tr -d ' ') bytes (removed at the end)"
echo "e2e $SECTION: webview $(adbd shell dumpsys webviewupdate | tr -d '\r' | grep -E 'Current WebView package|Current WebView provider' | head -n 1 | sed 's/^ *//')"

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
    echo "e2e $SECTION: log of the app follows" >&2
    adbd logcat -d -t 80 2>/dev/null | grep -iE "holzwave|chromium|tauri|RustStdoutStderr|FATAL" | tail -n 40 >&2
    die "no shell of the app after 120 s: the page did not load or no pane started"
fi
check "the page started a pane: the app has a shell process" ok
sleep 3

if [ "$SECTION" = tracer ]; then
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
fi

if [ "$SECTION" = service ]; then
    pid=$(adbd shell pidof "$PKG" | tr -d '\r')
    notrun=0
    skipped() { # <description> <reason>: a check that was not done is said so, and never counts as ok
        echo "e2e $SECTION: not run $1 ($2)"
        notrun=$((notrun + 1))
    }
    wait_until() { # <seconds> <command...>: run the command every second until it succeeds
        limit=$1
        shift
        waited=0
        while [ "$waited" -lt "$limit" ]; do
            if "$@"; then return 0; fi
            sleep 1
            waited=$((waited + 1))
        done
        return 1
    }
    # The block of dumpsys that belongs to the service: from its ServiceRecord line to the next one.
    service_block() {
        adbd shell dumpsys activity services "$PKG" | tr -d '\r' | awk '/ServiceRecord\{/ { on = ($0 ~ /HolzwaveService/) } on { print }'
    }
    service_line() { service_block | grep -m 1 'isForeground=true'; }
    service_up() { service_line >/dev/null; }
    service_gone() { [ -z "$(service_block)" ]; }
    # The block of dumpsys notification that belongs to the app's notification: from its NotificationRecord line to the next one.
    notification_block() {
        adbd shell dumpsys notification --noredact | tr -d '\r' | awk -v pkg="pkg=$PKG " '/NotificationRecord\(/ { on = (index($0, pkg) > 0) } on { print }'
    }
    notification_text() { notification_block | sed -n 's/.*android\.text=String (\(.*\))$/\1/p' | head -n 1; }
    notification_gone() { [ -z "$(notification_block)" ]; }
    text_is() { [ "$(notification_text)" = "$1" ]; }
    # The shells of the app: processes named sh or mksh whose parent is the app's process.
    shells_alive() {
        adbd shell ps -A -o PID,PPID,NAME | tr -d '\r' | awk -v app="$pid" '$2 == app && ($3 == "sh" || $3 == "mksh") { n++ } END { print n + 0 }'
    }
    no_shells() { [ "$(shells_alive)" -eq 0 ]; }
    two_shells() { [ "$(shells_alive)" -ge 2 ]; }
    has_focus() { adbd shell dumpsys window | grep mCurrentFocus | grep -q "$PKG/$ACTIVITY"; }
    # Tap the first node of the screen whose attributes match an awk pattern (read from a uiautomator dump). 1 when there is none or
    # the dump cannot be read: the caller then says `not run`, never ok.
    tap_node() { # <awk pattern for the node's attributes>
        adbd shell uiautomator dump /sdcard/holzwave-ui.xml >/dev/null 2>&1
        adbd exec-out cat /sdcard/holzwave-ui.xml >"$tmp/ui.xml" 2>/dev/null
        adbd shell rm -f /sdcard/holzwave-ui.xml >/dev/null 2>&1
        node=$(awk -v pat="$1" 'BEGIN { RS = "<" } $0 ~ pat { print }' "$tmp/ui.xml" \
            | sed -n 's/.*bounds="\[\([0-9]*\),\([0-9]*\)\]\[\([0-9]*\),\([0-9]*\)\]".*/\1 \2 \3 \4/p' | head -n 1)
        [ -n "$node" ] || return 1
        set -- $node
        adbd shell input tap $((($1 + $3) / 2)) $((($2 + $4) / 2))
    }
    hide_keyboard() { # the soft keyboard moves the bottom bar up: close it before a tap on the bar
        if adbd shell dumpsys input_method | grep -q 'mInputShown=true'; then
            adbd shell input keyevent 4
            sleep 2
        fi
    }
    bar_tap() {
        hide_keyboard # <plus>: tap the "+" of the bottom bar of the phone layout, at the place the layout gives it
        adbd shell input tap $((width * 78 / 100)) $((height * 924 / 1000))
    }
    type_marker() { # type a command that writes a marker file into the shell's folder, after a tap that focuses the pane
        adbd shell input tap $((width / 2)) $((height / 3))
        sleep 4
        adbd shell "input text '$1'"
        sleep 1
        adbd shell input keyevent 66
        sleep 2
    }
    size=$(adbd shell wm size | tr -d '\r' | sed -n 's/.*: *\([0-9]*\)x\([0-9]*\).*/\1 \2/p' | tail -n 1)
    width=${size% *}
    height=${size#* }
    [ -n "$width" ] && [ -n "$height" ] || die "cannot read the screen size"

    # 1. the foreground service and its type
    wait_until 60 service_up || die "no foreground service of the app after 60 s"
    line=$(service_line)
    case "$line" in
        *types=0x40000000*) check "HolzwaveService is a foreground service of the type specialUse (dumpsys prints types=0x40000000)" ok ;;
        *) check "HolzwaveService is a foreground service of the type specialUse" bad "wanted types=0x40000000 in: $line" ;;
    esac

    # 2. the notification
    wait_until 20 text_is '1 shell' && check "the notification says 1 shell" ok \
        || check "the notification says 1 shell" bad "its text is '$(notification_text)'"
    block=$(notification_block)
    echo "$block" | grep -q 'android.title=String (holzWave is running)' \
        && check "the title is holzWave is running" ok || check "the title is holzWave is running" bad
    echo "$block" | grep -q '\[0\] "Stop all"' \
        && check "the action is Stop all" ok || check "the action is Stop all" bad
    channel=$(echo "$block" | grep -m 1 'effectiveNotificationChannel=')
    case "$channel" in
        *"mName=Running sessions"*"mImportance=2"*"mSound=null"*"mShowBadge=false"*) check "the channel is Running sessions: low importance, no sound, no badge" ok ;;
        *) check "the channel is Running sessions: low importance, no sound, no badge" bad "$channel" ;;
    esac
    echo "$block" | head -n 1 | grep -q 'sound=null' \
        && check "the notification has no sound" ok || check "the notification has no sound" bad
    echo "$block" | head -n 1 | grep -q 'vis=PRIVATE' \
        && check "the notification is private on the lock screen" ok || check "the notification is private on the lock screen" bad

    # 6. counts: a second pane
    bar_tap
    wait_until 30 text_is '2 shells' && check "after a second pane is opened the text says 2 shells" ok \
        || check "after a second pane is opened the text says 2 shells" bad "its text is '$(notification_text)'"

    # 4. background
    HOMEREL=$(as_app 'find . -maxdepth 3 -type d -name home' | head -n 1 | sed 's|^\./||')
    [ -n "$HOMEREL" ] || die "the app made no folder named home inside its private storage"
    adbd shell input keyevent KEYCODE_HOME
    sleep 20
    alive=$(shells_alive)
    [ "$alive" -ge 2 ] && check "after 20 s in the background both shells are alive ($alive)" ok \
        || check "after 20 s in the background both shells are alive" bad "$alive shell(s) of the app"
    service_up && check "after 20 s in the background the service is still foreground" ok \
        || check "after 20 s in the background the service is still foreground" bad
    adbd shell am start -n "$PKG/$ACTIVITY" >/dev/null 2>&1
    wait_until 20 has_focus || die "the app has no focus after it was brought back"
    type_marker 'echo%sbg>e2e-bg'
    waited=0
    while [ "$waited" -lt 20 ]; do
        as_app "ls $HOMEREL" | grep -q e2e-bg && break
        sleep 2
        waited=$((waited + 2))
    done
    [ "$(as_app "cat $HOMEREL/e2e-bg" | head -n 1)" = bg ] && check "back in the front the pane still answers" ok \
        || check "back in the front the pane still answers" bad "the marker was not written"

    # 6. counts: one shell exits
    type_marker 'exit'
    wait_until 30 text_is '1 shell' && check "after one shell exits the text says 1 shell" ok \
        || check "after one shell exits the text says 1 shell" bad "its text is '$(notification_text)'"

    # 5. Stop all, tapped in the shade
    adbd shell cmd statusbar expand-notifications
    sleep 2
    # The node of the action: its description is `Stop all` (the system draws its text in capitals).
    if ! tap_node 'content-desc="Stop all"|text="STOP ALL"'; then
        adbd shell cmd statusbar collapse
        skipped "Stop all" "the notification shade could not be read with uiautomator on this image"
    else
        wait_until 30 no_shells && check "after Stop all no shell of the app lives" ok \
            || check "after Stop all no shell of the app lives" bad "$(shells_alive) shell(s) left"
        wait_until 15 service_gone && check "after Stop all the service is gone" ok \
            || check "after Stop all the service is gone" bad
        wait_until 15 notification_gone && check "after Stop all the notification is gone" ok \
            || check "after Stop all the notification is gone" bad
        adbd shell cmd statusbar collapse
        sleep 2
        [ -n "$(adbd shell pidof "$PKG" | tr -d '\r')" ] && has_focus \
            && check "after Stop all the app is still running with its window" ok \
            || check "after Stop all the app is still running with its window" bad
        # With nothing running the service stays stopped, and a new shell starts it again.
        sleep 6
        service_gone && check "with no session the service stays stopped" ok || check "with no session the service stays stopped" bad
        bar_tap
        wait_until 30 service_up && wait_until 20 text_is '1 shell' \
            && check "a new shell after Stop all starts the service again" ok \
            || check "a new shell after Stop all starts the service again" bad "its text is '$(notification_text)'"
    fi

    # 3. the permission revoked: the service still runs, its notification is not posted, and nothing asks again
    adbd shell pm revoke "$PKG" android.permission.POST_NOTIFICATIONS
    sleep 3
    # The app asks once: a debug build lets the script write the note that the ask was made, which is what a person who answered
    # the dialog once leaves behind. (The dialog itself is driven at the end.)
    as_app "sh -c 'mkdir -p shared_prefs && echo \"<map><boolean name=\\\"notifications_asked\\\" value=\\\"true\\\" /></map>\" > shared_prefs/holzwave_service.xml'"
    adbd shell am start -n "$PKG/$ACTIVITY" >/dev/null 2>&1
    wait_until 60 has_focus || die "the app has no focus after the revoked start"
    pid=$(adbd shell pidof "$PKG" | tr -d '\r')
    wait_until 60 service_up && check "with the permission revoked the service is still foreground" ok \
        || check "with the permission revoked the service is still foreground" bad
    sleep 3
    notification_gone && check "with the permission revoked the notification is not posted" ok \
        || check "with the permission revoked the notification is not posted" bad "$(notification_text)"
    has_focus && check "with the permission revoked no dialog asks again" ok \
        || check "with the permission revoked no dialog asks again" bad "$(adbd shell dumpsys window | grep mCurrentFocus)"
    # The dialog: a fresh install state (no grant, nothing asked) asks once at the first start of a session, and Allow lets the
    # notification show. The node is found by the resource id of the system's button, so the locale does not matter.
    adbd shell pm clear "$PKG" >/dev/null 2>&1
    adbd shell am start -n "$PKG/$ACTIVITY" >/dev/null 2>&1
    dialog_up() { adbd shell dumpsys window | grep mCurrentFocus | grep -q GrantPermissionsActivity; }
    if wait_until 90 dialog_up; then
        check "the first start of a session asks for the notification permission" ok
        if tap_node 'permission_allow_button'; then
            wait_until 60 service_up && wait_until 20 text_is '1 shell' \
                && check "after Allow the service runs and its notification shows" ok \
                || check "after Allow the service runs and its notification shows" bad "its text is '$(notification_text)'"
        else
            skipped "tapping Allow in the system's dialog" "the dialog could not be read with uiautomator on this image"
        fi
    else
        check "the first start of a session asks for the notification permission" bad "no permission dialog within 90 s"
    fi
    adbd shell pm clear "$PKG" >/dev/null 2>&1
fi

end_time=$(date +%s)
echo "e2e $SECTION: $((end_time - start_time)) s"
if [ "$fails" -ne 0 ]; then
    echo "e2e $SECTION: $fails check(s) failed" >&2
    exit 1
fi
if [ "${notrun:-0}" -ne 0 ]; then
    echo "e2e $SECTION: ok ($notrun check(s) not run, named above)"
else
    echo "e2e $SECTION: ok"
fi
