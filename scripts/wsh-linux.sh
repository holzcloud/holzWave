#!/bin/sh
# Build the two static Linux `wsh` binaries from any host, with no Linux cross compiler, and check them.
#
#   sh scripts/wsh-linux.sh [x86_64|aarch64|all]        (default: all)
#
# Result: app/src-tauri/binaries/wsh-linux-x86_64 and wsh-linux-aarch64 (mode 0755), the two resources that every
# package carries (app/src-tauri/tauri.wsh.conf.json) and that the app uploads to a remote Linux host.
#
# Why this works from a Mac while the core cannot: `wsh` depends only on the IPC crate (interprocess, serde,
# serde_json, getrandom). It has no C dependency and no `ring`, so the only foreign piece is the linker, and the
# musl target needs nothing from the host's C library. The linker is, in this order, the first that yields a binary
# that passes the checks below (the script prints which one):
#   1. `zig cc -target <arch>-linux-musl` through a small wrapper (Zig 0.16 is already a build requirement);
#   2. Rust's own `rust-lld` with self-contained linking.
# If neither links, the script exits 1 and prints both linker outputs. It adds no C cross compiler, uses no Docker
# and changes no manifest.
#
# Checks for every binary, with `file`: an ELF 64-bit LSB file, the machine (x86-64 or ARM aarch64), statically linked
# (`statically linked` or `static-pie linked`, never `dynamically linked`), and a size under 6 MiB.
#
# Needs: the Rust targets (once, a toolchain change and not a dependency):
#   rustup target add x86_64-unknown-linux-musl aarch64-unknown-linux-musl
#
# Exit status: 0 when every requested binary passed its checks, 1 when a build or a check failed, 2 for a bad
# argument or a missing tool.

here=$(cd "$(dirname "$0")" && pwd)
root=$(dirname "$here")
cd "$root" || exit 2
. "$here/env.sh"

MAX_BYTES=6291456
which=${1:-all}
[ $# -le 1 ] || {
    echo "usage: sh scripts/wsh-linux.sh [x86_64|aarch64|all]" >&2
    exit 2
}
case "$which" in
    x86_64) archs="x86_64" ;;
    aarch64) archs="aarch64" ;;
    all) archs="x86_64 aarch64" ;;
    *)
        echo "usage: sh scripts/wsh-linux.sh [x86_64|aarch64|all]" >&2
        exit 2
        ;;
esac

# rustup from Homebrew is keg-only and not on PATH.
if ! command -v cargo >/dev/null 2>&1 && command -v brew >/dev/null 2>&1; then
    rustup_bin="$(brew --prefix rustup 2>/dev/null)/bin"
    [ -d "$rustup_bin" ] && PATH="$rustup_bin:$PATH"
fi
command -v cargo >/dev/null 2>&1 || {
    echo "wsh-linux: cargo is missing" >&2
    exit 2
}
command -v file >/dev/null 2>&1 || {
    echo "wsh-linux: file is missing" >&2
    exit 2
}
command -v rustup >/dev/null 2>&1 || {
    echo "wsh-linux: rustup is missing" >&2
    exit 2
}
installed=$(rustup target list --installed 2>/dev/null)
for arch in $archs; do
    printf '%s\n' "$installed" | grep -qx "$arch-unknown-linux-musl" || {
        echo "wsh-linux: the Rust target $arch-unknown-linux-musl is missing: rustup target add $arch-unknown-linux-musl" >&2
        exit 2
    }
done

scratch=$(mktemp -d "${TMPDIR:-/tmp}/holzwave-wsh-linux.XXXXXX") || exit 2
trap 'rm -rf "$scratch"' EXIT INT TERM

# The checks of one built file: prints its `file` line and returns 0 or says what is wrong and returns 1.
check() { # <file> <arch>
    desc=$(file -b "$1")
    case "$2" in
        x86_64) machine="x86-64" ;;
        aarch64) machine="ARM aarch64" ;;
    esac
    magic=$(head -c 4 "$1" | od -An -c | tr -d ' \n')
    [ "$magic" = '177ELF' ] || {
        echo "wsh-linux: $1 is not an ELF file (magic $magic)" >&2
        return 1
    }
    case "$desc" in
        *"ELF 64-bit LSB"*) ;;
        *)
            echo "wsh-linux: $1 is not a 64-bit little-endian ELF: $desc" >&2
            return 1
            ;;
    esac
    case "$desc" in
        *"$machine"*) ;;
        *)
            echo "wsh-linux: $1 is not for $machine: $desc" >&2
            return 1
            ;;
    esac
    case "$desc" in
        *"dynamically linked"*)
            echo "wsh-linux: $1 is dynamically linked: $desc" >&2
            return 1
            ;;
        *"statically linked"* | *"static-pie linked"*) ;;
        *)
            echo "wsh-linux: $1 is not said to be statically linked: $desc" >&2
            return 1
            ;;
    esac
    size=$(wc -c <"$1" | tr -d ' ')
    [ "$size" -lt "$MAX_BYTES" ] || {
        echo "wsh-linux: $1 is $size bytes, over the bound of $MAX_BYTES" >&2
        return 1
    }
    echo "wsh-linux: $desc ($size bytes)"
    return 0
}

# One build with one linker; $1 is the arch, $2 the linker name. Leaves the output in $scratch/<arch>.<linker>.log.
build() { # <arch> <zig|lld>
    triple="$1-unknown-linux-musl"
    var=$(printf '%s' "$triple" | tr 'a-z-' 'A-Z_')
    log="$scratch/$1.$2.log"
    case "$2" in
        zig)
            command -v zig >/dev/null 2>&1 || {
                echo "zig is not on PATH" >"$log"
                return 1
            }
            # rustc passes --fix-cortex-a53-843419 for aarch64 (a link-time workaround for an old core's erratum) that
            # zig cc does not know; the wrapper drops it, every other argument goes through unchanged.
            printf '%s\n' '#!/bin/sh' \
                'for a; do shift; case "$a" in -Wl,--fix-cortex-a53-843419) ;; *) set -- "$@" "$a" ;; esac; done' \
                "exec zig cc -target $1-linux-musl \"\$@\"" >"$scratch/zig-cc-$1" &&
                chmod +x "$scratch/zig-cc-$1" || return 1
            env "CARGO_TARGET_${var}_LINKER=$scratch/zig-cc-$1" "CARGO_TARGET_${var}_RUSTFLAGS=-C link-self-contained=no" \
                cargo build --locked --release -p wsh --target "$triple" >"$log" 2>&1
            ;;
        lld)
            env "CARGO_TARGET_${var}_LINKER=rust-lld" "CARGO_TARGET_${var}_RUSTFLAGS=-C link-self-contained=yes -C linker-flavor=ld.lld" \
                cargo build --locked --release -p wsh --target "$triple" >"$log" 2>&1
            ;;
    esac
}

status=0
mkdir -p app/src-tauri/binaries || exit 1
for arch in $archs; do
    triple="$arch-unknown-linux-musl"
    out="app/src-tauri/binaries/wsh-linux-$arch"
    built="target/$triple/release/wsh"
    done_with=
    for linker in zig lld; do
        rm -f "$built"
        if build "$arch" "$linker" && [ -f "$built" ] && check "$built" "$arch" >/dev/null 2>"$scratch/$arch.$linker.check"; then
            done_with=$linker
            break
        fi
        [ -f "$scratch/$arch.$linker.check" ] && cat "$scratch/$arch.$linker.check" >>"$scratch/$arch.$linker.log"
    done
    if [ -z "$done_with" ]; then
        echo "wsh-linux: FAILED: neither linker produced a good $triple binary" >&2
        for linker in zig lld; do
            echo "---- $linker ----" >&2
            tail -n 25 "$scratch/$arch.$linker.log" >&2
        done
        status=1
        continue
    fi
    case "$done_with" in
        zig) name="zig cc" ;;
        lld) name="rust-lld" ;;
    esac
    echo "wsh-linux: $arch linked with $name"
    check "$built" "$arch" || {
        status=1
        continue
    }
    rm -f "$out"
    cp "$built" "$out" && chmod 0755 "$out" || {
        echo "wsh-linux: FAILED: copying to $out" >&2
        status=1
        continue
    }
    echo "wsh-linux: $out"
done
exit $status
