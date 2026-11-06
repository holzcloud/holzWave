#!/bin/sh
# The Android emulator of the project: one arm64 virtual device, headless.
#
#   sh scripts/android-emulator.sh create|start|stop|install|shot [argument]
#
#   create   make the virtual device `holzwave-arm64` from the system image that is already installed
#            (default system-images;android-37.0;default;arm64-v8a; HOLZWAVE_ANDROID_IMAGE names another one)
#   start    boot it without a window (-no-window -no-audio -no-snapshot -gpu swiftshader_indirect) and wait for the system to
#            finish booting; HOLZWAVE_EMULATOR_GPU=host switches to -gpu host, the documented alternative if the first fails
#   stop     end the emulator this script started (and any emulator on the pinned port)
#   install  adb install -r of the debug APK (the path of scripts/android-build.sh debug, or the argument)
#   shot     save a screenshot as a PNG to the path given as the argument
#
# arm64 only, on purpose. The script refuses an image id that does not end in `arm64-v8a`, because an x86_64 image cannot be
# run by the emulator on an Apple silicon Mac, and refuses a host that is not arm64, because an arm64 image needs the hardware
# of an arm64 host to be usable. It never installs an SDK package or a system image (a missing one is exit 2 and the
# command the owner can run, printed as text), and downloads nothing.
#
# Every subcommand cleans up what it started on every way out: a `start` that does not finish booting kills the emulator it
# launched and removes its pid file and its log (after printing the last lines of the log).
#
# Environment: HOLZWAVE_EMULATOR_PORT (default 5554) is the console port; the adb serial is emulator-<port> and is the only
# device these scripts talk to, so a real phone on the cable is never touched.
#
# Exit status: 0 success, 1 a step failed, 2 a missing tool or image or a refused request.

AVD=holzwave-arm64
IMAGE_DEFAULT='system-images;android-37.0;default;arm64-v8a'
PORT=${HOLZWAVE_EMULATOR_PORT:-5554}
BOOT_TIMEOUT=${HOLZWAVE_EMULATOR_BOOT_TIMEOUT:-300}

here=$(cd "$(dirname "$0")" && pwd -P)
cd "$here/.." || exit 2
. scripts/android-env.sh

SERIAL=emulator-$PORT
export ANDROID_SERIAL=$SERIAL
PIDFILE="${TMPDIR:-/tmp}/holzwave-android-emulator-$PORT.pid"
LOGFILE="${TMPDIR:-/tmp}/holzwave-android-emulator-$PORT.log"

usage() {
    echo "usage: sh scripts/android-emulator.sh create|start|stop|install|shot [argument]" >&2
    exit 2
}

need() { # <tool>: exit 2 when it is not on PATH
    if ! command -v "$1" >/dev/null 2>&1; then
        echo "android-emulator: $1 is missing: run . scripts/android-env.sh && sh scripts/android-env-check.sh" >&2
        exit 2
    fi
}

cmd=${1:-}
[ -n "$cmd" ] || usage

case "$cmd" in
    create)
        image=${HOLZWAVE_ANDROID_IMAGE:-$IMAGE_DEFAULT}
        case "$image" in
            *\;arm64-v8a) ;;
            *)
                echo "android-emulator: refused: the image '$image' is not arm64-v8a. Only arm64 images are used: the emulator cannot run an x86_64 image on an Apple silicon Mac, and the app is built for arm64-v8a only." >&2
                exit 2
                ;;
        esac
        case "$(uname -m)" in
            arm64 | aarch64) ;;
            *)
                echo "android-emulator: refused: this host is $(uname -m). An arm64 system image needs an arm64 host (an Apple silicon Mac) to run with hardware help." >&2
                exit 2
                ;;
        esac
        need avdmanager
        dir=$ANDROID_HOME/system-images/$(echo "$image" | cut -d';' -f2)/$(echo "$image" | cut -d';' -f3)/$(echo "$image" | cut -d';' -f4)
        if [ ! -d "$dir" ]; then
            echo "android-emulator: the image is not installed: $image. The owner installs it: sdkmanager \"$image\"" >&2
            exit 2
        fi
        # `no` answers the question about a custom hardware profile.
        echo no | avdmanager create avd --force --name "$AVD" --package "$image" --device pixel_7 || {
            echo "android-emulator: avdmanager failed" >&2
            exit 1
        }
        echo "android-emulator: created $AVD from $image"
        ;;
    start)
        need adb
        need emulator
        if adb devices 2>/dev/null | grep -q "^$SERIAL[[:space:]]*device"; then
            echo "android-emulator: $SERIAL is already running"
            exit 0
        fi
        gpu=swiftshader_indirect
        [ "${HOLZWAVE_EMULATOR_GPU:-}" = host ] && gpu=host
        started=
        # A start that does not finish kills the emulator it launched and removes its pid file and its log (the log's last
        # lines are printed first). A start that finishes leaves the emulator running; `stop` ends it and removes both.
        cleanup() {
            if [ -n "$started" ]; then
                kill "$started" 2>/dev/null
                rm -f "$PIDFILE" "$LOGFILE"
            fi
        }
        trap cleanup EXIT HUP INT TERM
        nohup emulator -avd "$AVD" -port "$PORT" -no-window -no-audio -no-snapshot -no-boot-anim -gpu "$gpu" >"$LOGFILE" 2>&1 &
        started=$!
        echo "$started" >"$PIDFILE"
        echo "android-emulator: started $AVD on $SERIAL (pid $started, gpu $gpu), waiting for the boot"
        waited=0
        while [ "$waited" -lt "$BOOT_TIMEOUT" ]; do
            if ! kill -0 "$started" 2>/dev/null; then
                echo "android-emulator: the emulator ended early; the last lines of its log:" >&2
                tail -n 15 "$LOGFILE" >&2
                exit 1
            fi
            if [ "$(adb -s "$SERIAL" shell getprop sys.boot_completed 2>/dev/null | tr -d '\r')" = 1 ]; then
                started= # booted: it stays running, stop ends it
                echo "android-emulator: $SERIAL has booted after $waited s"
                exit 0
            fi
            sleep 3
            waited=$((waited + 3))
        done
        echo "android-emulator: no boot within $BOOT_TIMEOUT s" >&2
        tail -n 15 "$LOGFILE" >&2
        exit 1
        ;;
    stop)
        need adb
        adb -s "$SERIAL" emu kill >/dev/null 2>&1
        if [ -f "$PIDFILE" ]; then
            pid=$(cat "$PIDFILE")
            kill "$pid" 2>/dev/null
            rm -f "$PIDFILE"
        fi
        rm -f "$LOGFILE"
        echo "android-emulator: stopped $SERIAL"
        ;;
    install)
        need adb
        apk=${2:-}
        if [ -z "$apk" ]; then
            for candidate in app/src-tauri/gen/android/app/build/outputs/apk/universal/debug/*.apk; do
                [ -f "$candidate" ] && apk=$candidate
            done
        fi
        if [ -z "$apk" ] || [ ! -f "$apk" ]; then
            echo "android-emulator: no debug APK to install: run sh scripts/android-build.sh debug" >&2
            exit 2
        fi
        adb -s "$SERIAL" install -r "$apk" || {
            echo "android-emulator: the install failed" >&2
            exit 1
        }
        ;;
    shot)
        need adb
        out=${2:-}
        [ -n "$out" ] || usage
        tmp=$(mktemp "${TMPDIR:-/tmp}/holzwave-shot.XXXXXX") || exit 1
        trap 'rm -f "$tmp"' EXIT HUP INT TERM
        if adb -s "$SERIAL" exec-out screencap -p >"$tmp" && [ -s "$tmp" ]; then
            mv "$tmp" "$out"
            echo "android-emulator: screenshot $out"
        else
            echo "android-emulator: the screenshot failed" >&2
            exit 1
        fi
        ;;
    *) usage ;;
esac
