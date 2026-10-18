#!/bin/sh
# Build the Linux packages (deb and rpm, then the AppImage as a try) in a Docker container, from the Mac, and inspect
# them there.
#
#   sh scripts/linux-container-bundle.sh
#
# The page is built on the host first (`npm --prefix app run build`); the container gets the working tree (without
# target, .git, node_modules and .planning), runs `npm ci` for the Linux build of the Tauri CLI (the CLI binary in
# the Mac's node_modules is for macOS), and runs `tauri build --bundles deb,rpm` with the page's beforeBuildCommand
# removed, so the host-built app/dist is used. That the CLI merges tauri.linux.conf.json over tauri.conf.json is
# proven by this run: the desktop file in the package has the categories of the template, which the default
# template does not give.
#
# wsh is part of every package: the container builds the sidecar for its own triple (scripts/wsh-sidecar.sh) and the two
# static Linux builds (scripts/wsh-linux.sh, zig cc), and every `tauri build` gets the overlay
# app/src-tauri/tauri.wsh.conf.json, which installs `wsh` beside the app binary (/usr/bin/wsh, the folder a pane's PATH
# gets) and the two Linux builds as resources. The overlay is merged after the one that removes beforeBuildCommand.
#
# Inspected, any mismatch fails the script: the deb depends on WebKitGTK 4.1 and GTK 3, lists the binary, the desktop
# file and the icons, its desktop file has `Categories=System;TerminalEmulator;`, and it has no maintainer script
# (T-2.2-26); the rpm requires the matching libraries, lists the same files and has no script. Both hold `/usr/bin/wsh`,
# byte-identical to the sidecar that was built, and the two Linux wsh resources. Then the AppImage is
# tried with APPIMAGE_EXTRACT_AND_RUN=1 (a container has no FUSE) and its result is printed: `appimage: built` or
# `appimage: not built, <first error line>`. It never fails the script: the AppImage is judged on the owner's
# machines (test-plan row L6).
#
# The image `holzwave-linux-bundle` is the image of scripts/linux-container-test.sh plus the packaging tools and
# Node (nodejs.org, verified against the sha256 it publishes for the container's architecture). The cargo
# volumes of that script are reused, so a second run does not start from nothing; `docker volume rm
# holzwave-linux-npm` forgets the npm cache. The release build of Ghostty fetches its Zig dependencies (some from
# codeberg.org) into the target volume the first time. The container is arm64 on an Apple Silicon Mac: a proxy for x86_64
# (package structure and dependencies, not the CPU). The packages are copied to target/linux-bundle/ (target is
# not in the repository); nothing is tagged, signed or published.
#
# Ghostty's build fetches its Zig packages from the network (some from codeberg.org). When that host is down, set
# GHOSTTY_ZIG_SYSTEM_DIR to a directory of prepared packages (one directory per package, named by its hash); it is
# mounted read-only into the container and handed to the build, which then fetches nothing.
#
# Exit status: 0 when every inspection passed (the last line is "bundle-check: ok"), 1 when a build or an
# inspection failed, 2 when a tool (Docker, npm) is missing. A missing tool is a failed gate, never a skip.

NODE_VERSION=26.11.0

# HOLZWAVE_LINUX_ARCH=amd64 builds the packages as linux/amd64 (emulated on an Apple Silicon Mac, see
# scripts/linux-container-test.sh): the images, the target volume and the output directory (target/linux-bundle-amd64)
# get an -amd64 suffix. HOLZWAVE_BUNDLE_APPIMAGE=required makes a missing AppImage a failure (exit 1) instead of a
# printed result: scripts/release-linux-container.sh sets both.
PLATFORM=
SUFFIX=
case "${HOLZWAVE_LINUX_ARCH:-}" in
    "") ;;
    amd64)
        PLATFORM=linux/amd64
        SUFFIX=-amd64
        ;;
    *)
        echo "bundle-check: HOLZWAVE_LINUX_ARCH must be unset or amd64" >&2
        exit 2
        ;;
esac
REQUIRE_APPIMAGE=0
[ "${HOLZWAVE_BUNDLE_APPIMAGE:-}" = required ] && REQUIRE_APPIMAGE=1
BASE_IMAGE=holzwave-linux-test$SUFFIX
IMAGE=holzwave-linux-bundle$SUFFIX

here=$(cd "$(dirname "$0")" && pwd)
root=$(dirname "$here")

if [ $# -gt 0 ]; then
    echo "usage: sh scripts/linux-container-bundle.sh" >&2
    exit 2
fi
for tool in docker npm; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        echo "bundle-check: $tool is missing" >&2
        exit 2
    fi
done
if ! docker info >/dev/null 2>&1; then
    echo "bundle-check: the Docker daemon does not answer: start Docker Desktop" >&2
    exit 2
fi

# The base image is the one of the test script (build tools, GTK and WebKitGTK headers, rustup, Zig).
sh "$here/linux-container-test.sh" image || {
    echo "bundle-check: the base image $BASE_IMAGE could not be built" >&2
    exit 1
}

# On top of it: what the bundlers and the inspections need, and Node. The tarball is checked against the line of
# SHASUMS256.txt that nodejs.org publishes for it before it is unpacked; nothing downloaded here enters the repository.
build_image() {
    echo "+ docker build -t $IMAGE (Dockerfile from stdin)"
    docker build ${PLATFORM:+--platform "$PLATFORM"} -t "$IMAGE" --build-arg "BASE_IMAGE=$BASE_IMAGE" --build-arg "NODE_VERSION=$NODE_VERSION" - <<'DOCKERFILE'
ARG BASE_IMAGE
FROM ${BASE_IMAGE}
ARG NODE_VERSION
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update \
    && apt-get install -y --no-install-recommends cpio desktop-file-utils dpkg-dev file patchelf rpm squashfs-tools \
        librsvg2-dev xdg-utils \
    && rm -rf /var/lib/apt/lists/*
RUN set -eu; \
    case "$(uname -m)" in aarch64) arch=arm64 ;; x86_64) arch=x64 ;; \
        *) echo "unsupported architecture $(uname -m)" >&2; exit 1 ;; esac; \
    name="node-v${NODE_VERSION}-linux-${arch}.tar.xz"; \
    base="https://nodejs.org/dist/v${NODE_VERSION}"; \
    curl --proto '=https' --tlsv1.2 -fsSL -o "/tmp/$name" "$base/$name"; \
    curl --proto '=https' --tlsv1.2 -fsSL -o /tmp/SHASUMS256.txt "$base/SHASUMS256.txt"; \
    sha=$(grep "  $name\$" /tmp/SHASUMS256.txt | cut -d' ' -f1); \
    case "$sha" in *[!0-9a-f]* | "") echo "no checksum for $name" >&2; exit 1 ;; esac; \
    [ ${#sha} = 64 ] || { echo "unexpected checksum: $sha" >&2; exit 1; }; \
    echo "$sha  /tmp/$name" | sha256sum -c -; \
    mkdir /opt/node; \
    tar -xJf "/tmp/$name" -C /opt/node --strip-components=1; \
    rm "/tmp/$name" /tmp/SHASUMS256.txt; \
    ln -s /opt/node/bin/node /usr/local/bin/node; \
    ln -s /opt/node/bin/npm /usr/local/bin/npm; \
    ln -s /opt/node/bin/npx /usr/local/bin/npx; \
    test "$(node --version)" = "v${NODE_VERSION}"
# x86_64 only (where Docker Desktop emulates it with Rosetta): Rosetta cannot start the type-2 runtime of the
# linuxdeploy AppImage output plugin that the Tauri bundler downloads ("Exec format error", a static-pie file), but it
# runs what is inside the file. So the plugin is unpacked here once (the squashfs starts where the ELF runtime ends)
# and INNER puts a one-line stand-in for the downloaded file in the bundler's cache. It is the same unpinned
# "continuous" download the bundler would make.
RUN set -eu; \
    if [ "$(uname -m)" = x86_64 ]; then \
        curl --proto '=https' --tlsv1.2 -fsSL -o /tmp/plugin.AppImage \
            https://github.com/linuxdeploy/linuxdeploy-plugin-appimage/releases/download/continuous/linuxdeploy-plugin-appimage-x86_64.AppImage; \
        off=$(readelf -h /tmp/plugin.AppImage | awk '/Start of section headers/{s=$5} /Size of section headers/{z=$5} /Number of section headers/{n=$5} END{print s+z*n}'); \
        unsquashfs -o "$off" -d /opt/linuxdeploy-plugin-appimage /tmp/plugin.AppImage >/dev/null; \
        rm /tmp/plugin.AppImage; \
        test "$(/opt/linuxdeploy-plugin-appimage/AppRun --plugin-api-version)" = 0; \
    fi
DOCKERFILE
}

if ! docker image inspect "$IMAGE" >/dev/null 2>&1; then
    build_image || {
        echo "bundle-check: the image build failed" >&2
        exit 1
    }
fi

echo "+ npm --prefix app run build"
(cd "$root" && npm --prefix app run build) || exit 1

out="$root/target/linux-bundle$SUFFIX"
rm -rf "$out"
mkdir -p "$out" || exit 1

# What runs in the container: build, copy out, inspect, try the AppImage. The working tree is already in /work.
INNER=$(
    cat <<'INNER'
set -u
fail() {
    echo "bundle-check: FAILED: $*" >&2
    exit 1
}
have() { printf '%s\n' "$2" | grep -q -- "$1"; }

cd /work/app || exit 1
npm ci --ignore-scripts --no-audit --no-fund || fail "npm ci"
# The page is the host's app/dist: no beforeBuildCommand (null removes the key from the merged configuration).
# The overlay of wsh follows it (the CLI merges the configurations in the order given).
tauri() { ./node_modules/.bin/tauri build --config '{"build":{"beforeBuildCommand":null}}' --config /work/app/src-tauri/tauri.wsh.conf.json "$@" -- --locked; }

# wsh: the sidecar for this container's own triple, and the two static Linux builds (they need their Rust targets).
# (if, not case: a case pattern's closing parenthesis inside this $( ) block trips older shells)
if [ "$(uname -m)" = x86_64 ]; then
    host=x86_64-unknown-linux-gnu
elif [ "$(uname -m)" = aarch64 ]; then
    host=aarch64-unknown-linux-gnu
else
    fail "unsupported architecture $(uname -m)"
fi
for t in x86_64 aarch64; do
    rustup target list --installed | grep -qx "$t-unknown-linux-musl" || rustup target add "$t-unknown-linux-musl" || fail "rustup target add $t-unknown-linux-musl"
done
sidecar=$(sh /work/scripts/wsh-sidecar.sh "$host" | tail -n 1) && [ -f "/work/$sidecar" ] || fail "building the wsh sidecar for $host"
sidecar=/work/$sidecar
sh /work/scripts/wsh-linux.sh all || fail "building the static Linux wsh"

# The target volume keeps the packages of earlier runs (and of earlier versions): the inspections below read the first
# file they find, so only what this run builds may be there, or an old package without wsh is inspected and shipped.
rm -rf /work/target/release/bundle/deb /work/target/release/bundle/rpm /work/target/release/bundle/appimage

tauri --bundles deb,rpm || fail "tauri build --bundles deb,rpm"

bundle=/work/target/release/bundle
deb=$(ls "$bundle"/deb/*.deb 2>/dev/null | head -n 1)
rpm=$(ls "$bundle"/rpm/*.rpm 2>/dev/null | head -n 1)
[ -n "$deb" ] || fail "no .deb was built"
[ -n "$rpm" ] || fail "no .rpm was built"
cp "$deb" "$rpm" /out/ || fail "copying the packages out"

# ---- deb -------------------------------------------------------------------------------------------------------
echo "== deb: $(basename "$deb")"
info=$(dpkg-deb -I "$deb") || fail "dpkg-deb -I"
echo "$info"
depends=$(printf '%s\n' "$info" | grep -i '^ *Depends:')
have 'libwebkit2gtk-4\.1' "$depends" || fail "the deb does not depend on libwebkit2gtk-4.1"
have 'libgtk-3' "$depends" || fail "the deb does not depend on libgtk-3"
files=$(dpkg-deb -c "$deb") || fail "dpkg-deb -c"
echo "$files"
# dpkg-deb -c prints the paths with or without a leading ./, depending on how the package was made.
bin=$(printf '%s\n' "$files" | grep -E ' (\./)?usr/bin/[^/]+$' | head -n 1)
[ -n "$bin" ] || fail "the deb lists no binary in usr/bin"
# The member name as the archive spells it, to read the file back (the bundler writes no leading ./).
# wsh beside the app binary, the same file that was built, and the two static Linux builds as resources.
printf '%s\n' "$files" | grep -qE ' (\./)?usr/bin/wsh$' || fail "the deb does not list usr/bin/wsh: a pane would find no wsh"
for arch in x86_64 aarch64; do
    printf '%s\n' "$files" | grep -qE " (\./)?usr/lib/[^ ]*/wsh/linux-$arch\$" || fail "the deb does not list the wsh resource linux-$arch"
done
wsh_member=$(dpkg-deb --fsys-tarfile "$deb" | tar -t | grep -E '(^|/)usr/bin/wsh$' | head -n 1)
dpkg-deb --fsys-tarfile "$deb" | tar -xO "$wsh_member" | cmp -s - "$sidecar" || fail "usr/bin/wsh in the deb is not the sidecar that was built"
echo "-- deb: usr/bin/wsh is the sidecar that was built, and the two Linux wsh resources are listed"
desktop=$(dpkg-deb --fsys-tarfile "$deb" | tar -t | grep -E '(^|/)usr/share/applications/[^/]+\.desktop$' | head -n 1)
[ -n "$desktop" ] || fail "the deb lists no desktop file"
have 'usr/share/icons/hicolor/.*\.png' "$files" || fail "the deb lists no icon"
dpkg-deb --fsys-tarfile "$deb" | tar -xO "$desktop" >/tmp/deb.desktop || fail "reading the desktop file of the deb"
echo "-- $desktop"
cat /tmp/deb.desktop
grep -qx 'Categories=System;TerminalEmulator;' /tmp/deb.desktop || fail "the deb's desktop file has no Categories=System;TerminalEmulator; line"
grep -q '^Keywords=terminal;shell;console;' /tmp/deb.desktop || fail "the deb's desktop file has no Keywords line"
grep -q '^StartupWMClass=ch.holzcloud.holzwave' /tmp/deb.desktop || fail "the deb's desktop file has no StartupWMClass"
desktop-file-validate /tmp/deb.desktop || fail "desktop-file-validate rejects the deb's desktop file"
control=$(dpkg-deb --ctrl-tarfile "$deb" | tar -t)
echo "-- control files: $(printf '%s' "$control" | tr '\n' ' ')"
if printf '%s\n' "$control" | grep -qE '(^|/)(preinst|postinst|prerm|postrm)$'; then
    fail "the deb has a maintainer script (T-2.2-26)"
fi

# ---- rpm -------------------------------------------------------------------------------------------------------
echo "== rpm: $(basename "$rpm")"
requires=$(rpm -qpR "$rpm") || fail "rpm -qpR"
echo "$requires"
have 'webkit2gtk4\.1\|libwebkit2gtk-4\.1' "$requires" || fail "the rpm does not require WebKitGTK 4.1"
have 'gtk3\|libgtk-3' "$requires" || fail "the rpm does not require GTK 3"
rfiles=$(rpm -qpl "$rpm") || fail "rpm -qpl"
echo "$rfiles"
printf '%s\n' "$rfiles" | grep -qE '^/usr/bin/[^/]+$' || fail "the rpm lists no binary in /usr/bin"
printf '%s\n' "$rfiles" | grep -qx '/usr/bin/wsh' || fail "the rpm does not list /usr/bin/wsh: a pane would find no wsh"
for arch in x86_64 aarch64; do
    printf '%s\n' "$rfiles" | grep -qE "^/usr/lib/[^ ]*/wsh/linux-$arch\$" || fail "the rpm does not list the wsh resource linux-$arch"
done
rpm2cpio "$rpm" | cpio -i --to-stdout ./usr/bin/wsh 2>/dev/null | cmp -s - "$sidecar" || fail "/usr/bin/wsh in the rpm is not the sidecar that was built"
echo "-- rpm: /usr/bin/wsh is the sidecar that was built, and the two Linux wsh resources are listed"
rdesktop=$(printf '%s\n' "$rfiles" | grep -E '^/usr/share/applications/.*\.desktop$' | head -n 1)
[ -n "$rdesktop" ] || fail "the rpm lists no desktop file"
have 'usr/share/icons/hicolor/.*\.png' "$rfiles" || fail "the rpm lists no icon"
rpm2cpio "$rpm" | cpio -i --to-stdout ".$rdesktop" >/tmp/rpm.desktop 2>/dev/null || fail "reading the desktop file of the rpm"
grep -qx 'Categories=System;TerminalEmulator;' /tmp/rpm.desktop || fail "the rpm's desktop file has no Categories=System;TerminalEmulator; line"
scripts=$(rpm -qp --scripts "$rpm")
[ -z "$scripts" ] || fail "the rpm has install scripts (T-2.2-26): $scripts"
echo "-- rpm scripts: none"

ls -l /out
# ---- AppImage (a try; never a gate) ------------------------------------------------------------------------------
echo "== AppImage (best effort, APPIMAGE_EXTRACT_AND_RUN=1)"
if [ -x /opt/linuxdeploy-plugin-appimage/AppRun ]; then
    # See the Dockerfile: the stand-in for the plugin AppImage that Rosetta cannot start.
    mkdir -p /root/.cache/tauri
    printf '#!/bin/sh\nexec /opt/linuxdeploy-plugin-appimage/AppRun "$@"\n' >/root/.cache/tauri/linuxdeploy-plugin-appimage.AppImage
    chmod +x /root/.cache/tauri/linuxdeploy-plugin-appimage.AppImage
fi
if APPIMAGE_EXTRACT_AND_RUN=1 tauri --bundles appimage >/tmp/appimage.log 2>&1; then
    image=$(ls "$bundle"/appimage/*.AppImage 2>/dev/null | head -n 1)
    if [ -n "$image" ] && cp "$image" /out/; then
        echo "appimage: built ($(basename "$image"))"
        # A note, not a gate: does the squashfs behind the ELF runtime hold usr/bin/wsh?
        off=$(readelf -h "$image" | awk '/Start of section headers/{s=$5} /Size of section headers/{z=$5} /Number of section headers/{n=$5} END{print s+z*n}')
        if unsquashfs -o "$off" -l "$image" 2>/dev/null | grep -q 'usr/bin/wsh$'; then
            echo "appimage: holds usr/bin/wsh"
        else
            echo "appimage: note: usr/bin/wsh was not found in the AppImage"
        fi
    else
        echo "appimage: not built, the build succeeded but left no .AppImage"
    fi
else
    reason=$(grep -iE 'error|failed' /tmp/appimage.log | head -n 1)
    echo "appimage: not built, ${reason:-$(tail -n 1 /tmp/appimage.log)}"
fi
if [ "${REQUIRE_APPIMAGE:-0}" = 1 ] && ! ls /out/*.AppImage >/dev/null 2>&1; then
    fail "the AppImage was required and is missing (see above)"
fi
exit 0
INNER
)

# The working tree as a tar stream. COPYFILE_DISABLE keeps macOS from adding ._ files.
zig_system=""
if [ -n "${GHOSTTY_ZIG_SYSTEM_DIR:-}" ]; then
    [ -d "$GHOSTTY_ZIG_SYSTEM_DIR" ] || {
        echo "bundle-check: GHOSTTY_ZIG_SYSTEM_DIR is not a directory: $GHOSTTY_ZIG_SYSTEM_DIR" >&2
        exit 2
    }
    zig_system=$(cd "$GHOSTTY_ZIG_SYSTEM_DIR" && pwd)
fi

echo "+ docker run $IMAGE (build and inspect)"
COPYFILE_DISABLE=1 tar -c -C "$root" --exclude target --exclude .git --exclude node_modules --exclude .planning . |
    docker run --rm -i ${PLATFORM:+--platform "$PLATFORM"} \
        -v holzwave-linux-cargo-registry:/root/.cargo/registry \
        -v holzwave-linux-cargo-git:/root/.cargo/git \
        -v holzwave-linux-target$SUFFIX:/work/target \
        -v holzwave-linux-npm:/root/.npm \
        -v "$out":/out \
        ${zig_system:+-v "$zig_system":/zig-system:ro -e GHOSTTY_ZIG_SYSTEM_DIR=/zig-system} \
        -e CARGO_TERM_COLOR=never \
        -e REQUIRE_APPIMAGE="$REQUIRE_APPIMAGE" \
        "$IMAGE" \
        sh -c 'tar -x --no-same-owner -C /work && exec sh -c "$1"' sh "$INNER"
status=$?

if [ "$status" != 0 ]; then
    echo "bundle-check: FAILED (docker exit $status)" >&2
    exit 1
fi
echo "bundle-check: packages are in target/linux-bundle$SUFFIX/"
ls -l "$out"
echo "bundle-check: ok"
