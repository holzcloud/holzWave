#!/bin/sh
# Build the Linux x86_64 preview packages (deb, rpm, AppImage) from the Mac, in an amd64 Docker container, name them by
# one scheme and checksum them. It never publishes.
#
#   sh scripts/release-linux-container.sh <version>
#
# <version> must equal the version in app/src-tauri/tauri.conf.json. The result, in target/release-cross/ (the folder
# scripts/release-windows-cross.sh writes to as well), is
#
#   holzWave_<version>_amd64.deb   holzWave_<version>_x86_64.rpm   holzWave_<version>_x86_64.AppImage
#
# and a .sha256 file for each; attach them to the existing GitHub release by hand:
# `gh release upload v<version> target/release-cross/holzWave_<version>_*`.
#
# WHAT THESE PACKAGES ARE (the script prints it again at the end):
#   - Built and inspected in an amd64 container, emulated on an Apple Silicon Mac (Docker Desktop with Rosetta). The code
#     is real x86_64 code; the owner has run the packages on Linux desktops (L rows of docs/test-plan.md). The container
#     itself has no FUSE and no display, so it cannot start the AppImage.
#   - The base image is Ubuntu 24.04, so the packages need at least glibc 2.39 and WebKitGTK 4.1 (libwebkit2gtk-4.1-0),
#     which means Ubuntu 24.04 or newer, Debian 13 or newer, Fedora 40 or newer, and the like. An older distribution
#     cannot install them; the build guide (docs/build-linux.md) builds from source there.
#   - Unsigned and not published anywhere by this script.
#
# This is a thin layer over scripts/linux-container-bundle.sh, which does the build in the container and the
# inspections (the deb and rpm dependencies, files, icons, desktop file and the absence of install scripts, and a built
# AppImage, which is required here): it is run with HOLZWAVE_LINUX_ARCH=amd64 and HOLZWAVE_BUNDLE_APPIMAGE=required.
# This script then checks that each package really is x86_64, renames, and writes the checksums. The first run builds
# the images (several minutes) and compiles everything in the emulated container (about an hour); later runs reuse the
# docker volumes. The Zig package override of plan 02.2-06 (GHOSTTY_ZIG_SYSTEM_DIR, for the case that codeberg.org does
# not answer) is passed through unchanged.
#
# Needs: Docker running, npm, a clean working tree. It installs nothing on the Mac (it pulls and builds docker images).
#
# Exit status: 0 when the three packages were built, inspected and checksummed (the last line is
# "release-linux-container: ok"), 1 when a build or an inspection failed, 2 when a tool is missing or the arguments
# are wrong.

here=$(cd "$(dirname "$0")" && pwd)
root=$(dirname "$here")
cd "$root" || exit 2

fail() {
    echo "release-linux-container: FAILED: $*" >&2
    exit 1
}
need() {
    echo "release-linux-container: $*" >&2
    exit 2
}

version=${1:-}
[ -n "$version" ] && [ $# -eq 1 ] || need "usage: sh scripts/release-linux-container.sh <version>"
case "$version" in
    *[!0-9A-Za-z.-]* | "" | -*) need "not a version: $version" ;;
esac
command -v docker >/dev/null 2>&1 || need "docker is missing: install Docker Desktop"
docker info >/dev/null 2>&1 || need "the Docker daemon does not answer: start Docker Desktop"
command -v npm >/dev/null 2>&1 || need "npm is missing"

conf=$(sed -n 's/^ *"version": "\(.*\)",/\1/p' app/src-tauri/tauri.conf.json | head -1)
[ "$conf" = "$version" ] || fail "version $version is not the version of tauri.conf.json ($conf)"
[ -z "$(git status --porcelain)" ] || fail "the working tree is not clean"
branch=$(git rev-parse --abbrev-ref HEAD)
commit=$(git rev-parse --short HEAD)
echo "release-linux-container: building holzWave $version from $branch at $commit"
[ "$branch" = main ] || echo "release-linux-container: note: this is not main; a preview for the owner's test rows may come from a branch"
started=$(date +%s)

HOLZWAVE_LINUX_ARCH=amd64 HOLZWAVE_BUNDLE_APPIMAGE=required sh "$here/linux-container-bundle.sh" || fail "the container build or its inspections"

built="$root/target/linux-bundle-amd64"
deb=$(ls "$built"/*.deb 2>/dev/null | head -n 1)
rpm=$(ls "$built"/*.rpm 2>/dev/null | head -n 1)
appimage=$(ls "$built"/*.AppImage 2>/dev/null | head -n 1)
[ -n "$deb" ] && [ -n "$rpm" ] && [ -n "$appimage" ] || fail "one of deb, rpm or AppImage is missing in $built"

# Each package must be x86_64. The architecture is read by the tools of the container (dpkg, rpm, file). The AppImage is
# also opened without being run (its squashfs starts where its ELF runtime ends): the program inside must be an x86-64
# ELF file, AppRun must be there and WebKitGTK must be bundled.
IMAGE=holzwave-linux-bundle-amd64
arch=$(docker run --rm --platform linux/amd64 -v "$built":/in:ro "$IMAGE" sh -c '
    echo "deb $(dpkg-deb -f /in/"$1" Architecture)"
    echo "rpm $(rpm -qp --qf "%{ARCH}" /in/"$2")"
    f=/in/$3
    echo "appimage $(file -b "$f")"
    off=$(readelf -h "$f" | awk "/Start of section headers/{s=\$5} /Size of section headers/{z=\$5} /Number of section headers/{n=\$5} END{print s+z*n}")
    unsquashfs -o "$off" -d /tmp/sq "$f" usr/bin/holzwave-app AppRun >/dev/null 2>&1
    echo "appimage-program $(file -b /tmp/sq/usr/bin/holzwave-app)"
    [ -x /tmp/sq/AppRun ] && echo "appimage-apprun yes"
    unsquashfs -o "$off" -l "$f" 2>/dev/null | grep -q "usr/lib/libwebkit2gtk-4.1.so" && echo "appimage-webkit yes"' sh "$(basename "$deb")" "$(basename "$rpm")" "$(basename "$appimage")") || fail "reading the architecture of the packages"
echo "$arch"
printf '%s\n' "$arch" | grep -qx 'deb amd64' || fail "the deb is not amd64"
printf '%s\n' "$arch" | grep -qx 'rpm x86_64' || fail "the rpm is not x86_64"
printf '%s\n' "$arch" | grep -q '^appimage ELF 64-bit LSB .*x86-64' || fail "the AppImage is not an x86-64 ELF file"
printf '%s\n' "$arch" | grep -q '^appimage-program ELF 64-bit LSB .*x86-64' || fail "the program inside the AppImage is not an x86-64 ELF file"
printf '%s\n' "$arch" | grep -qx 'appimage-apprun yes' || fail "the AppImage has no AppRun"
printf '%s\n' "$arch" | grep -qx 'appimage-webkit yes' || fail "the AppImage does not bundle WebKitGTK"

out=target/release-cross
mkdir -p "$out" || exit 1
publish() { # <source> <final name>
    cp "$1" "$out/$2" || exit 1
    sum=$(shasum -a 256 "$out/$2" | cut -d' ' -f1)
    echo "$sum  $2" >"$out/$2.sha256"
    echo "release-linux-container: $out/$2 ($(wc -c <"$out/$2" | tr -d ' ') bytes) sha256 $sum"
}
echo
publish "$deb" "holzWave_${version}_amd64.deb"
publish "$rpm" "holzWave_${version}_x86_64.rpm"
publish "$appimage" "holzWave_${version}_x86_64.AppImage"

echo "release-linux-container: built in an amd64 container (Ubuntu 24.04, emulated), not on Linux hardware: the owner's L rows record the runs on Linux."
echo "release-linux-container: they need glibc 2.39 or newer and WebKitGTK 4.1; they are unsigned; the container cannot start the AppImage."
echo "release-linux-container: nothing was published; to attach them: gh release upload v$version $out/holzWave_${version}_amd64.deb* $out/holzWave_${version}_x86_64.rpm* $out/holzWave_${version}_x86_64.AppImage*"
echo "release-linux-container: $(( $(date +%s) - started )) seconds"
echo "release-linux-container: ok"
