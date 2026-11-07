#!/bin/sh
# The Android emulator of the project: one arm64 virtual device, headless.
#
#   sh scripts/android-emulator.sh create|start|stop [profile]
#   sh scripts/android-emulator.sh install [apk] [profile]
#   sh scripts/android-emulator.sh shot <png>
#
# The profile names one of three virtual devices, all arm64 (the first is the default):
#
#   api37   `holzwave-arm64`        system-images;android-37.0;default;arm64-v8a            the newest image of the project
#   api31   `holzwave-arm64-api31`  system-images;android-31;default;arm64-v8a              the minimum version of the app (Android 12)
#   16k     `holzwave-arm64-16k`    system-images;android-37.0;google_apis_ps16k;arm64-v8a  the 16 KB page size image
#
#   create   make the virtual device of the profile from the system image that is already installed
#            (HOLZWAVE_ANDROID_IMAGE names another image for the profile; it must be arm64-v8a as well). The device is made with
#            hw.keyboard=no, so the soft keyboard shows. An image that is not installed is exit 2 and the sdkmanager line, as
#            text: the script never installs a package.
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

PORT=${HOLZWAVE_EMULATOR_PORT:-5554}
BOOT_TIMEOUT=${HOLZWAVE_EMULATOR_BOOT_TIMEOUT:-300}

here=$(cd "$(dirname "$0")" && pwd -P)
cd "$here/.." || exit 2
. scripts/android-env.sh

SERIAL=emulator-$PORT
export ANDROID_SERIAL=$SERIAL
. scripts/android-adb.sh
adb_serial_ok || {
    echo "android-emulator: refused: HOLZWAVE_EMULATOR_PORT '$PORT' is not a number" >&2
    exit 2
}

# The three profiles, one line each: the virtual device and the system image it is made from. An image id that does not end in
# arm64-v8a is refused by `create` below, whatever this table says or HOLZWAVE_ANDROID_IMAGE names.
profile_table() { # <profile>: sets AVD and IMAGE_DEFAULT; returns 1 for a name that is not in the table
    case "$1" in
        api37 | "") AVD=holzwave-arm64 IMAGE_DEFAULT='system-images;android-37.0;default;arm64-v8a' ;;
        api31) AVD=holzwave-arm64-api31 IMAGE_DEFAULT='system-images;android-31;default;arm64-v8a' ;;
        16k) AVD=holzwave-arm64-16k IMAGE_DEFAULT='system-images;android-37.0;google_apis_ps16k;arm64-v8a' ;;
        *) return 1 ;;
    esac
}
PIDFILE="${TMPDIR:-/tmp}/holzwave-android-emulator-$PORT.pid"
LOGFILE="${TMPDIR:-/tmp}/holzwave-android-emulator-$PORT.log"

usage() {
    echo "usage: sh scripts/android-emulator.sh create|start|stop [api37|api31|16k]   |   install [apk] [profile]   |   shot <png>" >&2
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
# The profile is the second word of create, start and stop, and the third of install.
profile=${2:-}
[ "$cmd" = install ] && profile=${3:-}
[ "$cmd" = shot ] && profile=
profile_table "$profile" || {
    echo "android-emulator: unknown profile '$profile' (api37, api31 or 16k)" >&2
    exit 2
}

case "$cmd" in
    create)
        image=${HOLZWAVE_ANDROID_IMAGE:-$IMAGE_DEFAULT}
        sdkline="sdkmanager \"$image\""
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
            echo "android-emulator: the image is not installed: $image. The owner installs it: $sdkline" >&2
            exit 2
        fi
        # `no` answers the question about a custom hardware profile.
        echo no | avdmanager create avd --force --name "$AVD" --package "$image" --device pixel_7 || {
            echo "android-emulator: avdmanager failed" >&2
            exit 1
        }
        # The soft keyboard must show (a virtual device with a hardware keyboard hides it): the setting is written into the device's own
        # configuration, which avdmanager made a moment ago.
        config=${ANDROID_AVD_HOME:-$HOME/.android/avd}/$AVD.avd/config.ini
        if [ -f "$config" ]; then
            grep -v '^hw\.keyboard=' "$config" >"$config.new" && echo 'hw.keyboard=no' >>"$config.new" && mv "$config.new" "$config" || {
                rm -f "$config.new"
                echo "android-emulator: the keyboard setting of $AVD could not be written" >&2
                exit 1
            }
        else
            echo "android-emulator: $config was not found after avdmanager ran: the keyboard setting could not be written" >&2
            exit 1
        fi
        echo "android-emulator: created $AVD from $image"
        ;;
    start)
        need adb
        need emulator
        # An emulator that is attached but not ready (adbd restarts after `adb root` and `adb unroot`: the device shows as offline for a
        # moment) is waited for, never started a second time: two emulators on one virtual device refuse each other.
        if [ -n "$(adb_state)" ]; then
            if adb_wait_ready 90; then
                echo "android-emulator: $SERIAL is already running"
                exit 0
            fi
            echo "android-emulator: $SERIAL is attached but did not become ready within 90 s (state: $(adb_state))" >&2
            exit 1
        fi
        if [ ! -f "${ANDROID_AVD_HOME:-$HOME/.android/avd}/$AVD.avd/config.ini" ]; then
            echo "android-emulator: the virtual device $AVD does not exist: run sh scripts/android-emulator.sh create ${profile:-api37}" >&2
            exit 2
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
            if [ "$(adbd shell getprop sys.boot_completed 2>/dev/null | tr -d '\r')" = 1 ]; then
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
        adbd emu kill >/dev/null 2>&1
        if [ -f "$PIDFILE" ]; then
            pid=$(cat "$PIDFILE")
            kill "$pid" 2>/dev/null
            rm -f "$PIDFILE"
        fi
        # `emu kill` returns at once and the emulator takes seconds to leave the list of devices; a script that starts the next image
        # right away would find the old one still listed. Wait until it is gone (at most 60 s).
        gone=0
        while [ "$gone" -lt 60 ] && [ -n "$(adb_state)" ]; do
            sleep 1
            gone=$((gone + 1))
        done
        rm -f "$LOGFILE"
        if [ -n "$(adb_state)" ]; then
            echo "android-emulator: $SERIAL is still listed after 60 s (state: $(adb_state))" >&2
            exit 1
        fi
        echo "android-emulator: stopped $SERIAL"
        ;;
    install)
        need adb
        adb_require android-emulator
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
        adbd install -r "$apk" || {
            echo "android-emulator: the install failed" >&2
            exit 1
        }
        ;;
    shot)
        need adb
        adb_require android-emulator
        out=${2:-}
        [ -n "$out" ] || usage
        tmp=$(mktemp "${TMPDIR:-/tmp}/holzwave-shot.XXXXXX") || exit 1
        trap 'rm -f "$tmp"' EXIT HUP INT TERM
        if adbd exec-out screencap -p >"$tmp" && [ -s "$tmp" ]; then
            mv "$tmp" "$out"
            echo "android-emulator: screenshot $out"
        else
            echo "android-emulator: the screenshot failed" >&2
            exit 1
        fi
        ;;
    *) usage ;;
esac
