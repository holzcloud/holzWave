#!/bin/sh
# A throwaway OpenSSH server on the loopback address of this computer, for the SSH check of the Android emulator.
#
#   sh scripts/android-ssh-server.sh start
#   sh scripts/android-ssh-server.sh stop <dir>
#
# `start` makes a private folder (mktemp -d, mode 0700), generates a throwaway Ed25519 host key and a throwaway Ed25519 client key in
# it with ssh-keygen (the client key is sealed with a throwaway passphrase, which the key generation is given through a private
# SSH_ASKPASS program that reads a file of the folder: no key and no passphrase is ever an argument of a command), writes an
# sshd_config that listens on 127.0.0.1 only, on a free port, lets in only the current user with that one key (key login only,
# forwarding off) and logs every login (LogLevel VERBOSE), starts /usr/sbin/sshd -D -e as the current user (no sudo, no root) in the
# background, waits for the port, and prints what the caller needs as `key=value` lines:
#
#   dir  port  user  host_key_type  host_key_blob  fingerprint  client_key  client_pass  proof  log
#
# `host_key_type` and `host_key_blob` are the two words of the host's public key (a public value), `fingerprint` is its SHA-256
# fingerprint, `client_key` and `client_pass` are files of the folder (the sealed private key and its passphrase), `proof` is a
# folder inside it where the commands typed into the session leave their proof files, and `log` is the server's log.
#
# `stop <dir>` kills the server and removes the folder; it is safe to call twice, and it refuses a folder that this script did not
# make (the folder holds a marker file of its own). `start` removes everything it made when it fails. The script that calls `start`
# calls `stop` from a trap on every way out. It never reads or writes the user's own ssh folder or their known hosts file, never
# uses their agent, and installs nothing.
#
# The server gives a shell of the current user to whoever holds the one throwaway key, for as long as it runs: the same trust level
# as scripts/ssh-loopback-check.sh, and it is bound to 127.0.0.1, so only this computer reaches it (the emulator reaches it through
# `adb reverse`, which this script does not run).
#
# Environment: HOLZWAVE_SSHD names the server program (default /usr/sbin/sshd; the stand-in test points it at a program that does not
# exist). Exit status: 0 success, 1 a step failed, 2 a missing tool or a refused request.

SSHD=${HOLZWAVE_SSHD:-/usr/sbin/sshd}
MARKER=.holzwave-android-ssh-server
PREFIX=holzwave-android-sshd

usage() {
    echo "usage: sh scripts/android-ssh-server.sh start | stop <dir>" >&2
    exit 2
}

need() { # <tool>
    command -v "$1" >/dev/null 2>&1 || {
        echo "android-ssh-server: $1 is missing" >&2
        exit 2
    }
}

# stop_dir <dir>: kill the server of the folder and remove the folder. Safe to call twice.
stop_dir() {
    d=$1
    [ -n "$d" ] && [ -d "$d" ] || return 0
    case "$(basename "$d")" in
        "$PREFIX".*) ;;
        *)
            echo "android-ssh-server: refused: $d is not a folder this script made" >&2
            return 2
            ;;
    esac
    [ -f "$d/$MARKER" ] || {
        echo "android-ssh-server: refused: $d has no marker of this script" >&2
        return 2
    }
    if [ -f "$d/server.pid" ]; then
        pid=$(cat "$d/server.pid" 2>/dev/null)
        case "$pid" in
            *[!0-9]* | "") ;;
            *)
                kill "$pid" 2>/dev/null
                waited=0
                while [ "$waited" -lt 20 ] && kill -0 "$pid" 2>/dev/null; do
                    sleep 0.2
                    waited=$((waited + 1))
                done
                kill -9 "$pid" 2>/dev/null
                ;;
        esac
    fi
    rm -rf "$d"
    return 0
}

cmd=${1:-}
case "$cmd" in
    stop)
        [ -n "${2:-}" ] || usage
        stop_dir "$2"
        exit $?
        ;;
    start) ;;
    *) usage ;;
esac

need ssh-keygen
[ -x "$SSHD" ] || {
    echo "android-ssh-server: $SSHD is missing" >&2
    exit 2
}

dir=
ready=
cleanup() {
    status=$?
    trap - EXIT INT TERM HUP
    [ -n "$ready" ] || stop_dir "$dir" >/dev/null 2>&1
    exit "$status"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP

user=$(id -un)
umask 077
dir=$(mktemp -d "${TMPDIR:-/tmp}/$PREFIX.XXXXXX") || exit 2
chmod 700 "$dir"
: >"$dir/$MARKER"
mkdir -m 700 "$dir/proof" || exit 1

# A free port on loopback: ask the system for one; without python3, a random one that nothing answers on.
port=
if command -v python3 >/dev/null 2>&1; then
    port=$(python3 -c 'import socket; s = socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1]); s.close()' 2>/dev/null)
fi
if [ -z "$port" ] && command -v nc >/dev/null 2>&1; then
    tries=0
    while [ "$tries" -lt 20 ]; do
        port=$(awk 'BEGIN { srand(); print int(20000 + rand() * 40000) }')
        nc -z -w 1 127.0.0.1 "$port" >/dev/null 2>&1 || break
        port=
        tries=$((tries + 1))
    done
fi
[ -n "$port" ] || {
    echo "android-ssh-server: no free port found" >&2
    exit 1
}

# The host key and the client key. The passphrase is made from the system's random source and lives in a file of the folder; the key
# generation reads it through a program of the folder (SSH_ASKPASS_REQUIRE=force makes ssh-keygen use the program with no terminal).
ssh-keygen -q -t ed25519 -N "" -C holzwave-e2e-host -f "$dir/host_ed25519" </dev/null || {
    echo "android-ssh-server: ssh-keygen failed for the host key" >&2
    exit 1
}
dd if=/dev/urandom bs=24 count=1 2>/dev/null | od -An -tx1 | tr -d ' \n' >"$dir/client_pass"
printf '#!/bin/sh\ncat "%s/client_pass"\n' "$dir" >"$dir/askpass"
chmod 700 "$dir/askpass"
SSH_ASKPASS="$dir/askpass" SSH_ASKPASS_REQUIRE=force DISPLAY=${DISPLAY:-none} ssh-keygen -q -t ed25519 -C holzwave-e2e-client -f "$dir/client_key" </dev/null || {
    echo "android-ssh-server: ssh-keygen failed for the client key" >&2
    exit 1
}
rm -f "$dir/askpass"
cp "$dir/client_key.pub" "$dir/authorized_keys"

# Only 127.0.0.1, only this user, only this key, no forwarding.
cat >"$dir/sshd_config" <<EOF
ListenAddress 127.0.0.1
Port $port
HostKey $dir/host_ed25519
PidFile $dir/sshd.pid
AuthorizedKeysFile $dir/authorized_keys
PubkeyAuthentication yes
PasswordAuthentication no
KbdInteractiveAuthentication no
UsePAM no
StrictModes no
AllowUsers $user
AllowTcpForwarding no
AllowAgentForwarding no
X11Forwarding no
PermitTunnel no
LogLevel VERBOSE
EOF

nohup "$SSHD" -D -e -f "$dir/sshd_config" >"$dir/sshd.log" 2>&1 </dev/null &
echo $! >"$dir/server.pid"

waited=0
while [ "$waited" -lt 50 ]; do
    kill -0 "$(cat "$dir/server.pid")" 2>/dev/null || {
        tail -n 20 "$dir/sshd.log" >&2
        echo "android-ssh-server: sshd stopped at start" >&2
        exit 1
    }
    if command -v ssh-keyscan >/dev/null 2>&1; then
        [ -n "$(ssh-keyscan -T 2 -p "$port" -t ed25519 127.0.0.1 2>/dev/null)" ] && break
    elif nc -z -w 1 127.0.0.1 "$port" >/dev/null 2>&1; then
        break
    fi
    waited=$((waited + 1))
    sleep 0.2
done
[ "$waited" -lt 50 ] || {
    tail -n 20 "$dir/sshd.log" >&2
    echo "android-ssh-server: sshd did not answer on 127.0.0.1:$port within 10 seconds" >&2
    exit 1
}

set -- $(cat "$dir/host_ed25519.pub")
key_type=$1
key_blob=$2
fingerprint=$(ssh-keygen -l -E sha256 -f "$dir/host_ed25519.pub" | awk '{ print $2 }')
ready=1
cat <<EOF
dir=$dir
port=$port
user=$user
host_key_type=$key_type
host_key_blob=$key_blob
fingerprint=$fingerprint
client_key=$dir/client_key
client_pass=$dir/client_pass
proof=$dir/proof
log=$dir/sshd.log
EOF
