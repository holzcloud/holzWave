#!/bin/sh
# Give a built macOS app bundle its fixed ad hoc signing identifiers.
#
#   sh scripts/sign-macos.sh <path to holzWave.app>
#
# What it does: signs `Contents/MacOS/wsh` ad hoc under the identifier `ch.holzcloud.holzwave.wsh`, then signs the app
# ad hoc under `ch.holzcloud.holzwave`, inside out, then verifies the whole app with `codesign --verify --strict --deep`.
#
# Why: macOS files the Local Network permission, and other privacy answers, under the signing identifier. The signature
# that the linker makes by itself carries a new identifier on every build, so every build counted as a new app and lost
# its answers. The Tauri bundler signs the app from `bundle.macOS.signingIdentity` in app/src-tauri/tauri.conf.json and
# takes the identifier from the bundle, but it names the sidecar after its file; this script gives `wsh` its own
# identifier and seals the app again around it.
#
# It signs without `--deep` (that would sign nested code with the options of the outer code, and Apple advises against
# it) and without the hardened runtime (see `hardenedRuntime` in tauri.conf.json). `--deep` appears on the verify line only.
#
# scripts/release-macos.sh runs it after the Tauri build. After a local build with the `tauri.wsh.conf.json` overlay, run
# it by hand on target/release/bundle/macos/holzWave.app.
#
# Exit status: 0 and the last line `sign-macos: ok`, 1 when a step failed or the bundle is another app, 2 for a bad
# argument or a missing tool.

fail() {
    echo "sign-macos: FAILED: $*" >&2
    exit 1
}
need() {
    echo "sign-macos: $*" >&2
    exit 2
}

[ $# -eq 1 ] || need "usage: sh scripts/sign-macos.sh <path to holzWave.app>"
app=${1%/}
[ -d "$app" ] && [ -f "$app/Contents/Info.plist" ] || need "not an app bundle: $1"
for tool in codesign plutil; do
    command -v "$tool" >/dev/null 2>&1 || need "$tool is missing"
done

found=$(plutil -extract CFBundleIdentifier raw -o - "$app/Contents/Info.plist" 2>/dev/null) ||
    fail "no CFBundleIdentifier in $app/Contents/Info.plist"
[ "$found" = "ch.holzcloud.holzwave" ] ||
    fail "the bundle identifier is '$found': this script will not sign another app under ch.holzcloud.holzwave"

wsh="$app/Contents/MacOS/wsh"
if [ -f "$wsh" ]; then
    out=$(codesign --force --sign - --identifier ch.holzcloud.holzwave.wsh "$wsh" 2>&1) ||
        fail "could not sign Contents/MacOS/wsh: $out"
    id=$(codesign -dv "$wsh" 2>&1 | sed -n 's/^Identifier=//p' | head -n 1)
    echo "sign-macos: wsh is signed as $id"
else
    echo "sign-macos: the app has no Contents/MacOS/wsh, only the app is signed"
fi

out=$(codesign --force --sign - --identifier ch.holzcloud.holzwave "$app" 2>&1) ||
    fail "could not sign $app: $out"

out=$(codesign --verify --strict --deep --verbose=2 "$app" 2>&1) ||
    fail "the signature of $app is not valid: $out"

id=$(codesign -dv "$app" 2>&1 | sed -n 's/^Identifier=//p' | head -n 1)
echo "sign-macos: the app is signed as $id"
echo "sign-macos: ok"
