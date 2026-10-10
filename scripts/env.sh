# Toolchain environment for local builds and tests of holzWave. Source it, do not run it:
#
#   . scripts/env.sh
#
# What it does: puts the stable Rust toolchain and Zig 0.16 on PATH, so that `cargo build`, `cargo test` and the
# libghostty-vt build step work from a fresh shell. Each directory is added only when it exists and is not on PATH
# yet, so sourcing the file twice changes nothing.
#
# Zig must be exactly 0.16.0. The pinned Ghostty commit needs it, and Zig 0.17 breaks Ghostty's build.zig
# (Phase 2 decision D-02 documents this blocker). Homebrew keeps 0.16 in a keg-only formula for that reason.
#
# This file is sourced into the interactive shell: it never calls `exit` and never sets `set -e`. It is plain POSIX
# sh and also works when sourced from bash or zsh.

_hw_prepend() {
    [ -d "$1" ] || return 0
    case ":$PATH:" in
        *":$1:"*) ;;
        *) PATH="$1:$PATH" ;;
    esac
}

# Order matters: the last directory prepended wins, so the rustup toolchain comes first on PATH.
_hw_prepend "$HOME/.cargo/bin"
_hw_prepend "/opt/homebrew/opt/zig@0.16/bin"

# The stable toolchain directory is named stable-<host triple>. It is found by listing the directory instead of a
# glob, so a shell that errors on an unmatched glob (zsh) behaves like one that does not.
if [ -d "$HOME/.rustup/toolchains" ]; then
    _hw_names=$(ls "$HOME/.rustup/toolchains" 2>/dev/null)
    while IFS= read -r _hw_name; do
        case "$_hw_name" in
            stable-*) _hw_prepend "$HOME/.rustup/toolchains/$_hw_name/bin" ;;
        esac
    done <<EOF
$_hw_names
EOF
    unset _hw_names _hw_name
fi

unset -f _hw_prepend 2>/dev/null || unset _hw_prepend
export PATH
