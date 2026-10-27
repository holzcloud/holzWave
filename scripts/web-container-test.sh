#!/bin/sh
# The container tests of the web build, from the Mac, in Docker.
#
#   sh scripts/web-container-test.sh [caps|acme|all]    (default: all)
#
# Modes: `caps` builds the privileged helper and its container test binary inside the Linux test image, gives one copy
# of the helper the file capabilities `cap_setuid,cap_setgid,cap_chown,cap_kill=p` and another none, and runs the test
# binary as an unprivileged user in a container that adds exactly those four capabilities. It proves what a worker is left
# with (no capability, no extra descriptor, a fixed environment) and that the helper refuses to run without its capabilities.
# `acme` builds the ACME test binary in the Linux test image, starts Pebble (the ACME test server of Let's Encrypt,
# ghcr.io/letsencrypt/pebble:2.10.1) on a user-defined Docker network, and runs the ignored test `acme_pebble` in a second
# container on that network whose network alias is the test domain, so that Pebble finds the server's TLS port when it
# validates the TLS-ALPN-01 challenge. The test proves a real issuance, serving, a renewal and a failed order that keeps the
# old certificate. The containers, the network and the volume are removed on every exit path.
# `all` runs every mode there is. Later modes are added as one function and one line in the `case` at the end.
#
# The image `holzwave-linux-test` of scripts/linux-container-test.sh is the base; the first run adds `libcap2-bin` and
# `util-linux` on top of it as `holzwave-web-test`. The working tree is streamed into the build container with tar (without
# target, .git, node_modules and .planning). Extended attributes do not survive a bind mount from the Mac, so the file
# capabilities are applied inside Linux on a named volume (recreated on every run) and checked with `getcap` before the
# tests start. The test container runs as user 10001 with `--cap-drop ALL`, adds `SETUID`, `SETGID`, `CHOWN` and `KILL`,
# has a read-only root file system and tmpfs mounts for `/data` and `/tmp`, and never gets more than that.
#
# Exit status: 0 when the tests ran and passed, 1 when a test failed or fewer tests ran than expected, 2 when a tool is
# missing (Docker). A missing tool is a failed gate, never a skip.

here=$(cd "$(dirname "$0")" && pwd)
root=$(dirname "$here")

# HOLZWAVE_LINUX_ARCH=amd64 selects the amd64 images, exactly as in scripts/linux-container-test.sh.
PLATFORM=
SUFFIX=
case "${HOLZWAVE_LINUX_ARCH:-}" in
    "") ;;
    amd64)
        PLATFORM=linux/amd64
        SUFFIX=-amd64
        ;;
    *)
        echo "web-container-test: HOLZWAVE_LINUX_ARCH must be unset or amd64" >&2
        exit 2
        ;;
esac
BASE_IMAGE=holzwave-linux-test$SUFFIX
WEB_IMAGE=holzwave-web-test$SUFFIX

usage() {
    echo "usage: sh scripts/web-container-test.sh [caps|acme|all]" >&2
    exit 2
}

mode=all
case "${1:-}" in
    caps | acme | all)
        mode=$1
        shift
        ;;
    "") ;;
    *) usage ;;
esac
[ $# -eq 0 ] || usage

if ! command -v docker >/dev/null 2>&1; then
    echo "web-container-test: docker is missing: install Docker Desktop" >&2
    exit 2
fi
if ! docker info >/dev/null 2>&1; then
    echo "web-container-test: the Docker daemon does not answer: start Docker Desktop" >&2
    exit 2
fi

# The working tree as a tar stream. COPYFILE_DISABLE keeps macOS from adding ._ files.
tree_stream() {
    COPYFILE_DISABLE=1 tar -c -C "$root" --exclude target --exclude .git --exclude node_modules --exclude .planning .
}

# The base image of the Linux script, and the web test image on top of it (file capability tools).
ensure_images() {
    sh "$here/linux-container-test.sh" image || {
        echo "web-container-test: the Linux test image $BASE_IMAGE could not be built" >&2
        exit 1
    }
    if ! docker image inspect "$WEB_IMAGE" >/dev/null 2>&1; then
        echo "+ docker build -t $WEB_IMAGE (Dockerfile from stdin)"
        docker build ${PLATFORM:+--platform "$PLATFORM"} -t "$WEB_IMAGE" --build-arg "BASE_IMAGE=$BASE_IMAGE" - <<'DOCKERFILE' || {
ARG BASE_IMAGE
FROM ${BASE_IMAGE}
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update \
    && apt-get install -y --no-install-recommends libcap2-bin util-linux \
    && rm -rf /var/lib/apt/lists/*
DOCKERFILE
            echo "web-container-test: the image build failed" >&2
            exit 1
        }
    fi
}

# ---- caps ---------------------------------------------------------------------------------------------------------

CAPS_VOLUME=holzwave-web-caps$SUFFIX

# The tests the caps run must report as passed, by name. A run that reports fewer is a failure, whatever its status says.
CAPS_TESTS="spawns_a_worker_as_the_person_with_nothing_left fails_closed_without_file_caps
    refuses_what_the_range_forbids refuses_a_symlink_as_home home_directory_is_private
    a_second_spawn_for_a_live_uid_is_refused the_worker_limit_is_enforced
    the_worker_gets_exactly_four_descriptors_and_a_fixed_environment a_person_cannot_run_the_helper
    the_helper_reports_a_worker_that_exits kill_uid_ends_every_process_of_the_person
    remove_home_removes_the_tree_and_the_directory the_helper_ends_its_workers_when_the_control_socket_closes
    no_process_in_the_container_has_uid_zero the_stand_in_supervisor_has_no_capabilities"

# Builds the helper and the test binary as root, and lays out /opt/hw on the volume: the helper twice (0750, root and the
# group of the test user, so that a person's worker cannot execute it), the file capabilities on one copy only, the test
# binary and the stand-in worker. The file mode and owner are set before `setcap`: a change of owner removes file capabilities.
caps_build() {
    docker volume rm -f "$CAPS_VOLUME" >/dev/null || return 1
    docker volume create "$CAPS_VOLUME" >/dev/null || return 1
    echo "+ docker run $WEB_IMAGE (build the helper and the test binary, apply the file capabilities)"
    tree_stream | docker run --rm -i --init ${PLATFORM:+--platform "$PLATFORM"} \
        -v holzwave-linux-cargo-registry:/root/.cargo/registry \
        -v holzwave-linux-cargo-git:/root/.cargo/git \
        -v holzwave-linux-target$SUFFIX:/work/target \
        -v "$CAPS_VOLUME":/opt/hw \
        -e CARGO_TERM_COLOR=never \
        ${CARGO_BUILD_JOBS:+-e CARGO_BUILD_JOBS="$CARGO_BUILD_JOBS"} \
        "$WEB_IMAGE" sh -c '
set -eu
tar -x --no-same-owner -C /work
cd /work
cargo build --locked -p holzwave-spawn-helper
test_bin=$(cargo test --locked -p holzwave-spawn-helper --test caps_container --no-run --message-format=json \
    | jq -r "select(.reason == \"compiler-artifact\" and .target.name == \"caps_container\" and .profile.test == true) | .executable" \
    | tail -n 1)
test -x "$test_bin"
chown root:root /opt/hw
chmod 0755 /opt/hw
install -m 0750 -o root -g 10001 target/debug/holzwave-spawn-helper /opt/hw/helper
install -m 0750 -o root -g 10001 target/debug/holzwave-spawn-helper /opt/hw/helper-plain
setcap cap_setuid,cap_setgid,cap_chown,cap_kill=p /opt/hw/helper
install -m 0755 -o root -g root "$test_bin" /opt/hw/caps_container
install -m 0755 -o root -g root spawn-helper/tests/support/fake-worker.sh /opt/hw/worker.sh
ls -l /opt/hw
'
}

# Runs the ignored tests as user 10001 with exactly four added capabilities, and checks what they reported.
caps_run() {
    log=$(mktemp "${TMPDIR:-/tmp}/holzwave-web-test.XXXXXX") || exit 2
    code=$(mktemp "${TMPDIR:-/tmp}/holzwave-web-test-code.XXXXXX") || exit 2
    echo "+ docker run --rm --init --user 10001:10001 --cap-drop ALL --cap-add SETUID --cap-add SETGID --cap-add CHOWN --cap-add KILL --read-only --tmpfs /data --tmpfs /tmp -v $CAPS_VOLUME:/opt/hw:ro $WEB_IMAGE"
    {
        docker run --rm --init ${PLATFORM:+--platform "$PLATFORM"} \
            --user 10001:10001 \
            --cap-drop ALL --cap-add SETUID --cap-add SETGID --cap-add CHOWN --cap-add KILL \
            --read-only \
            --tmpfs /data:rw,nosuid,nodev,noexec,uid=10001,gid=10001,mode=0711 \
            --tmpfs /tmp:rw,nosuid,nodev,noexec,mode=1777 \
            -v "$CAPS_VOLUME":/opt/hw:ro \
            -e HW_HELPER=/opt/hw/helper -e HW_HELPER_PLAIN=/opt/hw/helper-plain \
            -e HW_WORKER=/opt/hw/worker.sh -e HW_DATA=/data \
            "$WEB_IMAGE" sh -c '
set -eu
getcap /opt/hw/helper /opt/hw/helper-plain
getcap /opt/hw/helper | grep -Eq "cap_chown,cap_kill,cap_setgid,cap_setuid[ =+]*p" \
    || { echo "web-container-test: the helper does not carry the file capabilities" >&2; exit 1; }
test -z "$(getcap /opt/hw/helper-plain)" \
    || { echo "web-container-test: the plain copy of the helper carries file capabilities" >&2; exit 1; }
exec /opt/hw/caps_container --ignored --show-output
'
        echo $? >"$code"
    } 2>&1 | tee "$log"

    status=$(cat "$code")
    expected=$(echo $CAPS_TESTS | wc -w | tr -d ' ')
    passed=$(grep -c '^test .* \.\.\. ok$' "$log")
    missing=
    for name in $CAPS_TESTS; do
        grep -q "^test $name \.\.\. ok$" "$log" || missing="$missing $name"
    done
    rm -f "$log" "$code"
    echo
    echo "web-container-test: caps: $passed of $expected tests passed"
    if [ "$status" != 0 ]; then
        echo "web-container-test: FAILED (docker exit $status)" >&2
        exit 1
    fi
    if [ -n "$missing" ] || [ "$passed" != "$expected" ]; then
        echo "web-container-test: FAILED: not passed:${missing:- (the count differs)}" >&2
        exit 1
    fi
}

mode_caps() {
    ensure_images
    caps_build || {
        echo "web-container-test: the build in the container failed" >&2
        exit 1
    }
    caps_run
}

# ---- acme ---------------------------------------------------------------------------------------------------------

PEBBLE_IMAGE=ghcr.io/letsencrypt/pebble:2.10.1
ACME_NET=holzwave-web-acme$SUFFIX
ACME_VOLUME=holzwave-web-acme-bin$SUFFIX
ACME_PEBBLE=holzwave-web-acme-pebble$SUFFIX
ACME_TEST=holzwave-web-acme-test$SUFFIX
ACME_DOMAIN=hw.example.test
ACME_PORT=5001
ACME_CA_DIR=

# The test the acme run must report as passed, by name.
ACME_TESTS="acme_pebble_issues_serves_renews_and_keeps_the_old_certificate_when_an_order_fails"

acme_cleanup() {
    docker rm -f "$ACME_TEST" "$ACME_PEBBLE" >/dev/null 2>&1
    docker network rm "$ACME_NET" >/dev/null 2>&1
    docker volume rm -f "$ACME_VOLUME" >/dev/null 2>&1
    [ -z "$ACME_CA_DIR" ] || rm -rf "$ACME_CA_DIR"
}

mode_acme() {
    ensure_images
    # The port the test listens on must be the one Pebble validates on.
    grep -q "\"tlsPort\": $ACME_PORT" "$here/../server/tests/support/pebble.json" || {
        echo "web-container-test: server/tests/support/pebble.json does not name tlsPort $ACME_PORT" >&2
        exit 1
    }
    if ! docker image inspect "$PEBBLE_IMAGE" >/dev/null 2>&1; then
        echo "+ docker pull $PEBBLE_IMAGE"
        docker pull ${PLATFORM:+--platform "$PLATFORM"} "$PEBBLE_IMAGE" >/dev/null || {
            echo "web-container-test: the Pebble image $PEBBLE_IMAGE could not be pulled" >&2
            exit 1
        }
    fi
    trap acme_cleanup EXIT INT TERM
    acme_cleanup
    docker network create "$ACME_NET" >/dev/null && docker volume create "$ACME_VOLUME" >/dev/null || {
        echo "web-container-test: the network or the volume could not be created" >&2
        exit 1
    }

    echo "+ docker run $WEB_IMAGE (build the acme test binary)"
    tree_stream | docker run --rm -i --init ${PLATFORM:+--platform "$PLATFORM"} \
        -v holzwave-linux-cargo-registry:/root/.cargo/registry \
        -v holzwave-linux-cargo-git:/root/.cargo/git \
        -v holzwave-linux-target$SUFFIX:/work/target \
        -v "$ACME_VOLUME":/opt/hw \
        -e CARGO_TERM_COLOR=never \
        ${CARGO_BUILD_JOBS:+-e CARGO_BUILD_JOBS="$CARGO_BUILD_JOBS"} \
        "$WEB_IMAGE" sh -c '
set -eu
tar -x --no-same-owner -C /work
cd /work
test_bin=$(cargo test --locked -p holzwave-server --test acme --no-run --message-format=json \
    | jq -r "select(.reason == \"compiler-artifact\" and .target.name == \"acme\" and .profile.test == true) | .executable" \
    | tail -n 1)
test -x "$test_bin"
install -m 0755 "$test_bin" /opt/hw/acme
' || {
        echo "web-container-test: the build in the container failed" >&2
        exit 1
    }

    # Pebble's own CA file, copied out of the image, for the test's connection to Pebble.
    ACME_CA_DIR=$(mktemp -d "${TMPDIR:-/tmp}/holzwave-web-acme.XXXXXX") || exit 2
    chmod 0755 "$ACME_CA_DIR"
    docker create --name "$ACME_PEBBLE" "$PEBBLE_IMAGE" >/dev/null &&
        docker cp "$ACME_PEBBLE:/test/certs/pebble.minica.pem" "$ACME_CA_DIR/pebble.minica.pem" &&
        docker rm -f "$ACME_PEBBLE" >/dev/null || {
        echo "web-container-test: the CA file of Pebble could not be copied out of the image" >&2
        exit 1
    }
    chmod 0644 "$ACME_CA_DIR/pebble.minica.pem"

    echo "+ docker run -d $PEBBLE_IMAGE (PEBBLE_VA_NOSLEEP=1)"
    docker run -d --name "$ACME_PEBBLE" --network "$ACME_NET" --network-alias pebble ${PLATFORM:+--platform "$PLATFORM"} \
        -e PEBBLE_VA_NOSLEEP=1 \
        -v "$here/../server/tests/support/pebble.json":/pebble.json:ro \
        "$PEBBLE_IMAGE" -config /pebble.json >/dev/null || {
        echo "web-container-test: Pebble could not be started" >&2
        exit 1
    }
    tries=0
    until docker logs "$ACME_PEBBLE" 2>&1 | grep -q "ACME directory available"; do
        tries=$((tries + 1))
        if [ "$tries" -gt 60 ] || [ "$(docker inspect -f '{{.State.Running}}' "$ACME_PEBBLE" 2>/dev/null)" != true ]; then
            echo "web-container-test: Pebble did not come up" >&2
            docker logs "$ACME_PEBBLE" >&2
            exit 1
        fi
        sleep 1
    done

    log=$(mktemp "${TMPDIR:-/tmp}/holzwave-web-test.XXXXXX") || exit 2
    code=$(mktemp "${TMPDIR:-/tmp}/holzwave-web-test-code.XXXXXX") || exit 2
    echo "+ docker run --rm --network $ACME_NET --network-alias $ACME_DOMAIN $WEB_IMAGE /opt/hw/acme --ignored"
    {
        docker run --rm --init --name "$ACME_TEST" --network "$ACME_NET" --network-alias "$ACME_DOMAIN" \
            ${PLATFORM:+--platform "$PLATFORM"} \
            --user 10001:10001 --cap-drop ALL --read-only \
            --tmpfs /tmp:rw,nosuid,nodev,noexec,mode=1777 \
            -v "$ACME_VOLUME":/opt/hw:ro \
            -v "$ACME_CA_DIR":/acme-ca:ro \
            -e HW_ACME_DIRECTORY=https://pebble:14000/dir -e HW_ACME_CA_FILE=/acme-ca/pebble.minica.pem \
            -e HW_ACME_DOMAIN="$ACME_DOMAIN" -e HW_ACME_PORT="$ACME_PORT" -e HW_ACME_MANAGEMENT=pebble:15000 \
            "$WEB_IMAGE" /opt/hw/acme --ignored --show-output
        echo $? >"$code"
    } 2>&1 | tee "$log"

    status=$(cat "$code")
    missing=
    for name in $ACME_TESTS; do
        grep -q "^test $name \.\.\. ok$" "$log" || missing="$missing $name"
    done
    passed=$(grep '^test .* \.\.\. ok$' "$log" | sed 's/^test \(.*\) \.\.\. ok$/\1/')
    rm -f "$log" "$code"
    echo
    echo "web-container-test: acme: passed: $passed"
    if [ "$status" != 0 ] || [ -n "$missing" ]; then
        echo "web-container-test: FAILED (docker exit $status)${missing:+, not passed:$missing}" >&2
        exit 1
    fi
}

# ---- dispatch -----------------------------------------------------------------------------------------------------

case "$mode" in
    caps) mode_caps ;;
    acme) mode_acme ;;
    all)
        mode_caps
        mode_acme
        ;;
esac
echo "web-container-test: ok"
