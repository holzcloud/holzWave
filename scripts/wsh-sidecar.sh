#!/bin/sh
# Build `wsh` for one target and put it where the Tauri bundler looks for the sidecar.
#
#   sh scripts/wsh-sidecar.sh <rust target triple>
#
# The release scripts run this before `tauri build --config src-tauri/tauri.wsh.conf.json`. The overlay names the
# sidecar `binaries/wsh`; the bundler wants the file `app/src-tauri/binaries/wsh-<triple>` (with `.exe` for a Windows
# triple) and installs it as `wsh` beside the app executable, which is the folder a pane's PATH gets. The folder
# `app/src-tauri/binaries/` is ignored by git: the files are build products, made from the working tree at release time.
#
# Only the `wsh` package is built, so this needs no Zig and none of the app's dependencies. The caller names the
# triple: there is no default, because a sidecar of the wrong target in a package is exactly what the release scripts
# must not ship. For a cross target the caller sets the linker (CARGO_TARGET_<TRIPLE>_LINKER and _RUSTFLAGS).
#
# Exit status: 0 and the path on the last line, 1 when the build or the copy failed, 2 for a bad argument or a missing
# tool.

here=$(cd "$(dirname "$0")" && pwd)
root=$(dirname "$here")
cd "$root" || exit 2
. "$here/env.sh"

[ $# -eq 1 ] || {
    echo "usage: sh scripts/wsh-sidecar.sh <rust target triple>" >&2
    exit 2
}
triple=$1
case "$triple" in
    "" | -* | *[!a-z0-9_.-]*)
        echo "wsh-sidecar: not a target triple: $triple" >&2
        exit 2
        ;;
esac
# rustup from Homebrew is keg-only and not on PATH.
if ! command -v cargo >/dev/null 2>&1 && command -v brew >/dev/null 2>&1; then
    rustup_bin="$(brew --prefix rustup 2>/dev/null)/bin"
    [ -d "$rustup_bin" ] && PATH="$rustup_bin:$PATH"
fi
command -v cargo >/dev/null 2>&1 || {
    echo "wsh-sidecar: cargo is missing" >&2
    exit 2
}

ext=
case "$triple" in *windows*) ext=.exe ;; esac

cargo build --locked --release -p wsh --target "$triple" || {
    echo "wsh-sidecar: FAILED: cargo build for $triple" >&2
    exit 1
}
built="target/$triple/release/wsh$ext"
[ -f "$built" ] || {
    echo "wsh-sidecar: FAILED: no $built" >&2
    exit 1
}
dest_dir=app/src-tauri/binaries
dest="$dest_dir/wsh-$triple$ext"
mkdir -p "$dest_dir" || exit 1
rm -f "$dest"
cp "$built" "$dest" && chmod 0755 "$dest" || {
    echo "wsh-sidecar: FAILED: copying to $dest" >&2
    exit 1
}
echo "$dest"
