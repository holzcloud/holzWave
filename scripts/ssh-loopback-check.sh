#!/bin/sh
# A real OpenSSH server on loopback as the judge of the SSH client: public-key login with Ed25519 and RSA keys, in the
# OpenSSH format, in PEM (PKCS#1) and in PKCS#8, with and without a passphrase; a jump through the same server
# (direct-tcpip); one SFTP listing; one strict exec command. The in-process tests of core/tests/ssh_auth.rs prove the
# same paths against a fake server; this check proves that the fake and the real server agree.
#
#   sh scripts/ssh-loopback-check.sh
#
# What it does: makes a private folder (mktemp -d, mode 0700), generates throwaway keys in it with ssh-keygen, writes
# an sshd_config that listens on 127.0.0.1 only, on a free port, and lets only the current user in with those keys,
# starts /usr/sbin/sshd -D -e as the current user (no sudo, no root), learns its host key with ssh-keyscan and runs the
# ignored tests of core/tests/ssh_live.rs against it. The server and the folder are removed on every way out of the
# script (normal end, failure, Ctrl-C, a signal). It never reads or writes the user's own ssh folder or their own
# known hosts file, and it generates nothing that outlives the run.
#
# Needs: ssh-keygen, ssh-keyscan, /usr/sbin/sshd, an sftp-server binary (/usr/libexec/sftp-server, /usr/lib/openssh/
# sftp-server or /usr/lib/ssh/sftp-server) and the Rust toolchain (no Zig: the tests build without the engine).
# Present on macOS and on most Linux hosts. The passphrases of the generated keys are throwaway text and reach the test
# only through its environment.
#
# What it cannot cover: a host that is not OpenSSH, a password login (the server here allows keys only), the network
# (everything is loopback), and the user's own shell start-up beyond what the login shell prints.
#
# Exit status: 0 when every check passed (the last line is "ssh-loopback-check: ok"), 1 when a check failed, 2 when a
# tool is missing. A missing tool is a failed gate, never a skip. The script installs nothing.

here=$(cd "$(dirname "$0")" && pwd)
cd "$here/.." || exit 2

SSHD=/usr/sbin/sshd
dir=
sshd_pid=
pkcs8_pass=

fail() {
    echo "ssh-loopback-check: $*" >&2
    exit 1
}

need() {
    command -v "$1" >/dev/null 2>&1 || {
        echo "ssh-loopback-check: $1 is missing" >&2
        exit 2
    }
}

# Stop the server and remove the folder. Safe to call twice.
stop() {
    if [ -n "$sshd_pid" ]; then
        kill "$sshd_pid" 2>/dev/null
        wait "$sshd_pid" 2>/dev/null
        sshd_pid=
    fi
    if [ -n "$dir" ]; then
        rm -rf "$dir"
        dir=
    fi
}

# The way out on every path: a normal end, `exit`, and a signal.
cleanup() {
    status=$?
    trap - EXIT INT TERM HUP
    stop
    exit "$status"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP

need ssh-keygen
need ssh-keyscan
[ -x "$SSHD" ] || {
    echo "ssh-loopback-check: $SSHD is missing" >&2
    exit 2
}
sftp_server=
for candidate in /usr/libexec/sftp-server /usr/lib/openssh/sftp-server /usr/lib/ssh/sftp-server; do
    if [ -x "$candidate" ]; then
        sftp_server=$candidate
        break
    fi
done
[ -n "$sftp_server" ] || {
    echo "ssh-loopback-check: no sftp-server binary found" >&2
    exit 2
}

. "$here/env.sh"
if ! command -v cargo >/dev/null 2>&1 && command -v brew >/dev/null 2>&1; then
    # A Homebrew rustup is keg-only and not on PATH.
    PATH="$(brew --prefix rustup 2>/dev/null)/bin:$PATH"
fi
need cargo

user=$(id -un)
umask 077
dir=$(mktemp -d "${TMPDIR:-/tmp}/holzwave-sshd.XXXXXX") || exit 2
chmod 700 "$dir"
echo "ssh-loopback-check: scratch folder $dir (removed on exit)"

# A free port on loopback: ask the system for one; without python3, a random one that nothing answers on.
port=
if command -v python3 >/dev/null 2>&1; then
    port=$(python3 -c 'import socket; s = socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1]); s.close()')
fi
if [ -z "$port" ]; then
    tries=0
    while [ "$tries" -lt 20 ]; do
        port=$(awk 'BEGIN { srand(); print int(20000 + rand() * 40000) }')
        if [ -z "$(ssh-keyscan -T 1 -p "$port" 127.0.0.1 2>/dev/null)" ]; then
            break
        fi
        port=
        tries=$((tries + 1))
    done
fi
[ -n "$port" ] || fail "no free port found"

# Throwaway keys. The passphrase is only for the keys that are named for it.
pass="loopback passphrase $$"
keygen() {
    ssh-keygen -q -C holzwave-loopback "$@" || fail "ssh-keygen failed: $*"
}
keygen -t ed25519 -N "" -f "$dir/host_ed25519"
keygen -t ed25519 -N "" -f "$dir/user_ed25519"
keygen -t ed25519 -N "$pass" -f "$dir/user_ed25519_pass"
keygen -t rsa -b 3072 -m PEM -N "" -f "$dir/user_rsa_pem"
keygen -t rsa -b 3072 -m PEM -N "$pass" -f "$dir/user_rsa_pem_pass"
keygen -t rsa -b 3072 -m PKCS8 -N "" -f "$dir/user_rsa_pkcs8"
# The encrypted PKCS#8 key is the plain one sealed again by openssl with PBKDF2 and HMAC-SHA-256, the way OpenSSL 3 and
# later write it. ssh-keygen of LibreSSL (the macOS one) seals with HMAC-SHA-1, which the key library does not read; the
# client says so in words (see key_error_text in core/src/ssh.rs). Without an openssl that has -v2prf the key is not
# made, the script says so and the test reports it as not covered.
if command -v openssl >/dev/null 2>&1 &&
    openssl pkcs8 -topk8 -in "$dir/user_rsa_pkcs8" -out "$dir/user_rsa_pkcs8_pass" -v2 aes-256-cbc -v2prf hmacWithSHA256 \
        -passout "pass:$pass" 2>/dev/null; then
    chmod 600 "$dir/user_rsa_pkcs8_pass"
    pkcs8_pass="$dir/user_rsa_pkcs8_pass"
else
    rm -f "$dir/user_rsa_pkcs8_pass"
    pkcs8_pass=
    echo "ssh-loopback-check: note: no openssl with -v2prf found, so the encrypted PKCS#8 key is not covered" >&2
fi
cat "$dir"/user_*.pub >"$dir/authorized_keys"

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
AllowTcpForwarding yes
AllowStreamLocalForwarding yes
LogLevel VERBOSE
Subsystem sftp $sftp_server
EOF

"$SSHD" -D -e -f "$dir/sshd_config" >"$dir/sshd.log" 2>&1 &
sshd_pid=$!

# Wait up to 10 seconds for the server to answer, and learn its host key in the same step.
waited=0
while [ "$waited" -lt 50 ]; do
    kill -0 "$sshd_pid" 2>/dev/null || {
        tail -n 30 "$dir/sshd.log" >&2
        fail "sshd stopped at start"
    }
    ssh-keyscan -T 2 -p "$port" -t ed25519 127.0.0.1 >"$dir/known_hosts" 2>/dev/null
    [ -s "$dir/known_hosts" ] && break
    waited=$((waited + 1))
    sleep 0.2
done
[ -s "$dir/known_hosts" ] || {
    tail -n 30 "$dir/sshd.log" >&2
    fail "sshd did not answer on 127.0.0.1:$port within 10 seconds"
}
echo "ssh-loopback-check: $("$SSHD" -V 2>&1 | head -n 1) is listening on 127.0.0.1:$port as $user"

export HOLZWAVE_TEST_SSH_HOST=127.0.0.1
export HOLZWAVE_TEST_SSH_PORT="$port"
export HOLZWAVE_TEST_SSH_USER="$user"
export HOLZWAVE_TEST_SSH_KNOWN_HOSTS="$dir/known_hosts"
export HOLZWAVE_TEST_SSH_PASSPHRASE="$pass"
export HOLZWAVE_TEST_SSH_KEY_ED25519="$dir/user_ed25519"
export HOLZWAVE_TEST_SSH_KEY_ED25519_PASS="$dir/user_ed25519_pass"
export HOLZWAVE_TEST_SSH_KEY_RSA_PEM="$dir/user_rsa_pem"
export HOLZWAVE_TEST_SSH_KEY_RSA_PEM_PASS="$dir/user_rsa_pem_pass"
export HOLZWAVE_TEST_SSH_KEY_RSA_PKCS8="$dir/user_rsa_pkcs8"
export HOLZWAVE_TEST_SSH_KEY_RSA_PKCS8_PASS="$pkcs8_pass"

if cargo test --locked --offline -p holzwave-core --no-default-features --test ssh_live \
    -- --ignored --nocapture --test-threads=1; then
    result=0
else
    result=1
    echo "---- the last lines of the sshd log" >&2
    tail -n 30 "$dir/sshd.log" >&2
fi

# Remove the server and the folder now, so the last line is true only when nothing is left.
folder=$dir
stop
if [ -e "$folder" ]; then
    fail "the scratch folder $folder is still there"
fi
if [ "$result" -ne 0 ]; then
    fail "a check failed"
fi
echo "ssh-loopback-check: ok"
