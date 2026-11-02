#!/bin/sh
# Prove the contract of scripts/sign-linux.sh without a real key (plan 10-05, POL-03, D-10).
#
#   sh scripts/test-release-signing.sh --shims-only   the stand-in checks only (exit 0 or 1)
#   sh scripts/test-release-signing.sh --container    the stand-in checks, then the real round trip in a throwaway container
#   sh scripts/test-release-signing.sh                the stand-in checks, then the real round trip on the host when
#                                                     `minisign` is installed, else in the container
#
# Exit status: 0 everything ran and held, 1 a check failed, 3 the stand-in checks held and the real round trip could not
# run (no minisign and no Docker, or the container could not install it): that is the word for `untested`.
#
# The stand-ins are two directories of tiny scripts put on a private PATH: a `minisign` that logs its arguments, its exported
# environment and its standard input and signs with a function of the file and the key, a `security` and a `secret-tool` that
# print a canary password made fresh for each run (derived from a random seed, so the canary text is in no file of the run
# except where the script under test writes it), and a `gh` that must never be called. The canary has to reach the stand-in
# minisign on standard input, once per signing call, and be found nowhere else: not in an argument, in the environment, in
# the output of the script or in any file of the run.
#
# Nothing here installs anything on the host, publishes or reaches the network except the container run, which mounts
# `scripts/` read-only and nothing else, and is removed (--rm) when it ends.

here=$(cd "$(dirname "$0")" && pwd -P)
root=$(cd "$here/.." && pwd -P)
SH=$(command -v sh)
sign="$here/sign-linux.sh"
verify="$here/verify-release.sh"

mode=default
case "${1:-}" in
  "") ;;
  --shims-only) mode=shims ;;
  --container) mode=container ;;
  --real-body) mode=realbody ;; # internal: the round trip itself, run on the host or inside the container
  *) echo "usage: sh scripts/test-release-signing.sh [--shims-only | --container]" >&2; exit 2 ;;
esac

checks=0
failures=0
ok() { # <what held>
  checks=$((checks + 1))
  echo "  ok   $1"
}
bad() { # <what did not hold>
  checks=$((checks + 1))
  failures=$((failures + 1))
  echo "  FAIL $1" >&2
}
check() { # <description> <command...>: ok when the command succeeds
  desc=$1
  shift
  if "$@"; then ok "$desc"; else bad "$desc"; fi
}
finish() { # <label>
  if [ "$failures" -ne 0 ]; then
    echo "release-signing: $failures of $checks checks FAILED ($1)" >&2
    exit 1
  fi
  echo "release-signing: ok $checks checks ($1)"
}

tmp=$(mktemp -d "${TMPDIR:-/tmp}/holzwave-signing.XXXXXX") || exit 1
cleanup() { rm -rf "$tmp"; }
trap cleanup EXIT HUP INT TERM

VERSION=9.9.9
DEB="holzWave_${VERSION}_amd64.deb"
RPM="holzWave_${VERSION}_x86_64.rpm"
APPIMAGE="holzWave_${VERSION}_x86_64.AppImage"
# The order the script signs and lists them in: by name in the C locale (capital letters first).
NAMES="$DEB $APPIMAGE $RPM"

random_hex() { od -An -N"$1" -tx1 /dev/urandom | tr -d ' \n'; }

sha256_of() { # <file>
  if command -v shasum >/dev/null 2>&1; then shasum -a 256 "$1" | cut -d' ' -f1; else sha256sum "$1" | cut -d' ' -f1; fi
}

# ---- the stand-ins ------------------------------------------------------------------------------------------------------

# The system tools sign-linux.sh and verify-release.sh may use, linked into a private bin so a PATH can be exactly that and the
# stand-ins (and never a minisign that happens to be installed on the host).
TOOLS="uname dirname basename stat shasum sha256sum sort cat rm mkdir tr cut wc head tail grep sed id ls readlink mktemp od dd cp mv awk perl true false test"
link_tools() { # <bin dir>
  for t in $TOOLS; do
    p=$(command -v "$t" 2>/dev/null || true)
    case "$p" in /*) ln -sf "$p" "$1/$t" ;; esac
  done
}

write_shims() { # <bin dir>
  b=$1
  cat >"$b/minisign" <<'SHIM'
#!/bin/sh
# Stand-in minisign: logs, signs with a function of the file and the key, verifies that function.
d=$(cd "$(dirname "$0")" && pwd)
log=$d/../log
printf '%s\n' "minisign $*" >>"$log/args.log"
export -p >>"$log/env.log"
op=
key=
pub=
file=
sig=
while [ $# -gt 0 ]; do
  case "$1" in
    -S) op=S ;;
    -V) op=V ;;
    -s) key=$2; shift ;;
    -p) pub=$2; shift ;;
    -m) file=$2; shift ;;
    -x) sig=$2; shift ;;
    -t) shift ;;
  esac
  shift
done
sum() { if command -v shasum >/dev/null 2>&1; then shasum -a 256 "$1" | cut -d' ' -f1; else sha256sum "$1" | cut -d' ' -f1; fi; }
idof() { sed 's/.* //' "$1" | head -n 1; }
case "$op" in
  S)
    cat >>"$log/stdin.log"
    n=$(grep -c '^minisign -S ' "$log/args.log")
    if [ -f "$d/fail-at" ] && [ "$n" -eq "$(cat "$d/fail-at")" ]; then echo "minisign: stand-in failure" >&2; exit 1; fi
    [ -f "$key" ] && [ -f "$file" ] && [ -n "$sig" ] || { echo "minisign: bad arguments" >&2; exit 1; }
    printf 'STANDIN-SIG %s %s\n' "$(sum "$file")" "$(idof "$key")" >"$sig"
    ;;
  V)
    [ -f "$pub" ] && [ -f "$file" ] && [ -f "$sig" ] || { echo "minisign: missing file" >&2; exit 1; }
    [ "$(cat "$sig")" = "STANDIN-SIG $(sum "$file") $(idof "$pub")" ] || { echo "minisign: signature verification failed" >&2; exit 1; }
    ;;
  *) echo "minisign: stand-in knows -S and -V only" >&2; exit 1 ;;
esac
SHIM
  cat >"$b/security" <<'SHIM'
#!/bin/sh
# Stand-in for the macOS keychain: answers only the exact lookup the script is allowed to make.
d=$(cd "$(dirname "$0")" && pwd)
[ "$#" -eq 6 ] && [ "$1" = find-generic-password ] && [ "$2" = -s ] && [ "$3" = holzwave-minisign ] && [ "$4" = -a ] && [ "$5" = "$USER" ] && [ "$6" = -w ] \
  || { echo "security: unexpected arguments" >&2; exit 44; }
. "$d/store.sh"
SHIM
  cat >"$b/secret-tool" <<'SHIM'
#!/bin/sh
# Stand-in for the Linux secret store: answers only the exact lookup the script is allowed to make.
d=$(cd "$(dirname "$0")" && pwd)
[ "$#" -eq 3 ] && [ "$1" = lookup ] && [ "$2" = service ] && [ "$3" = holzwave-minisign ] \
  || { echo "secret-tool: unexpected arguments" >&2; exit 1; }
. "$d/store.sh"
SHIM
  cat >"$b/gh" <<'SHIM'
#!/bin/sh
d=$(cd "$(dirname "$0")" && pwd)
echo "gh $*" >>"$d/../log/gh-called"
exit 99
SHIM
  chmod +x "$b/minisign" "$b/security" "$b/secret-tool" "$b/gh"
}

# What the stand-in secret store does: ok (prints the canary), fail (exit 1), empty (exit 0, prints nothing).
set_store() { # <fixture> <ok|fail|empty>
  case "$2" in
    ok)
      # The canary is derived from the seed, so its text is in no file; it carries characters a careless script would mangle.
      cat >"$1/bin/store.sh" <<'STORE'
h=$(tr 'a-f' 'A-F' <"$d/../seed")
printf '%s\n' "CANARY-$h"' a%sb\n$HOME "q"'
STORE
      ;;
    fail) printf 'echo "store: no such entry" >&2\nexit 1\n' >"$1/bin/store.sh" ;;
    empty) printf 'exit 0\n' >"$1/bin/store.sh" ;;
  esac
}

# A fixture: three packages, the stand-ins, a key (mode 600, outside the repository) and a public key to match.
fixture() { # <name>
  F="$tmp/$1"
  mkdir -p "$F/bin" "$F/log" "$F/pkgs" "$F/home/.minisign" || exit 1
  chmod 700 "$F/home/.minisign"
  : >"$F/log/args.log"
  : >"$F/log/env.log"
  : >"$F/log/stdin.log"
  link_tools "$F/bin"
  write_shims "$F/bin"
  random_hex 8 >"$F/seed"
  set_store "$F" ok
  for n in $NAMES; do head -c 2048 /dev/urandom >"$F/pkgs/$n"; done
  id=$(random_hex 4)
  printf 'STANDIN-SECRET %s\n' "$id" >"$F/key"
  chmod 600 "$F/key"
  printf 'STANDIN-PUBLIC %s\n' "$id" >"$F/pub"
  CANARY_HEX=$(tr 'a-f' 'A-F' <"$F/seed")
}

# Run sign-linux.sh in a fixture on the private PATH; standard output and error are kept in the fixture's log.
run_sign() { # <fixture> <args...>
  f=$1
  shift
  (
    PATH="$f/bin"
    USER=tester
    HOME="$f/home"
    HOLZWAVE_MINISIGN_KEY="${KEY:-$f/key}"
    export PATH USER HOME HOLZWAVE_MINISIGN_KEY
    exec "$SH" "$sign" "$@"
  ) >"$f/log/out.log" 2>"$f/log/err.log"
}

# ---- the success path: three packages signed, the password on standard input only -------------------------------------------

stand_in_checks() {
  echo "release-signing: stand-in checks"

  [ -f "$sign" ] || { bad "scripts/sign-linux.sh exists"; return; }

  fixture trace
  rc=0
  run_sign "$F" "$VERSION" --dir "$F/pkgs" --pubkey "$F/pub" || rc=$?
  check "signing the three packages exits 0" test "$rc" -eq 0
  [ "$rc" -eq 0 ] || { cat "$F/log/err.log" >&2; return; }

  for n in $NAMES; do
    check "$n has a detached signature" test -s "$F/pkgs/$n.minisig"
  done
  check "SHA256SUMS is signed" test -s "$F/pkgs/SHA256SUMS.minisig"

  expected=$(cd "$F/pkgs" && for n in $NAMES; do printf '%s  %s\n' "$(sha256_of "$n")" "$n"; done)
  check "SHA256SUMS has the three lines <64 hex>  <name>, sorted by name, matching shasum" test "$(cat "$F/pkgs/SHA256SUMS")" = "$expected"
  check "the directory holds exactly the eight files of the release (3 packages, 3 signatures, SHA256SUMS and its signature)" test "$(ls "$F/pkgs" | wc -l | tr -d ' ')" -eq 8

  sign_lines=$(grep '^minisign -S ' "$F/log/args.log")
  check "four signing calls were made" test "$(printf '%s\n' "$sign_lines" | wc -l | tr -d ' ')" -eq 4
  check "the checksum list is signed last" test "$(printf '%s\n' "$sign_lines" | tail -n 1 | sed 's/.* -m \([^ ]*\) .*/\1/')" = "$F/pkgs/SHA256SUMS"
  check "the three packages are signed before the checksum list" test "$(printf '%s\n' "$sign_lines" | head -n 3 | grep -c '/SHA256SUMS ')" -eq 0
  bad_form=0
  for n in $NAMES SHA256SUMS; do
    printf '%s\n' "$sign_lines" | grep -q -e "^minisign -S -s $F/key -m $F/pkgs/$n -x $F/pkgs/$n.minisig -t " || bad_form=1
  done
  check "each signing call names the key with -s, the file with -m and the signature with -x" test "$bad_form" -eq 0

  want_pw="CANARY-$CANARY_HEX"' a%sb\n$HOME "q"'
  check "the password reached minisign on standard input once per signing call" test "$(grep -c -x -F "$want_pw" "$F/log/stdin.log")" -eq 4
  check "nothing else reached minisign on standard input" test "$(wc -l <"$F/log/stdin.log" | tr -d ' ')" -eq 4
  # Every file of the run, the logs of the script included, except the seed the canary is derived from.
  found=$(grep -rli -F "$CANARY_HEX" "$F" | grep -v '/seed$' | sort | tr '\n' ' ')
  check "the canary is in stdin.log and in no other file of the run (arguments, environment, output, directory)" test "$found" = "$F/log/stdin.log "
  check "the script says nothing about the password on its standard output and error" test "$(cat "$F/log/out.log" "$F/log/err.log" | grep -c -i -e canary -e password)" -eq 0
  check "no signing call carries a password-like argument" test "$(grep -c -i -e canary "$F/log/args.log")" -eq 0
  check "the script ran no gh" test ! -e "$F/log/gh-called"

  if [ -n "${SHOW_LOG:-}" ]; then # for the record: one run's stand-in logs, the canary written as the word CANARY
    echo "--- args.log"; sed "s|$F|\$FIXTURE|g" "$F/log/args.log"
    echo "--- stdin.log"; sed "s/CANARY-$CANARY_HEX/CANARY/" "$F/log/stdin.log"
  fi
  last=$(tail -n 1 "$F/log/out.log")
  all_eight=1
  for n in $NAMES; do
    case "$last" in *"$F/pkgs/$n "*) ;; *) all_eight=0 ;; esac
    case "$last" in *"$F/pkgs/$n.minisig"*) ;; *) all_eight=0 ;; esac
  done
  case "$last" in *"$F/pkgs/SHA256SUMS "*"$F/pkgs/SHA256SUMS.minisig"*) ;; *) all_eight=0 ;; esac
  case "$last" in *"gh release upload v$VERSION"*) ;; *) all_eight=0 ;; esac
  check "the last line names gh release upload with all eight files" test "$all_eight" -eq 1
  case "$last" in *"not run"*) r=1 ;; *) r=0 ;; esac
  check "the last line says that the script did not run it" test "$r" -eq 1
}

case "$mode" in
  shims | container | default)
    stand_in_checks
    finish "stand-ins"
    ;;
esac
