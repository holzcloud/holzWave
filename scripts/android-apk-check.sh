#!/bin/sh
# Check a built Android APK against what the project promises, on the real file.
#
#   sh scripts/android-apk-check.sh <apk> [debug|release]      (default: debug)
#
# It reads the APK with the tools of the pinned SDK (aapt2, zipalign, apksigner) and the pinned NDK (llvm-readelf), found through
# scripts/android-env.sh. A tool that is not in those folders is a missing tool (exit 2), never a skipped check. The script
# installs nothing, signs nothing, never calls an emulator or a device, and writes only to a temporary folder that it removes on
# every way out.
#
# What it checks (one line `FAIL <what>: found <x>, wanted <y>` per problem, on stderr):
#
#   identity      the package name is the identifier of tauri.conf.json (with the `.debug` suffix for debug only); the version code
#                 is what scripts/android-version-code.sh gives for the version of the app; the version name is that version
#   platform      minSdkVersion is the minimum SDK of the app; targetSdkVersion is printed; the native code is arm64-v8a and
#                 nothing else, and no file under lib/ belongs to another ABI
#   permissions   every permission of the merged manifest is on app/src-tauri/android-permissions.txt, and every listed one is
#                 asked for (a listed one that the APK lacks is reported as `info`); the one other name allowed is the signature
#                 permission that AndroidX defines for the app itself (<package>.DYNAMIC_RECEIVER_NOT_EXPORTED_PERMISSION)
#   manifest      allowBackup is false, a data extraction rules file is named and excludes the root, file, database, sharedpref and
#                 external domains for the cloud backup and for the device transfer and includes nothing, there is no
#                 fullBackupContent; the only exported component is the launcher activity and every other component says
#                 exported=false; `debuggable` and cleartext traffic exist in the debug APK only (release: neither
#                 usesCleartextTraffic nor a network security config; debug: no blanket usesCleartextTraffic)
#   16 KB pages   `zipalign -c -P 16 -v 4` passes, and every LOAD segment of every lib/arm64-v8a/*.so is aligned to 0x4000 or more
#   services      the dex files name no Google Play services, Firebase or Play core class
#   self-test     a release APK holds no trace of the debug-only self-test, neither in the dex files nor in the native library
#                 (the name of the intent extra that starts a step, and the tag its report uses); a debug APK may hold it and is
#                 only told so
#   signature     release: the APK is UNSIGNED (apksigner does not verify it and the file holds no signature files); debug: it is
#                 signed with the debug certificate (CN=Android Debug)
#
# Exit status: 0 and the last line `android-apk-check: ok`, 1 when a check failed, 2 when a tool or the file is missing or the
# arguments are wrong.

here=$(cd "$(dirname "$0")" && pwd -P)
cd "$here/.." || exit 2
. scripts/android-env.sh

apk=${1:-}
mode=${2:-debug}
case "$mode" in
    debug | release) ;;
    *)
        echo "usage: sh scripts/android-apk-check.sh <apk> [debug|release]" >&2
        exit 2
        ;;
esac
if [ -z "$apk" ] || [ ! -f "$apk" ]; then
    echo "android-apk-check: no APK at '$apk': build one with sh scripts/android-build.sh $mode" >&2
    exit 2
fi

tmp=$(mktemp -d "${TMPDIR:-/tmp}/holzwave-apk-check.XXXXXX") || exit 2
trap 'rm -rf "$tmp"' EXIT HUP INT TERM

# ---- the tools: only the pinned SDK and NDK folders, plus unzip and java from the machine --------------------------------------

build_tools="$ANDROID_HOME/build-tools/$HOLZWAVE_BUILD_TOOLS"
aapt2="$build_tools/aapt2"
zipalign="$build_tools/zipalign"
apksigner="$build_tools/apksigner"
readelf=
for candidate in "$ANDROID_NDK_HOME"/toolchains/llvm/prebuilt/*/bin/llvm-readelf; do
    [ -x "$candidate" ] && readelf=$candidate
done
missing=
for tool in "$aapt2" "$zipalign" "$apksigner"; do
    [ -x "$tool" ] || missing="$missing $tool"
done
[ -n "$readelf" ] || missing="$missing $ANDROID_NDK_HOME/toolchains/llvm/prebuilt/<host>/bin/llvm-readelf"
for tool in unzip java; do
    command -v "$tool" >/dev/null 2>&1 || missing="$missing $tool"
done
if [ -n "$missing" ]; then
    echo "android-apk-check: a tool is missing:$missing: run . scripts/android-env.sh && sh scripts/android-env-check.sh" >&2
    exit 2
fi

# ---- what the project says ------------------------------------------------------------------------------------------------------

conf=app/src-tauri/tauri.conf.json
json_string() { # <file> <key>: the first string value of "key": "value"
    sed -n "s/^[[:space:]]*\"$2\":[[:space:]]*\"\\([^\"]*\\)\".*/\\1/p" "$1" | head -n 1
}
identifier=$(json_string "$conf" identifier)
suffix=$(json_string "$conf" debugApplicationIdSuffix)
version=$(json_string "$conf" version)
permissions_file=app/src-tauri/android-permissions.txt
if [ -z "$identifier" ] || [ -z "$version" ] || [ ! -f "$permissions_file" ]; then
    echo "android-apk-check: cannot read the identifier and the version from $conf or the list $permissions_file" >&2
    exit 2
fi
want_package=$identifier
[ "$mode" = debug ] && want_package="$identifier$suffix"
want_code=$(sh scripts/android-version-code.sh "$version") || {
    echo "android-apk-check: the version $version of the app has no version code" >&2
    exit 2
}
want_min=$HOLZWAVE_ANDROID_API

failures=0
fail() { # <what>: found <x>, wanted <y>
    failures=$((failures + 1))
    echo "FAIL $1" >&2
}
info() { echo "info $1"; }
pass() { echo "ok   $1"; }

echo "android-apk-check: $apk ($mode)"

# ---- aapt2 badging: identity and platform ---------------------------------------------------------------------------------------

"$aapt2" dump badging "$apk" > "$tmp/badging.txt" 2> "$tmp/badging.err" || {
    echo "android-apk-check: aapt2 could not read the APK: $(head -n 1 "$tmp/badging.err")" >&2
    exit 2
}
package_line=$(grep '^package:' "$tmp/badging.txt" | head -n 1)
quoted() { # <line> <key>: the value of key='value'
    echo "$1" | sed -n "s/.*[[:space:]]$2='\\([^']*\\)'.*/\\1/p"
}
got_package=$(quoted "$package_line" name)
got_code=$(quoted "$package_line" versionCode)
got_name=$(quoted "$package_line" versionName)
if [ "$got_package" = "$want_package" ]; then pass "package name $got_package"; else fail "package name: found '$got_package', wanted '$want_package'"; fi
if [ "$got_code" = "$want_code" ]; then pass "version code $got_code (the scheme applied to $version)"; else fail "version code: found '$got_code', wanted '$want_code' (sh scripts/android-version-code.sh $version)"; fi
if [ "$got_name" = "$version" ]; then pass "version name $got_name"; else fail "version name: found '$got_name', wanted '$version'"; fi

got_min=$(sed -n "s/^minSdkVersion:'\\([0-9]*\\)'.*/\\1/p" "$tmp/badging.txt" | head -n 1)
got_target=$(sed -n "s/^targetSdkVersion:'\\([0-9]*\\)'.*/\\1/p" "$tmp/badging.txt" | head -n 1)
if [ "$got_min" = "$want_min" ]; then pass "minSdkVersion $got_min"; else fail "minSdkVersion: found '$got_min', wanted '$want_min'"; fi
info "targetSdkVersion ${got_target:-unknown} (the value of the project; the minimum that a store asks for was not checked)"

got_abis=$(sed -n "s/^native-code: //p" "$tmp/badging.txt" | head -n 1)
if [ "$got_abis" = "'arm64-v8a'" ]; then pass "native code arm64-v8a only"; else fail "native code: found '$got_abis', wanted 'arm64-v8a' only"; fi

# ---- the zip: other ABIs, signature files, native libraries -------------------------------------------------------------------

unzip -Z1 "$apk" > "$tmp/entries.txt" 2> "$tmp/unzip.err" || {
    echo "android-apk-check: unzip could not list the APK: $(head -n 1 "$tmp/unzip.err")" >&2
    exit 2
}
other_abis=$(sed -n 's|^lib/\([^/]*\)/.*|\1|p' "$tmp/entries.txt" | sort -u | grep -v '^arm64-v8a$' || true)
if [ -z "$other_abis" ]; then
    pass "no file under lib/ for another ABI"
else
    for abi in $other_abis; do fail "native libraries for another ABI: found lib/$abi/, wanted lib/arm64-v8a/ only"; done
fi

# ---- the merged manifest ------------------------------------------------------------------------------------------------------------

# Turn the tree that aapt2 prints into lines the checks can read: `E|<element>` where an element starts, `<element>|<attribute>|<value>`
# for each of its attributes (the namespace and the resource id of an attribute and the quotes of a string are dropped).
normalize() {
    awk '
        /^ *E: / { elem = $2; print "E|" elem; next }
        /^ *A: / {
            line = $0
            sub(/^ *A: /, "", line)
            sub(/^http:\/\/schemas\.android\.com\/apk\/res\/android:/, "", line)
            eq = index(line, "=")
            if (eq == 0) next
            name = substr(line, 1, eq - 1)
            value = substr(line, eq + 1)
            sub(/\(0x[0-9a-fA-F]+\)$/, "", name)
            if (value ~ /^"/) { sub(/^"/, "", value); sub(/".*$/, "", value) }
            print elem "|" name "|" value
        }'
}

"$aapt2" dump xmltree --file AndroidManifest.xml "$apk" 2> "$tmp/xmltree.err" | normalize > "$tmp/manifest.txt"
if [ ! -s "$tmp/manifest.txt" ]; then
    echo "android-apk-check: aapt2 could not read the manifest of the APK: $(head -n 1 "$tmp/xmltree.err")" >&2
    exit 2
fi
attr() { # <element> <attribute>: the value in the application or manifest, empty when not set
    sed -n "s/^$1|$2|//p" "$tmp/manifest.txt" | head -n 1
}

# Permissions.
sed -n 's/^uses-permission[a-z0-9-]*|name|//p' "$tmp/manifest.txt" | sort -u > "$tmp/asked.txt"
grep -v '^[[:space:]]*#' "$permissions_file" | sed 's/[[:space:]]*$//' | grep -v '^$' | sort -u > "$tmp/listed.txt"
own_permission="$want_package.DYNAMIC_RECEIVER_NOT_EXPORTED_PERMISSION"
# That name is accepted only when the manifest itself defines it, and with signature protection (0x2).
defines_own=$(awk -F'|' -v own="$own_permission" '
    $1 == "permission" && $2 == "name" && $3 == own { named = 1 }
    $1 == "permission" && $2 == "protectionLevel" && named && $3 == "0x00000002" { ok = 1 }
    END { if (ok) print "yes" }' "$tmp/manifest.txt")
permission_failures=0
while IFS= read -r permission; do
    [ -n "$permission" ] || continue
    if grep -qxF "$permission" "$tmp/listed.txt"; then continue; fi
    if [ "$permission" = "$own_permission" ] && [ "$defines_own" = yes ]; then
        info "permission $permission is the signature permission that AndroidX defines for the app itself"
        continue
    fi
    fail "permission: found $permission, wanted only those of $permissions_file"
    permission_failures=$((permission_failures + 1))
done < "$tmp/asked.txt"
while IFS= read -r permission; do
    [ -n "$permission" ] || continue
    grep -qxF "$permission" "$tmp/asked.txt" || info "the list names $permission, which the APK does not ask for"
done < "$tmp/listed.txt"
[ "$permission_failures" -eq 0 ] && pass "permissions are within the list"

# Backup.
allow_backup=$(attr application allowBackup)
if [ "$allow_backup" = false ]; then pass "allowBackup false"; else fail "allowBackup: found '${allow_backup:-not set}', wanted false"; fi
full_backup=$(attr application fullBackupContent)
if [ -z "$full_backup" ]; then pass "no fullBackupContent"; else fail "fullBackupContent: found '$full_backup', wanted none"; fi
rules_ref=$(attr application dataExtractionRules)
if [ -z "$rules_ref" ]; then
    fail "dataExtractionRules: found none, wanted a rules file"
else
    pass "dataExtractionRules $rules_ref"
    # The reference is a resource id; the resource table names the file behind it.
    rules_file=$("$aapt2" dump resources "$apk" 2> /dev/null | awk -v id="${rules_ref#@}" '
        $1 == "resource" && $2 == id { found = 1; next }
        found && /\(file\)/ { for (i = 1; i <= NF; i++) if ($i ~ /^res\/.*\.xml$/) { print $i; exit } }
        found && $1 == "resource" { exit }')
    if [ -z "$rules_file" ]; then
        fail "data extraction rules: found no file behind $rules_ref in the resource table, wanted res/xml/<name>.xml"
    else
        "$aapt2" dump xmltree --file "$rules_file" "$apk" 2> /dev/null | normalize > "$tmp/rules.txt"
        # Each section, the domains it excludes with path ".", and whether it includes anything.
        awk -F'|' '
            function flush() { if (section != "" && dom != "" && path == ".") excluded[section "|" dom] = 1; dom = ""; path = "" }
            $1 == "E" {
                flush()
                if ($2 == "cloud-backup" || $2 == "device-transfer") { section = $2; seen[$2] = 1 }
                if ($2 == "include") included = 1
                next
            }
            $1 == "exclude" && $2 == "domain" { dom = $3 }
            $1 == "exclude" && $2 == "path" { path = $3 }
            END {
                flush()
                n = split("cloud-backup device-transfer", sections, " ")
                m = split("root file database sharedpref external", domains, " ")
                for (i = 1; i <= n; i++) {
                    if (!seen[sections[i]]) print "no <" sections[i] "> section"
                    for (j = 1; j <= m; j++) if (!excluded[sections[i] "|" domains[j]]) print sections[i] " does not exclude the whole domain " domains[j]
                }
                if (included) print "an <include> element"
            }' "$tmp/rules.txt" > "$tmp/rules-problems.txt"
        if [ -s "$tmp/rules-problems.txt" ]; then
            while IFS= read -r problem; do fail "data extraction rules ($rules_file): found $problem, wanted every domain excluded for both and nothing included"; done < "$tmp/rules-problems.txt"
        else
            pass "data extraction rules exclude every domain for cloud backup and device transfer"
        fi
    fi
fi

# Components.
awk -F'|' '
    function flush() { if (cur != "") print cur "|" cname "|" exported "|" ((main && launcher) ? "launcher" : "-"); cur = "" }
    $1 == "E" {
        e = $2
        if (e == "activity" || e == "activity-alias" || e == "service" || e == "receiver" || e == "provider") {
            flush(); cur = e; cname = "?"; exported = "unset"; main = 0; launcher = 0; ctx = "component"
        } else ctx = e
        next
    }
    $1 == "activity" || $1 == "activity-alias" || $1 == "service" || $1 == "receiver" || $1 == "provider" {
        if ($2 == "name") cname = $3
        if ($2 == "exported") exported = $3
        next
    }
    $1 == "action" && $2 == "name" && $3 == "android.intent.action.MAIN" { main = 1 }
    $1 == "category" && $2 == "name" && $3 == "android.intent.category.LAUNCHER" { launcher = 1 }
    END { flush() }' "$tmp/manifest.txt" > "$tmp/components.txt"
exported_count=0
while IFS='|' read -r kind cname exported launcher; do
    [ -n "$kind" ] || continue
    case "$exported" in
        false) ;;
        true)
            exported_count=$((exported_count + 1))
            case "$kind $cname $launcher" in
                "activity "*".MainActivity launcher") ;;
                *) fail "exported component: found the $kind $cname exported, wanted only the launcher activity" ;;
            esac
            ;;
        *) fail "component $kind $cname: found android:exported '$exported', wanted false" ;;
    esac
done < "$tmp/components.txt"
if [ "$exported_count" -eq 1 ]; then
    pass "exactly one exported component, the launcher activity ($(wc -l < "$tmp/components.txt" | tr -d ' ') components in all)"
else
    fail "exported components: found $exported_count, wanted exactly 1 (the launcher activity)"
fi

# Debuggable and cleartext traffic.
debuggable=$(attr application debuggable)
cleartext=$(attr application usesCleartextTraffic)
netsec=$(attr application networkSecurityConfig)
if [ "$mode" = release ]; then
    if [ -z "$debuggable" ] || [ "$debuggable" = false ]; then pass "release is not debuggable"; else fail "debuggable: found '$debuggable' in the release APK, wanted not debuggable"; fi
    if [ -z "$cleartext" ] || [ "$cleartext" = false ]; then pass "release has no usesCleartextTraffic"; else fail "usesCleartextTraffic: found '$cleartext' in the release APK, wanted none"; fi
    if [ -z "$netsec" ]; then pass "release has no network security config"; else fail "networkSecurityConfig: found '$netsec' in the release APK, wanted none"; fi
else
    if [ "$debuggable" = true ]; then pass "debug is debuggable"; else fail "debuggable: found '${debuggable:-not set}' in the debug APK, wanted true"; fi
    if [ "$cleartext" = true ]; then fail "usesCleartextTraffic: found true in the debug APK, wanted none (cleartext goes only to 10.0.2.2 through the network security config)"; else pass "debug has no blanket usesCleartextTraffic"; fi
    if [ -n "$netsec" ]; then info "the debug APK names the network security config $netsec (cleartext to the emulator's host alias only)"; else info "the debug APK has no network security config"; fi
fi

# ---- 16 KB pages ----------------------------------------------------------------------------------------------------------------

if "$zipalign" -c -P 16 -v 4 "$apk" > "$tmp/zipalign.txt" 2>&1; then
    pass "zipalign -c -P 16 -v 4 passes"
else
    bad=$(grep -i 'BAD' "$tmp/zipalign.txt" | head -n 1)
    fail "zipalign: found '${bad:-a failed check}', wanted every uncompressed .so aligned for 16 KB pages"
fi
unzip -q -o "$apk" 'lib/arm64-v8a/*.so' -d "$tmp/x" > /dev/null 2>&1
libs=0
for so in "$tmp"/x/lib/arm64-v8a/*.so; do
    [ -f "$so" ] || continue
    libs=$((libs + 1))
    name=${so##*/}
    "$readelf" -lW "$so" 2> /dev/null | awk '$1 == "LOAD" { print $NF }' | sort -u > "$tmp/align.txt"
    if [ ! -s "$tmp/align.txt" ]; then
        fail "alignment of $name: found no LOAD segment, wanted LOAD segments aligned to 0x4000"
        continue
    fi
    weakest=
    while IFS= read -r value; do
        case "$value" in
            0x[0-9a-fA-F]*) [ "$((value))" -ge 16384 ] || weakest=$value ;;
            *) weakest=$value ;;
        esac
    done < "$tmp/align.txt"
    if [ -z "$weakest" ]; then pass "$name: every LOAD segment is aligned to 0x4000 or more"; else fail "alignment of $name: found a LOAD segment aligned to $weakest, wanted 0x4000 or more"; fi
done
[ "$libs" -gt 0 ] || fail "native libraries: found none under lib/arm64-v8a/, wanted the app library"

# ---- services and trackers --------------------------------------------------------------------------------------------------------

service_hits=$(unzip -p "$apk" 'classes*.dex' 2> /dev/null | grep -a -c -E 'Lcom/google/(android/gms|firebase|android/play/core)/' || true)
if [ "${service_hits:-0}" -eq 0 ]; then pass "the dex files name no Google Play services, Firebase or Play core class"; else fail "services: found $service_hits dex lines naming Google Play services, Firebase or Play core, wanted none"; fi

# ---- the debug-only self-test -----------------------------------------------------------------------------------------------------

# The emulator's self-test of the secret store starts from an intent extra that exists in the debug source set, and its Rust is
# compiled for debug builds only. A release APK must hold neither: the name of the extra and the tag of its report are searched
# in the dex files and in the native libraries.
selftest_hits=$( { unzip -p "$apk" 'classes*.dex' 2> /dev/null; unzip -p "$apk" 'lib/arm64-v8a/*.so' 2> /dev/null; } | grep -a -c -E 'holzwave[_-]selftest' || true)
if [ "$mode" = release ]; then
    if [ "${selftest_hits:-0}" -eq 0 ]; then pass "the release APK holds no self-test (no holzwave_selftest in the dex files or the native library)"; else fail "self-test: found $selftest_hits lines naming holzwave_selftest in the release APK, wanted none"; fi
else
    if [ "${selftest_hits:-0}" -eq 0 ]; then info "the debug APK holds no self-test trigger"; else info "the debug APK holds the self-test, as intended for the runs of a debug build"; fi
fi

# ---- the signature ------------------------------------------------------------------------------------------------------------------

signature_files=$(grep -E '^META-INF/.*\.(SF|RSA|DSA|EC)$' "$tmp/entries.txt" | head -n 1 || true)
if "$apksigner" verify --print-certs "$apk" > "$tmp/signer.txt" 2>&1; then verifies=yes; else verifies=no; fi
if [ "$mode" = release ]; then
    if [ "$verifies" = no ] && [ -z "$signature_files" ]; then
        pass "the release APK is UNSIGNED, as intended: the owner signs it with the owner's own key"
    else
        fail "signature: found a release APK that verifies or holds signature files (${signature_files:-apksigner verified it}), wanted it unsigned"
    fi
else
    dn=$(sed -n 's/^.*certificate DN: //p' "$tmp/signer.txt" | head -n 1)
    case "$dn" in
        *"CN=Android Debug"*) [ "$verifies" = yes ] && pass "the debug APK is signed with the debug certificate ($dn)" ;;
        *) fail "signature: found '${dn:-no certificate}', wanted the debug certificate CN=Android Debug" ;;
    esac
fi

if [ "$failures" -ne 0 ]; then
    echo "android-apk-check: $failures check(s) failed" >&2
    exit 1
fi
echo "android-apk-check: ok"
