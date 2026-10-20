#!/bin/sh
# probe-gui-path.sh: what PATH does an application that is started the way the Finder starts it see?
#
# usage: sh scripts/probe-gui-path.sh [--finder]
#
# macOS only (on another system it prints `not macOS` and exits 0). It builds a scratch application bundle in a temporary
# directory, `ProbePath.app`, whose executable writes PATH, SHELL, HOME, USER and `id -un` to a file in that directory. The
# bundle is started with `open -n -W`, the way the Finder opens an application (through Launch Services, with the
# environment of the user's launchd session and not the one of this terminal), under a 20 s limit. The script then prints
# the PATH the launched application saw beside the terminal's PATH, one line per tool of the integrations (kubectl, az, gam,
# talosctl: found or missing from the launched PATH), and for comparison `launchctl getenv PATH` and `path_helper -s`.
# The home directory is printed as `~`. When `open` cannot start the bundle (no GUI session, for example over ssh), it prints
# `gui launch not possible here` and exits 0.
#
# `open` hands its own environment on to the application, so a launch from a terminal does not show what a Finder launch
# shows. The script therefore launches the bundle twice with `open -n -W`: once as it is (the application inherits the
# terminal's environment, which proves nothing about the Finder), and once with a scrubbed environment (the launchd
# default PATH), which is the Finder's situation. `--finder` adds the real thing: the bundle is opened by the Finder itself
# through AppleScript. That may ask once for permission to control the Finder, so it is not part of the default run.
#
# The bundle has LSUIElement set, so nothing appears in the Dock. It is never installed anywhere else and is removed when
# the script ends. Plain POSIX sh.

set -u

if [ "$(uname -s)" != Darwin ]; then
    echo "not macOS"
    exit 0
fi

LIMIT=20
WORK=$(mktemp -d "${TMPDIR:-/tmp}/hw-gui-path.XXXXXX") || {
    echo "probe-gui-path: mktemp failed" >&2
    exit 1
}
APP="$WORK/ProbePath.app"
EXE="$APP/Contents/MacOS/probe"
RESULT="$WORK/result"
OPEN_PID=""

cleanup() {
    if [ -n "$OPEN_PID" ]; then
        kill "$OPEN_PID" 2>/dev/null
    fi
    # Stop a bundle that is still running: its executable lives in the scratch directory named by the unique path.
    pkill -f "$EXE" 2>/dev/null
    rm -rf "$WORK"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

mkdir -p "$APP/Contents/MacOS"
cat >"$APP/Contents/Info.plist" <<'EOF'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleIdentifier</key><string>invalid.example.probepath</string>
    <key>CFBundleName</key><string>ProbePath</string>
    <key>CFBundleExecutable</key><string>probe</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleVersion</key><string>1</string>
    <key>LSUIElement</key><true/>
</dict>
</plist>
EOF
# The executable: a shell script that writes what the launched application sees. The result path is the only value that
# is built into it, and it is a scratch path made by mktemp.
{
    printf '%s\n' '#!/bin/sh'
    printf "OUT='%s'\n" "$RESULT"
    printf '%s\n' '{'
    printf '%s\n' '    printf "PATH=%s\n" "${PATH-}"'
    printf '%s\n' '    printf "SHELL=%s\n" "${SHELL-}"'
    printf '%s\n' '    printf "HOME=%s\n" "${HOME-}"'
    printf '%s\n' '    printf "USER=%s\n" "${USER-}"'
    printf '%s\n' '    printf "ID=%s\n" "$(/usr/bin/id -un)"'
    printf '%s\n' '} >"$OUT.tmp" && mv "$OUT.tmp" "$OUT"'
} >"$EXE"
chmod 755 "$EXE"

hide_home() {
    sed "s|$(printf '%s' "$HOME" | sed 's/[][\.*^$|/]/\\&/g')|~|g"
}

# wait_result: wait up to LIMIT seconds for the file the application writes.
wait_result() {
    _waited=0
    while [ ! -f "$RESULT" ] && [ "$_waited" -lt $((LIMIT * 2)) ]; do
        sleep 0.5
        _waited=$((_waited + 1))
    done
    [ -f "$RESULT" ]
}

# report LABEL: print what the launched application saw.
report() {
    LAUNCHED=$(sed -n 's/^PATH=//p' "$RESULT")
    echo "== $1"
    sed -e '/^PATH=/d' "$RESULT" | hide_home | sed 's/^/  /'
    echo "  PATH of the launched app:"
    printf '%s\n' "$LAUNCHED" | tr ':' '\n' | hide_home | sed 's/^/    /'
    for _tool in kubectl az gam talosctl; do
        _hit="missing from"
        _rest="$LAUNCHED:"
        while [ -n "$_rest" ]; do
            _d=${_rest%%:*}
            _rest=${_rest#*:}
            case "$_d" in
                /*)
                    if [ -f "$_d/$_tool" ] && [ -x "$_d/$_tool" ]; then
                        _hit="found in"
                        break
                    fi
                    ;;
            esac
        done
        echo "  $_tool: $_hit launched PATH"
    done
}

# launch LABEL COMMAND...: run the command (it starts the bundle), wait for the result, report or say it failed.
launch() {
    _label=$1
    shift
    rm -f "$RESULT"
    "$@" </dev/null >/dev/null 2>&1 &
    OPEN_PID=$!
    if wait_result; then
        report "$_label"
        _ok=1
    else
        echo "== $_label"
        echo "  gui launch not possible here"
        _ok=0
    fi
    kill "$OPEN_PID" 2>/dev/null
    pkill -f "$EXE" 2>/dev/null
    OPEN_PID=""
}

FINDER=0
[ "${1:-}" = "--finder" ] && FINDER=1

LAUNCHD_PATH=$(launchctl getenv PATH 2>/dev/null)
[ -n "$LAUNCHD_PATH" ] || LAUNCHD_PATH=/usr/bin:/bin:/usr/sbin:/sbin

launch "open -n -W from this terminal (the terminal's environment is handed on)" /usr/bin/open -n -W "$APP"
if [ "$_ok" = 0 ]; then
    echo "gui launch not possible here"
    exit 0
fi
launch "open -n -W with a scrubbed environment (the launchd PATH: the situation of a Finder launch)" \
    /usr/bin/env -i HOME="$HOME" USER="${USER:-user}" SHELL="${SHELL:-/bin/zsh}" TMPDIR="${TMPDIR:-/tmp}" PATH="$LAUNCHD_PATH" \
    /usr/bin/open -n -W "$APP"
if [ "$FINDER" = 1 ]; then
    launch "opened by the Finder itself (AppleScript)" /usr/bin/osascript -e "tell application \"Finder\" to open (POSIX file \"$APP\" as alias)"
fi

echo "== the terminal's own PATH, for comparison"
printf '%s\n' "$PATH" | tr ':' '\n' | hide_home | sed 's/^/    /'
echo "== launchctl getenv PATH (empty: launchd has none set and the system default applies)"
launchctl getenv PATH 2>/dev/null | tr ':' '\n' | hide_home | sed 's/^/    /'
echo "== path_helper -s"
/usr/libexec/path_helper -s 2>/dev/null | hide_home | sed 's/^/    /'
