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
# Exit status: 0 signed and verified; 2 refused before anything was touched (the reason is on standard error); 1 minisign or a
# verification failed (the files this run made are removed, so no half-signed directory is left).
#
# RULE: never `set -x` in this script, never print the password variable, never put it in an argument. The test
# scripts/test-release-signing.sh and the bundle test fail on it, and the script refuses to run while tracing is on.

set -eu

here=$(cd "$(dirname "$0")" && pwd -P)
root=$(cd "$here/.." && pwd -P)
caller=$(pwd -P)

die() { # <status> <reason>
  echo "sign-linux: $2" >&2
  exit "$1"
}
fail() { die 2 "$1"; } # a refusal: nothing was touched

case "$-" in *x*) fail "tracing is on (sh -x): it would print the password; run the script without it" ;; esac

version=${1:-}
[ $# -gt 0 ] || fail "usage: sh scripts/sign-linux.sh <version> [--dir <dir>] [--pubkey <file>]"
shift
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
key=${HOLZWAVE_MINISIGN_KEY:-${HOME:-/nonexistent}/.minisign/holzwave.key}

# 1. the version is a version (the three names are built from it, so a stale or foreign file is never named by accident)
case "$version" in
  *[!0-9A-Za-z.-]*) fail "the version argument is not a version: $version" ;;
esac
printf '%s\n' "$version" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z]+(\.[0-9A-Za-z]+)*)?$' \
  || fail "the version argument is not a version (like 0.0.1 or 0.0.1-alpha1): $version"

deb="holzWave_${version}_amd64.deb"
rpm="holzWave_${version}_x86_64.rpm"
appimage="holzWave_${version}_x86_64.AppImage"
# Sorted by name in the C locale, the order of the signatures and of SHA256SUMS.
files="$deb $appimage $rpm"

# 2. the tools
command -v minisign >/dev/null 2>&1 || fail "minisign is not on the PATH (macOS: brew install minisign)"
case "$(uname -s)" in
  Darwin) command -v security >/dev/null 2>&1 || fail "the macOS keychain tool (security) is not on the PATH" ;;
  *) command -v secret-tool >/dev/null 2>&1 || fail "secret-tool is not on the PATH (libsecret-tools)" ;;
esac

# 3. the key: present, outside the repository (also through a symlink), and private to this account
[ -f "$key" ] || fail "the secret key file is missing: $key (create it with minisign -G; see the signing doc)"
real=$key
hops=0
while [ -L "$real" ] && [ "$hops" -lt 20 ]; do
  target=$(readlink "$real")
  case "$target" in /*) real=$target ;; *) real=$(dirname "$real")/$target ;; esac
  hops=$((hops + 1))
done
realdir=$(cd "$(dirname "$real")" && pwd -P) || fail "the directory of the secret key cannot be entered: $key"
case "$realdir/" in
  "$root"/*) fail "the secret key is inside the repository ($realdir): keep it outside, for example in \$HOME/.minisign" ;;
esac
case "$(uname -s)" in
  Darwin) mode=$(stat -L -f %Lp "$key") ;;
  *) mode=$(stat -L -c %a "$key") ;;
esac
case "$mode" in
  0 | 00 | ?00 | ??00) ;; # no group and no other bits (400, 600, 0600)
  *) fail "the secret key is readable by group or others (mode $mode): chmod 600 $key" ;;
esac

# 4. the public key and the packages
[ -f "$pubkey" ] || fail "the public key file is missing: $pubkey"
for n in $files; do
  [ -f "$dir/$n" ] || fail "a package of version $version is missing: $dir/$n"
done

# 5. the password, from the secret store only; an entry that is missing or empty is a refusal
case "$(uname -s)" in
  Darwin) password=$(security find-generic-password -s holzwave-minisign -a "${USER:-$(id -un)}" -w 2>/dev/null) \
    || fail "the keychain has no entry holzwave-minisign for this user, or answered an error (security add-generic-password -s holzwave-minisign -a \$USER -w)" ;;
  *) password=$(secret-tool lookup service holzwave-minisign 2>/dev/null) \
    || fail "the secret store has no entry (service holzwave-minisign), or answered an error" ;;
esac
[ -n "$password" ] || fail "the secret store gave an empty password for holzwave-minisign"

# Every precondition holds. From here on the files this run makes are removed if it does not finish.
finished=0
partial() {
  [ "$finished" -eq 1 ] || for n in $files SHA256SUMS; do rm -f "$dir/$n.minisig"; done
  [ "$finished" -eq 1 ] || rm -f "$dir/SHA256SUMS"
}
trap partial EXIT
trap 'exit 130' INT
trap 'exit 143' HUP TERM
for n in $files SHA256SUMS; do rm -f "$dir/$n.minisig"; done
rm -f "$dir/SHA256SUMS"

sums() { # <names...>: sha256 lines, names only
  if command -v shasum >/dev/null 2>&1; then shasum -a 256 "$@"; else sha256sum "$@"; fi
}

sign_one() { # <file> <comment>: the password goes in on standard input only
  printf '%s\n' "$password" | minisign -S -s "$key" -m "$1" -x "$1.minisig" -t "$2"
}

for n in $files; do
  sign_one "$dir/$n" "holzWave $version $n" || die 1 "minisign failed signing $n"
done
(cd "$dir" && sums $files >SHA256SUMS) || die 1 "could not write SHA256SUMS"
sign_one "$dir/SHA256SUMS" "holzWave $version SHA256SUMS" || die 1 "minisign failed signing SHA256SUMS"
unset password

for n in $files SHA256SUMS; do
  minisign -V -p "$pubkey" -m "$dir/$n" -x "$dir/$n.minisig" >/dev/null 2>&1 \
    || die 1 "the signature of $n does not verify against $pubkey (wrong key pair?)"
done
finished=1

upload=
for n in $files SHA256SUMS; do upload="$upload $dir/$n $dir/$n.minisig"; done
echo "sign-linux: signed and verified ${version}: the three packages and SHA256SUMS"
# (the verb is an argument so that no line of this script reads as a command that publishes: it only prints it)
printf 'sign-linux: not run (the owner uploads): gh %s upload v%s%s\n' release "$version" "$upload"
