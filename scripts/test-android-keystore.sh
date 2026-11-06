#!/bin/sh
# Prove the contract of scripts/android-keystore.sh without a real key and without a JDK or an Android SDK.
#
#   sh scripts/test-android-keystore.sh --shims-only   the stand-in checks only (exit 0 or 1)
#   sh scripts/test-android-keystore.sh                the stand-in checks, then a round trip with the real keytool when a
#                                                      working one is on the PATH, else the word `untested` and exit 3
#
# Exit status: 0 everything ran and held, 1 a check failed, 3 the stand-in checks held and the real round trip could not run
# (no working keytool): that is the word for `untested`. Nothing is installed.
#
# The stand-ins are directories of tiny scripts on a private PATH: a `keytool` that logs its arguments, its exported
# environment and its standard input and writes a marker keystore, a `security` and a `secret-tool` that print a canary password
# made fresh for each run (derived from a random seed, so the canary text is in no file of the run except where the script
# under test writes it), an `apksigner` that prints a certificate block, and a `gh` and a `curl` that must never be called. The
# canary has to reach the stand-in keytool on standard input and be found nowhere else: not in an argument, in the environment,
# in the output of the script or in any file of the run.

here=$(cd "$(dirname "$0")" && pwd -P)
root=$(cd "$here/.." && pwd -P)
SH=$(command -v sh)
script="$here/android-keystore.sh"

mode=default
case "${1:-}" in
  "") ;;
  --shims-only) mode=shims ;;
  *) echo "usage: sh scripts/test-android-keystore.sh [--shims-only]" >&2; exit 2 ;;
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
    echo "android-keystore: $failures of $checks checks FAILED ($1)" >&2
    exit 1
  fi
  echo "android-keystore: ok $checks checks ($1)"
}

tmp=$(mktemp -d "${TMPDIR:-/tmp}/holzwave-keystore.XXXXXX") || exit 1
REPO_TMP=
cleanup() {
  rm -rf "$tmp"
  [ -z "$REPO_TMP" ] || rm -rf "$REPO_TMP"
}
trap cleanup EXIT HUP INT TERM

random_hex() { od -An -N"$1" -tx1 /dev/urandom | tr -d ' \n'; }

sha256_text() { # <text>: sha256 of the text, lower-case hex
  if command -v shasum >/dev/null 2>&1; then printf '%s' "$1" | shasum -a 256 | cut -d' ' -f1; else printf '%s' "$1" | sha256sum | cut -d' ' -f1; fi
}
colons() { printf '%s\n' "$1" | tr 'a-f' 'A-F' | sed 's/\(..\)/\1:/g; s/:$//'; } # aabb -> AA:BB

# ---- the stand-ins ------------------------------------------------------------------------------------------------------

# The system tools the script may use, linked into a private bin so a PATH can be exactly that and the stand-ins (and never a
# keytool or apksigner that happens to be installed on the host).
TOOLS="uname dirname basename stat sort cat rm mkdir chmod tr cut wc head tail grep sed id ls readlink mktemp od dd cp mv awk perl true false test shasum sha256sum"
link_tools() { # <bin dir>
  for t in $TOOLS; do
    p=$(command -v "$t" 2>/dev/null || true)
    case "$p" in /*) ln -sf "$p" "$1/$t" ;; esac
  done
}

write_shims() { # <bin dir>
  b=$1
  cat >"$b/keytool" <<'SHIM'
#!/bin/sh
# Stand-in keytool: logs, reads the password lines from standard input, writes and reads a marker keystore.
d=$(cd "$(dirname "$0")" && pwd)
log=$d/../log
printf 'keytool %s\n' "$*" >>"$log/args.log"
export -p >>"$log/env.log"
in=$(cat)
printf '%s\n' "$in" >>"$log/stdin.log"
op=
ks=
alias=
while [ $# -gt 0 ]; do
  case "$1" in
    -genkeypair) op=gen ;;
    -list) op=list ;;
    -keystore) ks=$2; shift ;;
    -alias) alias=$2; shift ;;
  esac
  shift
done
printf '%s %s\n' "$op" "$(printf '%s\n' "$in" | wc -l | tr -d ' ')" >>"$log/stdin-lines.log"
sha() { if command -v shasum >/dev/null 2>&1; then printf '%s' "$1" | shasum -a 256 | cut -d' ' -f1; else printf '%s' "$1" | sha256sum | cut -d' ' -f1; fi; }
case "$op" in
  gen)
    [ -n "$ks" ] || exit 1
    if [ -e "$ks" ]; then echo "keytool error: the keystore exists" >&2; exit 1; fi
    printf 'STANDIN-KEYSTORE %s %s\n' "$(cat "$d/../keyid")" "$alias" >"$ks"
    [ ! -f "$d/fail-gen" ] || { echo "keytool error: stand-in failure after writing the file" >&2; exit 1; }
    ;;
  list)
    [ -f "$ks" ] || { echo "keytool error: no keystore" >&2; exit 1; }
    [ ! -f "$d/fail-list" ] || { echo "keytool error: password was incorrect" >&2; exit 1; }
    id=$(sed 's/^STANDIN-KEYSTORE \([^ ]*\) .*/\1/' "$ks")
    h=$(sha "$id" | tr 'a-f' 'A-F' | sed 's/\(..\)/\1:/g; s/:$//')
    short=$(printf '%s' "$h" | cut -c1-59)
    printf 'Alias name: %s\nCertificate fingerprints:\n\t SHA1: %s\n\t SHA256: %s\nSignature algorithm name: SHA256withECDSA\n' "$alias" "$short" "$h"
    ;;
  *) echo "keytool: stand-in knows -genkeypair and -list only" >&2; exit 1 ;;
esac
SHIM
  cat >"$b/apksigner" <<'SHIM'
#!/bin/sh
# Stand-in apksigner: the APK is a marker file naming the key id it is signed with.
d=$(cd "$(dirname "$0")" && pwd)
printf 'apksigner %s\n' "$*" >>"$d/../log/apksigner.log"
[ "$#" -eq 3 ] && [ "$1" = verify ] && [ "$2" = --print-certs ] || { echo "apksigner: unexpected arguments" >&2; exit 64; }
sha() { if command -v shasum >/dev/null 2>&1; then printf '%s' "$1" | shasum -a 256 | cut -d' ' -f1; else printf '%s' "$1" | sha256sum | cut -d' ' -f1; fi; }
read -r tag id <"$3" || true
case "$tag" in
  STANDIN-APK)
    printf 'Signer #1 certificate DN: CN=holzcloud.ch, OU=holzWave\nSigner #1 certificate SHA-256 digest: %s\nSigner #1 certificate SHA-1 digest: 0000\n' "$(sha "$id")"
    ;;
  STANDIN-APK-TWO)
    printf 'Signer #1 certificate SHA-256 digest: %s\nSigner #2 certificate SHA-256 digest: %s\n' "$(sha "$id")" "$(sha other)"
    ;;
  *) echo "DOES NOT VERIFY" >&2; echo "ERROR: no signature" >&2; exit 1 ;;
esac
SHIM
  cat >"$b/security" <<'SHIM'
#!/bin/sh
# Stand-in for the macOS keychain: answers only the exact lookup the script is allowed to make.
d=$(cd "$(dirname "$0")" && pwd)
[ "$#" -eq 6 ] && [ "$1" = find-generic-password ] && [ "$2" = -s ] && [ "$3" = holzwave-android-keystore ] && [ "$4" = -a ] && [ "$5" = "$USER" ] && [ "$6" = -w ] \
  || { echo "security: unexpected arguments" >&2; exit 44; }
. "$d/store.sh"
SHIM
  cat >"$b/secret-tool" <<'SHIM'
#!/bin/sh
# Stand-in for the Linux secret store: answers only the exact lookup the script is allowed to make.
d=$(cd "$(dirname "$0")" && pwd)
[ "$#" -eq 3 ] && [ "$1" = lookup ] && [ "$2" = service ] && [ "$3" = holzwave-android-keystore ] \
  || { echo "secret-tool: unexpected arguments" >&2; exit 1; }
. "$d/store.sh"
SHIM
  for n in gh curl; do
    cat >"$b/$n" <<'SHIM'
#!/bin/sh
d=$(cd "$(dirname "$0")" && pwd)
echo "$(basename "$0") $*" >>"$d/../log/net-called"
exit 99
SHIM
  done
  chmod +x "$b/keytool" "$b/apksigner" "$b/security" "$b/secret-tool" "$b/gh" "$b/curl"
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

# A fixture: the stand-ins, an empty home, a seed for the canary and the id of the key the stand-in keytool will make.
fixture() { # <name>
  F="$tmp/$1"
  mkdir -p "$F/bin" "$F/log" "$F/home" || exit 1
  : >"$F/log/args.log"
  : >"$F/log/env.log"
  : >"$F/log/stdin.log"
  : >"$F/log/stdin-lines.log"
  : >"$F/log/apksigner.log"
  link_tools "$F/bin"
  write_shims "$F/bin"
  random_hex 8 >"$F/seed"
  random_hex 8 >"$F/keyid"
  set_store "$F" ok
  CANARY_HEX=$(tr 'a-f' 'A-F' <"$F/seed")
  KEYFILE="$F/home/.android-keys/holzwave-release.p12"
}

# Run android-keystore.sh in a fixture on the private PATH; standard output and error are kept in the fixture's log.
run_ks() { # <fixture> <args...>
  f=$1
  shift
  (
    PATH="$f/bin"
    USER=tester
    HOME="$f/home"
    HOLZWAVE_ANDROID_KEYSTORE="${KS:-$f/home/.android-keys/holzwave-release.p12}"
    export PATH USER HOME HOLZWAVE_ANDROID_KEYSTORE
    [ "${KS:-}" != default ] || unset HOLZWAVE_ANDROID_KEYSTORE
    # shellcheck disable=SC2086 # SHFLAGS is a flag list on purpose (for example -x)
    exec "$SH" ${SHFLAGS:-} "$script" "$@"
  ) >"$f/log/out.log" 2>"$f/log/err.log"
}

mode_of() { # <file>
  case "$(uname -s)" in Darwin) stat -L -f %Lp "$1" ;; *) stat -L -c %a "$1" ;; esac
}
want_pw() { printf '%s' "CANARY-$CANARY_HEX"' a%sb\n$HOME "q"'; }
canary_files() { # every file of the run, the logs of the script included, except the seed the canary is derived from
  grep -rli -F "$CANARY_HEX" "$F" | grep -v '/seed$' | sort | tr '\n' ' '
}

# A refusal: exit 2, one line on standard error that names the cause, keytool never called, no secret anywhere.
refusal() { # <label> <fragment of the reason> <fixture> <args...>
  label=$1
  frag=$2
  f=$3
  shift 3
  rc=0
  run_ks "$f" "$@" || rc=$?
  check "$label: exit status 2" test "$rc" -eq 2
  check "$label: one line on standard error that says why ($frag)" test "$(wc -l <"$f/log/err.log" | tr -d ' ')" -eq 1 -a "$(grep -c -F -- "$frag" "$f/log/err.log")" -eq 1
  check "$label: keytool was never called and no secret leaked" test "$(wc -c <"$f/log/args.log" | tr -d ' ')" -eq 0 -a "$(grep -rlF "$CANARY_HEX" "$f/log" | wc -l | tr -d ' ')" -eq 0
}

stand_in_checks() {
  echo "android-keystore: stand-in checks"
  [ -f "$script" ] || { bad "scripts/android-keystore.sh exists"; return; }

  # ---- the script's constants (the certificate name and the alias are the ones every listing repeats) ----
  check "the alias is holzwave-release" grep -qx 'ALIAS=holzwave-release' "$script"
  check "the certificate subject is CN=holzcloud.ch, OU=holzWave" grep -qx "SUBJECT='CN=holzcloud.ch, OU=holzWave'" "$script"
  check "the key algorithm is a named constant" grep -q '^KEY_ALG=' "$script"
  check "the curve is a named constant" grep -q '^KEY_CURVE=' "$script"
  check "the signature algorithm is a named constant" grep -q '^SIG_ALG=' "$script"
  check "the constants say which Android page they were checked against" grep -q 'source.android.com/docs/security/features/apksigning' "$script"

  # ---- create: the key is made, the password reaches keytool on standard input only ----
  fixture create
  rc=0
  run_ks "$F" create || rc=$?
  check "create exits 0" test "$rc" -eq 0
  [ "$rc" -eq 0 ] || { cat "$F/log/err.log" >&2; return; }
  check "the keystore file exists at the default place under HOME" test -f "$KEYFILE"
  check "the keystore file has mode 600" test "$(mode_of "$KEYFILE")" = 600
  check "its directory is private (mode 700)" test "$(mode_of "$F/home/.android-keys")" = 700
  check "the keystore is outside the repository" test "$(cd "$F/home" && pwd -P | grep -c "^$root/")" -eq 0
  gen=$(grep '^keytool -genkeypair ' "$F/log/args.log")
  check "one -genkeypair call was made" test "$(printf '%s\n' "$gen" | wc -l | tr -d ' ')" -eq 1
  want_gen="keytool -genkeypair -keystore $KEYFILE -storetype PKCS12 -alias holzwave-release -dname CN=holzcloud.ch, OU=holzWave -keyalg EC -groupname secp256r1 -sigalg SHA256withECDSA -validity 36500"
  check "the call names the keystore, PKCS12, the alias, the subject, EC on secp256r1, SHA256withECDSA and 36500 days" test "$gen" = "$want_gen"
  check "no call carries -storepass, -keypass or any password argument" test "$(grep -c -i -e storepass -e keypass -e canary -e password "$F/log/args.log")" -eq 0
  check "keytool read two lines (enter and re-enter) to make the key and one to read it" test "$(cat "$F/log/stdin-lines.log")" = "$(printf 'gen 2\nlist 1')"
  check "all three lines on keytool's standard input are the password, exactly" test "$(grep -c -x -F "$(want_pw)" "$F/log/stdin.log")" -eq 3 -a "$(wc -l <"$F/log/stdin.log" | tr -d ' ')" -eq 3
  check "the canary is in stdin.log and in no other file of the run (arguments, environment, output, keystore)" test "$(canary_files)" = "$F/log/stdin.log "
  check "the script says nothing about the password on its standard output and error" test "$(cat "$F/log/out.log" "$F/log/err.log" | grep -c -i -e canary -e password)" -eq 0
  check "the output carries the SHA-256 fingerprint of the key" test "$(grep -c -F "SHA-256 certificate fingerprint: $(colons "$(sha256_text "$(cat "$F/keyid")")")" "$F/log/out.log")" -eq 1
  check "the output tells the owner to make an off-machine backup of the keystore and of the keychain entry" test "$(grep -c -i 'off-machine backup of the keystore file and of the keychain entry' "$F/log/out.log")" -eq 1
  check "the script called neither gh nor curl" test ! -e "$F/log/net-called"

  # ---- create refuses before it touches anything ----
  fixture r-exists
  mkdir -p "$F/home/.android-keys"
  printf 'an existing key\n' >"$KEYFILE"
  refusal "a keystore that already exists" "already exists" "$F" create
  check "the existing keystore is unchanged" test "$(cat "$KEYFILE")" = "an existing key"
  rm -f "$KEYFILE"
  ln -s "$F/nowhere" "$KEYFILE"
  refusal "a dangling link at the keystore path" "already exists" "$F" create
  check "the dangling link was not followed or replaced" test ! -e "$F/nowhere" -a -L "$KEYFILE"

  fixture r-inrepo
  REPO_TMP=$(mktemp -d "$root/.keystoretest.XXXXXX")
  KS="$REPO_TMP/sub/holzwave.p12" refusal "a keystore path inside the repository (the directory does not exist yet)" "inside the repository" "$F" create
  check "the refused run made no directory in the repository" test ! -e "$REPO_TMP/sub"
  KS="$REPO_TMP/holzwave.p12" refusal "a keystore path inside the repository" "inside the repository" "$F" create
  ln -s "$REPO_TMP" "$F/linkdir"
  KS="$F/linkdir/holzwave.p12" refusal "a keystore path reached through a symlinked directory into the repository" "inside the repository" "$F" create
  KS="$F/linkdir/new/holzwave.p12" refusal "a new directory below a symlinked directory into the repository" "inside the repository" "$F" create
  ln -s "$REPO_TMP/holzwave.p12" "$F/linkfile.p12"
  KS="$F/linkfile.p12" refusal "a keystore path that is a link to a file in the repository" "link into the repository" "$F" create
  check "no file was made in the repository" test -z "$(ls -A "$REPO_TMP")"
  rm -rf "$REPO_TMP"
  REPO_TMP=

  fixture r-nokeychain
  set_store "$F" fail
  refusal "the keychain has no entry" "has no entry" "$F" create
  check "no keystore and no directory were made" test ! -e "$F/home/.android-keys"
  fixture r-empty
  set_store "$F" empty
  refusal "the keychain entry is empty" "empty password" "$F" create
  fixture r-nokeytool
  rm -f "$F/bin/keytool"
  refusal "no keytool on the PATH" "keytool is not on the PATH" "$F" create
  fixture r-trace
  rc=0
  SHFLAGS=-x run_ks "$F" create || rc=$?
  # the shell's own trace lines share standard error, so this refusal is checked by its message and not by its line count
  check "tracing is on: exit status 2 and the reason is on standard error" test "$rc" -eq 2 -a "$(grep -c -x 'android-keystore: tracing is on.*' "$F/log/err.log")" -eq 1
  check "tracing is on: keytool was never called and no secret leaked" test "$(wc -c <"$F/log/args.log" | tr -d ' ')" -eq 0 -a "$(grep -rlF "$CANARY_HEX" "$F/log" | wc -l | tr -d ' ')" -eq 0
  fixture r-args
  refusal "no mode" "usage" "$F"
  refusal "an unknown mode" "unknown mode" "$F" delete
  refusal "create with an extra argument" "takes no argument" "$F" create now
  refusal "check without an APK" "needs one APK" "$F" check

  # ---- a failed keytool leaves nothing behind ----
  fixture r-keytoolfails
  : >"$F/bin/fail-gen"
  rc=0
  run_ks "$F" create || rc=$?
  check "a keytool that fails after writing the file: exit status 1" test "$rc" -eq 1
  check "the half-made keystore is removed" test ! -e "$KEYFILE"
  check "the failure names keytool and shows no secret" test "$(grep -c 'keytool did not make the key' "$F/log/err.log")" -eq 1 -a "$(canary_files)" = "$F/log/stdin.log "

  # ---- fingerprint and check ----
  fixture fp
  run_ks "$F" create || bad "create before fingerprint"
  fp=$(colons "$(sha256_text "$(cat "$F/keyid")")")
  rc=0
  run_ks "$F" fingerprint || rc=$?
  check "fingerprint exits 0 and prints the SHA-256 fingerprint of the certificate and nothing else" test "$rc" -eq 0 -a "$(cat "$F/log/out.log")" = "$fp" -a ! -s "$F/log/err.log"
  check "fingerprint printed nothing secret and the password reached keytool on standard input only" test "$(cat "$F/log/out.log" | grep -c -i -e canary -e password)" -eq 0 -a "$(canary_files)" = "$F/log/stdin.log "
  check "fingerprint is a read (no -genkeypair in this run's call)" test "$(grep -c -e '-genkeypair' "$F/log/args.log")" -eq 1

  printf 'STANDIN-APK %s\n' "$(cat "$F/keyid")" >"$F/good.apk"
  printf 'STANDIN-APK %s\n' "$(random_hex 8)" >"$F/other.apk"
  printf 'STANDIN-APK-TWO %s\n' "$(cat "$F/keyid")" >"$F/two.apk"
  printf 'not an apk\n' >"$F/unsigned.apk"
  rc=0
  run_ks "$F" check "$F/good.apk" || rc=$?
  check "check of an APK signed with the key exits 0 and says so" test "$rc" -eq 0 -a "$(grep -c 'is signed with the release key' "$F/log/out.log")" -eq 1
  check "check ran apksigner verify --print-certs on the APK and nothing else" test "$(cat "$F/log/apksigner.log")" = "apksigner verify --print-certs $F/good.apk"
  rc=0
  run_ks "$F" check "$F/other.apk" || rc=$?
  other_digest=$(sha256_text "$(sed 's/^STANDIN-APK //' "$F/other.apk")")
  check "check of an APK signed with another key exits 1" test "$rc" -eq 1
  check "check shows both values on a mismatch (the keystore's and the APK's)" test "$(grep -c -F "$(printf '%s' "$fp" | tr -d ':' | tr 'A-F' 'a-f')" "$F/log/err.log")" -eq 1 -a "$(grep -c -F "$other_digest" "$F/log/err.log")" -eq 1
  rc=0
  run_ks "$F" check "$F/two.apk" || rc=$?
  check "check of an APK with two signers exits 1" test "$rc" -eq 1
  rc=0
  run_ks "$F" check "$F/unsigned.apk" || rc=$?
  check "check of an APK that apksigner does not verify exits 1" test "$rc" -eq 1
  check "check never leaked the canary" test "$(canary_files)" = "$F/log/stdin.log "

  fixture fp-refusals
  run_ks "$F" create || bad "create before the refusals"
  printf 'STANDIN-APK %s\n' "$(cat "$F/keyid")" >"$F/good.apk"
  : >"$F/log/args.log"
  : >"$F/log/stdin.log"
  : >"$F/log/env.log"
  refusal "check of an APK that does not exist" "the APK is missing" "$F" check "$F/missing.apk"
  chmod 644 "$KEYFILE"
  refusal "fingerprint of a keystore with mode 644" "readable by group or others" "$F" fingerprint
  chmod 640 "$KEYFILE"
  refusal "check with a keystore of mode 640" "readable by group or others" "$F" check "$F/good.apk"
  chmod 600 "$KEYFILE"
  KS="$F/home/none.p12" refusal "fingerprint of a keystore that does not exist" "keystore is missing" "$F" fingerprint
  rm -f "$F/bin/apksigner"
  refusal "check without apksigner on the PATH" "apksigner is not on the PATH" "$F" check "$F/good.apk"
  : >"$F/bin/fail-list"
  rc=0
  run_ks "$F" fingerprint || rc=$?
  check "fingerprint with a keytool that cannot read the store (wrong password): exit status 1 and a reason" test "$rc" -eq 1 -a "$(grep -c 'keytool could not read the keystore' "$F/log/err.log")" -eq 1 -a ! -s "$F/log/out.log"
}

# ---- the real keytool, when a working one is installed --------------------------------------------------------------------

real_checks() { # <ok|untested>
  echo "android-keystore: real keytool round trip"
  kt=$(command -v keytool 2>/dev/null || true)
  if [ -z "$kt" ] || ! "$kt" -help >/dev/null 2>&1; then
    echo "android-keystore: untested: no working keytool on the PATH (it ships with the JDK; nothing is installed by this test)"
    return 3
  fi
  fixture real
  rm -f "$F/bin/keytool" "$F/bin/apksigner"
  rc=0
  (
    PATH="$F/bin:$(dirname "$kt"):$PATH"
    USER=tester
    HOME="$F/home"
    HOLZWAVE_ANDROID_KEYSTORE="$KEYFILE"
    export PATH USER HOME HOLZWAVE_ANDROID_KEYSTORE
    "$SH" "$script" create >"$F/log/out.log" 2>"$F/log/err.log" || exit 11
    "$SH" "$script" fingerprint >"$F/log/fp.log" 2>>"$F/log/err.log" || exit 12
  ) || rc=$?
  if [ "$rc" -ne 0 ]; then
    bad "the real keytool made the key and printed its fingerprint (step status $rc)"
    cat "$F/log/err.log" >&2
    return 0
  fi
  ok "the real keytool made the key and printed its fingerprint"
  check "the real keystore has mode 600" test "$(mode_of "$KEYFILE")" = 600
  check "the real fingerprint has the form of 32 colon-separated bytes" test "$(grep -Ec '^[0-9A-F]{2}(:[0-9A-F]{2}){31}$' "$F/log/fp.log")" -eq 1
  check "the password is in no file of the real run" test "$(grep -rli -F "$CANARY_HEX" "$F" | grep -v '/seed$' | wc -l | tr -d ' ')" -eq 0
  return 0
}

stand_in_checks
if [ "$mode" = shims ]; then
  finish "stand-ins only; the real keytool was not tried"
  exit 0
fi
real_rc=0
real_checks || real_rc=$?
if [ "$real_rc" -eq 3 ]; then
  if [ "$failures" -ne 0 ]; then finish "stand-ins; real keytool untested"; fi
  echo "android-keystore: ok $checks stand-in checks; the real keytool round trip is untested" >&2
  exit 3
fi
finish "stand-ins and the real keytool"
