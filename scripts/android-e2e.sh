#!/bin/sh
# The end to end proof on an arm64 emulator: the page, the bridge, the core, a pseudo terminal and the system shell.
#
#   sh scripts/android-e2e.sh tracer
#   sh scripts/android-e2e.sh service
#   sh scripts/android-e2e.sh vault
#   sh scripts/android-e2e.sh tls
#   sh scripts/android-e2e.sh handover
#   sh scripts/android-e2e.sh clipboard
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
# What `vault` does (it starts like the others: emulator, debug APK, fresh install; it does not wait for a shell) is run the steps
# of the debug-only self-test of the secret store, each by a launch of the debug app with the step named in an intent extra, one
# line per check. The vault is the real one over the real Android Keystore. The script sets a screen lock itself with
# `locksettings set-pin` and removes it again at every way out, and never touches a lock it did not set (a device that already has
# one makes the first step say so and the section stops):
#   a. no screen lock: a plain vault is made by the first save, and after a restart of the process it opens by itself, no prompt;
#   b. a PIN is set, and the vault is switched to protected mode: the system prompt shows (found by the focused window
#      `BiometricPrompt`) and the PIN is typed with `input text` and Enter; the file is ciphertext (magic, no account name);
#   c. after a restart the vault is locked and answers the locked fault; the prompt, the PIN, the secret reads back;
#   d. the page hidden and the vault locked, and the real device clock advancing;
#   e. an AI key saved while unlocked is there after a restart and an unlock;
#   e2. the app switcher picture (see below);
#   f. the PIN is removed, which makes the system delete the key: the vault is lost with key_gone and a reset gives a new one.
# The canary secret that the first step makes is searched for in every file of the app's data folder (and the app's folder on the
# shared storage), the whole log, the process environment and the argument lists of all processes: it must be in none of them.
# The app switcher picture: with the vault protected the script presses Home and, where `adb root` works (this image allows it),
# reads the picture the system keeps for the app's task (a placeholder of about a kilobyte when the system was told to hide it, a
# real image of tens of kilobytes otherwise) and saves a screenshot of the app switcher under target/android-e2e/; the same with
# protected mode off as the control. The mode the host reports must be `system` on Android 13 and later and `unsupported` below.
# What it does NOT prove: StrongBox, a fingerprint or face (the prompt is driven with the PIN), BIOMETRIC_STRONG on any phone,
# a key invalidated by a new fingerprint, vendor Keystore behaviour, the app switcher card of a real phone, any real phone.
#
# What `tls` does (it starts like `vault`: emulator, debug APK, fresh install) is run the steps of the debug-only self-test that
# use the two real network clients of the core, the WebDAV client of the sync and the client of the AI chat, which both take their
# trust decisions from the Android system trust store through the platform certificate verifier. The script starts a throwaway TLS
# server on the Mac with `openssl s_server`: a self-signed certificate made for this run in a private temporary folder, the
# server bound to the loopback address of the Mac and reached from the emulator at its own 127.0.0.1:8443 through `adb reverse`
# (removed at every way out, like the folder). The emulator's name for the Mac, 10.0.2.2, is not used for this: it is a
# local-network address, which an app that targets Android 17 may not reach without a runtime permission this app does not ask
# for. One line per check:
#   a. `tls-untrusted`: both clients request the server and both are refused with an error that names the certificate; the
#      server saw the connection attempts (its log), so the refusal is a verdict on the certificate and not a missing route;
#   b. `tls-public`: both clients request https://example.com/ and both get an HTTP-level answer, so the verifier accepts what the
#      system trusts (the check is skipped, and said to be, when the Mac cannot connect to that site: a skip is never reported as ok);
#   c. a PIN is set (the script clears it again at every way out) and the first unlock makes a protected vault; the sync is saved
#      through the real path, which is refused as a certificate error and keeps nothing, and the account a successful save would
#      have left is put in place; the app is stopped;
#   d. after the restart the vault is locked: the sync state is `paused`, with no error text, and the server's log has no new
#      connection since the restart;
#   e. the unlock (the system prompt, the PIN) starts one run, which reaches the server's TLS handshake, where the certificate is
#      refused: the state is `error` with a text that names the certificate (not a missing secret, and never `idle`), and the
#      server's log shows the connection attempt;
#   f. the canary secret, used as the sync's password and passphrase, is searched for in the app's files, the log, the process
#      environment and the argument lists of all processes: in none of them.
# What it does NOT prove: any real phone or other Android version than the image run; other trust stores, user-installed
# certificate authorities, proxies and TLS inspection, captive portals, certificate pinning, revocation and mobile networks; a real
# WebDAV server or a real AI provider.
#
# What `handover` does (it starts like `vault`: emulator, debug APK, fresh install; no shell is waited for) is hand single files over
# through the real document dialog of the image, which the script drives with uiautomator: it dumps the screen, finds a node by its
# exact text, content description or resource id (the parser is proved without an emulator by scripts/test-android-scripts.sh), and
# taps its centre. A step of the debug-only self-test opens the dialog; the script waits for the focused window of the documents
# app, taps, and reads the line the step logs. The files are pushed to the shared Download folder first and removed again at every way
# out, on the emulator and on the Mac. One line per check:
#   a. an open: a pushed file of 1 MiB (random bytes made for this run) is chosen in the dialog and the host reports it copied, with
#      the name and the size;
#   b. the copy in the shell's `home/handover` folder has the SHA-256 of the source;
#   c. the copy has mode -rw------- and no staging file is left in the folder;
#   d. a second copy of the same file is numbered (`-2`), has the same checksum, and the first copy is unchanged;
#   e. the dialog dismissed with Back is `cancelled` and the folder is exactly as before;
#   f. a file of 300 MiB (made on the emulator with truncate) is refused as `too_large` with its name, and nothing new is in the folder;
#   g. a file whose name has a space and a letter outside ASCII is copied in with the cleaned name the host chose (printed as seen);
#   h. a copy out: a fixed 4 KiB pattern is written to the shell's folder, the create dialog is answered with its Save button, and the
#      file the script pulls from the Download folder has the pattern's SHA-256 and the size.
# A check that cannot be driven (the file is not in view after three tries, a button is not found) ends `not run` with the reason and
# is never printed as ok. What it does NOT prove: another provider than the system's own (a cloud drive, a USB stick, an SD card,
# another app) which may report no or a wrong size, a revoked document, huge files on a phone, the dialogs of other Android
# versions and skins, and any real phone.
#
# What `clipboard` does (same start as `handover`) is run the debug-only step that writes the fixed test text through the app's
# clipboard path, reads it back, tries a text over the 512 KiB bound (which must be refused while the first text stays), and reports
# whether the clip carries the sensitive mark, which the system knows from Android 13 on: the script requires it from API 33 and
# requires its absence below. The clipboard of the system is then listed with `dumpsys clipboard` for information only (what the
# image shows there is printed, and is not a pass condition). What it does NOT prove: the clipboard strips of third party
# keyboards, the clipboard managers of phone makers, multi-window and foldable focus rules, and any real phone.
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
    tracer | service | vault | tls | handover | clipboard) ;;
    *)
        echo "usage: sh scripts/android-e2e.sh tracer|service|vault|tls|handover|clipboard" >&2
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
    # The screen lock that this script set is taken off again while the emulator is still there, and never one it did not set.
    if [ -n "$pin_set" ]; then
        adb -s "$SERIAL" shell locksettings clear --old "$PIN" >/dev/null 2>&1
    fi
    [ -n "$rooted" ] && adb -s "$SERIAL" unroot >/dev/null 2>&1
    # The canary the self-test wrote into the debug app's private folder is removed with the rest of what the run left.
    { [ "$SECTION" = vault ] || [ "$SECTION" = tls ]; } && adb -s "$SERIAL" shell run-as "$PKG" rm -f selftest-canary >/dev/null 2>&1
    # The files the handover section pushed to or made in the shared Download folder of the emulator (they all start with `hw-`), and
    # what it copied into the app's own folders. The Mac side is in $tmp, which is removed below.
    if [ "$SECTION" = handover ]; then
        adbd shell "rm -f /sdcard/Download/hw-* '/sdcard/Download/hw späce.bin' '/sdcard/Download/hw-out-test (1).bin'" >/dev/null 2>&1
        adbd shell run-as "$PKG" sh -c "'rm -f home/handover/hw-* home/handover/hw\ * home/hw-out-test.bin'" >/dev/null 2>&1
    fi
    # The throwaway TLS server of the tls section; its certificate and key are in $tmp, which is removed below.
    [ -n "$reversed" ] && adb -s "$SERIAL" reverse --remove tcp:8443 >/dev/null 2>&1
    if [ -n "$server_pid" ]; then
        kill "$server_pid" >/dev/null 2>&1
        wait "$server_pid" 2>/dev/null
    fi
    # The three files and the app's own process are cleaned while the emulator is still there.
    if [ -n "$HOMEREL" ]; then
        adb -s "$SERIAL" shell run-as "$PKG" rm -f "$HOMEREL/e2e-pwd" "$HOMEREL/e2e-echo" "$HOMEREL/e2e-sh" "$HOMEREL/e2e-bg" >/dev/null 2>&1
    fi
    [ -n "$started_emulator" ] && sh scripts/android-emulator.sh stop >/dev/null 2>&1
    rm -rf "$tmp"
}
HOMEREL=
pin_set=
rooted=
server_pid=
reversed=
PIN=1234
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

# The centre of the first node of a `uiautomator dump` (the file $1) whose text or content description is exactly $2, or, when $2
# starts with `id:`, whose resource id is the rest. Prints `x y` and returns 0; prints nothing and returns 1 when there is no node
# with bounds that match (the caller then says `not run`, never ok). The dump writes one `<node .../>` per element with its
# attributes in double quotes, the five predefined entities and `&#10;` for a line break in a value, and bounds as `[x1,y1][x2,y2]`.
# Entities that stand for a character outside ASCII are not decoded (the dump writes such text as it is, in UTF-8). The first match
# in the file wins. This is the whole parser the dialog driver relies on; it is tested without an emulator.
ui_center() {
    [ -r "$1" ] && [ -n "$2" ] && [ "$2" != "id:" ] || return 1
    awk -v want="$2" '
        function unesc(s) {
            gsub(/&lt;/, "<", s); gsub(/&gt;/, ">", s); gsub(/&quot;/, "\"", s); gsub(/&apos;/, "\047", s)
            gsub(/&#10;/, "\n", s); gsub(/&#13;/, "\r", s); gsub(/&#9;/, "\t", s)
            gsub(/&amp;/, "\\&", s)
            return s
        }
        function attr(node, name,    key, at) {
            key = " " name "=\""
            at = index(node, key)
            if (at == 0) return "\001"
            node = substr(node, at + length(key))
            return unesc(substr(node, 1, index(node, "\"") - 1))
        }
        BEGIN { RS = "<"; byid = (substr(want, 1, 3) == "id:"); if (byid) want = substr(want, 4) }
        {
            b = attr($0, "bounds")
            if (b == "\001") next
            if (byid) hit = (attr($0, "resource-id") == want)
            else hit = (attr($0, "text") == want || attr($0, "content-desc") == want)
            if (!hit) next
            if (!match(b, /^\[[0-9]+,[0-9]+\]\[[0-9]+,[0-9]+\]$/)) next
            gsub(/\[|\]/, " ", b); gsub(/,/, " ", b)
            split(b, p, " ")
            printf "%d %d\n", int((p[1] + p[3]) / 2), int((p[2] + p[4]) / 2)
            found = 1
            exit
        }
        END { exit found ? 0 : 1 }
    ' "$1"
}

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
# (Android 12 and 12L have no such permission: nothing to grant.)
if [ "$(adbd shell getprop ro.build.version.sdk | tr -d '\r')" -ge 33 ]; then
    adbd shell pm grant "$PKG" android.permission.POST_NOTIFICATIONS || die "the notification permission could not be granted"
fi

if [ "$SECTION" != vault ] && [ "$SECTION" != tls ] && [ "$SECTION" != handover ] && [ "$SECTION" != clipboard ]; then
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
fi

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


if [ "$SECTION" = vault ] || [ "$SECTION" = tls ] || [ "$SECTION" = handover ] || [ "$SECTION" = clipboard ]; then
    notrun=0
    skipped() { # <description> <reason>: a check that was not done is said so, and never counts as ok
        echo "e2e $SECTION: not run $1 ($2)"
        notrun=$((notrun + 1))
    }
    sdk=$(adbd shell getprop ro.build.version.sdk | tr -d '\r')
    outdir=target/android-e2e
    mkdir -p "$outdir"
    entered=
    seen_prompt=

    prompt_up() { adbd shell dumpsys window | grep mCurrentFocus | grep -q BiometricPrompt; }
    # The prompt of the system for the screen lock: the PIN goes in with the keys of the real input path, then Enter. (`input
    # text` reached the prompt on the image of the proof; the keycode route is the fallback if it ever does not.)
    enter_pin() {
        sleep 1
        adbd shell input text "$PIN"
        sleep 1
        adbd shell input keyevent 66
    }
    stop_app() {
        adbd shell am force-stop "$PKG"
        waited=0
        while [ "$waited" -lt 10 ] && [ -n "$(adbd shell pidof "$PKG" | tr -d '\r')" ]; do
            sleep 1
            waited=$((waited + 1))
        done
    }
    step_line() { adbd logcat -d 2>/dev/null | grep "holzwave-selftest $1:" | tail -n 1 | sed 's/.*RustStdoutStderr: *//'; }
    # run_step <step> <prompt|none> [seconds]: launch the debug app with the step in the intent, answer the system prompt when the
    # step is to show one (and say so when it does not show), and wait for the line the step logs. Sets $line.
    run_step() {
        step=$1
        want=$2
        limit=${3:-150}
        entered=
        seen_prompt=
        adbd logcat -c >/dev/null 2>&1
        adbd shell am start -W -n "$PKG/$ACTIVITY" --es holzwave_selftest "$step" >/dev/null 2>&1 || die "the activity did not start for $step"
        waited=0
        line=
        while [ "$waited" -lt "$limit" ]; do
            line=$(step_line "$step")
            [ -n "$line" ] && break
            if prompt_up; then
                seen_prompt=1
                if [ "$want" = prompt ] && [ -z "$entered" ]; then
                    enter_pin
                    entered=1
                fi
            fi
            sleep 1
            waited=$((waited + 1))
        done
        # The step may have logged while the prompt closed: look once more at the prompt's trace for the "none" case.
        if [ -z "$line" ]; then
            line="holzwave-selftest $step: FAIL no line in the log after $limit s"
        fi
    }
    expect_ok() { # <step> <prompt|none> <description> [seconds]
        run_step "$1" "$2" "${4:-150}"
        case "$line" in
            *"$1: ok"*) ;;
            *)
                check "$3" bad "$line"
                return 1
                ;;
        esac
        if [ "$2" = prompt ] && [ -z "$entered" ]; then
            check "$3" bad "the step ended without the system prompt: $line"
            return 1
        fi
        if [ "$2" = none ] && [ -n "$seen_prompt" ]; then
            check "$3" bad "a system prompt showed where none was wanted: $line"
            return 1
        fi
        check "$3 [${line#*"$1: ok "}]" ok
        return 0
    }
    stop_here() { # a step that cannot continue the section ends it
        die "$1"
    }

    if [ "$SECTION" = vault ]; then
    echo "e2e vault: image API $sdk; the prompt is driven with the PIN $PIN (set by this script), the keys of the real input path"

    # a. no screen lock: plain
    expect_ok vault-info none "the Keystore answers: the state of the new app" || stop_here "the self-test did not start"
    echo "e2e vault: device facts: ${line#*vault-info: ok }"
    expect_ok vault-plain-write none "no screen lock: the first save makes a plain vault and reads back" \
        || stop_here "step a failed (a device that already has a screen lock is never changed by this script)"
    stop_app
    expect_ok vault-plain-read none "after a restart the plain vault opens by itself, with no prompt" || stop_here "step a2 failed"

    # b. a PIN, protected mode
    adbd shell locksettings set-pin "$PIN" >/dev/null || stop_here "locksettings set-pin did not work on this image"
    pin_set=1
    sleep 5
    expect_ok vault-protect prompt "a screen lock exists: switching to protected mode shows the system prompt, the PIN opens it" \
        || stop_here "step b failed"
    HOMEREL=$(as_app 'find . -maxdepth 3 -type d -name home' | head -n 1 | sed 's|^\./||')
    [ -n "$HOMEREL" ] && rel_parent=$(dirname "$HOMEREL") || rel_parent=.
    magic=$(as_app "head -c 3 $rel_parent/vault.bin")
    [ "$magic" = HWV ] && check "the vault file exists and starts with the vault magic" ok || check "the vault file exists and starts with the vault magic" bad "got '$magic'"
    hits=$(as_app "grep -c ssh:selftest $rel_parent/vault.bin" | head -n 1)
    [ "${hits:-1}" = 0 ] && check "the vault file does not hold the account name in plaintext" ok || check "the vault file does not hold the account name in plaintext" bad "grep counted '$hits'"

    # c. restart: locked
    stop_app
    expect_ok vault-locked-read prompt "after a restart the vault is locked and answers the locked fault; the prompt and the PIN unlock it and the secret reads back" \
        || stop_here "step c failed"

    # the canary scan
    canary=$(as_app "cat $rel_parent/selftest-canary" | head -n 1)
    case "$canary" in
        HW-CANARY-????????????????????????????????) ;;
        *) stop_here "the canary file was not written ('$canary')" ;;
    esac
    app_pid=$(adbd shell pidof "$PKG" | tr -d '\r')
    echo "e2e vault: canary scan of: the app's data folder (every file but the canary's own), its folder on the shared storage, the whole log, /proc/$app_pid/environ, the argument lists of every process"
    # The log is searched first: adbd logs the command line of every `adb shell` it runs, so a scan whose command line holds the
    # canary would put the canary into the log itself, and a search of the log after it would find the script's own trace.
    in_log=$(adbd logcat -d 2>/dev/null | grep -c -F "$canary")
    [ "$in_log" = 0 ] && check "the canary is not in the log" ok \
        || check "the canary is not in the log" bad "$in_log lines: $(adbd logcat -d 2>/dev/null | grep -F "$canary" | head -n 2 | sed "s/$canary/<canary>/g" | cut -c1-240)"
    in_files=$(as_app "grep -rlaF $canary . --exclude=selftest-canary 2>/dev/null" | head -n 3)
    [ -z "$in_files" ] && check "the canary is in no file of the app's data folder" ok || check "the canary is in no file of the app's data folder" bad "found in: $in_files"
    control=$(as_app "grep -rlaF $canary . 2>/dev/null" | head -n 3)
    [ "$control" = "./selftest-canary" ] || [ "$control" = "selftest-canary" ] \
        && check "the scan can find the canary: it finds exactly the canary's own file when that is not excluded" ok \
        || check "the scan can find the canary: it finds exactly the canary's own file when that is not excluded" bad "found: $control"
    shared=$(adbd shell "grep -rlaF $canary /sdcard/Android/data/$PKG 2>/dev/null" | tr -d '\r' | head -n 3)
    [ -z "$shared" ] && check "the canary is in no file of the app's folder on the shared storage" ok || check "the canary is in no file of the app's folder on the shared storage" bad "found in: $shared"
    in_env=$(as_app "cat /proc/$app_pid/environ" | grep -a -c -F "$canary")
    [ "$in_env" = 0 ] && check "the canary is not in the environment of the app's process" ok || check "the canary is not in the environment of the app's process" bad "$in_env matches"
    in_args=$(adbd shell ps -A -o PID,ARGS 2>/dev/null | tr -d '\r' | grep -c -F "$canary")
    [ "$in_args" = 0 ] && check "the canary is in no process's argument list" ok || check "the canary is in no process's argument list" bad "$in_args matches"

    # d. hidden and locked, the real clock
    expect_ok vault-background none "hidden, then locked: the next read is the locked fault, and the device clock advances" || true

    # e. the AI key
    expect_ok vault-ai-key prompt "an AI key saved while unlocked is present (no provider is called)" || true
    stop_app
    expect_ok vault-ai-key-after-restart prompt "after a restart the key is held back while locked and present after the unlock" || true

    # e2. the app switcher picture
    snapshot_task() { adbd shell dumpsys activity recents | tr -d '\r' | sed -n "s/.*Recent #[0-9]*: Task{[^ ]* #\\([0-9][0-9]*\\) .*A=[0-9]*:$PKG.*/\\1/p" | head -n 1; }
    snapshot_size() { # <task id>: the size of the picture the system keeps for the task, 0 when there is none
        sz=$(adbd shell "f=\$(find /data/system_ce/0/snapshots -name '$1.jpg' 2>/dev/null | head -n 1); [ -n \"\$f\" ] && stat -c %s \"\$f\" || echo 0" | tr -d '\r')
        echo "${sz:-0}"
    }
    back_to_app() {
        adbd shell am start -n "$PKG/$ACTIVITY" >/dev/null 2>&1
        wait_focus=0
        while [ "$wait_focus" -lt 20 ]; do
            adbd shell dumpsys window | grep mCurrentFocus | grep -q "$PKG/$ACTIVITY" && return 0
            sleep 1
            wait_focus=$((wait_focus + 1))
        done
        return 1
    }
    switcher_shot() { # <name>: Home, then the app switcher, saved as a picture
        adbd shell input keyevent KEYCODE_HOME
        sleep 3
        snap_after_home=$(snapshot_size "$task")
        adbd shell input keyevent KEYCODE_APP_SWITCH
        sleep 2
        adbd exec-out screencap -p >"$outdir/recents-$1.png" 2>/dev/null
        echo "e2e vault: app switcher screenshot $outdir/recents-$1.png ($(wc -c <"$outdir/recents-$1.png" | tr -d ' ') bytes)"
    }
    mode_matches() { # <detail> <protected|plain>
        case "$1" in
            *"mode=system"*) [ "$sdk" -ge 33 ] ;;
            *"mode=unsupported"*) [ "$sdk" -lt 33 ] ;;
            *) false ;;
        esac
    }
    expect_ok vault-recents-protected none "the protected vault: the host told the plugin to hide the app switcher picture" || true
    if mode_matches "$line"; then
        check "the mode the host reports matches API $sdk (system from 33, unsupported below)" ok
    else
        check "the mode the host reports matches API $sdk (system from 33, unsupported below)" bad "$line"
    fi
    case "$line" in
        *"applied=true mode=system"*) [ "$sdk" -ge 33 ] && check "protected: the plugin reports applied=true" ok ;;
        *"applied=false mode=unsupported"*) [ "$sdk" -lt 33 ] && check "Android 12 and 12L: applied=false, mode=unsupported (no protection exists)" ok ;;
        *) check "protected: the plugin's applied value" bad "$line" ;;
    esac
    task=$(snapshot_task)
    if adbd root >/dev/null 2>&1; then
        sleep 3
        adbd wait-for-device
        rooted=1
    fi
    can_see=
    if [ -n "$rooted" ] && [ -n "$task" ] && [ "$(adbd shell id -u | tr -d '\r')" = 0 ]; then can_see=1; fi
    switcher_shot protected
    protected_size=$snap_after_home
    back_to_app || skipped "the app switcher control run" "the app did not come back to the front"
    expect_ok vault-recents-plain none "protection off: the host told the plugin to show the app switcher picture" || true
    switcher_shot plain
    plain_size=$snap_after_home
    if [ -z "$can_see" ]; then
        skipped "the system's snapshot of the app's task" "no root on this image or no task found (the screenshots are saved; the executor looks at them)"
    elif [ "$sdk" -ge 33 ]; then
        echo "e2e vault: the system's picture of the app's task: ${protected_size} bytes protected, ${plain_size} bytes with protection off"
        if [ "$plain_size" -le 20000 ]; then
            skipped "the system's snapshot comparison" "the control picture is only $plain_size bytes: the check cannot conclude"
        elif [ "$protected_size" -lt 8192 ]; then
            check "protected: the system kept no picture of the app (${protected_size} bytes), while it kept one with protection off (${plain_size} bytes)" ok
        else
            check "protected: the system kept no picture of the app" bad "$protected_size bytes, control $plain_size bytes"
        fi
    else
        echo "e2e vault: observation on API $sdk (no protection exists): the system's picture is ${protected_size} bytes protected and ${plain_size} bytes with protection off"
    fi
    back_to_app || true
    expect_ok vault-protect prompt "protected mode is switched on again (prompt and PIN)" || true

    # f. the PIN is removed: the system deletes the key
    stop_app
    adbd shell locksettings clear --old "$PIN" >/dev/null || stop_here "locksettings clear did not work"
    pin_set=
    sleep 5
    expect_ok vault-lost none "the screen lock removed: the vault is lost with key_gone, and a reset gives a new one" || true
    stop_app
    elif [ "$SECTION" = tls ]; then
    # ---- the tls section ------------------------------------------------------------------------------------------------
    echo "e2e tls: image API $sdk; the PIN $PIN is set by this script (and cleared again) for the vault steps"
    command -v openssl >/dev/null 2>&1 || die "openssl is missing: the throwaway TLS server needs it"
    certdir="$tmp/tls"
    mkdir -m 700 "$certdir" || die "no private folder for the certificate"
    openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -nodes -days 1 -subj /CN=127.0.0.1 \
        -addext subjectAltName=IP:127.0.0.1 -keyout "$certdir/key.pem" -out "$certdir/cert.pem" >/dev/null 2>&1 \
        || die "openssl could not make the self-signed certificate (an OpenSSL with -pkeyopt and -addext is needed)"
    # Bound to the loopback address of the Mac: nothing else on the network can reach it. The emulator reaches it at its own
    # loopback address, through the forward of adb reverse.
    openssl s_server -accept 127.0.0.1:8443 -cert "$certdir/cert.pem" -key "$certdir/key.pem" -www >"$tmp/server.log" 2>&1 &
    server_pid=$!
    adbd reverse tcp:8443 tcp:8443 >/dev/null || die "adb reverse did not work"
    reversed=1
    waited=0
    while [ "$waited" -lt 10 ] && ! grep -q ACCEPT "$tmp/server.log" 2>/dev/null; do
        sleep 1
        waited=$((waited + 1))
    done
    grep -q ACCEPT "$tmp/server.log" 2>/dev/null || die "the throwaway TLS server did not start on 127.0.0.1:8443 (is the port in use?): $(head -n 3 "$tmp/server.log" | tr '\n' ' ')"
    echo "e2e tls: a TLS server with a self-signed certificate (EC P-256, valid one day) listens on 127.0.0.1:8443 of the Mac, seen from the emulator as its own 127.0.0.1:8443 (adb reverse)"
    # The attempts the server has seen: its start and every connection (a refused certificate shows as an alert line).
    attempts() {
        n=$(grep -c -E 'ACCEPT|error:' "$tmp/server.log" 2>/dev/null)
        echo "${n:-0}"
    }

    expect_ok vault-info none "the self-test answers: the state of the new app" || stop_here "the self-test did not start"
    case "$line" in
        *"screen_lock=true"*) stop_here "the device already has a screen lock; this script never changes a lock it did not set" ;;
    esac

    # a. the verdict on a certificate the system does not trust
    before=$(attempts)
    expect_ok tls-untrusted none "both clients refuse a self-signed certificate, as an error that names the certificate" || true
    sleep 1
    after=$(attempts)
    if [ "$after" -gt "$before" ]; then
        check "the server saw the connection attempts of the clients ($((after - before)) lines in its log)" ok
    else
        check "the server saw the connection attempts of the clients" bad "its log did not grow"
    fi

    # b. the verdict on a certificate the system trusts
    # (A TCP connect decides; no downloader tool is used in these scripts, the stand-in test refuses one.)
    if nc -z -w 5 example.com 443 >/dev/null 2>&1; then
        expect_ok tls-public none "both clients get an HTTP answer from a public site: the system's trust store accepts it" || true
    else
        skipped "tls-public" "the Mac cannot open a connection to example.com port 443, and the emulator goes through the Mac's network"
    fi

    # c. a PIN, the protected vault, the sync saved
    adbd shell locksettings set-pin "$PIN" >/dev/null || stop_here "locksettings set-pin did not work on this image"
    pin_set=1
    sleep 5
    expect_ok vault-create prompt "a screen lock exists: the first unlock makes a protected vault (system prompt, the PIN)" || stop_here "the vault could not be made"
    expect_ok sync-save none "the save through the real path is refused as a certificate error and keeps nothing; the account is put in place" || stop_here "the sync could not be set up"
    HOMEREL=$(as_app 'find . -maxdepth 3 -type d -name home' | head -n 1 | sed 's|^\./||')
    [ -n "$HOMEREL" ] && rel_parent=$(dirname "$HOMEREL") || rel_parent=.
    # The log of the step that held the canary as a password is searched now, before the next step clears it.
    canary=$(as_app "cat $rel_parent/selftest-canary" | head -n 1)
    case "$canary" in
        HW-CANARY-????????????????????????????????) ;;
        *) stop_here "the canary file was not written ('$canary')" ;;
    esac
    in_log=$(adbd logcat -d 2>/dev/null | grep -c -F "$canary")
    [ "$in_log" = 0 ] && check "the canary is not in the log of the save step" ok \
        || check "the canary is not in the log of the save step" bad "$in_log lines"
    stop_app

    # d. restart: the vault is locked, the sync is paused, the server sees nothing
    before=$(attempts)
    expect_ok sync-paused none "after the restart the vault is locked and the sync is paused, with no error text" 60 || stop_here "the paused state was not reached"
    sleep 2
    after=$(attempts)
    if [ "$after" = "$before" ]; then
        check "the server saw no connection from the app while the sync was paused" ok
    else
        check "the server saw no connection from the app while the sync was paused" bad "its log grew from $before to $after lines"
    fi

    # e. the unlock starts one run that reaches the TLS handshake
    expect_ok sync-resume prompt "the unlock (system prompt, PIN) starts one sync run, which reaches the server and refuses its certificate" 120 || true
    waited=0
    while [ "$waited" -lt 15 ] && [ "$(attempts)" = "$after" ]; do
        sleep 1
        waited=$((waited + 1))
    done
    if [ "$(attempts)" -gt "$after" ]; then
        check "the server's log shows the connection attempt of the resumed run" ok
    else
        check "the server's log shows the connection attempt of the resumed run" bad "its log did not grow after the unlock"
    fi

    # f. the canary, used as the sync's password and passphrase, is nowhere
    app_pid=$(adbd shell pidof "$PKG" | tr -d '\r')
    echo "e2e tls: canary scan of: the app's data folder (every file but the canary's own), its folder on the shared storage, the whole log, /proc/$app_pid/environ, the argument lists of every process"
    in_log=$(adbd logcat -d 2>/dev/null | grep -c -F "$canary")
    [ "$in_log" = 0 ] && check "the canary is not in the log" ok \
        || check "the canary is not in the log" bad "$in_log lines: $(adbd logcat -d 2>/dev/null | grep -F "$canary" | head -n 2 | sed "s/$canary/<canary>/g" | cut -c1-240)"
    in_files=$(as_app "grep -rlaF $canary . --exclude=selftest-canary 2>/dev/null" | head -n 3)
    [ -z "$in_files" ] && check "the canary is in no file of the app's data folder" ok || check "the canary is in no file of the app's data folder" bad "found in: $in_files"
    control=$(as_app "grep -rlaF $canary . 2>/dev/null" | head -n 3)
    { [ "$control" = "./selftest-canary" ] || [ "$control" = "selftest-canary" ]; } \
        && check "the scan can find the canary: it finds exactly the canary's own file when that is not excluded" ok \
        || check "the scan can find the canary: it finds exactly the canary's own file when that is not excluded" bad "found: $control"
    shared=$(adbd shell "grep -rlaF $canary /sdcard/Android/data/$PKG 2>/dev/null" | tr -d '\r' | head -n 3)
    [ -z "$shared" ] && check "the canary is in no file of the app's folder on the shared storage" ok || check "the canary is in no file of the app's folder on the shared storage" bad "found in: $shared"
    in_env=$(as_app "cat /proc/$app_pid/environ" | grep -a -c -F "$canary")
    [ "$in_env" = 0 ] && check "the canary is not in the environment of the app's process" ok || check "the canary is not in the environment of the app's process" bad "$in_env matches"
    in_args=$(adbd shell ps -A -o PID,ARGS 2>/dev/null | tr -d '\r' | grep -c -F "$canary")
    [ "$in_args" = 0 ] && check "the canary is in no process's argument list" ok || check "the canary is in no process's argument list" bad "$in_args matches"

    # the PIN is cleared while the emulator is still there, and the app stopped
    stop_app
    adbd shell locksettings clear --old "$PIN" >/dev/null && pin_set=
    elif [ "$SECTION" = handover ]; then
    # ---- the handover section -------------------------------------------------------------------------------------------------
    echo "e2e handover: image API $sdk; the system's own document dialog (the documents app of the image) is driven with uiautomator"
    dl=/sdcard/Download
    dialog_up() { adbd shell dumpsys window | grep mCurrentFocus | grep -qi documentsui; }
    handover_listing() { as_app "ls -l home/handover 2>/dev/null" | awk 'NR > 1 { print $1, $5, $NF }' | sort; }
    sha_device() { adbd exec-out run-as "$PKG" cat "$1" 2>/dev/null | shasum -a 256 | cut -d ' ' -f 1; }
    sha_mac() { shasum -a 256 "$1" | cut -d ' ' -f 1; }

    # UI driving: a dump that cannot be read is tried again, and a dump that still cannot be read is a node that is not found.
    ui_dump() { # <file>
        tries=0
        while [ "$tries" -lt 3 ]; do
            adbd shell uiautomator dump /sdcard/hw-ui.xml >/dev/null 2>&1
            adbd exec-out cat /sdcard/hw-ui.xml >"$1" 2>/dev/null
            adbd shell rm -f /sdcard/hw-ui.xml >/dev/null 2>&1
            grep -q '<hierarchy' "$1" 2>/dev/null && return 0
            tries=$((tries + 1))
            sleep 1
        done
        : >"$1"
        return 1
    }
    ui_tap_text() { # <text, or id:resource-id>: tap the centre of the first node that matches; 1 when there is none
        ui_dump "$tmp/hw-ui.xml" || return 1
        xy=$(ui_center "$tmp/hw-ui.xml" "$1") || return 1
        set -- $xy
        adbd shell input tap "$1" "$2"
    }
    # The open dialog: the file is in the list of recent files, or it is found by opening the roots drawer and the root `Downloads`,
    # at most three tries.
    want_name=
    drive_open() {
        tries=0
        while [ "$tries" -lt 3 ]; do
            ui_tap_text "$want_name" && return 0
            ui_tap_text "Show roots" && {
                sleep 1
                ui_tap_text "Downloads"
            }
            sleep 2
            tries=$((tries + 1))
        done
        return 1
    }
    # The create dialog: the button Save of the dialog (the system draws its text in capitals, so the resource id is the way in).
    drive_save() {
        ui_tap_text "id:android:id/button1" && return 0
        ui_tap_text "SAVE" && return 0
        ui_tap_text "Save"
    }
    drive_back() {
        adbd shell input keyevent KEYCODE_BACK
        return 0
    }
    # run_dialog_step <step> <driver> [seconds]: start the step, wait for the focused window of the documents app, call the driver
    # once, and wait for the line the step logs. A driver that cannot do its work (a node was not found) presses Back so that the step
    # ends, and the check is `not run`. Sets $line and $driven (ok, notrun, none when no dialog showed).
    run_dialog_step() {
        step=$1
        driver=$2
        limit=${3:-90}
        driven=none
        adbd logcat -c >/dev/null 2>&1
        adbd shell am start -W -n "$PKG/$ACTIVITY" --es holzwave_selftest "$step" >/dev/null 2>&1 || die "the activity did not start for $step"
        waited=0
        line=
        while [ "$waited" -lt "$limit" ]; do
            line=$(step_line "$step")
            [ -n "$line" ] && break
            if [ "$driven" = none ] && dialog_up; then
                sleep 2
                if "$driver"; then
                    driven=ok
                else
                    driven=notrun
                    adbd shell input keyevent KEYCODE_BACK
                fi
            fi
            sleep 1
            waited=$((waited + 1))
        done
        [ -n "$line" ] || line="holzwave-selftest $step: FAIL no line in the log after $limit s"
    }
    # The back of a focus: after a step the app is in front again before the next one starts.
    wait_app_front() {
        waited=0
        while [ "$waited" -lt 30 ]; do
            adbd shell dumpsys window | grep mCurrentFocus | grep -q "$PKG/$ACTIVITY" && return 0
            sleep 1
            waited=$((waited + 1))
        done
        return 1
    }

    command -v shasum >/dev/null 2>&1 || die "shasum is missing (the checksums of the copies are made with it)"
    head -c 1048576 /dev/urandom >"$tmp/hw-handover-test.bin" || die "no random bytes for the test file"
    src_sha=$(sha_mac "$tmp/hw-handover-test.bin")
    adbd push "$tmp/hw-handover-test.bin" "$dl/hw-handover-test.bin" >/dev/null || die "the test file could not be pushed to the emulator"
    echo "e2e handover: the test file hw-handover-test.bin (1 MiB of random bytes, SHA-256 ${src_sha}) is in $dl on the emulator"
    # The app is started once, so that its folders exist and the self-test route answers.
    expect_ok vault-info none "the self-test answers" || stop_here "the self-test did not start"

    # a. an open
    want_name=hw-handover-test.bin
    run_dialog_step handover-in drive_open
    got_first=
    case "$driven:$line" in
        ok:*"handover-in: ok done name=\"hw-handover-test.bin\" size=1048576 "*)
            check "an open: the chosen file is copied, the host reports its name and its size [${line#*"handover-in: ok "}]" ok
            got_first=1
            ;;
        notrun:* | none:*) skipped "an open through the dialog" "the dialog could not be driven (driven=$driven; $line)" ;;
        *) check "an open: the chosen file is copied" bad "$line" ;;
    esac
    wait_app_front || die "the app did not come back to the front after the dialog"

    # b. the checksum of the copy
    if [ -n "$got_first" ]; then
        copy_sha=$(sha_device home/handover/hw-handover-test.bin)
        [ "$copy_sha" = "$src_sha" ] && check "the copy in home/handover has the SHA-256 of the source" ok \
            || check "the copy in home/handover has the SHA-256 of the source" bad "source $src_sha, copy '$copy_sha'"
        # c. the mode, and no staging file
        listing=$(as_app "ls -la home/handover")
        mode=$(echo "$listing" | awk '$NF == "hw-handover-test.bin" { print $1 }')
        [ "$mode" = "-rw-------" ] && check "the copy has mode -rw------- (the app alone reads it)" ok \
            || check "the copy has mode -rw-------" bad "mode '$mode'"
        if echo "$listing" | awk 'NR > 1 { print $NF }' | grep -q '^\.incoming-'; then
            check "no staging file is left in the folder" bad "$(echo "$listing" | grep incoming)"
        else
            check "no staging file is left in the folder" ok
        fi
        before_second=$(handover_listing)

        # d. a second copy of the same file is numbered
        run_dialog_step handover-in drive_open
        case "$driven:$line" in
            ok:*"handover-in: ok done name=\"hw-handover-test.bin\" size=1048576 shown=\"~/handover/hw-handover-test-2.bin\" renamed=true"*)
                second_sha=$(sha_device home/handover/hw-handover-test-2.bin)
                first_sha=$(sha_device home/handover/hw-handover-test.bin)
                if [ "$second_sha" = "$src_sha" ] && [ "$first_sha" = "$src_sha" ]; then
                    check "a second copy of the same file is numbered hw-handover-test-2.bin, has the same checksum, and the first copy is unchanged" ok
                else
                    check "a second copy is numbered and both copies have the checksum of the source" bad "first '$first_sha', second '$second_sha'"
                fi
                ;;
            notrun:* | none:*) skipped "the second copy" "the dialog could not be driven (driven=$driven; $line)" ;;
            *) check "a second copy of the same file is numbered" bad "$line" ;;
        esac
        wait_app_front || die "the app did not come back to the front after the dialog"
        before_cancel=$(handover_listing)

        # e. a dismissed dialog
        run_dialog_step handover-in drive_back
        case "$driven:$line" in
            ok:*"handover-in: ok cancelled"*)
                [ "$(handover_listing)" = "$before_cancel" ] && check "a dismissed dialog is cancelled and the folder is exactly as before" ok \
                    || check "a dismissed dialog changes nothing" bad "the folder changed"
                ;;
            none:*) skipped "the dismissed dialog" "no dialog showed" ;;
            *) check "a dismissed dialog is cancelled" bad "$line" ;;
        esac
        wait_app_front || die "the app did not come back to the front after the dialog"

        # f. a file above the limit
        adbd shell truncate -s 300M "$dl/hw-big.bin" >/dev/null 2>&1
        before_big=$(handover_listing)
        want_name=hw-big.bin
        run_dialog_step handover-in drive_open 120
        case "$driven:$line" in
            ok:*"handover-in: ok too_large name=\"hw-big.bin\""*)
                [ "$(handover_listing)" = "$before_big" ] && check "a file of 300 MiB is refused as too_large with its name and nothing new is in the folder [${line#*"handover-in: ok "}]" ok \
                    || check "a file above the limit adds nothing" bad "the folder changed"
                ;;
            notrun:* | none:*) skipped "the file above the limit" "the dialog could not be driven (driven=$driven; $line)" ;;
            *) check "a file above the limit is refused as too_large" bad "$line" ;;
        esac
        wait_app_front || die "the app did not come back to the front after the dialog"
    else
        skipped "the checks b to f, which need the first copy" "the first open could not be driven"
    fi

    # g. a name with a space and a letter outside ASCII
    odd="hw sp\303\244ce.bin"
    odd_name=$(printf "$odd")
    head -c 4096 /dev/urandom >"$tmp/$odd_name" || die "no random bytes for the second test file"
    odd_sha=$(sha_mac "$tmp/$odd_name")
    adbd push "$tmp/$odd_name" "$dl/$odd_name" >/dev/null || die "the file with the odd name could not be pushed"
    want_name=$odd_name
    run_dialog_step handover-in drive_open
    case "$driven:$line" in
        ok:*"handover-in: ok done "*)
            seen_name=$(echo "$line" | sed -n 's/.*handover-in: ok done name="\([^"]*\)".*/\1/p')
            case "$seen_name" in
                */* | *\\* | "") check "a name with a space and a letter outside ASCII keeps no separator" bad "name '$seen_name'" ;;
                *)
                    final=$(as_app "ls home/handover" | grep -F -x -- "$seen_name" | head -n 1)
                    if [ "$final" = "$seen_name" ] && [ "$(sha_device "home/handover/$seen_name")" = "$odd_sha" ]; then
                        check "a name with a space and a letter outside ASCII is copied in as '$seen_name' (the cleaned name, no separator), with the checksum of the source" ok
                    else
                        check "the file with the odd name is in the folder under the name the host reported" bad "reported '$seen_name', found '$final'"
                    fi
                    ;;
            esac
            ;;
        notrun:* | none:*) skipped "the odd name" "the dialog could not be driven (driven=$driven; $line)" ;;
        *) check "a name with a space and a letter outside ASCII is copied in" bad "$line" ;;
    esac
    wait_app_front || die "the app did not come back to the front after the dialog"

    # h. a copy out
    adbd shell "rm -f $dl/hw-out-test.bin" >/dev/null 2>&1
    run_dialog_step handover-out drive_save
    case "$driven:$line" in
        ok:*"handover-out: ok done name=\"hw-out-test.bin\" size=4096 "*)
            if adbd pull "$dl/hw-out-test.bin" "$tmp/hw-out-pulled.bin" >/dev/null 2>&1; then
                pattern_sha=
                if command -v perl >/dev/null 2>&1; then
                    perl -e 'binmode STDOUT; print map { chr } ((0 .. 255) x 16)' >"$tmp/hw-out-pattern.bin" && pattern_sha=$(sha_mac "$tmp/hw-out-pattern.bin")
                fi
                made_sha=$(sha_device home/hw-out-test.bin)
                pulled_sha=$(sha_mac "$tmp/hw-out-pulled.bin")
                pulled_size=$(wc -c <"$tmp/hw-out-pulled.bin" | tr -d ' ')
                if [ -n "$pattern_sha" ] && [ "$pulled_sha" = "$pattern_sha" ] && [ "$pulled_size" = 4096 ]; then
                    check "a copy out: the file pulled from the Download folder has the SHA-256 and the size (4096 bytes) of the pattern the step wrote" ok
                elif [ -z "$pattern_sha" ] && [ "$pulled_sha" = "$made_sha" ] && [ "$pulled_size" = 4096 ]; then
                    check "a copy out: the file pulled from the Download folder equals the file in the shell's folder (perl is missing, the pattern itself was not recomputed)" ok
                else
                    check "a copy out: the pulled file equals the pattern" bad "pulled $pulled_sha ($pulled_size bytes), written $made_sha, pattern '$pattern_sha'"
                fi
            else
                check "a copy out: the saved file could be pulled from the Download folder" bad "adb pull failed"
            fi
            ;;
        notrun:* | none:*) skipped "a copy out through the create dialog" "the dialog could not be driven (driven=$driven; $line)" ;;
        *) check "a copy out through the create dialog" bad "$line" ;;
    esac
    wait_app_front || true
    stop_app
    else
    # ---- the clipboard section --------------------------------------------------------------------------------------------------
    echo "e2e clipboard: image API $sdk"
    expect_ok clipboard-roundtrip none "the clipboard is written and read back through the plugin as plain text, a text over 512 KiB is refused and the first text stays" \
        || stop_here "the clipboard step did not run"
    case "$line" in
        *"sensitive=true"*)
            if [ "$sdk" -ge 33 ]; then
                check "the clip carries the sensitive mark on API $sdk (Android 13 and later hide its preview)" ok
            else
                check "the clip carries the sensitive mark only from API 33" bad "API $sdk reported sensitive=true"
            fi
            ;;
        *"sensitive=false"*)
            if [ "$sdk" -lt 33 ]; then
                check "below API 33 the clip carries no sensitive mark (the system has none; the text is written all the same)" ok
            else
                check "the clip carries the sensitive mark on API $sdk" bad "sensitive=false"
            fi
            ;;
        *) check "the step reports whether the clip is marked sensitive" bad "$line" ;;
    esac
    dump=$(adbd shell dumpsys clipboard 2>/dev/null | tr -d '\r')
    if echo "$dump" | grep -q -F "holzwave clipboard test"; then
        echo "e2e clipboard: information: dumpsys clipboard lists the test text ($(echo "$dump" | wc -l | tr -d ' ') lines)"
    else
        echo "e2e clipboard: information: dumpsys clipboard does not list the test text ($(echo "$dump" | wc -l | tr -d ' ') lines; not a pass condition)"
    fi
    stop_app
    fi
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
