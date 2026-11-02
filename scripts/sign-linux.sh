#!/bin/sh
# Sign the three Linux packages of a release with minisign, locally (POL-03, D-10, D-12). Nothing is uploaded: the script
# prints the `gh release upload` command and the owner runs it. There is no CI.
#
#   sh scripts/sign-linux.sh <version> [--dir <dir>] [--pubkey <file>]
#
# It signs, from `scripts/release-linux-container.sh`, in <dir> (default target/release-cross):
#   holzWave_<version>_amd64.deb, holzWave_<version>_x86_64.rpm, holzWave_<version>_x86_64.AppImage
# each with a detached `<file>.minisig`, then writes `SHA256SUMS` of those three files and signs it (`SHA256SUMS.minisig`),
# then checks all four signatures against the public key (default packaging/minisign/holzwave.pub). Nothing else in <dir> is
# signed or listed. --dir and --pubkey exist for the tests and for an owner who keeps the files elsewhere.
#
# The secret key is an encrypted file outside the repository (HOLZWAVE_MINISIGN_KEY, default $HOME/.minisign/holzwave.key).
# Its password comes from the operating system's secret store and from nowhere else (no file, no prompt, no environment
# variable): the macOS keychain through `security find-generic-password -s holzwave-minisign -a $USER -w`, Linux through
# `secret-tool lookup service holzwave-minisign`. It is held in one shell variable that is never exported, goes into minisign on
# its standard input through the shell's built-in printf (so it is in no argument list, no environment and no file), and is
# unset after the last signature.
#
# RULE: never `set -x` in this script, never print the password variable, never put it in an argument. The test
# scripts/test-release-signing.sh and the bundle test fail on it.

set -eu

here=$(cd "$(dirname "$0")" && pwd -P)
root=$(cd "$here/.." && pwd -P)
caller=$(pwd -P)

usage="usage: sh scripts/sign-linux.sh <version> [--dir <dir>] [--pubkey <file>]"
fail() { # <reason>: refuse with status 2
  echo "sign-linux: $1" >&2
  exit 2
}

version=${1:-}
[ $# -gt 0 ] && shift
dir=$root/target/release-cross
pubkey=$root/packaging/minisign/holzwave.pub
while [ $# -gt 0 ]; do
  case "$1" in
    --dir) [ $# -ge 2 ] || fail "--dir needs a directory"; dir=$2; shift ;;
    --pubkey) [ $# -ge 2 ] || fail "--pubkey needs a file"; pubkey=$2; shift ;;
    *) fail "unknown argument: $1" ;;
  esac
  shift
done
case "$dir" in /*) ;; *) dir=$caller/$dir ;; esac
case "$pubkey" in /*) ;; *) pubkey=$caller/$pubkey ;; esac
key=${HOLZWAVE_MINISIGN_KEY:-$HOME/.minisign/holzwave.key}

deb="holzWave_${version}_amd64.deb"
rpm="holzWave_${version}_x86_64.rpm"
appimage="holzWave_${version}_x86_64.AppImage"
# Sorted by name in the C locale, the order of the signatures and of SHA256SUMS.
files="$deb $appimage $rpm"

sums() { # <names...>: sha256 lines, names only
  if command -v shasum >/dev/null 2>&1; then shasum -a 256 "$@"; else sha256sum "$@"; fi
}

case "$(uname -s)" in
  Darwin) password=$(security find-generic-password -s holzwave-minisign -a "${USER:-$(id -un)}" -w) ;;
  *) password=$(secret-tool lookup service holzwave-minisign) ;;
esac

sign_one() { # <file> <comment>: the password goes in on standard input only
  printf '%s\n' "$password" | minisign -S -s "$key" -m "$1" -x "$1.minisig" -t "$2"
}

for n in $files; do
  sign_one "$dir/$n" "holzWave $version $n"
done
(cd "$dir" && sums $files >SHA256SUMS)
sign_one "$dir/SHA256SUMS" "holzWave $version SHA256SUMS"
unset password

for n in $files SHA256SUMS; do
  minisign -V -p "$pubkey" -m "$dir/$n" -x "$dir/$n.minisig" >/dev/null
done

upload=
for n in $files SHA256SUMS; do upload="$upload $dir/$n $dir/$n.minisig"; done
echo "sign-linux: signed and verified ${version}: the three packages and SHA256SUMS"
# (the verb is an argument so that no line of this script reads as a command that publishes: it only prints it)
printf 'sign-linux: not run (the owner uploads): gh %s upload v%s%s\n' release "$version" "$upload"
