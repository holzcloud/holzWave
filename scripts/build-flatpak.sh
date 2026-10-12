#!/bin/sh
# The Flatpak of holzWave: check its three files from a Mac, build and install it on a Linux machine.
#
#   sh scripts/build-flatpak.sh --check            validate the manifest, the AppStream file and the desktop file (Docker)
#   sh scripts/build-flatpak.sh --container-build  build the Flatpak in a privileged Docker container (slow, Docker)
#   sh scripts/build-flatpak.sh                    build the Flatpak and install it for the user (a Linux machine)
#
# Status: unverified. Flatpak ships in 0.0.1 only if it builds on the owner's Linux machine and row L10 of
# docs/test-plan.md passes (docs/flatpak.md).
#
# --check runs in a Docker container (image `holzwave-flatpak-check`: Ubuntu 26.04 with flatpak-builder, appstream and
# desktop-file-utils, built on the first run, `docker rmi holzwave-flatpak-check` forgets it). The three files are
# mounted read-only. Three validators must pass, and the script prints `flatpak-check: ok` as its last line only then:
#
#   flatpak-builder --show-manifest    parses the manifest; it exits 0 for a manifest it cannot fully read and only
#                                      prints a warning, so any output on stderr fails the check, and the parsed
#                                      manifest must still name the application id and the host-command permission
#   appstreamcli validate --no-net     the AppStream file (a warning fails it, not only an error)
#   desktop-file-validate              the desktop file
#
# That proves the files are well-formed. It does not prove that the Flatpak builds (runtime and extensions available,
# network at build time, the Zig and Ghostty downloads inside the sandbox) or runs: that needs a Linux machine.
#
# --container-build tries the real build from a Mac: in the same image, in a privileged container (flatpak-builder needs
# bubblewrap, which needs the privilege; Docker Desktop's Linux VM gives it), it installs the GNOME runtime, the SDK and
# the two extensions from Flathub into the Docker volume `holzwave-flatpak-data` (about 3 GB, kept between runs), builds the
# manifest with the working tree as source (without target, .git, node_modules, .planning) into the volume
# `holzwave-flatpak-build`, and checks the result: the binary and the three desktop files are installed, Zig is not, and
# `ldd` finds every library of the binary inside the runtime. It prints `flatpak-container-build: ok` as its last line only
# then. The container has the architecture of the Docker VM (arm64 on an Apple Silicon Mac), so it builds with the aarch64
# Zig archive of the manifest: it proves the manifest, the runtime and the SDK extensions, the network use and the
# source build, not the x86_64 archive and not a run (no display, no host to spawn on). The build takes a quarter of an
# hour; `docker volume rm holzwave-flatpak-build holzwave-flatpak-data` forgets everything.
#
# The default mode is for the owner's Linux machine: it checks that `flatpak` and `flatpak-builder` exist and that the
# GNOME runtime, SDK and the two SDK extensions of the manifest are installed (and says how to install them), then
# builds with `flatpak-builder --user --install --force-clean` and prints how to run the result. The build needs the
# network (crates, npm packages, the Zig archive and the Zig packages of Ghostty): it is a local build. It writes only
# below target/ (the build directory and flatpak-builder's state directory) and into the user's Flatpak installation;
# it pushes, signs, tags and publishes nothing.
#
# Exit status: 0 when it passed (--check) or built, 1 when a validator failed or the build failed, 2 when a tool is
# missing (Docker, flatpak, flatpak-builder, a runtime) or the usage is wrong. A missing tool is a failed gate, never a
# skip.

APP_ID=ch.holzcloud.holzwave
RUNTIME_VERSION=51
EXTENSION_VERSION=26.08
IMAGE=holzwave-flatpak-check
BASE_IMAGE=ubuntu:26.04

here=$(cd "$(dirname "$0")" && pwd)
root=$(dirname "$here")
dir="$root/packaging/flatpak"
manifest="$dir/$APP_ID.yml"

mode=build
case "${1:-}" in
    "") ;;
    --check) mode=check ;;
    --container-build) mode=container ;;
    *)
        echo "usage: sh scripts/build-flatpak.sh [--check | --container-build]" >&2
        exit 2
        ;;
esac
if [ $# -gt 1 ]; then
    echo "usage: sh scripts/build-flatpak.sh [--check | --container-build]" >&2
    exit 2
fi

for file in "$APP_ID.yml" "$APP_ID.metainfo.xml" "$APP_ID.desktop"; do
    if [ ! -f "$dir/$file" ]; then
        echo "flatpak: packaging/flatpak/$file is missing" >&2
        exit 2
    fi
done

# ---- --check ---------------------------------------------------------------------------------------------------

build_image() {
    echo "+ docker build -t $IMAGE (Dockerfile from stdin)"
    docker build -t "$IMAGE" --build-arg "BASE_IMAGE=$BASE_IMAGE" - <<'DOCKERFILE'
ARG BASE_IMAGE=ubuntu:26.04
FROM ${BASE_IMAGE}
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update \
    && apt-get install -y --no-install-recommends appstream binutils ca-certificates desktop-file-utils elfutils flatpak \
        flatpak-builder git jq libarchive-tools patch xz-utils zstd \
    && rm -rf /var/lib/apt/lists/*
RUN flatpak remote-add --system --if-not-exists flathub https://dl.flathub.org/repo/flathub.flatpakrepo
DOCKERFILE
}

# What runs in the container: the three validators. `fail` stops the run, so the end is reached only when all passed.
CHECK=$(
    cat <<'CHECK'
set -u
fail() {
    echo "flatpak-check: FAILED: $*" >&2
    exit 1
}
id=ch.holzcloud.holzwave
cd /tmp || exit 1

echo "== flatpak-builder --show-manifest"
flatpak-builder --show-manifest "/pk/$id.yml" >manifest.json 2>manifest.err || fail "flatpak-builder cannot read the manifest: $(cat manifest.err)"
[ ! -s manifest.err ] || fail "flatpak-builder warns about the manifest: $(cat manifest.err)"
jq -e --arg id "$id" '.id == $id' manifest.json >/dev/null || fail "the parsed manifest has another application id"
jq -e '.["finish-args"] | index("--talk-name=org.freedesktop.Flatpak")' manifest.json >/dev/null || fail "the parsed manifest has no host-command permission"
jq -e '(.modules | length) >= 2' manifest.json >/dev/null || fail "the parsed manifest has fewer modules than expected"
echo "manifest: ok ($(jq -r '.runtime + "//" + .["runtime-version"]' manifest.json))"

echo "== appstreamcli validate --no-net"
appstreamcli validate --no-net "/pk/$id.metainfo.xml" || fail "appstreamcli validate"

echo "== desktop-file-validate"
desktop-file-validate "/pk/$id.desktop" || fail "desktop-file-validate"
echo "desktop file: ok"
CHECK
)

check() {
    if ! command -v docker >/dev/null 2>&1; then
        echo "flatpak-check: docker is missing: install Docker Desktop" >&2
        exit 2
    fi
    if ! docker info >/dev/null 2>&1; then
        echo "flatpak-check: the Docker daemon does not answer: start Docker Desktop" >&2
        exit 2
    fi
    if ! docker image inspect "$IMAGE" >/dev/null 2>&1; then
        build_image || {
            echo "flatpak-check: the image build failed" >&2
            exit 1
        }
    fi
    echo "+ docker run $IMAGE (three validators)"
    docker run --rm -v "$dir":/pk:ro "$IMAGE" sh -c "$CHECK" || {
        echo "flatpak-check: FAILED" >&2
        exit 1
    }
    echo "flatpak-check: ok"
}

# ---- --container-build -----------------------------------------------------------------------------------------

# What runs in the privileged container, with the working tree unpacked to /work.
BUILD=$(
    cat <<'BUILD'
set -u
fail() {
    echo "flatpak-container-build: FAILED: $*" >&2
    exit 1
}
id=ch.holzcloud.holzwave
ext=26.08
gnome=51
# The working tree is the standard input; it is read first and to the end.
mkdir -p /work && tar -x --no-same-owner -C /work 2>/dev/null || fail "unpacking the working tree"
echo "+ flatpak install (the runtime and the SDK extensions; kept in the volume)"
flatpak install --system -y --noninteractive flathub "org.gnome.Platform//$gnome" "org.gnome.Sdk//$gnome" \
    "org.freedesktop.Sdk.Extension.rust-stable//$ext" "org.freedesktop.Sdk.Extension.node26//$ext" </dev/null || fail "installing the runtime and the extensions"
echo "+ flatpak-builder (a quarter of an hour on a cold cache)"
flatpak-builder --disable-rofiles-fuse --state-dir=/build/state --force-clean /build/out "/work/packaging/flatpak/$id.yml" || fail "flatpak-builder"

echo "== the result"
out=/build/out/files
[ -x "$out/bin/holzwave-app" ] || fail "no binary in /app/bin"
for file in "share/applications/$id.desktop" "share/metainfo/$id.metainfo.xml" "share/icons/hicolor/128x128/apps/$id.png"; do
    [ -f "$out/$file" ] || fail "$file is not installed"
done
[ ! -e "$out/opt/zig" ] || fail "Zig is still in the app (a build tool must not ship)"
ls -l "$out/bin/holzwave-app"
missing=$(flatpak-builder --disable-rofiles-fuse --state-dir=/build/state --run /build/out "/work/packaging/flatpak/$id.yml" ldd /app/bin/holzwave-app 2>&1 | grep 'not found')
[ -z "$missing" ] || fail "libraries the runtime does not have: $missing"
echo "ldd: every library of the binary is in the runtime"
BUILD
)

container_build() {
    if ! command -v docker >/dev/null 2>&1; then
        echo "flatpak-container-build: docker is missing: install Docker Desktop" >&2
        exit 2
    fi
    if ! docker info >/dev/null 2>&1; then
        echo "flatpak-container-build: the Docker daemon does not answer: start Docker Desktop" >&2
        exit 2
    fi
    if ! docker image inspect "$IMAGE" >/dev/null 2>&1; then
        build_image || {
            echo "flatpak-container-build: the image build failed" >&2
            exit 1
        }
    fi
    echo "+ docker run --privileged $IMAGE (install the runtime, build, inspect)"
    code=$(mktemp "${TMPDIR:-/tmp}/holzwave-flatpak-code.XXXXXX") || exit 2
    COPYFILE_DISABLE=1 tar -c -C "$root" --exclude ./target --exclude ./.git --exclude node_modules --exclude ./.planning \
        --exclude ./app/dist . 2>/dev/null |
        {
            docker run --rm -i --privileged \
                -v holzwave-flatpak-data:/var/lib/flatpak \
                -v holzwave-flatpak-build:/build \
                "$IMAGE" sh -c "$BUILD"
            echo $? >"$code"
        }
    status=$(cat "$code")
    rm -f "$code"
    if [ "$status" != 0 ]; then
        echo "flatpak-container-build: FAILED (docker exit $status)" >&2
        exit 1
    fi
    echo "flatpak-container-build: ok"
}

# ---- the owner's Linux machine ---------------------------------------------------------------------------------

build() {
    if [ "$(uname -s)" != Linux ]; then
        echo "flatpak: this builds on Linux; on another system run: sh scripts/build-flatpak.sh --check" >&2
        exit 2
    fi
    for tool in flatpak flatpak-builder; do
        if ! command -v "$tool" >/dev/null 2>&1; then
            echo "flatpak: $tool is missing: install it with your package manager (https://flatpak.org/setup/)" >&2
            exit 2
        fi
    done
    missing=""
    for ref in "org.gnome.Platform//$RUNTIME_VERSION" "org.gnome.Sdk//$RUNTIME_VERSION" \
        "org.freedesktop.Sdk.Extension.rust-stable//$EXTENSION_VERSION" "org.freedesktop.Sdk.Extension.node26//$EXTENSION_VERSION"; do
        flatpak info "$ref" >/dev/null 2>&1 || missing="$missing $ref"
    done
    if [ -n "$missing" ]; then
        echo "flatpak: the runtime or an SDK extension of the manifest is not installed:$missing" >&2
        echo "  add Flathub once:  flatpak remote-add --user --if-not-exists flathub https://dl.flathub.org/repo/flathub.flatpakrepo" >&2
        echo "  then install:      flatpak install --user flathub$missing" >&2
        exit 2
    fi

    state="$root/target/flatpak-state"
    out="$root/target/flatpak-build"
    mkdir -p "$state" || exit 1
    echo "+ flatpak-builder --user --install --force-clean (the build needs the network and takes a while)"
    flatpak-builder --user --install --force-clean --state-dir="$state" "$out" "$manifest" || {
        echo "flatpak: the build failed: see the message above; if the Flatpak cannot be built, it moves out of 0.0.1 (docs/flatpak.md)" >&2
        exit 1
    }
    echo
    echo "flatpak: built and installed for the user. Run it with:"
    echo "  flatpak run $APP_ID"
    echo "then work through row L10 of docs/test-plan.md."
}

case "$mode" in
    check) check ;;
    container) container_build ;;
    *) build ;;
esac
