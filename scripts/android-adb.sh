# The one way the Android scripts talk to a device. Source it, do not run it:
#
#   . scripts/android-adb.sh        (after SERIAL=emulator-<port> was set)
#
# The project's test devices are arm64 emulators, never a phone: the owner may have a real phone on the cable, and a command
# that reached it (a PIN set, a screen size changed, keys injected) would change something that is not the project's. So:
#
#   adbd <args>     adb with `-s $SERIAL`, always. A call that names no serial does not exist in the scripts.
#   adb_serial_ok   the serial is `emulator-<digits>` and nothing else (a typed port is checked, so no text reaches adb as a serial).
#   adb_state       the state of $SERIAL in `adb devices` (device, offline, ...), nothing when it is not listed at all.
#   adb_wait_ready  wait up to <seconds> for $SERIAL to be a `device` (adbd restarts after `adb root` or `adb unroot`, and the device
#                   is listed as offline for a moment; a script that took that for "no emulator" would start a second one).
#   adb_require     exit 2 unless $SERIAL is attached as a device in `adb devices` AND reports the emulator property
#                   (`ro.kernel.qemu` or `ro.boot.qemu` is 1). It is the first thing a script runs against a device and it issues
#                   no command at all to a device that is not in the list, so with only a physical device attached nothing is sent.
#
# The file never installs, signs or downloads anything and calls no `exit` outside `adb_require`.

adb_serial_ok() {
    case "$SERIAL" in
        emulator-*[!0-9]* | emulator- | "") return 1 ;;
        emulator-*) return 0 ;;
        *) return 1 ;;
    esac
}

adbd() { adb -s "$SERIAL" "$@"; }

adb_state() { adb devices 2>/dev/null | awk -v want="$SERIAL" '$1 == want { print $2; exit }'; }

adb_wait_ready() { # <seconds>
    waited_ready=0
    while [ "$waited_ready" -lt "$1" ]; do
        [ "$(adb_state)" = device ] && return 0
        sleep 1
        waited_ready=$((waited_ready + 1))
    done
    [ "$(adb_state)" = device ]
}

# $1 names the caller in the message.
adb_require() {
    if ! adb_serial_ok; then
        echo "$1: refused: '$SERIAL' is not an emulator serial (emulator-<port>); only emulators are used" >&2
        exit 2
    fi
    if ! adb devices 2>/dev/null | awk -v want="$SERIAL" '$1 == want && $2 == "device" { found = 1 } END { exit found ? 0 : 1 }'; then
        echo "$1: no emulator $SERIAL is attached (a phone on the cable is never used): start one with sh scripts/android-emulator.sh start" >&2
        exit 2
    fi
    adb_qemu=$(adbd shell getprop ro.kernel.qemu 2>/dev/null | tr -d '\r')
    [ "$adb_qemu" = 1 ] || adb_qemu=$(adbd shell getprop ro.boot.qemu 2>/dev/null | tr -d '\r')
    if [ "$adb_qemu" != 1 ]; then
        echo "$1: refused: $SERIAL does not report the emulator property (ro.kernel.qemu / ro.boot.qemu), so it may be a real device" >&2
        exit 2
    fi
}
