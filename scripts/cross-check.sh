#!/bin/sh
# Compile gate for Windows and Linux, run from the Mac. The cheap rung of the cross-check ladder of Phase 2.2: it
# proves that the code compiles for the other targets, it does not link and it runs nothing.
#
#   sh scripts/cross-check.sh [core|app|all]      (default: core)
#
# Modes:
#
#   core  `cargo check --all-targets` of holzwave-core for x86_64-unknown-linux-gnu and x86_64-pc-windows-gnu, so
#         every test target is compiled for both.
#   app   `cargo check --all-targets` of holzwave-app for x86_64-pc-windows-gnu. Tauri's build script needs a
#         resource compiler named x86_64-w64-mingw32-windres: a temporary directory gets a symlink of that name to
#         llvm-windres (Homebrew's llvm), is put first on PATH for that one command and removed afterwards.
#         The app crate for Linux cannot be checked from macOS (pkg-config for GTK and WebKit): use
#         scripts/linux-container-test.sh app.
#   all   both.
#
# The Windows target is the GNU ABI on purpose: Zig, which builds libghostty-vt, has no MSVC headers on a Mac, so
# the MSVC build and link only happen on a Windows host.
#
# Exit status: 0 when every check passed (the last line is "cross-check: ok"), 1 when a check failed, 2 when a tool
# or a rustup target is missing. A missing tool is a failed gate, never a skip. The script installs nothing.

TARGETS="x86_64-unknown-linux-gnu x86_64-pc-windows-gnu"

here=$(cd "$(dirname "$0")" && pwd)
cd "$here/.." || exit 2
. "$here/env.sh"

mode=${1:-core}
case "$mode" in
    core | app | all) ;;
    *)
        echo "usage: sh scripts/cross-check.sh [core|app|all]" >&2
        exit 2
        ;;
esac

# rustup is only needed to list the installed targets. A Homebrew rustup is keg-only and not on PATH.
rustup_bin=$(command -v rustup)
if [ -z "$rustup_bin" ] && command -v brew >/dev/null 2>&1; then
    candidate="$(brew --prefix rustup 2>/dev/null)/bin/rustup"
    [ -x "$candidate" ] && rustup_bin=$candidate
fi
if [ -z "$rustup_bin" ]; then
    echo "cross-check: rustup is missing: install rustup (brew install rustup) and run it once" >&2
    exit 2
fi
if ! command -v cargo >/dev/null 2>&1; then
    echo "cross-check: cargo is missing: install the stable toolchain with rustup" >&2
    exit 2
fi

installed=$("$rustup_bin" target list --installed 2>/dev/null)
missing=
for target in $TARGETS; do
    printf '%s\n' "$installed" | grep -qx "$target" || missing="$missing $target"
done
if [ -n "$missing" ]; then
    echo "cross-check: rustup target missing:$missing" >&2
    echo "cross-check: install both once (a toolchain change, not a dependency): rustup target add $TARGETS" >&2
    exit 2
fi

failed=0

# Echo a command, run it and remember a failure; the remaining checks still run.
run() {
    echo "+ $*"
    "$@" || failed=1
}

check_core() {
    for target in $TARGETS; do
        run cargo check --locked --all-targets -p holzwave-core --target "$target"
    done
}

# A directory with x86_64-w64-mingw32-windres -> llvm-windres, for the one command that needs it.
shim=
cleanup() {
    [ -n "$shim" ] && rm -rf "$shim"
}
trap cleanup EXIT

check_app() {
    windres=$(command -v llvm-windres)
    if [ -z "$windres" ] && command -v brew >/dev/null 2>&1; then
        # Homebrew's llvm is keg-only: the unversioned formula or a versioned one (llvm@21), not on PATH.
        brew_prefix=$(brew --prefix 2>/dev/null)
        for candidate in "$brew_prefix"/opt/llvm/bin/llvm-windres "$brew_prefix"/opt/llvm@*/bin/llvm-windres; do
            [ -x "$candidate" ] && windres=$candidate
        done
    fi
    if [ -z "$windres" ]; then
        echo "cross-check: llvm-windres is missing: brew install llvm (Tauri's build script needs a windres)" >&2
        exit 2
    fi
    shim=$(mktemp -d "${TMPDIR:-/tmp}/holzwave-windres.XXXXXX") || exit 2
    ln -s "$windres" "$shim/x86_64-w64-mingw32-windres" || exit 2
    echo "+ PATH=$shim:\$PATH cargo check --locked --all-targets -p holzwave-app --target x86_64-pc-windows-gnu"
    PATH="$shim:$PATH" cargo check --locked --all-targets -p holzwave-app --target x86_64-pc-windows-gnu || failed=1
    cleanup
    shim=
}

case "$mode" in
    core) check_core ;;
    app) check_app ;;
    all)
        check_core
        check_app
        ;;
esac

if [ "$failed" -ne 0 ]; then
    echo "cross-check: FAILED" >&2
    exit 1
fi
echo "cross-check: ok"
