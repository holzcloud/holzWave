#!/bin/sh
# The test suite on Linux, from the Mac, in a Docker container.
#
#   sh scripts/linux-container-test.sh [core|app|server|all] [-- <arguments for the test binaries>]    (default: core)
#   sh scripts/linux-container-test.sh image
#   sh scripts/linux-container-test.sh perf
#
# Modes: `core` runs `cargo test -p holzwave-core`, `app` runs `cargo test -p holzwave-app` (the app crate needs a
# built page: `npm --prefix app run build` runs first), `server` runs `cargo test` for the packages of the browser build
# (`holzwave-host`, `holzwave-server` and `holzwave-spawn-helper`; the server serves the built page, so the page is
# built first as for `app`), `all` runs the whole workspace. Everything after `--` goes to
# the test binaries, for example `-- --test-threads=1` or a test name.
#
# `perf` runs no test suite: it runs the two ignored release measurements, `holzwave-core --test perf` and
# `holzwave-server --test perf` (the reference of the server, D-16; `sh scripts/perf-check.sh container` and
# `perf-record.sh container` are its users), after the page has been built. It prints one `PERF-MACHINE {json}` line
# (`uname`, the CPU model of /proc/cpuinfo or `uname -m` where the kernel names none, and the core count) and then the
# `PERF` lines, and keeps everything in `target/perf-container.log` of the working tree. It takes no arguments after `--`.
#
# The container is `ubuntu:26.04`, which is arm64 on an Apple Silicon Mac: a proxy for x86_64 (the same kernel API
# and the same code paths, not the same CPU). The first run builds the image `holzwave-linux-test` (build tools,
# GTK and WebKitGTK headers, rustup stable and Zig 0.17.0); remove it with `docker rmi holzwave-linux-test` to get
# a newer Rust. `image` only makes sure the image exists and runs no test: scripts/linux-container-bundle.sh builds on it. Named volumes keep the cargo registry, the git checkouts and the target directory between runs, so
# a second run does not start from nothing: `docker volume rm holzwave-linux-target` forgets the build cache.
#
# The working tree is streamed into the container with tar (without target, node_modules and .planning), so the run
# sees the files as they are on disk, committed or not. `.git` goes in without its objects, config, hooks and logs: HEAD,
# the index and the refs are enough for `git ls-files`, which one test asks for (no tracked secret key), and none of the
# history, no remote URL, no hook script and no reflog identity enters a container that builds fetched dependencies.
# An empty objects directory is made in the container, because git takes a directory for a repository only with one. In a
# linked worktree `.git` is a file that points outside the tree: the listing test then fails with git's own message, and
# it fails, never skips, when there is no repository at all.
#
# Exit status: 0 when the tests ran and passed, 1 when a test failed or no test ran, 2 when a tool is missing
# (Docker, or npm for the app modes). A missing tool is a failed gate, never a skip.

ZIG_VERSION=0.17.0
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
    core | app | server | all | image | perf)
        mode=$1
        shift
        ;;
esac
case "${1:-}" in
    "") ;;
    --) shift ;;
    *)
        echo "usage: sh scripts/linux-container-test.sh [core|app|server|all|image|perf] [-- <arguments for the test binaries>]" >&2
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
LABEL ch.holzcloud.holzwave.zig="${ZIG_VERSION}"
WORKDIR /work
DOCKERFILE
}

# The working tree as a tar stream. COPYFILE_DISABLE keeps macOS from adding ._ files. Of .git only HEAD, the index, the
# refs and a few small marker files go in: not the objects (the history), not config (a remote URL, a token), not hooks
# (scripts), not logs (reflog identities) and not FETCH_HEAD (the URL of the last fetch). That is what `git ls-files` needs.
tree_stream() {
    COPYFILE_DISABLE=1 tar -c -C "$root" --exclude target --exclude .git/objects --exclude .git/config --exclude .git/hooks \
        --exclude .git/logs --exclude .git/FETCH_HEAD --exclude node_modules --exclude .planning .
}

# Run `cargo test` with the given arguments in a container and check what it printed.
run_tests() {
    log=$(mktemp "${TMPDIR:-/tmp}/holzwave-linux-test.XXXXXX") || exit 2
    code=$(mktemp "${TMPDIR:-/tmp}/holzwave-linux-test-code.XXXXXX") || exit 2
    echo "+ docker run $IMAGE cargo test --locked --no-fail-fast $*"
    # --init: the container's first process reaps orphans, as init does on a real machine. Without it a killed child that
    # was orphaned stays a zombie and still answers `kill -0`, which the runner tests read as "not gone".
    tree_stream | {
        docker run --rm -i --init ${PLATFORM:+--platform "$PLATFORM"} \
            -v holzwave-linux-cargo-registry:/root/.cargo/registry \
            -v holzwave-linux-cargo-git:/root/.cargo/git \
            -v holzwave-linux-target$SUFFIX:/work/target \
            -e CARGO_TERM_COLOR=never \
            ${CARGO_BUILD_JOBS:+-e CARGO_BUILD_JOBS="$CARGO_BUILD_JOBS"} \
            "$IMAGE" \
            sh -c 'tar -x --no-same-owner -C /work && { [ ! -d /work/.git ] || mkdir -p /work/.git/objects; } && exec "$@"' sh cargo test --locked --no-fail-fast "$@"
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

# The performance measurements in a container (the `perf` mode): the page is built on the Mac first (the server tests embed it),
# then the working tree is streamed in as for the tests and the two ignored release tests run, one after the other. The
# whole output, the PERF-MACHINE line first, goes to target/perf-container.log of the working tree (a tee to a file under
# $root/target, so that nothing but the working tree is written on the Mac). Exit status: 1 when a test binary failed.
run_perf() {
    mkdir -p "$root/target"
    perf_log="$root/target/perf-container.log"
    : >"$perf_log" || exit 2
    code=$(mktemp "${TMPDIR:-/tmp}/holzwave-linux-test-code.XXXXXX") || exit 2
    echo "+ docker run $IMAGE cargo test --locked --release -p holzwave-core --test perf, then -p holzwave-server --test perf"
    # The same image, --init and volumes as run_tests: no mount of the Mac's home, no added capability.
    tree_stream | {
        docker run --rm -i --init ${PLATFORM:+--platform "$PLATFORM"} \
            -v holzwave-linux-cargo-registry:/root/.cargo/registry \
            -v holzwave-linux-cargo-git:/root/.cargo/git \
            -v holzwave-linux-target$SUFFIX:/work/target \
            -e CARGO_TERM_COLOR=never \
            ${CARGO_BUILD_JOBS:+-e CARGO_BUILD_JOBS="$CARGO_BUILD_JOBS"} \
            "$IMAGE" \
            sh -c 'tar -x --no-same-owner -C /work || exit 1
                os=$(uname -s | tr "[:upper:]" "[:lower:]")
                arch=$(uname -m)
                cpu=$(sed -n "s/^model name[[:space:]]*:[[:space:]]*//p" /proc/cpuinfo | head -n 1 | tr -cd "A-Za-z0-9 ._()@:/+-")
                [ -n "$cpu" ] || cpu=$arch
                cores=$(getconf _NPROCESSORS_ONLN)
                printf "PERF-MACHINE {\"os\":\"%s\",\"arch\":\"%s\",\"cpu\":\"%s\",\"cores\":%s}\n" "$os" "$arch" "$cpu" "$cores"
                status=0
                cargo test --locked --release -p holzwave-core --test perf -- --ignored --nocapture --test-threads=1 || status=1
                cargo test --locked --release -p holzwave-server --features in-process-workers --test perf -- --ignored --nocapture --test-threads=1 || status=1
                exit $status'
        echo $? >"$code"
    } 2>&1 | tee "$perf_log"

    status=$(cat "$code")
    rm -f "$code"
    if [ "$status" != 0 ]; then
        echo "linux-container-test: perf FAILED (docker exit $status; log: target/perf-container.log)" >&2
        exit 1
    fi
    echo "linux-container-test: perf ok (log: target/perf-container.log)"
}

# An image that holds another Zig than ZIG_VERSION (or was built before the label existed) is built again: the terminal
# engine does not build with the wrong Zig, and a cached image would hide that behind a failure deep in the build.
image_zig=$(docker image inspect -f '{{index .Config.Labels "ch.holzcloud.holzwave.zig"}}' "$IMAGE" 2>/dev/null)
if [ "$image_zig" != "$ZIG_VERSION" ]; then
    [ -z "$image_zig" ] || echo "linux-container-test: the image $IMAGE holds Zig $image_zig, not $ZIG_VERSION: building it again"
    build_image || {
        echo "linux-container-test: the image build failed" >&2
        exit 1
    }
fi

if [ "$mode" = image ]; then
    echo "linux-container-test: the image $IMAGE is there"
    exit 0
fi

if [ "$mode" = perf ]; then
    if [ $# -gt 0 ]; then
        echo "linux-container-test: perf takes no arguments" >&2
        exit 2
    fi
    if ! command -v npm >/dev/null 2>&1; then
        echo "linux-container-test: npm is missing: the server tests need a built page (app/dist)" >&2
        exit 2
    fi
    echo "+ npm --prefix app run build"
    (cd "$root" && npm --prefix app run build) || exit 1
    run_perf
    exit 0
fi

case "$mode" in
    core) package="-p holzwave-core" ;;
    app) package="-p holzwave-app" ;;
    server) package="-p holzwave-host -p holzwave-server -p holzwave-spawn-helper" ;;
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
