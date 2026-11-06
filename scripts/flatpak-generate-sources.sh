#!/bin/sh
# The Cargo and npm source files of the Flatpak: every crate of Cargo.lock and every package of app/package-lock.json as a
# hashed source, so that the build needs no network (Flathub forbids it).
#
#   sh scripts/flatpak-generate-sources.sh [cargo | node | all]   write the generated files (default: all)
#   sh scripts/flatpak-generate-sources.sh --check                regenerate into a scratch folder and compare byte for byte
#
# The two files below packaging/flatpak/generated/ are TOOL OUTPUT, not hand-written: cargo-sources.json from
# `flatpak-cargo-generator.py` and node-sources.json from `flatpak_node_generator` of github.com/flatpak/flatpak-builder-tools,
# at the commit that packaging/flatpak/generated/generator-pin.txt names. Never edit them by hand: change a lock file, run this
# script again and commit both. The script verifies that the checkout is exactly the pinned commit and stops otherwise.
#
# Where it runs: in a throw-away Docker container (`docker run --rm`) of the image `holzwave-flatpak-sources`, which is the image
# of `sh scripts/build-flatpak.sh --check` plus Python 3 and its venv module from Ubuntu. The Python packages are installed into a
# virtual environment inside that container only, nothing on the host: exactly the packages and versions that the pin file lists
# (the list the owner approved), from PyPI, with `pip --require-hashes --only-binary :all: --no-deps` so that pip refuses any file
# whose SHA-256 the pin file does not name, and the installed set is compared with the list afterwards. The generators are run
# from the pinned checkout, not installed from PyPI. The repository is mounted read-only; the output goes to a scratch folder
# below target/ and is moved into place only when the whole container run succeeded.
#
# Running it needs the NETWORK (the Docker image, the checkout, PyPI, and the crate and package metadata the generators read, the
# two git dependencies of Cargo.lock are cloned to read their manifests). That is the tool, not a build step of the Flatpak: the
# Flatpak build itself has no network. It writes only the generated files and a scratch folder below target/flatpak-sources/;
# it publishes, pushes and submits nothing.
#
# Exit status: 0 when it passed, 1 when a step failed or the files differ (--check), 2 when a tool is missing or the usage is wrong.

set -eu

IMAGE=holzwave-flatpak-sources
BASE_IMAGE=holzwave-flatpak-check

here=$(cd "$(dirname "$0")" && pwd)
root=$(dirname "$here")
generated="$root/packaging/flatpak/generated"
pin="$generated/generator-pin.txt"

mode=all
check=0
case "${1:-}" in
    "" | all) ;;
    cargo) mode=cargo ;;
    node) mode=node ;;
    --check) check=1 ;;
    *)
        echo "usage: sh scripts/flatpak-generate-sources.sh [cargo | node | all | --check]" >&2
        exit 2
        ;;
esac
if [ $# -gt 1 ]; then
    echo "usage: sh scripts/flatpak-generate-sources.sh [cargo | node | all | --check]" >&2
    exit 2
fi

if [ ! -f "$pin" ]; then
    echo "flatpak-sources: packaging/flatpak/generated/generator-pin.txt is missing" >&2
    exit 2
fi
if ! command -v docker >/dev/null 2>&1; then
    echo "flatpak-sources: docker is missing: install Docker Desktop" >&2
    exit 2
fi
if ! docker info >/dev/null 2>&1; then
    echo "flatpak-sources: the Docker daemon does not answer: start Docker Desktop" >&2
    exit 2
fi
if ! docker image inspect "$BASE_IMAGE" >/dev/null 2>&1; then
    echo "flatpak-sources: the image $BASE_IMAGE is missing: run sh scripts/build-flatpak.sh --check once" >&2
    exit 2
fi

build_image() {
    echo "+ docker build -t $IMAGE (Dockerfile from stdin: $BASE_IMAGE plus python3 and python3-venv)"
    docker build -t "$IMAGE" --build-arg "BASE_IMAGE=$BASE_IMAGE" - <<'DOCKERFILE'
ARG BASE_IMAGE
FROM ${BASE_IMAGE}
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update \
    && apt-get install -y --no-install-recommends python3 python3-venv \
    && rm -rf /var/lib/apt/lists/*
DOCKERFILE
}

# What runs in the container: /src is the repository (read-only), /out the scratch folder, $1 the mode.
INNER=$(
    cat <<'INNER'
set -eu
fail() {
    echo "flatpak-sources: FAILED: $*" >&2
    exit 1
}
mode=$1
pin=/src/packaging/flatpak/generated/generator-pin.txt
setting() { sed -n "s/^$1=//p" "$pin"; }
repo=$(setting flatpak-builder-tools-repository)
commit=$(setting flatpak-builder-tools-commit)
echo "$commit" | grep -Eq '^[0-9a-f]{40}$' || fail "the pin file has no 40-character commit: $commit"
[ "$repo" = https://github.com/flatpak/flatpak-builder-tools.git ] || fail "the pinned repository is not flatpak-builder-tools: $repo"

echo "+ the pinned checkout of flatpak-builder-tools"
git init -q /tools
git -C /tools -c advice.detachedHead=false fetch -q --depth 1 "$repo" "$commit" || fail "fetching $commit"
git -C /tools -c advice.detachedHead=false checkout -q FETCH_HEAD || fail "checking out $commit"
[ "$(git -C /tools rev-parse HEAD)" = "$commit" ] || fail "the checkout is not the pinned commit $commit"
echo "flatpak-builder-tools: $commit"

# The requirements file is made from the pin file: one line per approved package, with the hashes of its files.
: >/tmp/requirements.txt
setting python-package >/tmp/approved.txt
[ -s /tmp/approved.txt ] || fail "the pin file lists no Python package"
while read -r spec; do
    name=${spec%%==*}
    line=$spec
    hashes=$(sed -n "s/^python-hash=$name sha256:\([0-9a-f]\{64\}\) .*/\1/p" "$pin")
    [ -n "$hashes" ] || fail "the pin file has no hash for $name"
    for h in $hashes; do line="$line --hash=sha256:$h"; done
    echo "$line" >>/tmp/requirements.txt
done </tmp/approved.txt

echo "+ a virtual environment inside this container, with exactly the approved packages (hashes required)"
python3 -m venv /venv || fail "creating the virtual environment"
/venv/bin/pip install --disable-pip-version-check --quiet --require-hashes --only-binary :all: --no-deps -r /tmp/requirements.txt ||
    fail "installing the approved packages"
# The installed set must be the approved set, nothing more, nothing less (pip itself is not a package of the list).
/venv/bin/pip list --format=freeze --disable-pip-version-check | grep -v '^pip==' | tr 'A-Z_' 'a-z-' | sort >/tmp/installed.txt
tr 'A-Z_' 'a-z-' </tmp/approved.txt | sort >/tmp/wanted.txt
if ! diff -u /tmp/wanted.txt /tmp/installed.txt; then fail "the installed packages are not the approved ones"; fi
echo "python packages: $(wc -l </tmp/installed.txt | tr -d ' ') installed, the approved set"

if [ "$mode" = cargo ] || [ "$mode" = all ]; then
    echo "+ flatpak-cargo-generator.py Cargo.lock"
    cd /tmp
    /venv/bin/python /tools/cargo/flatpak-cargo-generator.py /src/Cargo.lock -o /out/cargo-sources.json || fail "the cargo generator"
    echo "cargo-sources.json: $(wc -c </out/cargo-sources.json | tr -d ' ') bytes"
fi
if [ "$mode" = node ] || [ "$mode" = all ]; then
    echo "+ flatpak_node_generator npm app/package-lock.json"
    cd /tools/node
    # The generator redraws a progress line with carriage returns; only its other lines are kept.
    PYTHONPATH=/tools/node /venv/bin/python -m flatpak_node_generator --no-requests-cache npm /src/app/package-lock.json -o /out/node-sources.json \
        >/tmp/node-generator.log 2>&1 || {
        tr '\r' '\n' </tmp/node-generator.log | grep -v '^ *Generating packages' | tail -n 20 >&2
        fail "the node generator"
    }
    tr '\r' '\n' </tmp/node-generator.log | grep -v -e '^ *Generating packages' -e '^ *$'
    echo "node-sources.json: $(wc -c </out/node-sources.json | tr -d ' ') bytes"
fi
INNER
)

run() { # run <mode> <scratch folder>
    docker run --rm -v "$root":/src:ro -v "$2":/out "$IMAGE" sh -c "$INNER" sh "$1"
}

if ! docker image inspect "$IMAGE" >/dev/null 2>&1; then
    build_image || {
        echo "flatpak-sources: the image build failed" >&2
        exit 1
    }
fi

work="$root/target/flatpak-sources"
rm -rf "$work"
mkdir -p "$work/out"

if [ "$check" = 1 ]; then
    run all "$work/out" || {
        echo "flatpak-sources: FAILED: the regeneration did not finish" >&2
        exit 1
    }
    differ=""
    for file in cargo-sources.json node-sources.json; do
        if [ ! -f "$generated/$file" ] || ! cmp -s "$work/out/$file" "$generated/$file"; then
            differ="$differ $file"
        fi
    done
    if [ -n "$differ" ]; then
        echo "flatpak-sources: these files differ from a fresh run of the pinned tool:$differ" >&2
        echo "flatpak-sources: regenerate them with: sh scripts/flatpak-generate-sources.sh" >&2
        exit 1
    fi
    echo "flatpak-sources: up to date"
    exit 0
fi

run "$mode" "$work/out" || {
    echo "flatpak-sources: FAILED: nothing was written to packaging/flatpak/generated/" >&2
    exit 1
}
for file in cargo-sources.json node-sources.json; do
    if [ -f "$work/out/$file" ]; then
        mv "$work/out/$file" "$generated/$file"
        echo "wrote packaging/flatpak/generated/$file"
    fi
done
echo "flatpak-sources: ok"
