#!/bin/sh
# Build the Windows x64 preview installer from the Mac, without a Windows host, and checksum it. It never publishes.
#
#   sh scripts/release-windows-cross.sh <version>
#
# <version> must equal the version in app/src-tauri/tauri.conf.json. The result is
# target/release-cross/holzWave_<version>_x64-setup.exe and its .sha256 file; attach both to the existing GitHub
# release by hand: `gh release upload v<version> target/release-cross/holzWave_<version>_x64-setup.exe*`.
#
# WHAT THIS BUILD IS NOT (the script prints it again at the end):
#   - It is the GNU ABI (x86_64-pc-windows-gnu, linked with mingw-w64), not the MSVC build that
#     docs/build-windows.md and scripts/release-windows.ps1 make on a Windows host. The MSVC build is the better
#     build and replaces this one. A GNU-ABI holzWave has never run on Windows: the owner's W rows are its first run.
#   - It is unsigned (Windows SmartScreen warns on the first start) and it is a per-user NSIS installer.
#
# How it works, and why each step is there (research: .planning/phases/02.2-windows-linux/02.2-RESEARCH-10.md):
#   - Rust cross-compiles for x86_64-pc-windows-gnu with Homebrew's mingw-w64 (linker x86_64-w64-mingw32-gcc and the
#     resource compiler x86_64-w64-mingw32-windres, both found on PATH). Zig builds libghostty for x86_64-windows-gnu.
#   - GNU ld drops the section that holds the weak stack probe ___chkstk_ms of Zig's runtime and then fails with
#     "relocation truncated to fit" while linking. `-Wl,--no-gc-sections` for this target only avoids it (the
#     executable is a few MB larger). It is passed through CARGO_TARGET_X86_64_PC_WINDOWS_GNU_RUSTFLAGS.
#   - The Tauri bundler on a Mac needs a `makensis` on PATH and has no Mac NSIS of its own. Nothing is installed for
#     it: a one-line `makensis` that runs NSIS in a Docker container (image holzwave-nsis: Ubuntu with the nsis and
#     7zip packages) is put first on PATH for the one build command and removed afterwards. The repository and
#     the bundler's cache directory are mounted at the same paths, so the paths in the NSIS script are valid inside.
#   - The bundler itself adds WebView2Loader.dll (the GNU ABI loads it as a DLL, the MSVC build links it in) and patches
#     the bundle type into the executable. The release executable is a GUI program (no console window).
#
# Inspected here, any mismatch fails the script: the executable is a 64-bit GUI PE and imports no mingw runtime DLL,
# the installer is an NSIS installer that holds holzwave-app.exe (the built one, with the bundle type marker the
# bundler writes) and WebView2Loader.dll (the built one).
#
# Needs: this Mac (Darwin), Docker running, `brew install mingw-w64`, `rustup target add x86_64-pc-windows-gnu`, npm,
# a clean working tree, and the environment of docs/building.md (scripts/env.sh puts Rust and Zig 0.16 on the PATH).
# It installs nothing; a missing tool is exit 2.
#
# Exit status: 0 when the installer was built and inspected (the last line is "release-windows-cross: ok"), 1 when a
# build or an inspection failed, 2 when a tool is missing or the arguments are wrong.

TARGET=x86_64-pc-windows-gnu
IMAGE=holzwave-nsis
BASE_IMAGE=ubuntu:26.04

here=$(cd "$(dirname "$0")" && pwd)
root=$(dirname "$here")
cd "$root" || exit 2
. "$here/env.sh"

fail() {
    echo "release-windows-cross: FAILED: $*" >&2
    exit 1
}
need() {
    echo "release-windows-cross: $*" >&2
    exit 2
}

version=${1:-}
[ -n "$version" ] && [ $# -eq 1 ] || need "usage: sh scripts/release-windows-cross.sh <version>"
case "$version" in
    *[!0-9A-Za-z.-]* | "" | -*) need "not a version: $version" ;;
esac
[ "$(uname -s)" = Darwin ] || need "this script builds from a Mac; this machine is $(uname -s)"

# rustup from Homebrew is keg-only and not on PATH.
if ! command -v cargo >/dev/null 2>&1 && command -v brew >/dev/null 2>&1; then
    rustup_bin="$(brew --prefix rustup 2>/dev/null)/bin"
    [ -d "$rustup_bin" ] && PATH="$rustup_bin:$PATH"
fi
command -v cargo >/dev/null 2>&1 || need "cargo is missing: install rustup (brew install rustup) and run it once"
command -v npm >/dev/null 2>&1 || need "npm is missing"
command -v docker >/dev/null 2>&1 || need "docker is missing: install Docker Desktop"
docker info >/dev/null 2>&1 || need "the Docker daemon does not answer: start Docker Desktop"
for tool in x86_64-w64-mingw32-gcc x86_64-w64-mingw32-windres x86_64-w64-mingw32-objdump; do
    command -v "$tool" >/dev/null 2>&1 || need "$tool is missing: brew install mingw-w64 (the GNU-ABI toolchain)"
done
rustup_cmd=$(command -v rustup)
if [ -n "$rustup_cmd" ]; then
    "$rustup_cmd" target list --installed 2>/dev/null | grep -qx "$TARGET" ||
        need "rustup target missing: rustup target add $TARGET (a toolchain change, not a dependency)"
fi
[ "$(zig version 2>/dev/null)" = 0.16.0 ] || need "Zig 0.16.0 is not on PATH (. scripts/env.sh; docs/building.md)"

conf=$(sed -n 's/^ *"version": "\(.*\)",/\1/p' app/src-tauri/tauri.conf.json | head -1)
[ "$conf" = "$version" ] || fail "version $version is not the version of tauri.conf.json ($conf)"
[ -z "$(git status --porcelain)" ] || fail "the working tree is not clean"
branch=$(git rev-parse --abbrev-ref HEAD)
commit=$(git rev-parse --short HEAD)
echo "release-windows-cross: building holzWave $version from $branch at $commit"
[ "$branch" = main ] || echo "release-windows-cross: note: this is not main; a preview for the owner's test rows may come from a branch"
started=$(date +%s)

# The container that runs NSIS (and 7-Zip, to look into the installer afterwards). Built once.
build_image() {
    echo "+ docker build -t $IMAGE (Dockerfile from stdin)"
    docker build -t "$IMAGE" --build-arg "BASE_IMAGE=$BASE_IMAGE" - <<'DOCKERFILE'
ARG BASE_IMAGE
FROM ${BASE_IMAGE}
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update \
    && apt-get install -y --no-install-recommends nsis 7zip \
    && rm -rf /var/lib/apt/lists/* \
    && makensis -VERSION && 7z i >/dev/null
DOCKERFILE
}
docker image inspect "$IMAGE" >/dev/null 2>&1 || build_image || fail "the image $IMAGE could not be built"

# The `makensis` the bundler runs. It lives in a temporary directory that is first on PATH for the build command only.
cache="$HOME/Library/Caches/tauri"
mkdir -p "$cache" || exit 2
shim=$(mktemp -d "${TMPDIR:-/tmp}/holzwave-makensis.XXXXXX") || exit 2
cleanup() {
    rm -rf "$shim"
    git checkout -- app/src-tauri/Cargo.toml 2>/dev/null # the Tauri CLI rewrites feature lists there; the commit stays as it is
}
trap cleanup EXIT
printf '%s\n' '#!/bin/sh' \
    "exec docker run --rm -v \"$root:$root\" -v \"$cache:$cache\" -w \"\$PWD\" $IMAGE makensis \"\$@\"" >"$shim/makensis"
chmod +x "$shim/makensis" || exit 2

echo "+ npm --prefix app ci && npm --prefix app run build"
npm --prefix app ci >/dev/null && npm --prefix app run build >/dev/null || fail "npm ci or the page build"

echo "+ tauri build --target $TARGET --bundles nsis (mingw-w64 link, makensis in Docker)"
PATH="$shim:$PATH" CARGO_TARGET_X86_64_PC_WINDOWS_GNU_RUSTFLAGS="-C link-arg=-Wl,--no-gc-sections" \
    npm --prefix app run tauri -- build --target "$TARGET" --bundles nsis || fail "tauri build"

rel="target/$TARGET/release"
exe="$rel/holzwave-app.exe"
loader="$rel/WebView2Loader.dll"
built="$rel/bundle/nsis/holzWave_${version}_x64-setup.exe"
[ -f "$exe" ] || fail "no $exe"
[ -f "$built" ] || fail "no $built"

# ---- inspection --------------------------------------------------------------------------------------------------
echo "== executable: $exe"
ftype=$(file -b "$exe")
echo "$ftype"
case "$ftype" in
    "PE32+ executable (GUI) x86-64"*) ;;
    *) fail "the executable is not a 64-bit GUI PE (a console program opens a console window)" ;;
esac
imports=$(x86_64-w64-mingw32-objdump -p "$exe" | sed -n 's/^.*DLL Name: //p' | sort -u)
echo "imports: $(printf '%s' "$imports" | tr '\n' ' ')"
if printf '%s\n' "$imports" | grep -qiE '^(lib[^.]*|mingw.*)\.dll$'; then
    fail "the executable imports a mingw runtime DLL that a Windows machine does not have"
fi
printf '%s\n' "$imports" | grep -qx 'WebView2Loader.dll' || fail "the executable does not import WebView2Loader.dll (the GNU ABI loads it)"

echo "== installer: $built"
itype=$(file -b "$built")
echo "$itype"
case "$itype" in
    *"Nullsoft Installer self-extracting archive"*) ;;
    *) fail "the installer is not an NSIS installer" ;;
esac
bdir=$(cd "$(dirname "$built")" && pwd)
listing=$(docker run --rm -v "$bdir":/in:ro "$IMAGE" sh -c '7z l /in/*-setup.exe') || fail "7z could not read the installer"
printf '%s\n' "$listing" | grep -E '(holzwave-app|WebView2Loader)\.dll|holzwave-app\.exe|Type = |SubType'
printf '%s\n' "$listing" | grep -q ' holzwave-app\.exe$' || fail "the installer does not hold holzwave-app.exe"
printf '%s\n' "$listing" | grep -q ' WebView2Loader\.dll$' || fail "the installer does not hold WebView2Loader.dll"
# Take both files out of the installer (into a scratch directory under target/) and compare them with the built ones. The
# bundler writes the bundle type into a copy of the executable (the 3 letters of __TAURI_BUNDLE_TYPE_VAR_UNK become NSS),
# so the executable may differ in exactly those 3 bytes and must carry the NSS marker; the loader must be identical.
scratch="$root/target/release-cross-inspect"
rm -rf "$scratch"
mkdir -p "$scratch" || exit 1
docker run --rm -v "$bdir":/in:ro -v "$scratch":/out "$IMAGE" sh -c '7z e -bso0 -bsp0 -o/out /in/*-setup.exe holzwave-app.exe WebView2Loader.dll' ||
    fail "7z could not take the files out of the installer"
[ "$(cmp -l "$scratch/holzwave-app.exe" "$exe" | wc -l | tr -d ' ')" -le 3 ] || fail "holzwave-app.exe in the installer differs from the built one by more than the bundle type marker"
grep -aq '__TAURI_BUNDLE_TYPE_VAR_NSS' "$scratch/holzwave-app.exe" || fail "holzwave-app.exe in the installer does not carry the NSS bundle type marker"
cmp -s "$scratch/WebView2Loader.dll" "$loader" || fail "WebView2Loader.dll in the installer is not the built one"
rm -rf "$scratch"
echo "installer content: holzwave-app.exe (up to the bundle type marker) and WebView2Loader.dll are the built files"

# ---- the artifact: one name, one checksum ------------------------------------------------------------------------
name="holzWave_${version}_x64-setup.exe"
out=target/release-cross
mkdir -p "$out" || exit 1
cp "$built" "$out/$name" || exit 1
sum=$(shasum -a 256 "$out/$name" | cut -d' ' -f1)
echo "$sum  $name" >"$out/$name.sha256"
size=$(wc -c <"$out/$name" | tr -d ' ')

echo
echo "release-windows-cross: built $out/$name ($size bytes)"
echo "release-windows-cross: sha256 $sum"
echo "release-windows-cross: this is a GNU-ABI build (mingw-w64), not the MSVC build, and it is unsigned."
echo "release-windows-cross: it has never run on Windows: the owner's W rows of docs/test-plan.md are its first run."
echo "release-windows-cross: nothing was published; to attach it: gh release upload v$version $out/$name $out/$name.sha256"
echo "release-windows-cross: $(( $(date +%s) - started )) seconds"
echo "release-windows-cross: ok"
