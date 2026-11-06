#!/bin/sh
# Print the Android versionCode of an app version, on one line and nothing else.
#
#   sh scripts/android-version-code.sh 0.0.1-beta2        prints 1202
#
# Why this exists. Android refuses to install an update whose versionCode is not higher than the installed one, and a store
# or an update checker orders builds by it. The code the Tauri command line derives when none is written down is
# `major * 1000000 + minor * 1000 + patch`, which ignores the pre-release part: 0.0.1-beta2 and 0.0.1 would both be 1. This
# scheme keeps every alpha, beta, release candidate and final build of every version in order, and the result is written
# into app/src-tauri/tauri.android.conf.json (bundle.android.versionCode) where it can be read without running anything.
# A test ties that number to the version in tauri.conf.json.
#
# The scheme, in digits (decimal):
#
#   versionCode = major * 10000000 + minor * 100000 + patch * 1000 + stage * 100 + number
#
#   major   0 to 209     the code must stay below Android's limit of 2100000000, so 209.99.99 is the last version
#   minor   0 to 99
#   patch   0 to 99
#   stage   1 alpha, 2 beta, 3 release candidate (rc), 9 final (no pre-release part)
#   number  1 to 99 for a pre-release (alpha1, beta2, rc1), 0 for a final
#
#   0.0.1-alpha1 -> 1101    0.0.1-beta1 -> 1201    0.0.1-beta2 -> 1202    0.0.1-rc1 -> 1301
#   0.0.1        -> 1900    0.0.2-alpha1 -> 2101   0.1.0 -> 100900        1.0.0 -> 10000900
#
# Within one patch, alpha < beta < rc < final; a later patch, minor or major is above every build of an earlier one.
#
# Refused, with exit status 2, one line on stderr and nothing on stdout: a number above its range, a number with a leading
# zero, an unknown stage word, a pre-release written in any form but `<word><number>`, build metadata (`+...`), and anything
# that is not `major.minor.patch` with an optional `-<word><number>`. Nothing is ever clamped.

refuse() {
    echo "android-version-code: $1" >&2
    exit 2
}

[ "$#" -eq 1 ] || refuse "usage: sh scripts/android-version-code.sh <version>, for example 0.0.1-beta2"
version=$1
[ -n "$version" ] || refuse "the version is empty"

case "$version" in
    *+*) refuse "build metadata is not allowed in '$version'" ;;
esac

core=${version%%-*}
pre=
case "$version" in
    *-*) pre=${version#*-} ;;
esac

# major.minor.patch: exactly three numbers.
oldifs=$IFS
IFS=.
# shellcheck disable=SC2086
set -- $core
IFS=$oldifs
[ "$#" -eq 3 ] || refuse "'$version' is not major.minor.patch"

number() { # <name> <text> <max>: print the value or refuse
    case "$2" in
        '' | *[!0-9]*) refuse "$1 '$2' is not a number in '$version'" ;;
        0) ;;
        0*) refuse "$1 '$2' has a leading zero in '$version'" ;;
    esac
    [ "${#2}" -le 3 ] || refuse "$1 '$2' is above $3 in '$version'"
    [ "$2" -le "$3" ] || refuse "$1 '$2' is above $3 in '$version'"
    printf '%s' "$2"
}

major=$(number major "$1" 209) || exit 2
minor=$(number minor "$2" 99) || exit 2
patch=$(number patch "$3" 99) || exit 2

stage=9
n=0
if [ -n "$pre" ]; then
    word=${pre%%[0-9]*}
    digits=${pre#"$word"}
    case "$word" in
        alpha) stage=1 ;;
        beta) stage=2 ;;
        rc) stage=3 ;;
        *) refuse "the pre-release stage '$word' of '$version' is not alpha, beta or rc" ;;
    esac
    [ -n "$digits" ] || refuse "the pre-release '$pre' of '$version' has no number (write ${word}1)"
    n=$(number "pre-release number" "$digits" 99) || exit 2
    [ "$n" -ge 1 ] || refuse "the pre-release number of '$version' must be 1 to 99"
fi

echo $((major * 10000000 + minor * 100000 + patch * 1000 + stage * 100 + n))
