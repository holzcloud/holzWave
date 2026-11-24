#!/bin/sh
# Inspect a built macOS app bundle for the `wsh` that ships with it.
#
#   sh scripts/bundle-check-macos.sh <path to holzWave.app> [<the wsh sidecar that was built>]
#
# The app puts the folder of its own executable on every pane's PATH when `wsh` is in that folder, so the PATH-entry
# check is: `Contents/MacOS/wsh` sits in the same folder as the executable that `CFBundleExecutable` of Info.plist
# names. Checked, any mismatch fails:
#   - `Contents/MacOS/wsh` exists, is executable, and is beside the app's own executable;
#   - `file` says it is a Mach-O 64-bit executable for arm64 (the Apple Silicon app);
#   - `codesign --verify --strict` passes on it and it is signed as `ch.holzcloud.holzwave.wsh` (`codesign -dv`,
#     `Identifier=`; an ad hoc signature is fine, there is no notarisation); the app is signed as
#     `ch.holzcloud.holzwave`, equal to `CFBundleIdentifier`, and `codesign --verify --strict --deep` passes on the
#     whole app (`sh scripts/sign-macos.sh <app>` sets the identifiers);
#   - `wsh help` prints a line that starts `usage: wsh` and exits 2 (it runs, it is not a stale or foreign file);
#   - `Contents/Resources/wsh/linux-x86_64` and `linux-aarch64` exist and are static 64-bit ELF files of the right
#     machine (the builds that the app installs on a remote Linux host);
#   - neither the app's executable nor `wsh` is world-writable;
#   - with the second argument (the file scripts/wsh-sidecar.sh wrote), `wsh` in the bundle is identical to it with both
#     signatures removed (the bundler and `scripts/sign-macos.sh` replace the signature of the bundled copy) and the two
#     Linux resources are byte-identical to the files of the same folder (the package holds what was built, not a
#     stale or planted file).
#   - `Contents/Info.plist` holds a non-empty `NSLocalNetworkUsageDescription` (the text of the macOS Local Network
#     question) and the Bonjour type `_holzwave-probe._tcp` in `NSBonjourServices`; both come from
#     `app/src-tauri/Info.plist`, which the Tauri bundler merges into the bundle.
#
# Exit status: 0 and the last line `bundle-check-macos: ok`, 1 when a check failed, 2 for a bad argument or a
# missing tool.

fail() {
    echo "bundle-check-macos: FAILED: $*" >&2
    exit 1
}
need() {
    echo "bundle-check-macos: $*" >&2
    exit 2
}

[ $# -ge 1 ] && [ $# -le 2 ] || need "usage: sh scripts/bundle-check-macos.sh <path to holzWave.app> [<built wsh sidecar>]"
app=${1%/}
built=${2:-}
[ -z "$built" ] || [ -f "$built" ] || need "not a file: $built"
[ -d "$app" ] && [ -f "$app/Contents/Info.plist" ] || need "not an app bundle: $1"
for tool in file plutil codesign; do
    command -v "$tool" >/dev/null 2>&1 || need "$tool is missing"
done

exe_name=$(plutil -extract CFBundleExecutable raw -o - "$app/Contents/Info.plist" 2>/dev/null) || fail "no CFBundleExecutable in Info.plist"
[ -n "$exe_name" ] || fail "CFBundleExecutable is empty"
macos="$app/Contents/MacOS"
exe="$macos/$exe_name"
wsh="$macos/wsh"
echo "bundle-check-macos: CFBundleExecutable $exe_name, folder $macos"
[ -f "$exe" ] && [ -x "$exe" ] || fail "the app executable $exe is missing"
[ -f "$wsh" ] || fail "no Contents/MacOS/wsh: a pane would find no wsh"
[ -x "$wsh" ] || fail "Contents/MacOS/wsh is not executable"
[ "$(dirname "$wsh")" = "$(dirname "$exe")" ] || fail "wsh is not beside the app executable"
echo "bundle-check-macos: Contents/MacOS/wsh is beside $exe_name"

wsh_type=$(file -b "$wsh")
echo "bundle-check-macos: wsh: $wsh_type"
case "$wsh_type" in
    "Mach-O 64-bit executable arm64"*) ;;
    *) fail "Contents/MacOS/wsh is not a Mach-O 64-bit arm64 executable" ;;
esac

sig=$(codesign --verify --strict --verbose=2 "$wsh" 2>&1) || fail "the signature of Contents/MacOS/wsh is not valid: $sig"
echo "bundle-check-macos: wsh signature valid"
wsh_id=$(codesign -dv "$wsh" 2>&1 | sed -n 's/^Identifier=//p' | head -n 1)
[ "$wsh_id" = "ch.holzcloud.holzwave.wsh" ] ||
    fail "wsh is signed as '$wsh_id', not ch.holzcloud.holzwave.wsh: sh scripts/sign-macos.sh <app> sets it"
echo "bundle-check-macos: wsh is signed as ch.holzcloud.holzwave.wsh"

out=$("$wsh" help 2>&1)
status=$?
first=$(printf '%s\n' "$out" | head -n 1)
case "$first" in
    "usage: wsh"*) ;;
    *) fail "wsh help printed '$first', not a usage line" ;;
esac
[ "$status" = 2 ] || fail "wsh help exited $status, not 2"
echo "bundle-check-macos: wsh help: $first (exit $status)"

for arch in x86_64 aarch64; do
    res="$app/Contents/Resources/wsh/linux-$arch"
    [ -f "$res" ] || fail "no Contents/Resources/wsh/linux-$arch"
    desc=$(file -b "$res")
    echo "bundle-check-macos: linux-$arch: $desc"
    case "$arch" in
        x86_64) machine="x86-64" ;;
        aarch64) machine="ARM aarch64" ;;
    esac
    case "$desc" in
        *"ELF 64-bit LSB"*"$machine"*) ;;
        *) fail "Contents/Resources/wsh/linux-$arch is not a 64-bit ELF for $machine" ;;
    esac
    case "$desc" in
        *"dynamically linked"*) fail "Contents/Resources/wsh/linux-$arch is dynamically linked" ;;
        *"statically linked"* | *"static-pie linked"*) ;;
        *) fail "Contents/Resources/wsh/linux-$arch is not statically linked" ;;
    esac
done

if [ -n "$built" ]; then
    # The signature of the bundled copy was replaced, so the code is compared with both signatures removed.
    tmp=$(mktemp -d) || fail "mktemp failed"
    trap 'rm -rf "$tmp"' EXIT
    cp "$wsh" "$tmp/bundled" && cp "$built" "$tmp/built" || fail "could not copy the sidecars to $tmp"
    for copy in bundled built; do
        out=$(codesign --remove-signature "$tmp/$copy" 2>&1) || fail "could not remove the signature of the $copy sidecar: $out"
    done
    cmp -s "$tmp/bundled" "$tmp/built" || fail "Contents/MacOS/wsh is not the code of the sidecar that was built ($built)"
    for arch in x86_64 aarch64; do
        cmp -s "$app/Contents/Resources/wsh/linux-$arch" "$(dirname "$built")/wsh-linux-$arch" ||
            fail "Contents/Resources/wsh/linux-$arch is not the file that was built"
    done
    echo "bundle-check-macos: wsh is the built code (compared without its signature) and the two Linux resources are the files that were built"
fi

for f in "$exe" "$wsh"; do
    [ -z "$(find "$f" -perm -002 2>/dev/null)" ] || fail "$f is world-writable"
done
echo "bundle-check-macos: the executables are not world-writable"

bundle_id=$(plutil -extract CFBundleIdentifier raw -o - "$app/Contents/Info.plist" 2>/dev/null) ||
    fail "no CFBundleIdentifier in Info.plist"
[ "$bundle_id" = "ch.holzcloud.holzwave" ] || fail "CFBundleIdentifier is '$bundle_id', not ch.holzcloud.holzwave"
app_id=$(codesign -dv "$app" 2>&1 | sed -n 's/^Identifier=//p' | head -n 1)
[ "$app_id" = "$bundle_id" ] ||
    fail "the app is signed as '$app_id', not $bundle_id: a build signed only by the linker carries a new identifier on every build, so macOS treats each build as a new app and the Local Network permission is lost"
echo "bundle-check-macos: the app is signed as $app_id, the same as CFBundleIdentifier"
sig=$(codesign --verify --strict --deep --verbose=2 "$app" 2>&1) || fail "the signature of the app is not valid: $sig"
echo "bundle-check-macos: the app signature is valid (strict, deep)"

usage=$(plutil -extract NSLocalNetworkUsageDescription raw -o - "$app/Contents/Info.plist" 2>/dev/null) ||
    fail "no NSLocalNetworkUsageDescription in Info.plist: macOS would ask about the local network without a reason"
[ -n "$usage" ] || fail "NSLocalNetworkUsageDescription is empty"
bonjour=$(plutil -extract NSBonjourServices json -o - "$app/Contents/Info.plist" 2>/dev/null) ||
    fail "no NSBonjourServices in Info.plist"
case "$bonjour" in
    *'"_holzwave-probe._tcp"'*) ;;
    *) fail "NSBonjourServices does not name _holzwave-probe._tcp: $bonjour" ;;
esac
echo "bundle-check-macos: Info.plist declares the local network use (usage text, Bonjour type _holzwave-probe._tcp)"
echo "bundle-check-macos: ok"
