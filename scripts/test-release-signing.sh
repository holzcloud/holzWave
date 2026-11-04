#!/bin/sh
# Prove the contract of scripts/sign-linux.sh without a real key (POL-03, D-10).
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
REPO_TMP=
cleanup() {
  rm -rf "$tmp"
  [ -z "$REPO_TMP" ] || rm -rf "$REPO_TMP"
}
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
    [ "${KEY:-}" != default ] || unset HOLZWAVE_MINISIGN_KEY
    # shellcheck disable=SC2086 # SHFLAGS is a flag list on purpose (for example -x)
    exec "$SH" ${SHFLAGS:-} "$sign" "$@"
  ) >"$f/log/out.log" 2>"$f/log/err.log"
}

# ---- the success path: three packages signed, the password on standard input only -------------------------------------------

stand_in_checks() {
  echo "release-signing: stand-in checks"

  [ -f "$sign" ] || { bad "scripts/sign-linux.sh exists"; return; }

  fixture trace
  before=$(cd "$F/pkgs" && for n in $NAMES; do sha256_of "$n"; done)
  : >"$F/pkgs/holzWave_${VERSION}_amd64.deb.sha256" # what release-linux-container.sh also leaves there
  echo "not a package" >"$F/pkgs/README"
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
  check "the directory holds the eight files of the release (3 packages, 3 signatures, SHA256SUMS and its signature) and the two it had" test "$(ls "$F/pkgs" | wc -l | tr -d ' ')" -eq 10
  check "only the three packages and the checksum list are signed or listed (the .sha256 and README are left alone)" test ! -e "$F/pkgs/README.minisig" -a ! -e "$F/pkgs/holzWave_${VERSION}_amd64.deb.sha256.minisig" -a "$(wc -l <"$F/pkgs/SHA256SUMS" | tr -d ' ')" -eq 3
  after=$(cd "$F/pkgs" && for n in $NAMES; do sha256_of "$n"; done)
  check "signing did not change a package" test "$before" = "$after"

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


# ---- the refusals: exit 2, one line that names the cause, nothing signed ----------------------------------------------------

refusal() { # <label> <fragment of the reason> <fixture> <args...>
  label=$1
  frag=$2
  f=$3
  shift 3
  rc=0
  run_sign "$f" "$@" || rc=$?
  check "$label: exit status 2" test "$rc" -eq 2
  check "$label: one line on standard error that says why ($frag)" test "$(wc -l <"$f/log/err.log" | tr -d ' ')" -eq 1 -a "$(grep -c -F -- "$frag" "$f/log/err.log")" -eq 1
  check "$label: no signature and no SHA256SUMS" test -z "$(ls "$f/pkgs" | grep -e minisig -e SHA256SUMS || true)"
  check "$label: minisign was never called and no secret leaked" test "$(wc -c <"$f/log/args.log" | tr -d ' ')" -eq 0 -a "$(grep -rlF "$CANARY_HEX" "$f/log" | wc -l | tr -d ' ')" -eq 0
}

refusal_checks() {
  echo "release-signing: refusals"
  fixture r-nominisign
  rm -f "$F/bin/minisign"
  refusal "no minisign on the PATH" "minisign is not on the PATH" "$F" "$VERSION" --dir "$F/pkgs" --pubkey "$F/pub"

  fixture r-nokey
  KEY="$F/nokey" refusal "missing key file" "secret key file is missing" "$F" "$VERSION" --dir "$F/pkgs" --pubkey "$F/pub"

  fixture r-inrepo
  REPO_TMP=$(mktemp -d "$root/.signtest.XXXXXX")
  cp "$F/key" "$REPO_TMP/key"
  chmod 600 "$REPO_TMP/key"
  KEY="$REPO_TMP/key" refusal "key file inside the repository" "inside the repository" "$F" "$VERSION" --dir "$F/pkgs" --pubkey "$F/pub"
  ln -s "$REPO_TMP/key" "$F/linkkey"
  KEY="$F/linkkey" refusal "key reached through a symlink into the repository" "inside the repository" "$F" "$VERSION" --dir "$F/pkgs" --pubkey "$F/pub"
  rm -rf "$REPO_TMP"
  REPO_TMP=

  fixture r-mode644
  chmod 644 "$F/key"
  refusal "key file with mode 644" "readable by group or others" "$F" "$VERSION" --dir "$F/pkgs" --pubkey "$F/pub"
  chmod 640 "$F/key"
  refusal "key file with mode 640" "readable by group or others" "$F" "$VERSION" --dir "$F/pkgs" --pubkey "$F/pub"
  chmod 400 "$F/key"
  rc=0
  run_sign "$F" "$VERSION" --dir "$F/pkgs" --pubkey "$F/pub" || rc=$?
  check "key file with mode 400 is accepted" test "$rc" -eq 0

  fixture r-nopub
  refusal "missing public key file" "public key file is missing" "$F" "$VERSION" --dir "$F/pkgs" --pubkey "$F/nopub"
  refusal "missing default public key file (packaging/minisign/holzwave.pub is the owner's to publish)" "public key file is missing" "$F" "$VERSION" --dir "$F/pkgs"

  fixture r-storefail
  set_store "$F" fail
  refusal "secret store that exits 1" "no entry" "$F" "$VERSION" --dir "$F/pkgs" --pubkey "$F/pub"

  fixture r-storeempty
  set_store "$F" empty
  refusal "secret store that prints nothing" "empty password" "$F" "$VERSION" --dir "$F/pkgs" --pubkey "$F/pub"

  fixture r-nostore
  rm -f "$F/bin/security" "$F/bin/secret-tool"
  refusal "no secret store tool at all" "is not on the PATH" "$F" "$VERSION" --dir "$F/pkgs" --pubkey "$F/pub"

  fixture r-nopkg
  rm -f "$F/pkgs/$RPM"
  echo old >"$F/pkgs/$DEB.minisig"
  rc=0
  run_sign "$F" "$VERSION" --dir "$F/pkgs" --pubkey "$F/pub" || rc=$?
  check "missing package: exit status 2" test "$rc" -eq 2
  check "missing package: the reason names the file" grep -q -F "$RPM" "$F/log/err.log"
  check "missing package: an old signature is not removed by a refused run" test "$(cat "$F/pkgs/$DEB.minisig")" = old
  rm -f "$F/pkgs/$DEB.minisig"
  refusal "no packages directory" "a package of version $VERSION is missing" "$F" "$VERSION" --dir "$F/nowhere" --pubkey "$F/pub"

  fixture r-version
  refusal "version latest" "not a version" "$F" latest --dir "$F/pkgs" --pubkey "$F/pub"
  refusal "version 1.0" "not a version" "$F" 1.0 --dir "$F/pkgs" --pubkey "$F/pub"
  refusal "version with a path part" "not a version" "$F" "../$VERSION" --dir "$F/pkgs" --pubkey "$F/pub"
  refusal "no argument" "usage" "$F"
  refusal "unknown option" "unknown argument" "$F" "$VERSION" --upload --dir "$F/pkgs"

  fixture r-trace
  rc=0
  SHFLAGS=-x run_sign "$F" "$VERSION" --dir "$F/pkgs" --pubkey "$F/pub" || rc=$?
  check "tracing on (sh -x): exit status 2" test "$rc" -eq 2
  check "tracing on: the reason says so" grep -q -F "tracing is on" "$F/log/err.log"
  check "tracing on: the secret store was never asked, so no trace shows a password" test "$(grep -rlF "$CANARY_HEX" "$F/log" | wc -l | tr -d ' ')" -eq 0
}

# ---- failures after the preconditions: exit 1, and no half-signed directory -------------------------------------------------

failure_checks() {
  echo "release-signing: failures after the start"
  for at in 2 4; do
    fixture f-sign-$at
    echo "$at" >"$F/bin/fail-at"
    rc=0
    run_sign "$F" "$VERSION" --dir "$F/pkgs" --pubkey "$F/pub" || rc=$?
    check "minisign fails on signing call $at: exit status 1" test "$rc" -eq 1
    check "minisign fails on signing call $at: the reason is one line" test "$(wc -l <"$F/log/err.log" | tr -d ' ')" -eq 2 -a "$(grep -c 'minisign failed signing' "$F/log/err.log")" -eq 1
    check "minisign fails on signing call $at: the partial output is removed" test -z "$(ls "$F/pkgs" | grep -e minisig -e SHA256SUMS || true)"
    check "minisign fails on signing call $at: the three packages are still there" test "$(ls "$F/pkgs" | wc -l | tr -d ' ')" -eq 3
    check "minisign fails on signing call $at: no canary in the output" test "$(cat "$F/log/out.log" "$F/log/err.log" | grep -c -F "$CANARY_HEX")" -eq 0
  done

  fixture f-wrongpub
  printf 'STANDIN-PUBLIC %s\n' "$(random_hex 4)" >"$F/otherpub"
  rc=0
  run_sign "$F" "$VERSION" --dir "$F/pkgs" --pubkey "$F/otherpub" || rc=$?
  check "signatures that do not verify against the public key: exit status 1" test "$rc" -eq 1
  check "...the reason says so" grep -q "does not verify" "$F/log/err.log"
  check "...and nothing signed is left behind" test -z "$(ls "$F/pkgs" | grep -e minisig -e SHA256SUMS || true)"

  fixture f-rerun
  echo old >"$F/pkgs/SHA256SUMS.minisig"
  run_sign "$F" "$VERSION" --dir "$F/pkgs" --pubkey "$F/pub" && run_sign "$F" "$VERSION" --dir "$F/pkgs" --pubkey "$F/pub"
  check "a second run replaces the old signatures and still leaves the eight files" test "$(ls "$F/pkgs" | wc -l | tr -d ' ')" -eq 8 -a "$(head -c 11 "$F/pkgs/SHA256SUMS.minisig")" = STANDIN-SIG
  fixture f-default
  mkdir -p "$F/home/.minisign"
  chmod 700 "$F/home/.minisign"
  cp "$F/key" "$F/home/.minisign/holzwave.key"
  chmod 600 "$F/home/.minisign/holzwave.key"
  rc=0
  KEY=default run_sign "$F" "$VERSION" --dir "$F/pkgs" --pubkey "$F/pub" || rc=$?
  check "without HOLZWAVE_MINISIGN_KEY the key is read from \$HOME/.minisign/holzwave.key" test "$rc" -eq 0 && grep -q -F -- "-s $F/home/.minisign/holzwave.key " "$F/log/args.log"
}

# ---- verify-release.sh ------------------------------------------------------------------------------------------------------

run_verify() { # <fixture> <args...>
  f=$1
  shift
  (
    PATH="$f/bin"
    export PATH
    exec "$SH" "$verify" "$@"
  ) >"$f/log/vout.log" 2>"$f/log/verr.log"
}

signed_fixture() { # <name>
  fixture "$1"
  run_sign "$F" "$VERSION" --dir "$F/pkgs" --pubkey "$F/pub" || { bad "fixture $1 could not be signed"; cat "$F/log/err.log" >&2; }
}

flip_first_hash() { # <SHA256SUMS>: print it with the first character of the first hash changed
  awk 'NR == 1 { c = substr($0, 1, 1); $0 = (c == "0" ? "1" : "0") substr($0, 2) } { print }' "$1"
}

flip_byte() { # <file>: change the first byte
  printf 'X' | dd of="$1" bs=1 count=1 conv=notrunc 2>/dev/null
}

verify_checks() {
  echo "release-signing: verify-release.sh"
  [ -f "$verify" ] || { bad "scripts/verify-release.sh exists"; return; }

  signed_fixture v-clean
  rc=0
  run_verify "$F" "$F/pkgs" --pubkey "$F/pub" || rc=$?
  check "a clean release verifies: exit status 0" test "$rc" -eq 0
  check "a clean release: one line per check (SHA256SUMS signature, then signature and hash of each package) and ok" test "$(grep -c '^verify-release: ok ' "$F/log/vout.log")" -eq 7 -a "$(tail -n 1 "$F/log/vout.log")" = "verify-release: ok"

  signed_fixture v-byte
  flip_byte "$F/pkgs/$APPIMAGE"
  rc=0
  run_verify "$F" "$F/pkgs" --pubkey "$F/pub" || rc=$?
  check "a changed byte in a package: exit status 1" test "$rc" -eq 1
  check "a changed byte: both the signature and the hash of that file fail, the others do not" test "$(grep -c "^verify-release: FAILED $APPIMAGE" "$F/log/verr.log")" -eq 2 -a "$(grep -c '^verify-release: FAILED holzWave' "$F/log/verr.log")" -eq 2
  check "a changed byte: the last line says FAILED" test "$(tail -n 1 "$F/log/verr.log")" = "verify-release: FAILED"

  signed_fixture v-nosig
  rm -f "$F/pkgs/$DEB.minisig"
  rc=0
  run_verify "$F" "$F/pkgs" --pubkey "$F/pub" || rc=$?
  check "a deleted .minisig: exit status 1, and the reason names it" test "$rc" -eq 1 -a "$(grep -c "$DEB: no signature" "$F/log/verr.log")" -eq 1

  signed_fixture v-sums
  flip_first_hash "$F/pkgs/SHA256SUMS" >"$F/sums.new"
  cp "$F/sums.new" "$F/pkgs/SHA256SUMS"
  rc=0
  run_verify "$F" "$F/pkgs" --pubkey "$F/pub" || rc=$?
  check "a SHA256SUMS with a changed hash (not re-signed): exit status 1 and its signature fails" test "$rc" -eq 1 -a "$(grep -c 'SHA256SUMS: the signature does not verify' "$F/log/verr.log")" -eq 1
  # now sign the changed list properly, so only the hash check is left to catch it
  printf 'x\n' | "$F/bin/minisign" -S -s "$F/key" -m "$F/pkgs/SHA256SUMS" -x "$F/pkgs/SHA256SUMS.minisig" -t t
  rc=0
  run_verify "$F" "$F/pkgs" --pubkey "$F/pub" || rc=$?
  check "a changed hash in a correctly signed list: exit status 1 by the hash check alone" test "$rc" -eq 1 -a "$(grep -c 'the sha256 is not the one in SHA256SUMS' "$F/log/verr.log")" -eq 1 -a "$(grep -c 'signature does not verify' "$F/log/verr.log")" -eq 0

  signed_fixture v-wrongkey
  printf 'STANDIN-PUBLIC %s\n' "$(random_hex 4)" >"$F/otherpub"
  rc=0
  run_verify "$F" "$F/pkgs" --pubkey "$F/otherpub" || rc=$?
  check "a wrong public key: exit status 1" test "$rc" -eq 1

  signed_fixture v-stranger
  cp "$F/pkgs/$DEB" "$F/pkgs/other.bin"
  cp "$F/pkgs/$DEB.minisig" "$F/pkgs/other.bin.minisig"
  rc=0
  run_verify "$F" "$F/pkgs" --pubkey "$F/pub" || rc=$?
  check "a signature that SHA256SUMS does not cover: exit status 1" test "$rc" -eq 1 -a "$(grep -c 'other.bin.minisig: SHA256SUMS does not name' "$F/log/verr.log")" -eq 1

  signed_fixture v-path
  echo "0000000000000000000000000000000000000000000000000000000000000000  ../evil" >>"$F/pkgs/SHA256SUMS"
  rc=0
  run_verify "$F" "$F/pkgs" --pubkey "$F/pub" || rc=$?
  check "a name with a path part in SHA256SUMS is refused, never opened" test "$rc" -eq 1 -a "$(grep -c 'path part' "$F/log/verr.log")" -eq 1

  signed_fixture v-nosums
  rm -f "$F/pkgs/SHA256SUMS" "$F/pkgs/SHA256SUMS.minisig"
  rc=0
  run_verify "$F" "$F/pkgs" --pubkey "$F/pub" || rc=$?
  check "no SHA256SUMS: exit status 1" test "$rc" -eq 1

  signed_fixture v-env
  rc=0
  run_verify "$F" "$F/nowhere" --pubkey "$F/pub" || rc=$?
  check "a directory that does not exist: exit status 2" test "$rc" -eq 2
  rc=0
  run_verify "$F" "$F/pkgs" --pubkey "$F/nopub" || rc=$?
  check "a missing public key: exit status 2" test "$rc" -eq 2
  rc=0
  run_verify "$F" || rc=$?
  check "no argument: exit status 2" test "$rc" -eq 2
  rm -f "$F/bin/minisign"
  rc=0
  run_verify "$F" "$F/pkgs" --pubkey "$F/pub" || rc=$?
  check "no minisign: exit status 2, and the reason says how to get it" test "$rc" -eq 2 -a "$(grep -c 'minisign is not on the PATH' "$F/log/verr.log")" -eq 1
}

# ---- the real tool, with a throwaway key -----------------------------------------------------------------------------------

# Runs where the real minisign is: on the host, or inside the throwaway container (see run_container).
real_body() {
  where=${RT_WHERE:-host}
  command -v minisign >/dev/null 2>&1 || { echo "release-signing: no minisign here" >&2; exit 4; }
  echo "release-signing: real tool: $(minisign -v 2>&1 | head -n 1) ($where)"
  rt="$tmp/rt"
  mkdir -p "$rt/bin" "$rt/log" "$rt/pkgs" "$rt/home" || exit 1
  # the throwaway key pairs, each with its own password (a space and a quote in it, as a real password may have)
  pw1="rt-$(random_hex 6) with space"'"q"'
  pw2="rt-$(random_hex 6) other"
  mkkey() { # <name> <password>
    printf '%s\n%s\n' "$2" "$2" | minisign -G -p "$rt/$1.pub" -s "$rt/$1.key" >/dev/null 2>"$rt/log/gen-$1.err"
  }
  gen=0
  mkkey a "$pw1" || gen=1
  mkkey b "$pw2" || gen=1
  check "minisign -G makes a password-protected key pair from a password on standard input" test "$gen" -eq 0 -a -s "$rt/a.key" -a -s "$rt/a.pub"
  [ "$gen" -eq 0 ] || { cat "$rt"/log/gen-*.err >&2; finish "real minisign"; }
  chmod 600 "$rt/a.key" "$rt/b.key"

  write_shims "$rt/bin"
  rm -f "$rt/bin/minisign" "$rt/bin/gh" # the real minisign is the one on the PATH
  printf '%s\n' "$pw1" >"$rt/pw1"
  printf 'cat "$d/../pw1"\n' >"$rt/bin/store.sh"
  mkdir -p "$rt/log"
  for n in $NAMES; do head -c 4096 /dev/urandom >"$rt/pkgs/$n"; done
  cp -R "$rt/pkgs" "$rt/orig"

  signit() { # <key> <pub> <extra>: run sign-linux.sh with the real minisign on the PATH and the stand-in secret store first
    (
      PATH="$rt/bin:$PATH"
      USER=tester
      HOME="$rt/home"
      HOLZWAVE_MINISIGN_KEY="$1"
      export PATH USER HOME HOLZWAVE_MINISIGN_KEY
      exec "$SH" "$sign" "$VERSION" --dir "$rt/pkgs" --pubkey "$2"
    ) >"$rt/log/out" 2>"$rt/log/err"
  }
  verifyit() { # <pub>
    (
      PATH="$rt/bin:$PATH"
      export PATH
      exec "$SH" "$verify" "$rt/pkgs" --pubkey "$1"
    ) >"$rt/log/vout" 2>"$rt/log/verr"
  }

  rc=0
  signit "$rt/a.key" "$rt/a.pub" || rc=$?
  check "real minisign: sign-linux.sh signs with the key and the password from the stand-in secret store" test "$rc" -eq 0
  [ "$rc" -eq 0 ] || { cat "$rt/log/err" >&2; finish "real minisign"; }
  check "real minisign: eight files" test "$(ls "$rt/pkgs" | wc -l | tr -d ' ')" -eq 8
  rc=0
  verifyit "$rt/a.pub" || rc=$?
  check "real minisign: verify-release.sh exits 0" test "$rc" -eq 0
  user_check() { minisign -V -m "$rt/pkgs/$DEB" -p "$rt/a.pub" -x "$rt/pkgs/$DEB.minisig" >"$rt/log/user" 2>&1 && grep -q "holzWave $VERSION $DEB" "$rt/log/user"; }
  check "real minisign: a user's check, minisign -V -m file -p pub, passes and shows the trusted comment" user_check
  check "real minisign: the password is in no file of the run's directory" test "$(grep -rlF "$pw1" "$rt/pkgs" "$rt/log" | wc -l | tr -d ' ')" -eq 0

  flip_byte "$rt/pkgs/$RPM"
  rc=0
  verifyit "$rt/a.pub" || rc=$?
  check "real minisign: one changed byte makes verify-release.sh fail" test "$rc" -eq 1
  check "real minisign: and minisign -V alone rejects that file" test "$(minisign -V -q -m "$rt/pkgs/$RPM" -p "$rt/a.pub" -x "$rt/pkgs/$RPM.minisig" >/dev/null 2>&1; echo $?)" -ne 0
  cp "$rt/orig/$RPM" "$rt/pkgs/$RPM"
  rc=0
  verifyit "$rt/a.pub" || rc=$?
  check "real minisign: restoring the byte makes it pass again" test "$rc" -eq 0

  rc=0
  verifyit "$rt/b.pub" || rc=$?
  check "real minisign: a second, wrong public key fails" test "$rc" -eq 1

  flip_first_hash "$rt/pkgs/SHA256SUMS" >"$rt/sums.new"
  cp "$rt/pkgs/SHA256SUMS" "$rt/sums.good"
  cp "$rt/sums.new" "$rt/pkgs/SHA256SUMS"
  rc=0
  verifyit "$rt/a.pub" || rc=$?
  check "real minisign: a changed SHA256SUMS fails" test "$rc" -eq 1
  cp "$rt/sums.good" "$rt/pkgs/SHA256SUMS"

  rc=0
  signit "$rt/b.key" "$rt/a.pub" || rc=$?
  check "real minisign: the password of key a does not open key b (minisign refuses, exit 1)" test "$rc" -eq 1 -a "$(grep -c 'minisign failed signing' "$rt/log/err")" -eq 1
  check "real minisign: a failed run leaves no signature behind" test -z "$(ls "$rt/pkgs" | grep -e minisig -e SHA256SUMS || true)"
  printf '%s\n' "$pw2" >"$rt/pw1"
  rc=0
  signit "$rt/b.key" "$rt/a.pub" || rc=$?
  check "real minisign: a key with the right password but another public key is caught after signing (exit 1)" test "$rc" -eq 1 -a "$(grep -c 'does not verify' "$rt/log/err")" -eq 1
  check "real minisign: and that run leaves no signature behind either" test -z "$(ls "$rt/pkgs" | grep -e minisig -e SHA256SUMS || true)"
  rc=0
  signit "$rt/b.key" "$rt/b.pub" || rc=$?
  check "real minisign: the second key pair signs and verifies with its own password and public key" test "$rc" -eq 0
  rm -f "$rt/pkgs"/*.minisig "$rt/pkgs/SHA256SUMS"
  printf '%s\n' "$pw1" >"$rt/pw1"

  chmod 644 "$rt/a.key"
  rc=0
  signit "$rt/a.key" "$rt/a.pub" || rc=$?
  check "real stat: a key with mode 644 is refused with status 2" test "$rc" -eq 2 -a "$(grep -c 'readable by group or others' "$rt/log/err")" -eq 1
  chmod 600 "$rt/a.key"

  finish "real minisign, $where"
}

run_container() {
  if ! command -v docker >/dev/null 2>&1 || ! docker info >/dev/null 2>&1; then
    echo "release-signing: real round trip NOT RUN: no minisign and no Docker"
    exit 3
  fi
  echo "release-signing: real round trip in a throwaway container (ubuntu:26.04; apt installs minisign inside it only)"
  rc=0
  # Only scripts/ is mounted, read-only; the container works in its own /tmp and is removed when it ends.
  docker run --rm -e RT_WHERE="container ubuntu:26.04" -v "$here":/work/scripts:ro ubuntu:26.04 sh -c '
    apt-get update -qq >/dev/null 2>&1 && apt-get install -y -qq minisign >/dev/null 2>&1 || exit 4
    echo "release-signing: apt installed $(dpkg-query -W -f="\${Package} \${Version}" minisign)"
    exec sh /work/scripts/test-release-signing.sh --real-body' || rc=$?
  case "$rc" in
    0) echo "release-signing: real round trip ok" ;;
    4 | 125 | 126 | 127)
      echo "release-signing: real round trip NOT RUN: the container could not start or could not install minisign (status $rc)"
      exit 3
      ;;
    *)
      echo "release-signing: real round trip FAILED (status $rc)" >&2
      exit 1
      ;;
  esac
}

case "$mode" in
  realbody)
    real_body
    ;;
  shims | container | default)
    stand_in_checks
    refusal_checks
    failure_checks
    verify_checks
    finish "stand-ins"
    case "$mode" in
      shims) exit 0 ;;
      container) run_container ;;
      default)
        if command -v minisign >/dev/null 2>&1; then
          RT_WHERE=host "$SH" "$here/test-release-signing.sh" --real-body && echo "release-signing: real round trip ok"
        else
          run_container
        fi
        ;;
    esac
    ;;
esac
