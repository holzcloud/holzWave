#!/bin/sh
# The sync against a real Nextcloud, in Docker: the repeatable half of row S1 of docs/test-plan.md.
#
#   sh scripts/nextcloud-live-test.sh [--keep]
#
# What it does: starts the official `nextcloud` image with SQLite and a random admin password, waits until the server
# has installed itself, makes an app password with `occ` when the server can (the account password otherwise; the output
# says which), starts a Caddy container with its own internal CA in front of it (the TLS check), and runs the ignored
# tests of core/tests/sync_nextcloud.rs against both. Everything it started is removed on every exit path: the
# containers (with their anonymous volumes) and the private network, by name, never by a prune. `--keep` leaves the
# containers running and prints how to remove them.
#
# What it proves: that the WebDAV client and the sync engine agree with a real server on PROPFIND, MKCOL, PUT and GET,
# on the ETags the server sends, on `If-Match` and `If-None-Match: *`, on a conflict, on a wrong app password and a wrong
# passphrase, on sizes and many names, and that a server whose certificate the system does not trust is refused with a
# TLS reason before any credential is sent. It also proves that writes within one second are never lost: the server's
# ETag moves once per second, so the test provokes a real collision of two ETags (no pause anywhere), shows that the
# checksum the server reports tells the two versions apart, and shows that two devices writing without pauses lose no
# version; and that the longest theme and widget names (112 and 111 characters) upload, download and lose a conflict,
# while a longer name is reported with the limit and its length. What it cannot prove stays the owner's (S1): two real computers, the
# owner's own server and its app-password policy, a private CA in the system store, the keychain prompts.
#
# Needs: Docker (the daemon must answer), curl, and the Rust toolchain from scripts/env.sh (no Zig: the test builds
# `holzwave-core` without the terminal engine). The first run pulls about 1 GB and the first start of a new Nextcloud
# installs itself for one to five minutes; every wait is bounded. The test never runs in a plain `cargo test`: every
# test is `#[ignore]` and needs the HOLZWAVE_TEST_NEXTCLOUD_* variables this script sets.
#
# Exit status: 0 when every test passed, 1 when a check failed, 2 when a tool is missing or Docker does not answer.
# A missing tool is a failed gate, never a skip.
#
# Defaults that are decisions (each is one place to change): the image is `nextcloud:stable-apache` (the newest stable
# tag moves; the version and the digest of the run are printed), override with HOLZWAVE_NEXTCLOUD_IMAGE; the proxy is
# `caddy:2`, override with HOLZWAVE_CADDY_IMAGE; the protocol is checked over `http` on 127.0.0.1, which the client
# allows for a loopback address only, and TLS is checked separately through the proxy; the containers and the network
# are named `holzwave-nc-...-<process id>` and publish their ports on 127.0.0.1 only; no secret is ever printed.

IMAGE=${HOLZWAVE_NEXTCLOUD_IMAGE:-nextcloud:stable-apache}
CADDY_IMAGE=${HOLZWAVE_CADDY_IMAGE:-caddy:2}
ACCOUNT=holz
NC_WAIT=600
TLS_WAIT=60

here=$(cd "$(dirname "$0")" && pwd)
root=$(dirname "$here")

NAME=holzwave-nc-$$
TLS_NAME=holzwave-nc-tls-$$
NET=holzwave-nc-net-$$
keep=no
scratch=

say() { echo "nextcloud-live-test: $*"; }
fail() {
    echo "nextcloud-live-test: FAILED: $*" >&2
    exit 1
}
need() {
    command -v "$1" >/dev/null 2>&1 || {
        echo "nextcloud-live-test: $1 is not installed ($2)" >&2
        exit 2
    }
}

for arg in "$@"; do
    case "$arg" in
        --keep) keep=yes ;;
        *)
            echo "usage: sh scripts/nextcloud-live-test.sh [--keep]" >&2
            exit 2
            ;;
    esac
done

# Removes what this run started, by name, on every exit path. It never touches an image or a volume it did not make.
cleanup() {
    if [ "$keep" != yes ]; then
        docker rm -f -v "$NAME" "$TLS_NAME" >/dev/null 2>&1
        docker network rm "$NET" >/dev/null 2>&1
    fi
    [ -n "$scratch" ] && rm -rf "$scratch"
    return 0
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

need docker "the containers run in Docker"
need curl "the script waits for the server with it"
docker info >/dev/null 2>&1 || {
    echo "nextcloud-live-test: Docker does not answer (is the daemon or Docker Desktop running?)" >&2
    exit 2
}

# The Rust toolchain: the same environment as the other scripts.
# shellcheck disable=SC1091
. "$root/scripts/env.sh" >/dev/null 2>&1
need cargo "scripts/env.sh puts it on PATH"

scratch=$(mktemp -d "${TMPDIR:-/tmp}/holzwave-nc.XXXXXX") || exit 2

# The admin password is made here and handed to the container through the environment by name, so it is on no
# command line and in no output.
NEXTCLOUD_ADMIN_PASSWORD=$(od -An -N24 -tx1 /dev/urandom | tr -d ' \n')
[ -n "$NEXTCLOUD_ADMIN_PASSWORD" ] || fail "no random bytes for the admin password"
export NEXTCLOUD_ADMIN_PASSWORD
NEXTCLOUD_ADMIN_USER=$ACCOUNT
SQLITE_DATABASE=nextcloud
NEXTCLOUD_TRUSTED_DOMAINS="127.0.0.1 localhost"
export NEXTCLOUD_ADMIN_USER SQLITE_DATABASE NEXTCLOUD_TRUSTED_DOMAINS

docker network create "$NET" >/dev/null || fail "could not create the private network $NET"
[ "$keep" = yes ] && say "--keep: the containers stay; remove them with: docker rm -f -v $NAME $TLS_NAME; docker network rm $NET"

say "starting $IMAGE as $NAME (SQLite, port on 127.0.0.1 only)"
docker run -d --name "$NAME" --network "$NET" -p 127.0.0.1::80 \
    -e NEXTCLOUD_ADMIN_USER -e NEXTCLOUD_ADMIN_PASSWORD -e SQLITE_DATABASE -e NEXTCLOUD_TRUSTED_DOMAINS \
    "$IMAGE" >/dev/null || fail "could not start $IMAGE"

port=$(docker port "$NAME" 80/tcp | sed -n 's/.*:\([0-9][0-9]*\)$/\1/p' | head -n 1)
[ -n "$port" ] || fail "Docker did not report the published port"
base="http://127.0.0.1:$port"

started=$(date +%s)
say "waiting for the first start (up to $NC_WAIT s)"
while :; do
    status=$(curl -fsS --max-time 5 "$base/status.php" 2>/dev/null)
    case "$status" in
        *'"installed":true'*)
            case "$status" in *'"maintenance":true'*) ;; *) break ;; esac
            ;;
    esac
    [ "$(docker inspect -f '{{.State.Running}}' "$NAME" 2>/dev/null)" = true ] || {
        docker logs --tail 20 "$NAME" >&2 2>&1
        fail "the Nextcloud container stopped before it was ready"
    }
    waited=$(($(date +%s) - started))
    [ "$waited" -lt "$NC_WAIT" ] || fail "Nextcloud was not ready after $NC_WAIT s"
    sleep 3
done
say "Nextcloud ready after $(($(date +%s) - started)) s"

version=$(printf '%s' "$status" | sed -n 's/.*"versionstring":"\([^"]*\)".*/\1/p')
say "image: $IMAGE"
say "digest: $(docker image inspect --format '{{if .RepoDigests}}{{index .RepoDigests 0}}{{else}}none (local image){{end}}' "$IMAGE")"
say "Nextcloud version: ${version:-unknown}"

# An app password where the server can make one, the account password otherwise. Either goes to the test through the
# environment only.
credential="account password"
secret=$NEXTCLOUD_ADMIN_PASSWORD
if docker exec -u www-data "$NAME" php occ list --raw 2>/dev/null | grep -q '^user:auth-tokens:add'; then
    NC_PASS=$NEXTCLOUD_ADMIN_PASSWORD
    export NC_PASS
    token=$(docker exec -u www-data -e NC_PASS "$NAME" php occ user:auth-tokens:add --password-from-env "$ACCOUNT" 2>/dev/null | grep -v '^[[:space:]]*$' | tail -n 1 | tr -d '[:space:]')
    unset NC_PASS
    case "$token" in
        "" | *[!A-Za-z0-9]*) ;;
        *)
            secret=$token
            credential="app password"
            ;;
    esac
fi
say "account: $ACCOUNT"
say "credential: $credential"

# The same folder for the protocol over loopback http and, through the proxy, over https with its own CA.
folder="holzwave-live-$(date +%s)"
HOLZWAVE_TEST_NEXTCLOUD_URL="$base/remote.php/dav/files/$ACCOUNT/$folder"
HOLZWAVE_TEST_NEXTCLOUD_USER=$ACCOUNT
HOLZWAVE_TEST_NEXTCLOUD_PASSWORD=$secret
export HOLZWAVE_TEST_NEXTCLOUD_URL HOLZWAVE_TEST_NEXTCLOUD_USER HOLZWAVE_TEST_NEXTCLOUD_PASSWORD

# The proxy has its own internal CA, which no system trusts: the client must refuse it. `localhost:8443` with
# `tls internal` makes the certificate; the proxy reaches Nextcloud by name on the private network.
cat >"$scratch/Caddyfile" <<EOF
{
	admin off
	auto_https disable_redirects
}
localhost:8443 {
	tls internal
	log
	reverse_proxy $NAME:80
}
EOF
say "starting $CADDY_IMAGE as $TLS_NAME (internal CA, port on 127.0.0.1 only)"
docker run -d --name "$TLS_NAME" --network "$NET" -p 127.0.0.1::8443 \
    -v "$scratch/Caddyfile:/etc/caddy/Caddyfile:ro" "$CADDY_IMAGE" >/dev/null || fail "could not start $CADDY_IMAGE"
tls_port=$(docker port "$TLS_NAME" 8443/tcp | sed -n 's/.*:\([0-9][0-9]*\)$/\1/p' | head -n 1)
[ -n "$tls_port" ] || fail "Docker did not report the published TLS port"
started=$(date +%s)
# `-k` only here, to see that the proxy answers; the test itself trusts nothing it was not given.
until curl -ksS --max-time 5 --resolve "localhost:$tls_port:127.0.0.1" "https://localhost:$tls_port/status.php" 2>/dev/null | grep -q '"installed":true'; do
    [ "$(($(date +%s) - started))" -lt "$TLS_WAIT" ] || {
        docker logs --tail 20 "$TLS_NAME" >&2 2>&1
        fail "the TLS proxy did not answer after $TLS_WAIT s"
    }
    sleep 2
done
HOLZWAVE_TEST_NEXTCLOUD_TLS_URL="https://localhost:$tls_port/remote.php/dav/files/$ACCOUNT/$folder"
export HOLZWAVE_TEST_NEXTCLOUD_TLS_URL
say "TLS proxy ready"

say "running core/tests/sync_nextcloud.rs"
cd "$root" || exit 2
cargo test --locked --offline -p holzwave-core --no-default-features --test sync_nextcloud -- --ignored --nocapture --test-threads=1 ||
    fail "the live tests did not pass"

# The refusal of the untrusted certificate must come before any credential is sent: the proxy's access log holds the
# start-up probe of this script and no request for the account's folder. (A log without the probe would prove nothing.)
proxy_log=$(docker logs "$TLS_NAME" 2>&1)
printf '%s\n' "$proxy_log" | grep -q '"uri":"/status.php"' || fail "the proxy's access log is empty, so the TLS check proves nothing"
if printf '%s\n' "$proxy_log" | grep -q '"uri":"/remote.php'; then
    fail "a request for the account's folder reached the proxy: the client sent a credential to an untrusted certificate"
fi
say "TLS: no request reached the proxy behind the untrusted certificate"

say "ok"
