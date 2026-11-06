#!/bin/sh
# The container tests of the web build, from the Mac, in Docker.
#
#   sh scripts/web-container-test.sh [caps|acme|isolation|image|multiarch|all] [-- <test filter>]    (default: all)
#
# The words after `--` go to the test binary of the `isolation` mode as its filter (`tracer`, `homes`, `procs`, `caps`,
# `limits`, `teardown` select a group).
#
# Modes: `caps` builds the privileged helper and its container test binary inside the Linux test image, gives one copy
# of the helper the file capabilities `cap_setuid,cap_setgid,cap_chown,cap_kill=p` and another none, and runs the test
# binary as an unprivileged user in a container that adds exactly those four capabilities. It proves what a worker is left
# with (no capability, no extra descriptor, a fixed environment) and that the helper refuses to run without its capabilities.
# `isolation` proves what keeps two people apart in one container, the way the product runs it. It builds the server binary
# (the worker) and the test binary without the `in-process-workers` feature, lays the same capability-bearing copy of the
# helper out on a volume as `caps` does, and runs the ignored tests of server/tests/isolation.rs in a container with the same
# flags as `caps`: the test process is the supervisor (user 10001, no capability), the helper is the real one, the workers
# and the shells are real, and the people have the first user ids of the range. It fails when fewer container tests ran than
# the test binary declares.
# `acme` builds the ACME test binary in the Linux test image, starts Pebble (the ACME test server of Let's Encrypt,
# ghcr.io/letsencrypt/pebble:2.10.1) on a user-defined Docker network, and runs the ignored test `acme_pebble` in a second
# container on that network whose network alias is the test domain, so that Pebble finds the server's TLS port when it
# validates the TLS-ALPN-01 challenge. The test proves a real issuance, serving, a renewal and a failed order that keeps the
# old certificate. The containers, the network and the volume are removed on every exit path.
# `image` builds the server image from the tree (packaging/docker/Dockerfile) and proves its runtime contract on a running
# container started the way the Compose file and the Helm chart start it: a read-only root, user 10001, all capabilities
# dropped and exactly CHOWN, KILL, SETGID and SETUID added. It makes a key with `keygen`, a throwaway certificate for the name
# `holzwave.test`, starts the server on a private network and asks it for the sign-in state over TLS 1.3 from a second
# container. HOLZWAVE_WEB_CONTEXT names another build context than the repository root (a clean checkout, for example), and
# HOLZWAVE_ZIG_VERSION another Zig than the default of the Dockerfile (0.17.1) for a tree whose terminal engine needs it.
# `multiarch` builds the image for linux/amd64 and linux/arm64, one platform at a time, and on each runs `uname -m`, `ldd`,
# `keygen` and `check-config` on the example configuration with a generated certificate. A platform that cannot be built or
# run on this machine is reported as `NOT RUN <platform>: <reason>` and the mode exits 1; it is never a pass.
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
    echo "usage: sh scripts/web-container-test.sh [caps|acme|isolation|image|multiarch|all] [-- <test filter>]" >&2
    exit 2
}

mode=all
case "${1:-}" in
    caps | acme | isolation | image | multiarch | all)
        mode=$1
        shift
        ;;
    "" | --) ;;
    *) usage ;;
esac
# `-- <filter>` is for the isolation tests; any other leftover is a mistake.
if [ "${1:-}" = "--" ]; then
    shift
    case "$mode" in
        isolation | all) ;;
        *) usage ;;
    esac
    FILTER="$*"
    case "$FILTER" in
        *[!A-Za-z0-9_\ :]*)
            echo "web-container-test: a test filter may hold letters, digits, underscores, colons and spaces only" >&2
            exit 2
            ;;
    esac
else
    FILTER=
    [ $# -eq 0 ] || usage
fi

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
# An image built on the base keeps the Zig label of the base it was built on. When the base was built again for another Zig,
# the image on top of it is removed here so that it is built again too and does not carry the old Zig.
zig_label() { docker image inspect -f '{{index .Config.Labels "ch.holzcloud.holzwave.zig"}}' "$1" 2>/dev/null; }
ensure_images() {
    sh "$here/linux-container-test.sh" image || {
        echo "web-container-test: the Linux test image $BASE_IMAGE could not be built" >&2
        exit 1
    }
    if docker image inspect "$WEB_IMAGE" >/dev/null 2>&1 && [ "$(zig_label "$WEB_IMAGE")" != "$(zig_label "$BASE_IMAGE")" ]; then
        echo "web-container-test: $WEB_IMAGE was built on an older base image: building it again"
        docker rmi -f "$WEB_IMAGE" >/dev/null 2>&1 || true
    fi
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

# Builds the helper as root and lays out /opt/hw on a fresh volume: the helper (0750, root and the group of the test user, so
# that a person's worker cannot execute it) with the file capabilities, then whatever `$2` adds. `$1` is the volume, `$2` the
# shell commands that follow in the build container (they build and install the test binary). The file mode and owner are
# set before `setcap`: a change of owner removes file capabilities. Both modes that need the helper call this function.
web_build() {
    volume=$1
    extra=$2
    docker volume rm -f "$volume" >/dev/null || return 1
    docker volume create "$volume" >/dev/null || return 1
    echo "+ docker run $WEB_IMAGE (build the helper and the test binary, apply the file capabilities)"
    tree_stream | docker run --rm -i --init ${PLATFORM:+--platform "$PLATFORM"} \
        -v holzwave-linux-cargo-registry:/root/.cargo/registry \
        -v holzwave-linux-cargo-git:/root/.cargo/git \
        -v holzwave-linux-target$SUFFIX:/work/target \
        -v "$volume":/opt/hw \
        -e CARGO_TERM_COLOR=never \
        ${CARGO_BUILD_JOBS:+-e CARGO_BUILD_JOBS="$CARGO_BUILD_JOBS"} \
        "$WEB_IMAGE" sh -c '
set -eu
tar -x --no-same-owner -C /work
cd /work
cargo build --locked -p holzwave-spawn-helper
chown root:root /opt/hw
chmod 0755 /opt/hw
install -m 0750 -o root -g 10001 target/debug/holzwave-spawn-helper /opt/hw/helper
setcap cap_setuid,cap_setgid,cap_chown,cap_kill=p /opt/hw/helper
'"$extra"'
ls -l /opt/hw
'
}

# The `caps` build: the test binary of the helper, a copy of the helper without file capabilities, the stand-in worker.
caps_build() {
    web_build "$CAPS_VOLUME" '
test_bin=$(cargo test --locked -p holzwave-spawn-helper --test caps_container --no-run --message-format=json \
    | jq -r "select(.reason == \"compiler-artifact\" and .target.name == \"caps_container\" and .profile.test == true) | .executable" \
    | tail -n 1)
test -x "$test_bin"
install -m 0750 -o root -g 10001 target/debug/holzwave-spawn-helper /opt/hw/helper-plain
install -m 0755 -o root -g root "$test_bin" /opt/hw/caps_container
install -m 0755 -o root -g root spawn-helper/tests/support/fake-worker.sh /opt/hw/worker.sh'
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
    passed=$(grep -c 'test [A-Za-z0-9_:]* \.\.\. ok$' "$log")
    missing=
    for name in $CAPS_TESTS; do
        grep -q "test $name \.\.\. ok$" "$log" || missing="$missing $name"
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

# ---- isolation ----------------------------------------------------------------------------------------------------

ISO_VOLUME=holzwave-web-iso$SUFFIX

# The `isolation` build: the helper as for `caps`, the server binary (the worker the helper starts) and the test binary, both
# built without the `in-process-workers` feature (the default), so that the supervisor under test can only use the helper.
isolation_build() {
    web_build "$ISO_VOLUME" '
cargo build --locked -p holzwave-server
test_bin=$(cargo test --locked -p holzwave-server --test isolation --no-run --message-format=json \
    | jq -r "select(.reason == \"compiler-artifact\" and .target.name == \"isolation\" and .profile.test == true) | .executable" \
    | tail -n 1)
test -x "$test_bin"
install -m 0755 -o root -g root target/debug/holzwave-server /opt/hw/holzwave-server
install -m 0755 -o root -g root "$test_bin" /opt/hw/isolation'
}

# Runs the ignored tests as user 10001 with exactly four added capabilities, counts what ran against what the test binary
# declares, and prints the per-binary counts. The same flags as the `caps` run; nothing is added.
isolation_run() {
    log=$(mktemp "${TMPDIR:-/tmp}/holzwave-web-test.XXXXXX") || exit 2
    code=$(mktemp "${TMPDIR:-/tmp}/holzwave-web-test-code.XXXXXX") || exit 2
    echo "+ docker run --rm --init --user 10001:10001 --cap-drop ALL --cap-add SETUID --cap-add SETGID --cap-add CHOWN --cap-add KILL --read-only --tmpfs /data --tmpfs /tmp -v $ISO_VOLUME:/opt/hw:ro $WEB_IMAGE /opt/hw/isolation --ignored --test-threads=1 $FILTER"
    {
        # shellcheck disable=SC2086 # $FILTER is a list of plain words, checked at the start
        docker run --rm --init ${PLATFORM:+--platform "$PLATFORM"} \
            --user 10001:10001 \
            --cap-drop ALL --cap-add SETUID --cap-add SETGID --cap-add CHOWN --cap-add KILL \
            --read-only \
            --tmpfs /data:rw,nosuid,nodev,noexec,uid=10001,gid=10001,mode=0711 \
            --tmpfs /tmp:rw,nosuid,nodev,noexec,mode=1777 \
            -v "$ISO_VOLUME":/opt/hw:ro \
            -e HW_HELPER=/opt/hw/helper -e HW_WORKER=/opt/hw/holzwave-server -e HW_DATA=/data \
            "$WEB_IMAGE" sh -c '
set -eu
getcap /opt/hw/helper
getcap /opt/hw/helper | grep -Eq "cap_chown,cap_kill,cap_setgid,cap_setuid[ =+]*p" \
    || { echo "web-container-test: the helper does not carry the file capabilities" >&2; exit 1; }
ls -ld /opt/hw/helper /opt/hw/holzwave-server /opt/hw/isolation
echo "web-container-test: declared: $(/opt/hw/isolation --list --ignored "$@" 2>/dev/null | grep -c ": test\$")"
exec /opt/hw/isolation --ignored --show-output --test-threads=1 "$@"
' sh $FILTER
        echo $? >"$code"
    } 2>&1 | tee "$log"

    status=$(cat "$code")
    declared=$(sed -n 's/^web-container-test: declared: \([0-9][0-9]*\)$/\1/p' "$log" | tail -n 1)
    # The counts come from the summary line of the test binary: the helper writes to the same stream, so the `ok` of a test
    # can land on a later line than its name.
    result=$(grep '^test result: ' "$log" | tail -n 1)
    passed=$(echo "$result" | sed -n 's/^test result: [A-Za-z]*\. \([0-9][0-9]*\) passed; \([0-9][0-9]*\) failed; \([0-9][0-9]*\) ignored.*$/\1/p')
    failed=$(echo "$result" | sed -n 's/^test result: [A-Za-z]*\. \([0-9][0-9]*\) passed; \([0-9][0-9]*\) failed; \([0-9][0-9]*\) ignored.*$/\2/p')
    ignored=$(echo "$result" | sed -n 's/^test result: [A-Za-z]*\. \([0-9][0-9]*\) passed; \([0-9][0-9]*\) failed; \([0-9][0-9]*\) ignored.*$/\3/p')
    passed=${passed:-0}
    failed=${failed:-0}
    rm -f "$log" "$code"
    echo
    echo "web-container-test: isolation: declared ${declared:-0}, passed $passed, failed $failed, ignored ${ignored:-0}"
    if [ "$status" != 0 ] || [ "$failed" != 0 ]; then
        echo "web-container-test: FAILED (docker exit $status)" >&2
        exit 1
    fi
    if [ "${declared:-0}" = 0 ] || [ "$passed" != "${declared:-0}" ]; then
        echo "web-container-test: FAILED: ${declared:-0} container tests declared and $passed passed: a test did not run" >&2
        exit 1
    fi
}

mode_isolation() {
    ensure_images
    isolation_build || {
        echo "web-container-test: the build in the container failed" >&2
        exit 1
    }
    isolation_run
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
        grep -q "test $name \.\.\. ok$" "$log" || missing="$missing $name"
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

# ---- image --------------------------------------------------------------------------------------------------------

IMG_TAG=holzwave-server-test$SUFFIX:local
IMG_CONTEXT=${HOLZWAVE_WEB_CONTEXT:-$root}
IMG_RUN=hwtest-$$
IMG_NET=$IMG_RUN-net
IMG_SERVER=$IMG_RUN-server
IMG_DATA=$IMG_RUN-data
IMG_KEY=$IMG_RUN-key
IMG_TLS=$IMG_RUN-tls
IMG_CONF=$IMG_RUN-conf
IMG_OIDC=$IMG_RUN-oidc
IMG_HOST=holzwave.test
IMG_PORT=8443
IMG_SCRATCH=
IMG_COMPOSE_FILE=$root/packaging/compose/docker-compose.yml
# The Zig the terminal engine is built with: the default of the Dockerfile, or HOLZWAVE_ZIG_VERSION when the tree needs another.
IMG_ZIG_ARG=
case "${HOLZWAVE_ZIG_VERSION:-}" in
    "") ;;
    *[!0-9.]*)
        echo "web-container-test: HOLZWAVE_ZIG_VERSION may hold digits and dots only" >&2
        exit 2
        ;;
    *) IMG_ZIG_ARG="--build-arg ZIG_VERSION=$HOLZWAVE_ZIG_VERSION" ;;
esac

# The way the Compose file and the Helm chart start the server: no root, nothing writable but the volumes and the tmpfs, no
# capability but the four the spawn helper needs. The option that stops a process from gaining privileges is left out on
# purpose: it would stop the file capabilities of the helper.
IMG_TMPFS="--tmpfs /run/holzwave:rw,nosuid,nodev,noexec,uid=10001,gid=10001,mode=0700 --tmpfs /tmp:rw,nosuid,nodev,noexec,mode=1777"
IMG_HARDEN="--read-only --user 10001:10001 --cap-drop ALL --cap-add CHOWN --cap-add KILL --cap-add SETGID --cap-add SETUID $IMG_TMPFS"
IMG_CONFINED="--read-only --user 10001:10001 --cap-drop ALL $IMG_TMPFS"

img_ok() {
    echo "ok: $*"
}

# One failed step: what failed, the end of the log of the server container if there is one, and status 1.
img_fail() {
    echo "web-container-test: FAILED: $*" >&2
    if docker inspect "$IMG_SERVER" >/dev/null 2>&1; then
        echo "web-container-test: the last 40 lines of the log of $IMG_SERVER:" >&2
        docker logs --tail 40 "$IMG_SERVER" >&2
    fi
    exit 1
}

img_cleanup() {
    docker compose -p "$IMG_RUN-compose" -f "$IMG_COMPOSE_FILE" down -v --remove-orphans >/dev/null 2>&1
    ids=$(docker ps -aq --filter "name=$IMG_RUN" 2>/dev/null)
    # shellcheck disable=SC2086 # a list of container ids
    [ -z "$ids" ] || docker rm -f $ids >/dev/null 2>&1
    volumes=$(docker volume ls -q --filter "name=$IMG_RUN" 2>/dev/null)
    # shellcheck disable=SC2086 # a list of volume names
    [ -z "$volumes" ] || docker volume rm -f $volumes >/dev/null 2>&1
    networks=$(docker network ls -q --filter "name=$IMG_RUN" 2>/dev/null)
    # shellcheck disable=SC2086 # a list of network ids
    [ -z "$networks" ] || docker network rm $networks >/dev/null 2>&1
    [ -z "$IMG_SCRATCH" ] || rm -rf "$IMG_SCRATCH"
}

# jq in the Linux test image (the Mac may not have it): the program is the arguments, the input is standard input.
img_jq() {
    docker run --rm -i "$BASE_IMAGE" jq "$@"
}

# The version of the workspace.
img_version() {
    sed -n 's/^version = "\(.*\)"$/\1/p' "$root/Cargo.toml" | head -n 1
}

# The image from the tree. The base images, the Zig and Rust toolchains and the Node image are pinned in the Dockerfile.
img_build() {
    version=$(img_version)
    revision=$(git -C "$root" rev-parse HEAD 2>/dev/null || echo unknown)
    echo "+ docker build -t $IMG_TAG -f packaging/docker/Dockerfile $IMG_CONTEXT"
    # shellcheck disable=SC2086 # IMG_ZIG_ARG is empty or one option
    docker build ${PLATFORM:+--platform "$PLATFORM"} -t "$IMG_TAG" -f "$IMG_CONTEXT/packaging/docker/Dockerfile" $IMG_ZIG_ARG \
        --build-arg "VERSION=$version" --build-arg "REVISION=$revision" "$IMG_CONTEXT" ||
        img_fail "the image build failed (packaging/docker/Dockerfile)"
    img_ok "built $IMG_TAG ($(docker image inspect -f '{{.Size}}' "$IMG_TAG") bytes)"
}

# Replaces a text in a file of the examples, and fails when nothing changed (a replacement that changes nothing proves nothing).
# $1 the example, $2 the new file, $3 the old text, $4 the new text.
img_replace() {
    sed "s#$3#$4#" "$1" >"$2" || img_fail "$1 cannot be read"
    if cmp -s "$1" "$2"; then
        img_fail "the replacement of '$3' in $1 changed nothing"
    fi
}

# A certificate for the test name in the certificate volume, made and owned the way an operator's files are (not by the image).
img_make_certificate() {
    docker run --rm --user 0:0 -v "$IMG_TLS":/m "$BASE_IMAGE" sh -ec '
openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -days 2 \
    -keyout /m/tls.key -out /m/tls.crt -subj "/CN='"$IMG_HOST"'" -addext "subjectAltName=DNS:'"$IMG_HOST"'" 2>/dev/null
chown -R 10001:10001 /m
chmod 0755 /m
chmod 0644 /m/tls.crt
chmod 0600 /m/tls.key
' || img_fail "the test certificate could not be made"
    img_ok "a throwaway certificate for $IMG_HOST (P-256, 2 days)"
}

# The operator's side: a key made by `keygen` in the key volume, a certificate for the test name in the certificate volume
# (made and owned the way an operator's files are, not by the image), a secret file for a single sign-on provider, and the
# configurations in their own volume: the files example (with and without a provider), the ACME example, and a file with the
# key of the in-process workers.
img_materials() {
    docker network create "$IMG_NET" >/dev/null && docker volume create "$IMG_DATA" >/dev/null &&
        docker volume create "$IMG_KEY" >/dev/null && docker volume create "$IMG_TLS" >/dev/null &&
        docker volume create "$IMG_OIDC" >/dev/null && docker volume create "$IMG_CONF" >/dev/null ||
        img_fail "the network or a volume could not be created"

    # shellcheck disable=SC2086 # IMG_CONFINED is a list of options
    docker run --rm $IMG_CONFINED -v "$IMG_KEY":/etc/holzwave/key "$IMG_TAG" keygen >/dev/null ||
        img_fail "keygen did not make the key"
    shape=$(docker run --rm --user 10001:10001 --read-only -v "$IMG_KEY":/etc/holzwave/key:ro --entrypoint stat "$IMG_TAG" \
        -c '%a %u:%g' /etc/holzwave/key/server.key) || img_fail "the key file cannot be read"
    [ "$shape" = "600 10001:10001" ] || img_fail "the key file has mode and owner '$shape', not '600 10001:10001'"
    IMG_KEY_SHAPE=$shape
    img_ok "keygen made the key: mode and owner $shape"

    img_make_certificate

    # A throwaway client secret of a single sign-on provider: a file the server reads, with a marker that must be found nowhere.
    IMG_CANARY=$(docker run --rm "$BASE_IMAGE" sh -c 'echo CANARY-$(head -c 24 /dev/urandom | base64 | tr -d "=+/\n")')
    docker run --rm --user 0:0 -v "$IMG_OIDC":/m "$BASE_IMAGE" sh -ec '
echo "'"$IMG_CANARY"'" >/m/secret
chown -R 10001:10001 /m
chmod 0755 /m
chmod 0600 /m/secret
' || img_fail "the client secret file could not be made"

    example=$root/packaging/compose/server.files.example.json
    mkdir -p "$IMG_SCRATCH/conf"
    img_replace "$example" "$IMG_SCRATCH/conf/files.json" "https://holzwave.example.net" "https://$IMG_HOST:$IMG_PORT"
    img_jq '.auth.oidc = [{id: "corp", name: "Company", issuer: "https://idp.example.test", client_id: "holzwave",
        client_secret_file: "/etc/holzwave/oidc/corp/secret"}]' <"$IMG_SCRATCH/conf/files.json" >"$IMG_SCRATCH/conf/server.json" ||
        img_fail "the provider could not be added to the configuration"
    img_jq '.workers = {mode: "in_process"}' <"$IMG_SCRATCH/conf/files.json" >"$IMG_SCRATCH/conf/in-process.json" ||
        img_fail "the workers key could not be added to the configuration"
    img_replace "$root/packaging/compose/server.acme.example.json" "$IMG_SCRATCH/conf/acme.json" "holzwave.example.net" "$IMG_HOST"
    img_replace "$IMG_SCRATCH/conf/acme.json" "$IMG_SCRATCH/conf/acme-agreed.json" '"terms_agreed": false' '"terms_agreed": true'
    COPYFILE_DISABLE=1 tar -c --no-xattrs -C "$IMG_SCRATCH/conf" . |
        docker run --rm -i --user 0:0 -v "$IMG_CONF":/m "$BASE_IMAGE" sh -ec 'tar -x --no-same-owner -C /m; chmod 0755 /m; chmod 0644 /m/*' ||
        img_fail "the configurations could not be written"
    img_ok "the configurations are the examples with https://$IMG_HOST:$IMG_PORT (files, files with a provider, ACME, ACME agreed, in-process)"
}

# The server container as Compose and the chart start it, plus the options of a test: the name on the private network and a
# fast health check (the command is the image's own).
img_start() {
    # shellcheck disable=SC2086 # IMG_HARDEN is a list of options
    docker run -d --name "$IMG_SERVER" --network "$IMG_NET" --network-alias "$IMG_HOST" $IMG_HARDEN \
        --health-interval 2s --health-start-period 0s \
        -v "$IMG_DATA":/data -v "$IMG_KEY":/etc/holzwave/key:ro -v "$IMG_TLS":/etc/holzwave/tls:ro \
        -v "$IMG_OIDC":/etc/holzwave/oidc/corp:ro \
        -v "$IMG_CONF":/etc/holzwave/conf:ro -e HOLZWAVE_SERVER_CONFIG=/etc/holzwave/conf/server.json \
        "$IMG_TAG" >/dev/null || img_fail "the server container did not start"
}

# Waits until the image's own health check prints `healthy`, and until docker reports the container healthy.
img_wait_healthy() {
    started=$(date +%s)
    until docker exec "$IMG_SERVER" holzwave-server healthcheck >/dev/null 2>&1; do
        [ "$(docker inspect -f '{{.State.Running}}' "$IMG_SERVER")" = true ] || img_fail "the server container stopped"
        [ $(($(date +%s) - started)) -lt 90 ] || img_fail "the server did not become healthy within 90 seconds"
        sleep 1
    done
    say=$(docker exec "$IMG_SERVER" holzwave-server healthcheck)
    [ "$say" = healthy ] || img_fail "healthcheck printed '$say', not 'healthy'"
    until [ "$(docker inspect -f '{{.State.Health.Status}}' "$IMG_SERVER")" = healthy ]; do
        [ $(($(date +%s) - started)) -lt 90 ] || img_fail "docker does not report the container healthy within 90 seconds"
        sleep 1
    done
    IMG_HEALTHY_AFTER=$(($(date +%s) - started))
    img_ok "healthy by holzwave-server healthcheck and by docker inspect after $IMG_HEALTHY_AFTER s"
}

# A client container on the network: the sign-in state over TLS 1.3 with the certificate as the only trust root.
img_client() {
    docker run --rm --network "$IMG_NET" --user 10001:10001 --read-only --cap-drop ALL --tmpfs /tmp:rw,mode=1777 \
        -v "$IMG_TLS":/m:ro "$BASE_IMAGE" "$@"
}

img_tls_answer() {
    out=$(img_client sh -c '
curl -sS -v --tlsv1.3 --http2 --cacert /m/tls.crt --max-time 20 -o /tmp/body -w "HTTP %{http_code}\n" \
    https://'"$IMG_HOST:$IMG_PORT"'/api/session 2>/tmp/trace
grep -E "SSL connection using|ALPN: server accepted" /tmp/trace
if jq -e ".state == \"anonymous\" and .bootstrap == true" /tmp/body >/dev/null; then echo "STATE anonymous bootstrap"; fi
cat /tmp/body
') || img_fail "the client could not fetch /api/session over TLS"
    echo "$out" | grep -q '^HTTP 200$' || img_fail "/api/session did not answer 200: $out"
    tls=$(echo "$out" | sed -n 's/.*SSL connection using \(TLSv[0-9.]*\).*/\1/p')
    [ "$tls" = TLSv1.3 ] || img_fail "the connection was not TLS 1.3: $out"
    alpn=$(echo "$out" | sed -n 's/.*ALPN: server accepted \(.*\)$/\1/p')
    [ "$alpn" = h2 ] || img_fail "HTTP/2 was not negotiated: $out"
    echo "$out" | grep -q '^STATE anonymous bootstrap$' || img_fail "the sign-in state is not anonymous with bootstrap true: $out"
    body=$(echo "$out" | tail -n 1)
    img_ok "https://$IMG_HOST:$IMG_PORT/api/session: HTTP 200, $tls, ALPN $alpn, $body"
}

# The configuration of the image as the registry would show it.
img_check_image_config() {
    version=$(img_version)
    docker image inspect -f '{{json .Config}}' "$IMG_TAG" | img_jq -e --arg version "$version" '
        .User == "10001:10001"
        and .Entrypoint[0] == "/usr/bin/tini"
        and .Cmd == ["serve"]
        and (.Healthcheck.Test | map(tostring) | join(" ") | contains("healthcheck"))
        and ((.Volumes | keys) == ["/data"])
        and ((.ExposedPorts | keys) == ["8443/tcp"])
        and .Labels["org.opencontainers.image.source"] == "https://github.com/holzcloud/holzWave"
        and .Labels["org.opencontainers.image.licenses"] == "GPL-3.0-only"
        and .Labels["org.opencontainers.image.version"] == $version
        and (([.Env[] | select(startswith("PATH=") | not)] | sort)
            == ["HOLZWAVE_SERVER_CONFIG=/etc/holzwave/server.json", "HOLZWAVE_SERVER_KEY_FILE=/etc/holzwave/key/server.key", "LANG=C.UTF-8"])
        and ([.Env[] | select(test("hws1:|hwb_|secret|password"; "i"))] | length) == 0' >/dev/null ||
        img_fail "the configuration of the image is not the contract (user, entrypoint, command, health check, volume, port, labels, environment)"
    img_ok "image configuration: user 10001:10001, tini, serve, health check on healthcheck, volume /data, port 8443, labels (version $version), environment of two paths and LANG"
}

img_keygen_twice() {
    before=$(docker run --rm --user 10001:10001 --read-only -v "$IMG_KEY":/etc/holzwave/key:ro --entrypoint sha256sum "$IMG_TAG" \
        /etc/holzwave/key/server.key)
    # shellcheck disable=SC2086 # IMG_CONFINED is a list of options
    docker run --rm $IMG_CONFINED -v "$IMG_KEY":/etc/holzwave/key "$IMG_TAG" keygen >"$IMG_SCRATCH/keygen.out" 2>"$IMG_SCRATCH/keygen.err"
    code=$?
    [ "$code" = 1 ] || img_fail "the second keygen exited $code, not 1"
    after=$(docker run --rm --user 10001:10001 --read-only -v "$IMG_KEY":/etc/holzwave/key:ro --entrypoint sha256sum "$IMG_TAG" \
        /etc/holzwave/key/server.key)
    [ "$before" = "$after" ] || img_fail "the second keygen changed the key file"
    [ ! -s "$IMG_SCRATCH/keygen.out" ] || img_fail "the second keygen printed on standard output"
    img_ok "keygen twice: the second run exits 1 and the key file is unchanged ($(head -n 1 "$IMG_SCRATCH/keygen.err"))"
}

img_check_readonly() {
    [ "$(docker inspect -f '{{.HostConfig.ReadonlyRootfs}}' "$IMG_SERVER")" = true ] || img_fail "the root file system is not read-only"
    docker exec "$IMG_SERVER" sh -c 'grep " / " /proc/mounts | head -n 1 | grep -Eq "[ ,]ro[ ,]"' ||
        img_fail "the root mount is not read-only in /proc/mounts"
    for dir in /usr/local/bin /etc /etc/holzwave /var/tmp /opt /usr/share/doc/holzwave; do
        if docker exec "$IMG_SERVER" sh -c ": >$dir/hw-probe" 2>/dev/null; then
            img_fail "a file could be written in $dir"
        fi
    done
    for dir in /tmp /data /run/holzwave; do
        docker exec "$IMG_SERVER" sh -c ": >$dir/hw-probe && rm $dir/hw-probe" || img_fail "$dir cannot be written, but it is a volume or a tmpfs"
    done
    img_ok "read-only root: no write outside the volumes and tmpfs; /tmp, /data and /run/holzwave are writable"
}

img_check_no_setid() {
    found=$(docker run --rm --user 0:0 --entrypoint find "$IMG_TAG" / -xdev -perm /6000 -type f -print)
    [ -z "$found" ] || img_fail "the image has setuid or setgid files: $found"
    img_ok "no setuid or setgid file in the image"
}

# A person's user id (20001) must not execute the helper, and can run the server binary.
img_check_person_cannot_run_helper() {
    out=$(docker exec --user 20001:20001 "$IMG_SERVER" /usr/local/bin/holzwave-spawn-helper 2>&1)
    code=$?
    [ "$code" = 126 ] || img_fail "uid 20001 got status $code from the helper, not 126: $out"
    echo "$out" | grep -qi 'permission denied' || img_fail "uid 20001 got no permission error from the helper: $out"
    docker exec --user 20001:20001 "$IMG_SERVER" /usr/local/bin/holzwave-server >/dev/null 2>&1
    code=$?
    [ "$code" = 2 ] || img_fail "uid 20001 got status $code from the server binary, not the usage status 2"
    img_ok "uid 20001: the helper is 'permission denied' (126), the server binary runs"
}

# Writes `pid|command line|ids|capability sets` of every process of a container to a file, and judges it: no uid 0 in any of the
# four uid fields; the bounding set of process 1 and of the supervisor is the four capabilities; the supervisor holds none; the
# helper holds exactly the four as permitted. $1 the container, $2 the file, $3 the label.
img_check_procs() {
    docker exec "$1" sh -c '
for d in /proc/[0-9]*; do
    [ -r "$d/status" ] || continue
    printf "PROC %s|%s|" "${d#/proc/}" "$(tr "\0" " " <"$d/cmdline" 2>/dev/null)"
    grep -E "^(Uid|Gid|CapInh|CapPrm|CapEff|CapBnd|CapAmb):" "$d/status" | tr -s "\t " " " | tr "\n" "|"
    echo
done' >"$2" || img_fail "the process list of $1 cannot be read"
    awk -F'|' '
function fields(line, n, a, i, name) {
    for (i = 3; i <= NF; i++) {
        n = split($i, a, " ")
        if (n == 0) continue
        name = a[1]
        if (name == "Uid:") { for (j = 2; j <= 5; j++) if (a[j] == 0) printf "BAD uid0 pid %s\n", pid }
        else if (name ~ /^Cap/) cap[pid, name] = a[2]
    }
}
{
    pid = $1; sub(/^PROC /, "", pid); cmd[pid] = $2; seen++
    fields()
    if (pid == 1) init = pid
    if ($2 ~ /^\/usr\/local\/bin\/holzwave-server serve/) { server = pid; servers++ }
    if ($2 ~ /^\/usr\/local\/bin\/holzwave-spawn-helper/) { helper = pid; helpers++ }
}
END {
    if (seen < 3) print "BAD too few processes"
    if (servers != 1) print "BAD supervisors found: " servers + 0
    if (helpers != 1) print "BAD helpers found: " helpers + 0
    full = "00000000000000e1"; none = "0000000000000000"
    if (cap[init, "CapBnd:"] != full) print "BAD init CapBnd " cap[init, "CapBnd:"]
    if (cap[server, "CapBnd:"] != full) print "BAD supervisor CapBnd " cap[server, "CapBnd:"]
    if (cap[server, "CapPrm:"] != none) print "BAD supervisor CapPrm " cap[server, "CapPrm:"]
    if (cap[server, "CapEff:"] != none) print "BAD supervisor CapEff " cap[server, "CapEff:"]
    if (cap[helper, "CapPrm:"] != full) print "BAD helper CapPrm " cap[helper, "CapPrm:"]
    printf "PROCS %d processes, none with uid 0\n", seen
    printf "INIT pid %s CapBnd %s CapPrm %s CapEff %s\n", init, cap[init, "CapBnd:"], cap[init, "CapPrm:"], cap[init, "CapEff:"]
    printf "SUPERVISOR pid %s CapBnd %s CapPrm %s CapEff %s\n", server, cap[server, "CapBnd:"], cap[server, "CapPrm:"], cap[server, "CapEff:"]
    printf "HELPER pid %s CapBnd %s CapPrm %s CapEff %s\n", helper, cap[helper, "CapBnd:"], cap[helper, "CapPrm:"], cap[helper, "CapEff:"]
}' "$2" >"$2.verdict"
    if grep -q '^BAD' "$2.verdict"; then
        img_fail "$3: the process list breaks the contract: $(grep '^BAD' "$2.verdict" | tr '\n' ';')"
    fi
    uids=$(grep -h '^PROC' "$2" | sed -n 's/^PROC \([0-9]*\)|.*|Uid: \([0-9]*\) \([0-9]*\) \([0-9]*\) \([0-9]*\)|.*/\1:\2,\3,\4,\5/p' | sort -u | tr '\n' ' ')
    img_ok "$3: $(grep '^PROCS' "$2.verdict" | cut -d' ' -f2-); uid fields (pid:real,effective,saved,fs) $uids"
    img_ok "$3: $(grep '^INIT' "$2.verdict" | cut -d' ' -f2-)"
    img_ok "$3: $(grep '^SUPERVISOR' "$2.verdict" | cut -d' ' -f2-)"
    img_ok "$3: $(grep '^HELPER' "$2.verdict" | cut -d' ' -f2-)"
}

# The key, the client secret and the setup token are in no process environment, no argument list, no log, no container
# configuration and no image history. The same search is run on a file that holds the text, so that it is shown to find it.
img_check_secrets() {
    docker run --rm --user 10001:10001 --read-only -v "$IMG_KEY":/etc/holzwave/key:ro --entrypoint cat "$IMG_TAG" \
        /etc/holzwave/key/server.key >"$IMG_SCRATCH/needles" || img_fail "the key cannot be read"
    echo "$IMG_CANARY" >>"$IMG_SCRATCH/needles"
    docker logs "$IMG_SERVER" 2>/dev/null | sed -n 's/.*\(hwb_[A-Za-z0-9_-]*\).*/\1/p' >"$IMG_SCRATCH/token"
    [ -s "$IMG_SCRATCH/token" ] || img_fail "the setup token is not in the log"
    docker exec "$IMG_SERVER" sh -c 'for f in /proc/[0-9]*/environ; do tr "\0" "\n" <"$f"; done 2>/dev/null' >"$IMG_SCRATCH/environ.txt"
    docker exec "$IMG_SERVER" sh -c 'for f in /proc/[0-9]*/cmdline; do tr "\0" "\n" <"$f"; echo; done 2>/dev/null' >"$IMG_SCRATCH/cmdline.txt"
    docker logs "$IMG_SERVER" >"$IMG_SCRATCH/logs.txt" 2>&1
    docker inspect "$IMG_SERVER" >"$IMG_SCRATCH/inspect.txt"
    docker history --no-trunc "$IMG_TAG" >"$IMG_SCRATCH/history.txt"
    grep -q 'HOLZWAVE_SERVER_CONFIG=' "$IMG_SCRATCH/environ.txt" || img_fail "the environment search read nothing"
    grep -q 'holzwave-server' "$IMG_SCRATCH/cmdline.txt" || img_fail "the argument search read nothing"
    cp "$IMG_SCRATCH/needles" "$IMG_SCRATCH/control.txt"
    [ "$(grep -c -F -f "$IMG_SCRATCH/needles" "$IMG_SCRATCH/control.txt")" -ge 2 ] || img_fail "the search does not find a text that is there"
    report=
    for place in environ cmdline logs inspect history; do
        hits=$(grep -c -F -f "$IMG_SCRATCH/needles" "$IMG_SCRATCH/$place.txt")
        [ "$hits" = 0 ] || img_fail "the key or the client secret is in the $place of the container ($hits lines)"
        report="$report $place=0"
    done
    for place in environ cmdline inspect history; do
        hits=$(grep -c -F -f "$IMG_SCRATCH/token" "$IMG_SCRATCH/$place.txt")
        [ "$hits" = 0 ] || img_fail "the setup token is in the $place of the container ($hits lines)"
    done
    img_ok "secrets: the key and the client secret (and, apart from the log, the setup token) found in:$report; the search finds the text in a control file"
}

img_check_ldd() {
    for binary in holzwave-server holzwave-spawn-helper; do
        out=$(docker exec "$IMG_SERVER" ldd "/usr/local/bin/$binary" 2>&1)
        if [ $? -ne 0 ] || echo "$out" | grep -q 'not found'; then
            img_fail "ldd of $binary does not find every library: $out"
        fi
    done
    img_ok "ldd: every library of holzwave-server and holzwave-spawn-helper is found"
}

# The first start prints one line with the one-time token on standard output and nothing secret anywhere else.
img_check_token_line() {
    out=$(docker logs "$IMG_SERVER" 2>/dev/null | grep -c 'hwb_')
    err=$(docker logs "$IMG_SERVER" 2>&1 >/dev/null | grep -c 'hwb_')
    [ "$out" = 1 ] || img_fail "standard output has $out lines with a setup token, not 1"
    [ "$err" = 0 ] || img_fail "standard error has $err lines with a setup token"
    img_ok "setup token: exactly 1 line on standard output, 0 on standard error"
}

# Plain HTTP to the TLS port gets no HTTP answer (what comes back, if anything, is a TLS alert, which is not text); an unknown
# path is a bare 404.
img_check_plain_and_404() {
    out=$(img_client sh -c '
curl -sS --http0.9 --http1.1 --max-time 10 -o /tmp/plain http://'"$IMG_HOST:$IMG_PORT"'/ 2>/tmp/plain.err; echo "curl=$?"; cat /tmp/plain.err
echo "bytes=$(wc -c </tmp/plain 2>/dev/null || echo 0)"
echo "http=$(grep -c "HTTP/" /tmp/plain 2>/dev/null)"
echo "first=$(head -c 1 /tmp/plain 2>/dev/null | od -An -tx1 | tr -d " ")"
curl -s --tlsv1.3 --cacert /m/tls.crt --max-time 10 -o /tmp/b -w "status=%{http_code}\n" https://'"$IMG_HOST:$IMG_PORT"'/no-such-path
cat /tmp/b; echo
') || img_fail "the client could not run"
    code=$(echo "$out" | sed -n 's/^curl=//p')
    case "$code" in 0 | 52 | 56) ;; *) img_fail "plain HTTP to the TLS port ended with curl status $code: $out" ;; esac
    echo "$out" | grep -q '^http=0$' || img_fail "plain HTTP to the TLS port got an HTTP answer: $out"
    echo "$out" | grep -q '^status=404$' || img_fail "an unknown path did not answer 404: $out"
    [ "$(echo "$out" | tail -n 1)" = '{"code":"not_found"}' ] || img_fail "the 404 body is not the bare one: $out"
    img_ok "plain HTTP to the TLS port: curl status $code, $(echo "$out" | grep '^bytes='), first byte 0x$(echo "$out" | sed -n 's/^first=//p') (a TLS alert), no HTTP; an unknown path: 404 {\"code\":\"not_found\"}"
}

# A container without the four capabilities does not become healthy and says why; nothing answers.
img_check_fail_closed() {
    name=$IMG_RUN-nocaps
    docker volume create "$IMG_RUN-data2" >/dev/null || img_fail "a volume could not be created"
    started=$(date +%s)
    # shellcheck disable=SC2086 # IMG_CONFINED is a list of options
    docker run -d --name "$name" --network "$IMG_NET" $IMG_CONFINED --health-interval 2s --health-start-period 0s \
        -v "$IMG_RUN-data2":/data -v "$IMG_KEY":/etc/holzwave/key:ro -v "$IMG_TLS":/etc/holzwave/tls:ro \
        -v "$IMG_OIDC":/etc/holzwave/oidc/corp:ro \
        -v "$IMG_CONF":/etc/holzwave/conf:ro -e HOLZWAVE_SERVER_CONFIG=/etc/holzwave/conf/server.json \
        "$IMG_TAG" >/dev/null || img_fail "the container without capabilities did not start"
    while [ "$(docker inspect -f '{{.State.Running}}' "$name")" = true ]; do
        [ $(($(date +%s) - started)) -lt 30 ] || img_fail "the container without the four capabilities was still running after 30 seconds"
        [ "$(docker inspect -f '{{.State.Health.Status}}' "$name")" != healthy ] || img_fail "the container without the four capabilities became healthy"
        sleep 1
    done
    code=$(docker inspect -f '{{.State.ExitCode}}' "$name")
    took=$(($(date +%s) - started))
    [ "$code" != 0 ] || img_fail "the container without the four capabilities exited 0"
    docker logs "$name" >"$IMG_SCRATCH/nocaps.log" 2>&1
    grep -q 'not permitted to the helper' "$IMG_SCRATCH/nocaps.log" ||
        img_fail "the log of the container without capabilities does not say that the helper lacks them: $(cat "$IMG_SCRATCH/nocaps.log")"
    img_client sh -c "curl -sk --max-time 5 -o /dev/null https://$name:$IMG_PORT/" >/dev/null 2>&1 &&
        img_fail "something answered on the port of the container without capabilities"
    sentence=$(grep 'not permitted to the helper' "$IMG_SCRATCH/nocaps.log" | head -n 1)
    img_ok "fail closed: exit $code after $took s, never healthy, nothing answers; the log says: $sentence"
    docker rm -f "$name" >/dev/null 2>&1
}

# Runs check-config of the image's server with the volumes mounted the way the server container has them. $1 the file in the
# configuration volume, the rest are more options of docker run. Sets IMG_CC_CODE and IMG_CC_OUT.
img_check_config() {
    file=$1
    shift
    # shellcheck disable=SC2086 # IMG_CONFINED is a list of options
    IMG_CC_OUT=$(docker run --rm $IMG_CONFINED -v "$IMG_KEY":/etc/holzwave/key:ro -v "$IMG_TLS":/etc/holzwave/tls:ro \
        -v "$IMG_OIDC":/etc/holzwave/oidc/corp:ro -v "${IMG_CC_CONF:-$IMG_CONF}":/etc/holzwave/conf:ro \
        -e "HOLZWAVE_SERVER_CONFIG=/etc/holzwave/conf/$file" "$@" "$IMG_TAG" check-config 2>&1)
    IMG_CC_CODE=$?
}

# A release build has no in-process workers: the key is an unknown field.
img_check_release_build() {
    img_check_config in-process.json
    [ "$IMG_CC_CODE" = 1 ] || img_fail "check-config with workers.mode exited $IMG_CC_CODE, not 1: $IMG_CC_OUT"
    echo "$IMG_CC_OUT" | grep -q 'unknown field' || img_fail "the workers.mode refusal is not an unknown-field refusal: $IMG_CC_OUT"
    img_ok "release build: workers.mode is refused (exit 1): $IMG_CC_OUT"
}

# The examples are judged by the real server.
img_check_examples() {
    img_check_config files.json
    [ "$IMG_CC_CODE" = 0 ] && [ "$IMG_CC_OUT" = "configuration ok" ] || img_fail "check-config of the files example: $IMG_CC_OUT"
    img_ok "example files: check-config says '$IMG_CC_OUT'"
    img_check_config acme.json
    [ "$IMG_CC_CODE" = 1 ] || img_fail "check-config of the ACME example as shipped exited $IMG_CC_CODE"
    echo "$IMG_CC_OUT" | grep -q 'terms_agreed' || img_fail "the ACME example as shipped is not refused for the agreement: $IMG_CC_OUT"
    img_ok "example ACME as shipped: refused (exit 1): $IMG_CC_OUT"
    img_check_config acme-agreed.json
    [ "$IMG_CC_CODE" = 0 ] && [ "$IMG_CC_OUT" = "configuration ok" ] || img_fail "check-config of the ACME example with the agreement set: $IMG_CC_OUT"
    img_ok "example ACME with terms_agreed set by the test: check-config says '$IMG_CC_OUT'"
}

# The layout of a key mounted from a Kubernetes secret: owned by root, group 10001, mode 0440.
img_check_mounted_key() {
    k8s=$IMG_RUN-k8skey
    docker volume create "$k8s" >/dev/null || img_fail "a volume could not be created"
    docker run --rm --user 0:0 -v "$IMG_KEY":/src:ro -v "$k8s":/dst "$BASE_IMAGE" sh -ec '
install -d -m 0755 /dst
install -m 0440 -o 0 -g 10001 /src/server.key /dst/server.key
ls -ln /dst/server.key' >"$IMG_SCRATCH/k8s.ls" || img_fail "the mounted key layout could not be made"
    img_check_config files.json -v "$k8s":/etc/holzwave/k8skey:ro -e HOLZWAVE_SERVER_KEY_FILE=/etc/holzwave/k8skey/server.key
    [ "$IMG_CC_CODE" = 0 ] && [ "$IMG_CC_OUT" = "configuration ok" ] || img_fail "a key owned by root with group 10001 and mode 0440 is refused: $IMG_CC_OUT"
    img_ok "a mounted key (root:10001, 0440) is accepted by check-config as uid 10001: $(cut -d' ' -f1,3,4 "$IMG_SCRATCH/k8s.ls")"
}

# A restart keeps the volume and the server comes back healthy and within the contract.
img_check_persistence() {
    docker exec "$IMG_SERVER" sh -c 'echo marker-from-before-the-restart >/data/hw-marker'
    docker exec "$IMG_SERVER" sh -c 'cd /data && find . -type f | sort' >"$IMG_SCRATCH/files.before"
    [ -s "$IMG_SCRATCH/files.before" ] || img_fail "the data volume holds no file before the restart"
    docker restart "$IMG_SERVER" >/dev/null || img_fail "the container did not restart"
    img_wait_healthy
    [ "$(docker exec "$IMG_SERVER" cat /data/hw-marker)" = marker-from-before-the-restart ] || img_fail "the marker file did not survive the restart"
    docker exec "$IMG_SERVER" sh -c 'cd /data && find . -type f | sort' >"$IMG_SCRATCH/files.after"
    lost=$(comm -23 "$IMG_SCRATCH/files.before" "$IMG_SCRATCH/files.after")
    [ -z "$lost" ] || img_fail "the restart ended volume files: $lost"
    img_tls_answer
    img_check_procs "$IMG_SERVER" "$IMG_SCRATCH/procs.after" "after the restart"
    img_ok "persistence: the marker and $(wc -l <"$IMG_SCRATCH/files.before" | tr -d ' ') files of /data survived docker restart ($(tr '\n' ' ' <"$IMG_SCRATCH/files.before")); healthy again"
}

# What a person's user id sees of the data volume.
img_check_person_view() {
    docker exec --user 20001:20001 "$IMG_SERVER" sh -c 'id; ls -ld /data /data/homes' >"$IMG_SCRATCH/person.txt" 2>&1 || img_fail "uid 20001 cannot run id and ls"
    docker exec --user 20001:20001 "$IMG_SERVER" sh -c 'test -x /data && test -x /data/homes' || img_fail "uid 20001 cannot walk through /data and /data/homes"
    if docker exec --user 20001:20001 "$IMG_SERVER" sh -c 'ls /data >/dev/null 2>&1 || ls /data/homes >/dev/null 2>&1'; then
        img_fail "uid 20001 can list /data or /data/homes"
    fi
    entries=$(docker exec "$IMG_SERVER" ls /data)
    [ "$(echo "$entries" | grep -vc '^homes$')" -ge 1 ] || img_fail "the data volume holds nothing but the homes: the server has made no file of its own"
    for entry in $entries; do
        if docker exec --user 20001:20001 "$IMG_SERVER" sh -c "ls /data/$entry >/dev/null 2>&1 || cat /data/$entry >/dev/null 2>&1"; then
            img_fail "uid 20001 can list or read /data/$entry"
        fi
    done
    img_ok "person view (uid 20001): $(tr '\n' ';' <"$IMG_SCRATCH/person.txt"); can walk through /data and /data/homes, cannot list or read any of: $(echo "$entries" | tr '\n' ' ')"
}

# The Compose example: valid, judged by the checker, and brought up for real with an override that points the configuration and
# the certificates at the scratch area and the image at the test tag.
img_compose() {
    docker compose -f "$IMG_COMPOSE_FILE" config -q || img_fail "docker compose config of the shipped file fails"
    img_ok "docker compose config -q passes with defaults for every variable"
    docker compose --profile setup -f "$IMG_COMPOSE_FILE" config --format json >"$IMG_SCRATCH/compose.json" || img_fail "docker compose config --format json fails"
    node "$here/web-deploy-check.mjs" compose <"$IMG_SCRATCH/compose.json" || img_fail "the Compose file breaks the deployment contract"
    img_ok "web-deploy-check.mjs compose: no violation"

    project=$IMG_RUN-compose
    mkdir -p "$IMG_SCRATCH/compose"
    cp "$IMG_SCRATCH/conf/files.json" "$IMG_SCRATCH/compose/server.json"
    chmod 0755 "$IMG_SCRATCH/compose"
    chmod 0644 "$IMG_SCRATCH/compose/server.json"
    cat >"$IMG_SCRATCH/compose/override.yml" <<OVERRIDE
services:
  holzwave:
    image: $IMG_TAG
    pull_policy: never
    volumes:
      - $IMG_SCRATCH/compose/server.json:/etc/holzwave/server.json:ro
      - tls:/etc/holzwave/tls:ro
    networks:
      default:
        aliases:
          - $IMG_HOST
    healthcheck:
      interval: 2s
      start_period: 0s
  keygen:
    image: $IMG_TAG
    pull_policy: never
volumes:
  tls:
    external: true
    name: $IMG_TLS
OVERRIDE
    port=$((20000 + $$ % 20000))
    compose="docker compose -p $project -f $IMG_COMPOSE_FILE -f $IMG_SCRATCH/compose/override.yml"
    # shellcheck disable=SC2086 # compose is a command with options
    HOLZWAVE_PORT=$port $compose config -q || img_fail "docker compose config with the test override fails"
    # shellcheck disable=SC2086
    HOLZWAVE_PORT=$port $compose --profile setup run --rm keygen >"$IMG_SCRATCH/compose/keygen.log" 2>&1 ||
        { cat "$IMG_SCRATCH/compose/keygen.log" >&2; img_fail "docker compose run keygen failed"; }
    shape=$(docker run --rm --user 10001:10001 --read-only -v "$project"_key:/etc/holzwave/key:ro --entrypoint stat "$IMG_TAG" \
        -c '%a %u:%g' /etc/holzwave/key/server.key) || img_fail "the key of the Compose project cannot be read"
    [ "$shape" = "600 10001:10001" ] || img_fail "the key of the Compose project has mode and owner '$shape'"
    img_ok "compose keygen: the key volume holds the key with mode and owner $shape without any command on the host"
    started=$(date +%s)
    # shellcheck disable=SC2086
    HOLZWAVE_PORT=$port $compose up -d --wait --wait-timeout 120 >"$IMG_SCRATCH/compose/up.log" 2>&1 ||
        { cat "$IMG_SCRATCH/compose/up.log" >&2; img_fail "docker compose up --wait did not bring the server up healthy"; }
    healthy=$(($(date +%s) - started))
    # shellcheck disable=SC2086
    container=$($compose ps -q holzwave)
    [ -n "$container" ] || img_fail "the Compose project has no server container"
    img_ok "compose up --wait: healthy after $healthy s"
    published=$(docker inspect -f '{{json .NetworkSettings.Ports}}' "$container" | img_jq -r 'keys | join(",")')
    [ "$published" = "8443/tcp" ] || img_fail "the Compose server publishes '$published', not only 8443/tcp"
    img_ok "compose: the only port is $published"
    save_net=$IMG_NET save_server=$IMG_SERVER
    IMG_NET=${project}_default IMG_SERVER=$(docker inspect -f '{{.Name}}' "$container" | tr -d '/')
    img_tls_answer
    img_check_procs "$container" "$IMG_SCRATCH/procs.compose" "compose"
    IMG_NET=$save_net IMG_SERVER=$save_server
    # shellcheck disable=SC2086
    HOLZWAVE_PORT=$port $compose down -v >/dev/null 2>&1 || img_fail "docker compose down -v failed"
    left=$(docker ps -a -q --filter "name=$project"; docker volume ls -q --filter "name=$project"; docker network ls -q --filter "name=$project")
    [ -z "$left" ] || img_fail "docker compose down -v left something behind: $left"
    img_ok "compose down -v leaves no container, volume or network"
}

# ---- the chart ----

# The Helm chart: lint, render for the three TLS modes, judged by the same checker as the Compose file, the configuration it
# renders judged by the real server, and the refusals of the schema and the templates.
img_chart() {
    for tool in helm kubectl node; do
        command -v "$tool" >/dev/null 2>&1 || {
            echo "web-container-test: $tool is missing" >&2
            exit 2
        }
    done
    chart=$root/packaging/helm/holzwave
    mkdir -p "$IMG_SCRATCH/chart/conf"
    app=$(sed -n 's/^appVersion: "\(.*\)"$/\1/p' "$chart/Chart.yaml")
    [ "$app" = "$(img_version)" ] || img_fail "appVersion '$app' of the chart is not the workspace version '$(img_version)'"
    img_ok "chart appVersion $app is the workspace version"
    for set in files acme proxy; do
        values=$chart/ci/$set-values.yaml
        out=$(helm lint --strict "$chart" -f "$values" 2>&1) || img_fail "helm lint --strict ($set) failed: $out"
        lint=$(echo "$out" | tail -n 1)
        helm template holzwave "$chart" -f "$values" >"$IMG_SCRATCH/chart/$set.yaml" || img_fail "helm template ($set) failed"
        KUBECONFIG=/dev/null kubectl annotate --local -f "$IMG_SCRATCH/chart/$set.yaml" -o json holzwave.example/checked=yes \
            >"$IMG_SCRATCH/chart/$set.json" || img_fail "kubectl could not convert the render of $set"
        node "$here/web-deploy-check.mjs" chart <"$IMG_SCRATCH/chart/$set.json" || img_fail "the render of $set breaks the deployment contract"
        [ "$(grep -c '^kind: Secret$' "$IMG_SCRATCH/chart/$set.yaml")" = 0 ] || img_fail "the chart renders a Secret ($set)"
        [ "$(grep -c '^kind: Deployment$' "$IMG_SCRATCH/chart/$set.yaml")" = 1 ] || img_fail "the chart does not render one Deployment ($set)"
        img_jq -r 'select(.kind == "ConfigMap") | .data["server.json"]' <"$IMG_SCRATCH/chart/$set.json" >"$IMG_SCRATCH/chart/conf/$set.json" ||
            img_fail "the configuration of $set cannot be read from the render"
        img_ok "chart $set: helm lint --strict ($lint); helm template; web-deploy-check.mjs chart: no violation; no Secret; one Deployment"
    done
    ingresses=$(grep -c '^kind: Ingress$' "$IMG_SCRATCH/chart/proxy.yaml")
    [ "$ingresses" = 1 ] || img_fail "the proxy render has $ingresses Ingress objects, not 1"
    for set in files acme; do
        [ "$(grep -c '^kind: Ingress$' "$IMG_SCRATCH/chart/$set.yaml")" = 0 ] || img_fail "the $set render has an Ingress"
    done
    helm template holzwave "$chart" -f "$chart/ci/files-values.yaml" --set ingress.enabled=true >"$IMG_SCRATCH/chart/files-ingress.yaml" ||
        img_fail "helm template with the Ingress enabled in the files mode failed"
    [ "$(grep -c '^kind: Ingress$' "$IMG_SCRATCH/chart/files-ingress.yaml")" = 0 ] || img_fail "an Ingress was rendered in the files mode"
    img_ok "ingress: rendered in the proxy mode only (proxy 1, files 0 even when enabled, acme 0)"

    # The configuration each render holds, judged by the real server with the files mounted where the chart puts them.
    chartconf=$IMG_RUN-chartconf
    docker volume create "$chartconf" >/dev/null || img_fail "a volume could not be created"
    COPYFILE_DISABLE=1 tar -c --no-xattrs -C "$IMG_SCRATCH/chart/conf" . |
        docker run --rm -i --user 0:0 -v "$chartconf":/m "$BASE_IMAGE" sh -ec 'tar -x --no-same-owner -C /m; chmod 0755 /m; chmod 0644 /m/*' ||
        img_fail "the rendered configurations could not be written"
    IMG_CC_CONF=$chartconf
    for set in files acme proxy; do
        img_check_config "$set.json"
        [ "$IMG_CC_CODE" = 0 ] && [ "$IMG_CC_OUT" = "configuration ok" ] || img_fail "check-config of the configuration rendered for $set: $IMG_CC_OUT"
        img_ok "chart $set: the rendered server.json passes check-config of the image: '$IMG_CC_OUT'"
    done
    IMG_CC_CONF=

    img_reject "replicaCount=2" replicaCount --set replicaCount=2
    img_reject "a securityContext block" securityContext --set securityContext.runAsUser=0
    img_reject "an unknown top-level key" bogus --set bogus=1
    img_reject "config.tls.mode=ssh" mode --set config.tls.mode=ssh
    img_reject "a missing serverKey.existingSecret" existingSecret --set serverKey.existingSecret=null
    img_reject "a missing config.public_url" public_url --set config.public_url=null
    img_reject "image.tag=latest" "image/tag" --set image.tag=latest
    img_reject "a chart-owned key (config.data_dir)" data_dir --set config.data_dir=/elsewhere
    out=$(helm template holzwave "$chart" -f "$chart/ci/proxy-values.yaml" --set-json 'config.tls.trusted_proxies=[]' 2>&1) &&
        img_fail "the proxy mode rendered with an empty list of trusted proxies"
    echo "$out" | grep -q 'trusted_proxies is required' || img_fail "the proxy refusal does not name trusted_proxies: $out"
    img_ok "template: the proxy mode without trusted proxies is refused: $(echo "$out" | tr '\n' ' ' | cut -c1-200)"
}

# One refusal of the schema: $1 what, $2 a word the message must hold, the rest are options of helm template.
img_reject() {
    label=$1 word=$2
    shift 2
    chart=$root/packaging/helm/holzwave
    if out=$(helm template holzwave "$chart" -f "$chart/ci/files-values.yaml" "$@" 2>&1); then
        img_fail "the chart accepted $label"
    fi
    echo "$out" | grep -q "$word" || img_fail "the refusal of $label does not name '$word': $out"
    img_ok "schema: $label is refused: $(echo "$out" | tr '\n' ' ' | cut -c1-220)"
}

mode_image() {
    sh "$here/linux-container-test.sh" image || {
        echo "web-container-test: the Linux test image $BASE_IMAGE could not be built" >&2
        exit 1
    }
    trap img_cleanup EXIT INT TERM
    img_cleanup
    IMG_SCRATCH=$(mktemp -d "${TMPDIR:-/tmp}/holzwave-web-image.XXXXXX") || exit 2
    img_build
    img_check_image_config
    img_materials
    img_check_no_setid
    img_start
    img_wait_healthy
    img_tls_answer
    img_keygen_twice
    img_check_readonly
    img_check_person_cannot_run_helper
    img_check_procs "$IMG_SERVER" "$IMG_SCRATCH/procs.before" "the server container"
    img_check_secrets
    img_check_ldd
    img_check_token_line
    img_check_plain_and_404
    img_check_fail_closed
    img_check_release_build
    img_check_examples
    img_check_mounted_key
    img_check_person_view
    img_check_persistence
    img_compose
    img_chart
}

# ---- multiarch ----

# The image for both architectures, one platform at a time (`--load` takes one): what the machine says it is, the libraries
# found, a key made, and the real server's check of the example configuration with a generated certificate. A platform that
# cannot be built or run here is NOT RUN with the reason, and the mode exits 1.
mode_multiarch() {
    docker buildx version >/dev/null 2>&1 || {
        echo "web-container-test: docker buildx is missing" >&2
        exit 2
    }
    sh "$here/linux-container-test.sh" image || {
        echo "web-container-test: the Linux test image $BASE_IMAGE could not be built" >&2
        exit 1
    }
    trap img_cleanup EXIT INT TERM
    img_cleanup
    IMG_SCRATCH=$(mktemp -d "${TMPDIR:-/tmp}/holzwave-web-image.XXXXXX") || exit 2
    docker volume create "$IMG_TLS" >/dev/null && docker volume create "$IMG_CONF" >/dev/null || img_fail "a volume could not be created"
    img_make_certificate
    mkdir -p "$IMG_SCRATCH/conf"
    img_replace "$root/packaging/compose/server.files.example.json" "$IMG_SCRATCH/conf/files.json" "https://holzwave.example.net" "https://$IMG_HOST:$IMG_PORT"
    COPYFILE_DISABLE=1 tar -c --no-xattrs -C "$IMG_SCRATCH/conf" . |
        docker run --rm -i --user 0:0 -v "$IMG_CONF":/m "$BASE_IMAGE" sh -ec 'tar -x --no-same-owner -C /m; chmod 0755 /m; chmod 0644 /m/*' ||
        img_fail "the configuration could not be written"
    version=$(img_version)
    revision=$(git -C "$root" rev-parse HEAD 2>/dev/null || echo unknown)
    status=0
    for platform in linux/amd64 linux/arm64; do
        arch=${platform#linux/}
        case "$arch" in amd64) machine=x86_64 ;; arm64) machine=aarch64 ;; esac
        tag=holzwave-server-multiarch:$arch
        echo "+ docker buildx build --platform $platform --load -t $tag -f packaging/docker/Dockerfile $IMG_CONTEXT"
        # shellcheck disable=SC2086 # IMG_ZIG_ARG is empty or one option
        if ! docker buildx build --platform "$platform" --load -t "$tag" -f "$IMG_CONTEXT/packaging/docker/Dockerfile" $IMG_ZIG_ARG \
            --build-arg "VERSION=$version" --build-arg "REVISION=$revision" "$IMG_CONTEXT" >"$IMG_SCRATCH/build-$arch.log" 2>&1; then
            echo "multiarch: NOT RUN $platform: the image could not be built: $(tail -n 3 "$IMG_SCRATCH/build-$arch.log" | tr '\n' ' ')"
            status=1
            continue
        fi
        got=$(docker run --rm --platform "$platform" --entrypoint uname "$tag" -m 2>&1)
        if [ "$got" != "$machine" ]; then
            echo "multiarch: NOT RUN $platform: the container reports '$got', not $machine (the machine cannot run this platform)"
            status=1
            continue
        fi
        libs=$(docker run --rm --platform "$platform" --entrypoint ldd "$tag" /usr/local/bin/holzwave-server 2>&1)
        if [ $? -ne 0 ] || echo "$libs" | grep -q 'not found'; then
            echo "multiarch: FAILED $platform: ldd of the server: $libs"
            status=1
            continue
        fi
        # shellcheck disable=SC2086 # IMG_CONFINED is a list of options
        out=$(docker run --rm --platform "$platform" $IMG_CONFINED --tmpfs /etc/holzwave/key:rw,nosuid,nodev,noexec,uid=10001,gid=10001,mode=0700 \
            -v "$IMG_TLS":/etc/holzwave/tls:ro -v "$IMG_CONF":/etc/holzwave/conf:ro -e HOLZWAVE_SERVER_CONFIG=/etc/holzwave/conf/files.json \
            --entrypoint sh "$tag" -ec '/usr/local/bin/holzwave-server keygen && /usr/local/bin/holzwave-server check-config' 2>&1)
        if [ $? -ne 0 ] || ! echo "$out" | grep -q '^configuration ok$'; then
            echo "multiarch: FAILED $platform: keygen and check-config: $out"
            status=1
            continue
        fi
        img_ok "multiarch $platform: uname -m $got; ldd finds every library; keygen made a key; check-config says 'configuration ok'"
    done
    [ "$status" = 0 ] || exit 1
}

# ---- dispatch -----------------------------------------------------------------------------------------------------

case "$mode" in
    caps) mode_caps ;;
    acme) mode_acme ;;
    isolation) mode_isolation ;;
    image) mode_image ;;
    multiarch) mode_multiarch ;;
    all)
        mode_caps
        mode_acme
        mode_isolation
        mode_image
        mode_multiarch
        ;;
esac
echo "web-container-test: ok"
