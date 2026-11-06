#!/bin/sh
# The release keystore of the Android app: make it once, print its fingerprint, check an APK against it. Nothing is uploaded,
# copied or backed up by this script, and it needs no network. There is no CI.
#
#   sh scripts/android-keystore.sh create             make the key (the only mode that writes; refuses an existing key)
#   sh scripts/android-keystore.sh fingerprint        print the SHA-256 fingerprint of the key's certificate
#   sh scripts/android-keystore.sh check <apk>        check that <apk> is signed with exactly this key
#
# The keystore is a PKCS12 file outside the repository: HOLZWAVE_ANDROID_KEYSTORE, default
# $HOME/.android-keys/holzwave-release.p12. Its password comes from the operating system's secret store and from nowhere else
# (no file, no prompt, no environment variable): the macOS keychain through
# `security find-generic-password -s holzwave-android-keystore -a $USER -w`, Linux through
# `secret-tool lookup service holzwave-android-keystore`. The owner puts the password there first; this script never makes,
# prints or stores one. It is held in one shell variable that is never exported, goes into keytool on its standard input
# through the shell's built-in printf (so it is in no argument list, no environment and no file), and is unset after use.
#
# The key: one alias, one subject, one algorithm, valid for about a hundred years. An APK can only be updated by one signed with
# the same key, so a lost key or password means no update for anybody: after `create` the owner makes an off-machine backup of
# the keystore file and of the keychain entry. The SHA-256 fingerprint is what a person can check an APK against:
#   apksigner verify --print-certs <apk>
#
# Exit status: 0 done; 2 refused before anything was touched (the reason is on standard error); 1 keytool, apksigner or the
# comparison failed (a keystore this run was making is removed).
#
# RULE: never `set -x` in this script, never print the password variable, never put it in an argument (-storepass and -keypass
# show in the process list). The test scripts/test-android-keystore.sh and the bundle test fail on it, and the script refuses to
# run while tracing is on.

set -eu

here=$(cd "$(dirname "$0")" && pwd -P)
root=$(cd "$here/.." && pwd -P)
caller=$(pwd -P)

# The certificate and the key. Android's APK Signature Scheme v2 and v3 list RSASSA-PSS, RSASSA-PKCS1-v1_5 and ECDSA with
# SHA-256 or SHA-512 as the algorithms for a signer's key (https://source.android.com/docs/security/features/apksigning/v2,
# read 2026-11-06); Ed25519 is not among them, so an APK cannot be signed with it. ECDSA on the NIST curve P-256 (secp256r1)
# with SHA-256 is the smallest and fastest of those and every Android version this app runs on (12 and later) verifies it.
# RSA 4096 is the older, larger choice and stays the fallback if a store or tool ever refuses the curve.
ALIAS=holzwave-release
SUBJECT='CN=holzcloud.ch, OU=holzWave'
KEY_ALG=EC
KEY_CURVE=secp256r1
SIG_ALG=SHA256withECDSA
VALIDITY_DAYS=36500
STORE_TYPE=PKCS12
SECRET_SERVICE=holzwave-android-keystore

die() { # <status> <reason>
  echo "android-keystore: $2" >&2
  exit "$1"
}
fail() { die 2 "$1"; } # a refusal: nothing was touched

case "$-" in *x*) fail "tracing is on (sh -x): it would print the password; run the script without it" ;; esac

usage="usage: sh scripts/android-keystore.sh create | fingerprint | check <apk>"
mode=${1:-}
[ $# -gt 0 ] || fail "$usage"
shift
apk=
case "$mode" in
  create | fingerprint)
    [ $# -eq 0 ] || fail "$mode takes no argument; $usage"
    ;;
  check)
    [ $# -eq 1 ] || fail "check needs one APK file; $usage"
    apk=$1
    case "$apk" in /*) ;; *) apk=$caller/$apk ;; esac
    ;;
  *) fail "unknown mode: $mode; $usage" ;;
esac

ks=${HOLZWAVE_ANDROID_KEYSTORE:-${HOME:-/nonexistent}/.android-keys/holzwave-release.p12}
case "$ks" in /*) ;; *) ks=$caller/$ks ;; esac

# 1. the tools
command -v keytool >/dev/null 2>&1 || fail "keytool is not on the PATH (it ships with the JDK; the build uses JDK 21)"
if [ "$mode" = check ]; then
  command -v apksigner >/dev/null 2>&1 || fail "apksigner is not on the PATH (it ships with the Android SDK build tools)"
fi
case "$(uname -s)" in
  Darwin) command -v security >/dev/null 2>&1 || fail "the macOS keychain tool (security) is not on the PATH" ;;
  *) command -v secret-tool >/dev/null 2>&1 || fail "secret-tool is not on the PATH (libsecret-tools)" ;;
esac

# 2. the keystore path is outside the repository, also when a directory or the file is a symlink into it
anc=$(dirname "$ks")
rest=
hops=0
while [ ! -d "$anc" ] && [ "$hops" -lt 64 ]; do
  rest=/$(basename "$anc")$rest
  anc=$(dirname "$anc")
  hops=$((hops + 1))
done
realanc=$(cd "$anc" && pwd -P) || fail "the directory of the keystore cannot be entered: $ks"
case "$realanc$rest/" in
  "$root"/*) fail "the keystore path is inside the repository ($realanc$rest): keep it outside, for example in \$HOME/.android-keys" ;;
esac
if [ -L "$ks" ]; then
  real=$ks
  hops=0
  while [ -L "$real" ] && [ "$hops" -lt 20 ]; do
    target=$(readlink "$real")
    case "$target" in /*) real=$target ;; *) real=$(dirname "$real")/$target ;; esac
    hops=$((hops + 1))
  done
  realdir=$(cd "$(dirname "$real")" 2>/dev/null && pwd -P) || realdir=$(dirname "$real")
  case "$realdir/" in
    "$root"/*) fail "the keystore is a link into the repository ($realdir): keep it outside" ;;
  esac
fi

# 3. the state of the file: create needs none (a key is never overwritten), the other modes need a private one
if [ "$mode" = create ]; then
  if [ -e "$ks" ] || [ -L "$ks" ]; then
    fail "a keystore already exists at $ks: a key is never overwritten (a new key could never update an app signed with the old one)"
  fi
else
  [ -f "$ks" ] || fail "the keystore is missing: $ks (make it with: sh scripts/android-keystore.sh create)"
  case "$(uname -s)" in
    Darwin) fmode=$(stat -L -f %Lp "$ks") ;;
    *) fmode=$(stat -L -c %a "$ks") ;;
  esac
  case "$fmode" in
    0 | 00 | ?00 | ??00) ;; # no group and no other bits (400, 600, 0600)
    *) fail "the keystore is readable by group or others (mode $fmode): chmod 600 $ks" ;;
  esac
fi
if [ "$mode" = check ]; then
  [ -f "$apk" ] || fail "the APK is missing: $apk"
fi

# 4. the password, from the secret store only; an entry that is missing or empty is a refusal
case "$(uname -s)" in
  Darwin) password=$(security find-generic-password -s "$SECRET_SERVICE" -a "${USER:-$(id -un)}" -w 2>/dev/null) \
    || fail "the keychain has no entry $SECRET_SERVICE for this user, or answered an error (security add-generic-password -s $SECRET_SERVICE -a \$USER -w)" ;;
  *) password=$(secret-tool lookup service "$SECRET_SERVICE" 2>/dev/null) \
    || fail "the secret store has no entry (service $SECRET_SERVICE), or answered an error" ;;
esac
[ -n "$password" ] || fail "the secret store gave an empty password for $SECRET_SERVICE"

# The password goes to keytool on standard input only: once to read a store, twice (enter and re-enter) to make one.
feed_once() { printf '%s\n' "$password"; }
feed_twice() { printf '%s\n' "$password"; printf '%s\n' "$password"; }

fingerprint_of() { # prints the SHA-256 fingerprint of the certificate as keytool writes it (AA:BB:...)
  listing=$(feed_once | keytool -list -v -keystore "$ks" -storetype "$STORE_TYPE" -alias "$ALIAS" 2>/dev/null) \
    || die 1 "keytool could not read the keystore (wrong password, or no key with the alias $ALIAS)"
  fp=$(printf '%s\n' "$listing" | sed -n 's/^[[:space:]]*SHA256:[[:space:]]*\([0-9A-Fa-f:]*\)[[:space:]]*$/\1/p' | head -n 1)
  printf '%s\n' "$fp" | grep -Eq '^[0-9A-Fa-f]{2}(:[0-9A-Fa-f]{2}){31}$' \
    || die 1 "keytool printed no SHA-256 fingerprint for the alias $ALIAS"
  printf '%s\n' "$fp"
}

plain() { printf '%s\n' "$1" | tr -d ':' | tr 'A-F' 'a-f'; } # AA:BB -> aabb, lower case

case "$mode" in
  create)
    # Every precondition holds. From here on a keystore this run made is removed if the run does not finish.
    finished=0
    partial() { [ "$finished" -eq 1 ] || rm -f "$ks"; }
    trap partial EXIT
    trap 'exit 130' INT
    trap 'exit 143' HUP TERM
    umask 077
    mkdir -p "$(dirname "$ks")" || die 1 "cannot make the directory of the keystore"
    chmod 700 "$(dirname "$ks")" 2>/dev/null || true
    feed_twice | keytool -genkeypair -keystore "$ks" -storetype "$STORE_TYPE" -alias "$ALIAS" -dname "$SUBJECT" \
      -keyalg "$KEY_ALG" -groupname "$KEY_CURVE" -sigalg "$SIG_ALG" -validity "$VALIDITY_DAYS" >/dev/null 2>&1 \
      || die 1 "keytool did not make the key"
    [ -f "$ks" ] || die 1 "keytool reported success but there is no keystore at $ks"
    chmod 600 "$ks"
    fp=$(fingerprint_of)
    unset password
    finished=1
    echo "android-keystore: the key $ALIAS ($SUBJECT) is made: $ks"
    echo "android-keystore: SHA-256 certificate fingerprint: $fp"
    echo "android-keystore: publish this fingerprint where people read about the Android app, so they can check an APK against it."
    echo "android-keystore: NOW make an off-machine backup of the keystore file and of the keychain entry $SECRET_SERVICE:"
    echo "android-keystore: if either is lost, nobody can ever update the app (nothing was copied or uploaded by this script)."
    ;;
  fingerprint)
    fp=$(fingerprint_of)
    unset password
    echo "$fp"
    ;;
  check)
    fp=$(fingerprint_of)
    unset password
    out=$(apksigner verify --print-certs "$apk" 2>&1) || die 1 "apksigner does not verify $apk: $(printf '%s\n' "$out" | head -n 1)"
    digests=$(printf '%s\n' "$out" | sed -n 's/^Signer #[0-9]* certificate SHA-256 digest:[[:space:]]*\([0-9A-Fa-f]*\)[[:space:]]*$/\1/p')
    [ -n "$digests" ] || die 1 "apksigner printed no certificate digest for $apk"
    [ "$(printf '%s\n' "$digests" | wc -l | tr -d ' ')" -eq 1 ] || die 1 "the APK has more than one signer; expected exactly the release key"
    want=$(plain "$fp")
    got=$(plain "$digests")
    if [ "$want" != "$got" ]; then
      echo "android-keystore: the APK is NOT signed with the release key" >&2
      echo "android-keystore:   keystore: $want" >&2
      echo "android-keystore:   APK:      $got" >&2
      exit 1
    fi
    echo "android-keystore: $apk is signed with the release key (SHA-256 $want)"
    ;;
esac
