#!/bin/sh
# Check a signed Linux release the way a user would (POL-03, D-10): every signature against the public key, and every line of
# the checksum list against its file.
#
#   sh scripts/verify-release.sh <dir> [--arch <x86_64|aarch64> | --sums <list>] [--pubkey <file>]
#
# <dir> holds the packages, their `.minisig` files, the list (`SHA256SUMS` for x86_64, `SHA256SUMS-aarch64` for aarch64) and its
# `.minisig`, as `scripts/sign-linux.sh` leaves them and as the GitHub release offers them. Without --arch and --sums the list
# is `SHA256SUMS`, as before; `--arch aarch64` is the same as `--sums SHA256SUMS-aarch64`. A folder that holds both
# architectures is verified once per list. The public key defaults to packaging/minisign/holzwave.pub of this repository.
# Checked, one line each:
#   - the list against its `.minisig`
#   - for each file named in the list: the file exists, its `.minisig` verifies, and its hash is the one listed
#   - a `.minisig` whose file no list of the folder names is a failure (a signature that nothing covers). A file that only
#     another list names is covered by that list: it is checked when that list is verified, not in this run.
# The names in a list are untrusted input: a name with a path part or a leading dash is a failure, never opened. A list
# name given with --sums is checked the same way: it must be `SHA256SUMS` followed by letters, digits, dots, plus or
# hyphens, and nothing else is accepted.
#
# Exit status: 0 everything holds; 1 anything failed (a changed byte, a missing signature, a wrong key); 2 minisign, the key or the
# directory is missing. It reads files and runs minisign and a checksum tool; it changes nothing and reaches no network.

set -eu

here=$(cd "$(dirname "$0")" && pwd -P)
root=$(cd "$here/.." && pwd -P)
caller=$(pwd -P)

usage="usage: sh scripts/verify-release.sh <dir> [--arch <x86_64|aarch64> | --sums <list>] [--pubkey <file>]"
refuse() { # <reason>
  echo "verify-release: $1" >&2
  exit 2
}

dir=${1:-}
[ -n "$dir" ] || refuse "$usage"
shift
pubkey=$root/packaging/minisign/holzwave.pub
sums=SHA256SUMS
picked=0
while [ $# -gt 0 ]; do
  case "$1" in
    --arch)
      [ $# -ge 2 ] || refuse "--arch needs x86_64 or aarch64"
      [ "$picked" -eq 0 ] || refuse "give --arch or --sums once, not both"
      case "$2" in
        x86_64) sums=SHA256SUMS ;;
        aarch64) sums=SHA256SUMS-aarch64 ;;
        *) refuse "unknown architecture for --arch: $2 (x86_64 or aarch64)" ;;
      esac
      picked=1
      shift
      ;;
    --sums)
      [ $# -ge 2 ] || refuse "--sums needs a list name"
      [ "$picked" -eq 0 ] || refuse "give --arch or --sums once, not both"
      printf '%s\n' "$2" | grep -Eq '^SHA256SUMS[A-Za-z0-9._+-]*$' \
        || refuse "not a list name (SHA256SUMS or SHA256SUMS-<name>, no path part, no leading dash): $2"
      case "$2" in *.minisig) refuse "a list name does not end in .minisig: $2" ;; esac
      sums=$2
      picked=1
      shift
      ;;
    --pubkey) [ $# -ge 2 ] || refuse "--pubkey needs a file"; pubkey=$2; shift ;;
    *) refuse "unknown argument: $1" ;;
  esac
  shift
done
case "$dir" in /*) ;; *) dir=$caller/$dir ;; esac
case "$pubkey" in /*) ;; *) pubkey=$caller/$pubkey ;; esac

command -v minisign >/dev/null 2>&1 || refuse "minisign is not on the PATH (macOS: brew install minisign; Debian and Ubuntu: apt install minisign)"
[ -f "$pubkey" ] || refuse "the public key file is missing: $pubkey"
[ -d "$dir" ] || refuse "not a directory: $dir"

failed=0
pass() { echo "verify-release: ok     $1"; }
miss() {
  failed=1
  echo "verify-release: FAILED $1" >&2
}

sig_ok() { # <name>: the signature of dir/<name> verifies against the public key
  [ -f "$dir/$1" ] || { miss "$1: the file is missing"; return; }
  [ -f "$dir/$1.minisig" ] || { miss "$1: no signature ($1.minisig is missing)"; return; }
  if minisign -V -q -p "$pubkey" -m "$dir/$1" -x "$dir/$1.minisig" >/dev/null 2>&1; then
    pass "$1: signature"
  else
    miss "$1: the signature does not verify against $pubkey"
  fi
}

listed=
if [ -f "$dir/$sums" ]; then
  sig_ok "$sums"
  while IFS= read -r line || [ -n "$line" ]; do
    if ! printf '%s\n' "$line" | grep -Eq '^[0-9a-f]{64}  [A-Za-z0-9_][A-Za-z0-9._+-]*$'; then
      miss "$sums: a line is not '<64 hex>  <file name>' (or the name has a path part or a leading dash)"
      continue
    fi
    name=${line#*  }
    listed="$listed $name"
    sig_ok "$name"
    if [ -f "$dir/$name" ]; then
      if (cd "$dir" && if command -v shasum >/dev/null 2>&1; then printf '%s\n' "$line" | shasum -a 256 -c; else printf '%s\n' "$line" | sha256sum -c; fi) >/dev/null 2>&1; then
        pass "$name: sha256"
      else
        miss "$name: the sha256 is not the one in $sums"
      fi
    fi
  done <"$dir/$sums"
  [ -n "$listed" ] || miss "$sums names no file"
else
  miss "$sums is missing"
fi

# What the other lists of the folder name (an architecture's list covers its own files). Names are read only when they have
# the shape of a file name; a line that does not is that list's own failure, found when it is verified.
others=
lists=
for other in "$dir"/SHA256SUMS*; do
  [ -f "$other" ] || continue
  oname=$(basename "$other")
  case "$oname" in *.minisig) continue ;; esac
  printf '%s\n' "$oname" | grep -Eq '^SHA256SUMS[A-Za-z0-9._+-]*$' || continue
  lists="$lists $oname"
  [ "$oname" = "$sums" ] && continue
  while IFS= read -r line || [ -n "$line" ]; do
    printf '%s\n' "$line" | grep -Eq '^[0-9a-f]{64}  [A-Za-z0-9_][A-Za-z0-9._+-]*$' || continue
    others="$others ${line#*  }"
  done <"$other"
done

for sig in "$dir"/*.minisig; do
  [ -e "$sig" ] || continue
  base=$(basename "$sig" .minisig)
  [ "$base" = "$sums" ] && continue
  case " $lists " in
    *" $base "*) continue ;; # the signature of another list that is in the folder
  esac
  case " $listed $others " in
    *" $base "*) ;;
    *) miss "$base.minisig: $sums does not name $base, so the list does not cover this signature, and no other list of the folder names it" ;;
  esac
done

if [ "$failed" -eq 0 ]; then
  echo "verify-release: ok"
  exit 0
fi
echo "verify-release: FAILED" >&2
exit 1
