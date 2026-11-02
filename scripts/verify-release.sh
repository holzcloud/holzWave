#!/bin/sh
# Check a signed Linux release the way a user would (POL-03, D-10): every signature against the public key, and every line of
# SHA256SUMS against its file.
#
#   sh scripts/verify-release.sh <dir> [--pubkey <file>]
#
# <dir> holds the packages, their `.minisig` files, `SHA256SUMS` and `SHA256SUMS.minisig`, as `scripts/sign-linux.sh` leaves them
# and as the GitHub release offers them. The public key defaults to packaging/minisign/holzwave.pub of this repository.
# Checked, one line each:
#   - SHA256SUMS against SHA256SUMS.minisig
#   - for each file named in SHA256SUMS: the file exists, its `.minisig` verifies, and its hash is the one listed
#   - a `.minisig` whose file SHA256SUMS does not name is a failure (a signature the list does not cover)
# The names in SHA256SUMS are untrusted input: a name with a path part or a leading dash is a failure, never opened.
#
# Exit status: 0 everything holds; 1 anything failed (a changed byte, a missing signature, a wrong key); 2 minisign, the key or the
# directory is missing. It reads files and runs minisign and a checksum tool; it changes nothing and reaches no network.

set -eu

here=$(cd "$(dirname "$0")" && pwd -P)
root=$(cd "$here/.." && pwd -P)
caller=$(pwd -P)

refuse() { # <reason>
  echo "verify-release: $1" >&2
  exit 2
}

dir=${1:-}
[ -n "$dir" ] || refuse "usage: sh scripts/verify-release.sh <dir> [--pubkey <file>]"
shift
pubkey=$root/packaging/minisign/holzwave.pub
while [ $# -gt 0 ]; do
  case "$1" in
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
if [ -f "$dir/SHA256SUMS" ]; then
  sig_ok SHA256SUMS
  while IFS= read -r line || [ -n "$line" ]; do
    if ! printf '%s\n' "$line" | grep -Eq '^[0-9a-f]{64}  [A-Za-z0-9_][A-Za-z0-9._+-]*$'; then
      miss "SHA256SUMS: a line is not '<64 hex>  <file name>' (or the name has a path part or a leading dash)"
      continue
    fi
    name=${line#*  }
    listed="$listed $name"
    sig_ok "$name"
    if [ -f "$dir/$name" ]; then
      if (cd "$dir" && if command -v shasum >/dev/null 2>&1; then printf '%s\n' "$line" | shasum -a 256 -c; else printf '%s\n' "$line" | sha256sum -c; fi) >/dev/null 2>&1; then
        pass "$name: sha256"
      else
        miss "$name: the sha256 is not the one in SHA256SUMS"
      fi
    fi
  done <"$dir/SHA256SUMS"
  [ -n "$listed" ] || miss "SHA256SUMS names no file"
else
  miss "SHA256SUMS is missing"
fi

for sig in "$dir"/*.minisig; do
  [ -e "$sig" ] || continue
  base=$(basename "$sig" .minisig)
  [ "$base" = SHA256SUMS ] && continue
  case " $listed " in
    *" $base "*) ;;
    *) miss "$base.minisig: SHA256SUMS does not name $base, so the list does not cover this signature" ;;
  esac
done

if [ "$failed" -eq 0 ]; then
  echo "verify-release: ok"
  exit 0
fi
echo "verify-release: FAILED" >&2
exit 1
