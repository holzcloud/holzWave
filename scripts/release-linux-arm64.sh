#!/bin/sh
# Build the Linux aarch64 preview packages (deb, rpm, AppImage) on an Apple Silicon Mac, in the native arm64 Docker
# container, name them by one scheme and checksum them. It never signs and never publishes.
#
#   sh scripts/release-linux-arm64.sh <version>
#
# <version> must equal the version in app/src-tauri/tauri.conf.json. The result, in target/release-cross/ (the folder
# scripts/release-linux-container.sh and scripts/release-windows-cross.sh write to as well; their files are left alone), is
#
#   holzWave_<version>_arm64.deb   holzWave_<version>_aarch64.rpm   holzWave_<version>_aarch64.AppImage
#
# and a .sha256 file for each. To sign them next to the x86_64 set: `sh scripts/sign-linux.sh <version> --arch aarch64`.
#
# WHAT THESE PACKAGES ARE (the script prints it again at the end):
#   - Built and inspected in a native arm64 container on an Apple Silicon Mac. The code is real aarch64 code, but the
#     container is a stand-in for an ARM Linux machine: the packages have not been run on ARM Linux hardware, and the
#     container has no FUSE and no display, so it cannot start the AppImage.
#   - The base image is Ubuntu, so the packages need the glibc and the WebKitGTK 4.1 of a current distribution.
#   - Unsigned and not published anywhere by this script.
#
# WHY ONLY HERE: the AppImage tool (linuxdeploy) cannot cross-compile an ARM AppImage from an x86_64 machine, so the
# aarch64 AppImage is built where aarch64 code runs natively. On an Intel Mac the arm64 container would be emulated,
# slow and untried, so the script refuses there and says so.
#
# This is a thin layer over scripts/linux-container-bundle.sh, which does the build in the container and the inspections
# (the deb and rpm dependencies, files, icons, desktop file and the absence of install scripts, and a built AppImage,
# which is required here): it is run without an architecture variable (native) and with HOLZWAVE_BUNDLE_APPIMAGE=required.
# This script then checks that each package really is aarch64, renames, and writes the checksums. The first run builds the
# images and compiles everything (long); later runs reuse the docker volumes. The Zig package override
# GHOSTTY_ZIG_SYSTEM_DIR (for the case that codeberg.org does not answer) is passed through unchanged.
#
# Needs: Docker running on an arm64 host, npm, a clean working tree. It installs nothing on the Mac (it pulls and builds
# docker images).
#
# Exit status: 0 when the three packages were built, inspected and checksummed (the last line is
# "release-linux-arm64: ok"), 1 when a build or an inspection failed, 2 when a tool is missing, the host is not arm64 or
# the arguments are wrong.

here=$(cd "$(dirname "$0")" && pwd)
root=$(dirname "$here")
cd "$root" || exit 2

fail() {
    echo "release-linux-arm64: FAILED: $*" >&2
    exit 1
}
need() {
    echo "release-linux-arm64: $*" >&2
    exit 2
}

version=${1:-}
[ -n "$version" ] && [ $# -eq 1 ] || need "usage: sh scripts/release-linux-arm64.sh <version>"
case "$version" in
    *[!0-9A-Za-z.-]* | "" | -*) need "not a version: $version" ;;
esac
case "$(uname -m)" in
    arm64 | aarch64) ;;
    *) need "this host is $(uname -m), not arm64: the arm64 container would be emulated (slow, untried) and the AppImage tool cannot cross-compile ARM; build on an Apple Silicon Mac or on ARM Linux" ;;
esac
command -v docker >/dev/null 2>&1 || need "docker is missing: install Docker Desktop"
docker info >/dev/null 2>&1 || need "the Docker daemon does not answer: start Docker Desktop"
command -v npm >/dev/null 2>&1 || need "npm is missing"

conf=$(sed -n 's/^ *"version": "\(.*\)",/\1/p' app/src-tauri/tauri.conf.json | head -1)
[ "$conf" = "$version" ] || fail "version $version is not the version of tauri.conf.json ($conf)"
[ -z "$(git status --porcelain)" ] || fail "the working tree is not clean"
branch=$(git rev-parse --abbrev-ref HEAD)
commit=$(git rev-parse --short HEAD)
echo "release-linux-arm64: building holzWave $version from $branch at $commit"
[ "$branch" = main ] || echo "release-linux-arm64: note: this is not main; a preview for the owner's test rows may come from a branch"
started=$(date +%s)

HOLZWAVE_BUNDLE_APPIMAGE=required sh "$here/linux-container-bundle.sh" || fail "the container build or its inspections"

built="$root/target/linux-bundle"
deb=$(ls "$built"/*.deb 2>/dev/null | head -n 1)
rpm=$(ls "$built"/*.rpm 2>/dev/null | head -n 1)
appimage=$(ls "$built"/*.AppImage 2>/dev/null | head -n 1)
[ -n "$deb" ] && [ -n "$rpm" ] && [ -n "$appimage" ] || fail "one of deb, rpm or AppImage is missing in $built"

# Each package must be aarch64. The architecture is read by the tools of the container (dpkg, rpm, file). The AppImage is
# also opened without being run (its squashfs starts where its ELF runtime ends): the program inside must be an aarch64
# ELF file, AppRun must be there and WebKitGTK must be bundled.
IMAGE=holzwave-linux-bundle
arch=$(docker run --rm -v "$built":/in:ro "$IMAGE" sh -c '
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
printf '%s\n' "$arch" | grep -qx 'deb arm64' || fail "the deb is not arm64"
printf '%s\n' "$arch" | grep -qx 'rpm aarch64' || fail "the rpm is not aarch64"
printf '%s\n' "$arch" | grep -q '^appimage ELF 64-bit LSB .*ARM aarch64' || fail "the AppImage is not an aarch64 ELF file"
printf '%s\n' "$arch" | grep -q '^appimage-program ELF 64-bit LSB .*ARM aarch64' || fail "the program inside the AppImage is not an aarch64 ELF file"
printf '%s\n' "$arch" | grep -qx 'appimage-apprun yes' || fail "the AppImage has no AppRun"
printf '%s\n' "$arch" | grep -qx 'appimage-webkit yes' || fail "the AppImage does not bundle WebKitGTK"

out=target/release-cross
mkdir -p "$out" || exit 1
publish() { # <source> <final name>
    cp "$1" "$out/$2" || exit 1
    sum=$(shasum -a 256 "$out/$2" | cut -d' ' -f1)
    echo "$sum  $2" >"$out/$2.sha256"
    echo "release-linux-arm64: $out/$2 ($(wc -c <"$out/$2" | tr -d ' ') bytes) sha256 $sum"
}
echo
publish "$deb" "holzWave_${version}_arm64.deb"
publish "$rpm" "holzWave_${version}_aarch64.rpm"
publish "$appimage" "holzWave_${version}_aarch64.AppImage"

echo "release-linux-arm64: built in a native arm64 container on an Apple Silicon Mac; the packages were not run on ARM Linux hardware."
echo "release-linux-arm64: the container cannot start the AppImage; they need a current glibc and WebKitGTK 4.1; they are unsigned."
echo "release-linux-arm64: unsigned until the owner signs: sh scripts/sign-linux.sh $version --arch aarch64 (a .minisig for each of the three and a signed SHA256SUMS-aarch64; the key and its password stay on this machine)"
echo "release-linux-arm64: nothing was signed or published; after signing, to attach them: gh release upload v$version $out/holzWave_${version}_arm64.deb $out/holzWave_${version}_arm64.deb.minisig $out/holzWave_${version}_aarch64.rpm $out/holzWave_${version}_aarch64.rpm.minisig $out/holzWave_${version}_aarch64.AppImage $out/holzWave_${version}_aarch64.AppImage.minisig $out/SHA256SUMS-aarch64 $out/SHA256SUMS-aarch64.minisig (sh scripts/sign-linux.sh $version --arch aarch64 prints this line with the full paths)"
echo "release-linux-arm64: $(( $(date +%s) - started )) seconds"
echo "release-linux-arm64: ok"
