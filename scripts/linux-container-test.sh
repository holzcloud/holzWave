#!/bin/sh
# The test suite on Linux, from the Mac, in a Docker container.
#
#   sh scripts/linux-container-test.sh [core|app|all] [-- <arguments for the test binaries>]    (default: core)
#   sh scripts/linux-container-test.sh image
#
# Modes: `core` runs `cargo test -p holzwave-core`, `app` runs `cargo test -p holzwave-app` (the app crate needs a
# built page: `npm --prefix app run build` runs first), `all` runs the whole workspace. Everything after `--` goes to
# the test binaries, for example `-- --test-threads=1` or a test name.
#
# The container is `ubuntu:26.04`, which is arm64 on an Apple Silicon Mac: a proxy for x86_64 (the same kernel API
# and the same code paths, not the same CPU). The first run builds the image `holzwave-linux-test` (build tools,
# GTK and WebKitGTK headers, rustup stable and Zig 0.16.0); remove it with `docker rmi holzwave-linux-test` to get
# a newer Rust. `image` only makes sure the image exists and runs no test: scripts/linux-container-bundle.sh builds on it. Named volumes keep the cargo registry, the git checkouts and the target directory between runs, so
# a second run does not start from nothing: `docker volume rm holzwave-linux-target` forgets the build cache.
#
# The working tree is streamed into the container with tar (without target, .git, node_modules and .planning), so
# the run sees the files as they are on disk, committed or not.
#
# Exit status: 0 when the tests ran and passed, 1 when a test failed or no test ran, 2 when a tool is missing
# (Docker, or npm for the app modes). A missing tool is a failed gate, never a skip.

ZIG_VERSION=0.16.0
BASE_IMAGE=ubuntu:26.04

# HOLZWAVE_LINUX_ARCH=amd64 runs everything as linux/amd64 (on an Apple Silicon Mac Docker Desktop emulates it, with
# Rosetta when that is enabled): the image is holzwave-linux-test-amd64 and the target volume holzwave-linux-target-amd64,
# so the two architectures never share a build cache. scripts/release-linux-container.sh sets it. Unset: the native
# architecture, exactly as before. The amd64 image is Ubuntu 24.04, not 26.04: under Rosetta the glibc 2.43 of 26.04
# fails to create files (GNU tar: "Cannot open: Function not implemented"), and the older glibc 2.39 lets the packages
# run on more distributions (a package needs at least the glibc it was built with).
PLATFORM=
SUFFIX=
case "${HOLZWAVE_LINUX_ARCH:-}" in
    "") ;;
    amd64)
        PLATFORM=linux/amd64
        SUFFIX=-amd64
        BASE_IMAGE=ubuntu:24.04
        ;;
    *)
        echo "linux-container-test: HOLZWAVE_LINUX_ARCH must be unset or amd64" >&2
        exit 2
        ;;
esac
IMAGE=holzwave-linux-test$SUFFIX

here=$(cd "$(dirname "$0")" && pwd)
root=$(dirname "$here")

mode=core
case "${1:-}" in
    core | app | all | image)
        mode=$1
        shift
        ;;
esac
case "${1:-}" in
    "") ;;
    --) shift ;;
    *)
        echo "usage: sh scripts/linux-container-test.sh [core|app|all|image] [-- <arguments for the test binaries>]" >&2
        exit 2
        ;;
esac

if ! command -v docker >/dev/null 2>&1; then
    echo "linux-container-test: docker is missing: install Docker Desktop" >&2
    exit 2
fi
if ! docker info >/dev/null 2>&1; then
    echo "linux-container-test: the Docker daemon does not answer: start Docker Desktop" >&2
    exit 2
fi

# The image of the Linux runs, built once. Plan 06 builds on the same image.
#
# The Zig tarball is checked against the sha256 of ziglang.org's published index before it is unpacked, and
# rustup comes from its official installer over TLS 1.2 or newer. Nothing downloaded here enters the repository.
build_image() {
    echo "+ docker build -t $IMAGE (Dockerfile from stdin)"
    docker build ${PLATFORM:+--platform "$PLATFORM"} -t "$IMAGE" --build-arg "BASE_IMAGE=$BASE_IMAGE" --build-arg "ZIG_VERSION=$ZIG_VERSION" - <<'DOCKERFILE'
ARG BASE_IMAGE
FROM ${BASE_IMAGE}
ARG ZIG_VERSION
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update \
    && apt-get install -y --no-install-recommends build-essential ca-certificates curl git jq pkg-config xz-utils \
        libwebkit2gtk-4.1-dev libgtk-3-dev \
    && rm -rf /var/lib/apt/lists/*
RUN set -eu; \
    case "$(uname -m)" in aarch64) key=aarch64-linux ;; x86_64) key=x86_64-linux ;; \
        *) echo "unsupported architecture $(uname -m)" >&2; exit 1 ;; esac; \
    index=$(curl --proto '=https' --tlsv1.2 -fsSL https://ziglang.org/download/index.json); \
    url=$(printf '%s' "$index" | jq -r --arg v "$ZIG_VERSION" --arg k "$key" '.[$v][$k].tarball'); \
    sha=$(printf '%s' "$index" | jq -r --arg v "$ZIG_VERSION" --arg k "$key" '.[$v][$k].shasum'); \
    case "$url" in https://ziglang.org/download/*) ;; *) echo "unexpected Zig url: $url" >&2; exit 1 ;; esac; \
    case "$sha" in *[!0-9a-f]* | "") echo "unexpected Zig checksum: $sha" >&2; exit 1 ;; esac; \
    curl --proto '=https' --tlsv1.2 -fsSL -o /tmp/zig.tar.xz "$url"; \
    echo "$sha  /tmp/zig.tar.xz" | sha256sum -c -; \
    mkdir /opt/zig; \
    tar -xJf /tmp/zig.tar.xz -C /opt/zig --strip-components=1; \
    rm /tmp/zig.tar.xz; \
    ln -s /opt/zig/zig /usr/local/bin/zig; \
    test "$(zig version)" = "$ZIG_VERSION"
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain stable
ENV PATH=/root/.cargo/bin:${PATH}
WORKDIR /work
DOCKERFILE
}

# The working tree as a tar stream. COPYFILE_DISABLE keeps macOS from adding ._ files.
tree_stream() {
    COPYFILE_DISABLE=1 tar -c -C "$root" --exclude target --exclude .git --exclude node_modules --exclude .planning .
}

# Run `cargo test` with the given arguments in a container and check what it printed.
run_tests() {
    log=$(mktemp "${TMPDIR:-/tmp}/holzwave-linux-test.XXXXXX") || exit 2
    code=$(mktemp "${TMPDIR:-/tmp}/holzwave-linux-test-code.XXXXXX") || exit 2
    echo "+ docker run $IMAGE cargo test --locked --no-fail-fast $*"
    tree_stream | {
        docker run --rm -i ${PLATFORM:+--platform "$PLATFORM"} \
            -v holzwave-linux-cargo-registry:/root/.cargo/registry \
            -v holzwave-linux-cargo-git:/root/.cargo/git \
            -v holzwave-linux-target$SUFFIX:/work/target \
            -e CARGO_TERM_COLOR=never \
            "$IMAGE" \
            sh -c 'tar -x --no-same-owner -C /work && exec "$@"' sh cargo test --locked --no-fail-fast "$@"
        echo $? >"$code"
    } 2>&1 | tee "$log"

    echo
    echo "linux-container-test: per-binary counts (passed, failed, ignored)"
    awk '
        /Running / { name = $2 }
        /Doc-tests/ { name = "doc-tests " $2 }
        /^test result:/ { gsub(/;/, ""); printf "  %-60s %5d %3d %3d\n", name, $4, $6, $8 }
    ' "$log"

    status=$(cat "$code")
    results=$(grep -c '^test result: ok' "$log")
    failures=$(grep -c '^test result: FAILED' "$log")
    rm -f "$log" "$code"
    if [ "$status" != 0 ] || [ "$failures" != 0 ]; then
        echo "linux-container-test: FAILED (docker exit $status, $failures failing test binaries)" >&2
        exit 1
    fi
    if [ "$results" = 0 ]; then
        echo "linux-container-test: FAILED: no test binary reported a result" >&2
        exit 1
    fi
    echo "linux-container-test: ok ($results test binaries)"
}

if ! docker image inspect "$IMAGE" >/dev/null 2>&1; then
    build_image || {
        echo "linux-container-test: the image build failed" >&2
        exit 1
    }
fi

if [ "$mode" = image ]; then
    echo "linux-container-test: the image $IMAGE is there"
    exit 0
fi

case "$mode" in
    core) package="-p holzwave-core" ;;
    app) package="-p holzwave-app" ;;
    all) package="--workspace" ;;
esac

if [ "$mode" != core ]; then
    if ! command -v npm >/dev/null 2>&1; then
        echo "linux-container-test: npm is missing: the app crate needs a built page (app/dist)" >&2
        exit 2
    fi
    echo "+ npm --prefix app run build"
    (cd "$root" && npm --prefix app run build) || exit 1
fi

if [ $# -gt 0 ]; then
    # shellcheck disable=SC2086  # $package is one or two words by construction
    run_tests $package -- "$@"
else
    # shellcheck disable=SC2086
    run_tests $package
fi
