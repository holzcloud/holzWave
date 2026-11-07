#!/bin/sh
# Prove every rule of scripts/android-apk-check.sh, once passing and once failing, without any Android software.
#
#   sh scripts/test-android-apk-check.sh
#
# Each case builds a throwaway directory with a copy of the script, android-env.sh, the version script, a small tauri.conf.json
# and the permission list, a fake SDK and NDK tree, and shims for aapt2, zipalign, apksigner, llvm-readelf, unzip and java that
# print fixed texts (the shapes are those of the real tools). The shims log every call; the last checks assert that nothing
# outside them was reached: no adb, no emulator, no sdkmanager, no keytool, no network tool. Nothing is installed or signed.
#
# Exit status: 0 everything held, 1 a check failed.

here=$(cd "$(dirname "$0")" && pwd -P)
root=$(cd "$here/.." && pwd -P)

checks=0
failures=0
ok() {
    checks=$((checks + 1))
    echo "  ok   $1"
}
bad() {
    checks=$((checks + 1))
    failures=$((failures + 1))
    echo "  FAIL $1" >&2
}

tmp=$(mktemp -d "${TMPDIR:-/tmp}/holzwave-apk-check-test.XXXXXX") || exit 1
trap 'rm -rf "$tmp"' EXIT HUP INT TERM
LOG="$tmp/calls.log"
: > "$LOG"

pin() { (cd "$root" && . scripts/android-env.sh >/dev/null 2>&1 && eval "printf %s \"\$$1\""); }
NDK=$(pin HOLZWAVE_NDK_VERSION)
API=$(pin HOLZWAVE_ANDROID_API)
BT=$(pin HOLZWAVE_BUILD_TOOLS)

# ---- the fixtures: what the real tools print for a good APK, with a hole for each rule ------------------------------------------

# The manifest tree of aapt2. Variables (defaults in brackets) switch one fact each:
#   F_PACKAGE        the package name [per mode]
#   F_BACKUP         the allowBackup attribute line [false]
#   F_FULL_BACKUP    an extra application attribute line []
#   F_RULES          the dataExtractionRules attribute line [a reference]
#   F_DEBUGGABLE     the debuggable attribute line [debug: true, release: none]
#   F_CLEARTEXT      an attribute line for usesCleartextTraffic or networkSecurityConfig [debug: the config, release: none]
#   F_PERMISSION     an extra uses-permission name []
#   F_COMPONENTS     extra component lines []
#   F_MAIN_EXPORTED  the exported value of the launcher activity [true]
manifest_tree() {
    pkg=${F_PACKAGE:-$1}
    printf '%s\n' \
        'N: android=http://schemas.android.com/apk/res/android (line=2)' \
        '  E: manifest (line=2)' \
        '    A: http://schemas.android.com/apk/res/android:versionCode(0x0101021b)=1202' \
        '    A: http://schemas.android.com/apk/res/android:versionName(0x0101021c)="0.0.1-beta2" (Raw: "0.0.1-beta2")' \
        "    A: package=\"$pkg\" (Raw: \"$pkg\")" \
        '      E: uses-sdk (line=7)' \
        '        A: http://schemas.android.com/apk/res/android:minSdkVersion(0x0101020c)=31' \
        '        A: http://schemas.android.com/apk/res/android:targetSdkVersion(0x01010270)=37' \
        '      E: uses-permission (line=11)' \
        '        A: http://schemas.android.com/apk/res/android:name(0x01010003)="android.permission.INTERNET" (Raw: "android.permission.INTERNET")' \
        '      E: permission (line=18)' \
        "        A: http://schemas.android.com/apk/res/android:name(0x01010003)=\"$pkg.DYNAMIC_RECEIVER_NOT_EXPORTED_PERMISSION\" (Raw: \"x\")" \
        '        A: http://schemas.android.com/apk/res/android:protectionLevel(0x01010009)=0x00000002' \
        '      E: uses-permission (line=22)' \
        "        A: http://schemas.android.com/apk/res/android:name(0x01010003)=\"$pkg.DYNAMIC_RECEIVER_NOT_EXPORTED_PERMISSION\" (Raw: \"x\")"
    if [ -n "${F_PERMISSION:-}" ]; then
        printf '%s\n' '      E: uses-permission (line=23)' \
            "        A: http://schemas.android.com/apk/res/android:name(0x01010003)=\"$F_PERMISSION\" (Raw: \"x\")"
    fi
    printf '%s\n' '      E: application (line=24)'
    echo "        ${F_BACKUP:-A: http://schemas.android.com/apk/res/android:allowBackup(0x01010280)=false}"
    echo "        ${F_RULES:-A: http://schemas.android.com/apk/res/android:dataExtractionRules(0x010105f1)=@0x7f120000}"
    [ -n "${F_FULL_BACKUP:-}" ] && echo "        $F_FULL_BACKUP"
    if [ "$1" = ch.holzcloud.holzwave.debug ]; then
        echo "        ${F_DEBUGGABLE-A: http://schemas.android.com/apk/res/android:debuggable(0x0101000f)=true}"
        echo "        ${F_CLEARTEXT-A: http://schemas.android.com/apk/res/android:networkSecurityConfig(0x01010527)=@0x7f120001}"
    else
        [ -n "${F_DEBUGGABLE:-}" ] && echo "        $F_DEBUGGABLE"
        [ -n "${F_CLEARTEXT:-}" ] && echo "        $F_CLEARTEXT"
    fi
    printf '%s\n' \
        '          E: activity (line=32)' \
        '            A: http://schemas.android.com/apk/res/android:name(0x01010003)="ch.holzcloud.holzwave.MainActivity" (Raw: "ch.holzcloud.holzwave.MainActivity")' \
        "            A: http://schemas.android.com/apk/res/android:exported(0x01010010)=${F_MAIN_EXPORTED:-true}" \
        '              E: intent-filter (line=38)' \
        '                  E: action (line=39)' \
        '                    A: http://schemas.android.com/apk/res/android:name(0x01010003)="android.intent.action.MAIN" (Raw: "android.intent.action.MAIN")' \
        '                  E: category (line=41)' \
        '                    A: http://schemas.android.com/apk/res/android:name(0x01010003)="android.intent.category.LAUNCHER" (Raw: "android.intent.category.LAUNCHER")' \
        '          E: provider (line=56)' \
        '            A: http://schemas.android.com/apk/res/android:name(0x01010003)="androidx.startup.InitializationProvider" (Raw: "x")' \
        '            A: http://schemas.android.com/apk/res/android:exported(0x01010010)=false' \
        '              E: meta-data (line=60)' \
        '                A: http://schemas.android.com/apk/res/android:name(0x01010003)="androidx.lifecycle.ProcessLifecycleInitializer" (Raw: "x")'
    [ -n "${F_COMPONENTS:-}" ] && printf '%s\n' "$F_COMPONENTS"
    return 0
}

rules_tree() {
    # $1: the domains excluded in each section
    printf '%s\n' 'N: android=http://schemas.android.com/apk/res/android (line=1)' '  E: data-extraction-rules (line=7)'
    for section in cloud-backup device-transfer; do
        printf '    E: %s (line=8)\n' "$section"
        for domain in $1; do
            printf '      E: exclude (line=9)\n        A: domain="%s" (Raw: "%s")\n        A: path="." (Raw: ".")\n' "$domain" "$domain"
        done
        [ "$section" = cloud-backup ] && [ -n "${F_INCLUDE:-}" ] && printf '      E: include (line=9)\n        A: domain="file" (Raw: "file")\n        A: path="vault" (Raw: "vault")\n'
    done
    return 0
}

n=0
# mkcase <debug|release>: the fake machine in $c with good fixtures; the F_ variables and the C_ variables below change one fact.
#   C_ENTRIES   the zip listing [one library of arm64-v8a, a manifest, a dex]
#   C_ZIPALIGN  the exit status of zipalign [0]
#   C_ALIGN     the alignment printed by llvm-readelf [0x4000]
#   C_CODE      the version code in the badging [1202]
#   C_ABIS      the native-code line [arm64-v8a]
#   C_MIN       the minSdkVersion [31]
#   C_SIGNED    for the apksigner shim: yes or no
#   C_DN        the certificate DN [debug certificate]
#   C_DEX       text that the dex files hold []
#   C_SO        text that the native library holds []
#   C_NO_VERIFIER  set: the dex files do not hold the verifier component (it is there by default)
#   C_NO_INIT   set: the native library does not export the verifier's set-up function (it does by default)
#   C_TOOL      a tool to leave out of the fake SDK: aapt2, zipalign, apksigner, readelf, unzip
#   C_RULES     the domains excluded in the rules file [root file database sharedpref external device_root]
mkcase() {
    n=$((n + 1))
    c="$tmp/case$n"
    mode=$1
    sdk="$c/sdk"
    bin="$sdk/ndk/$NDK/toolchains/llvm/prebuilt/darwin-x86_64/bin"
    mkdir -p "$c/bin" "$c/home" "$c/brew" "$c/tmpdir" "$c/jdk/bin" "$c/repo/scripts" "$c/repo/app/src-tauri" \
        "$bin" "$sdk/build-tools/$BT" "$sdk/platform-tools" "$sdk/emulator" "$sdk/cmdline-tools/latest/bin"
    printf 'Pkg.Revision = %s\n' "$NDK" > "$sdk/ndk/$NDK/source.properties"
    : > "$bin/aarch64-linux-android$API-clang"
    chmod +x "$bin/aarch64-linux-android$API-clang"
    : > "$c/app.apk"
    cp "$here/android-env.sh" "$here/android-apk-check.sh" "$here/android-version-code.sh" "$c/repo/scripts/"
    printf '{\n  "version": "0.0.1-beta2",\n  "identifier": "ch.holzcloud.holzwave",\n  "bundle": {\n    "android": {\n      "debugApplicationIdSuffix": ".debug"\n    }\n  }\n}\n' > "$c/repo/app/src-tauri/tauri.conf.json"
    printf '# the list\nandroid.permission.INTERNET\n' > "$c/repo/app/src-tauri/android-permissions.txt"

    if [ "$mode" = debug ]; then pkg=ch.holzcloud.holzwave.debug; else pkg=ch.holzcloud.holzwave; fi
    {
        echo "package: name='${F_PACKAGE:-$pkg}' versionCode='${C_CODE:-1202}' versionName='0.0.1-beta2' platformBuildVersionName='17'"
        echo "minSdkVersion:'${C_MIN:-31}'"
        echo "targetSdkVersion:'37'"
        echo "native-code: '${C_ABIS:-arm64-v8a}'"
    } > "$c/badging.txt"
    manifest_tree "$pkg" > "$c/manifest.txt"
    printf '%s\n' '  resource 0x7f120000 xml/data_extraction_rules' '      () (file) res/xml/data_extraction_rules.xml type=XML' \
        '  resource 0x7f120001 xml/network_security_config' '      () (file) res/xml/network_security_config.xml type=XML' > "$c/resources.txt"
    rules_tree "${C_RULES:-root file database sharedpref external device_root}" > "$c/rules.txt"
    printf '%s\n' "${C_ENTRIES:-AndroidManifest.xml
classes.dex
lib/arm64-v8a/libholzwave_app_lib.so
resources.arsc}" > "$c/entries.txt"
    echo "${C_ZIPALIGN:-0}" > "$c/zipalign.rc"
    echo "Verification ${C_ZIPALIGN:-successful}" > "$c/zipalign.txt"
    [ "${C_ZIPALIGN:-0}" -ne 0 ] && printf '   4096 lib/arm64-v8a/libholzwave_app_lib.so (BAD - 4096)\nVerification FAILED\n' > "$c/zipalign.txt"
    printf '  Type Offset VirtAddr PhysAddr FileSiz MemSiz Flg Align\n  LOAD 0x000000 0x0000000000000000 0x0000000000000000 0x001234 0x001234 R   %s\n  LOAD 0x004000 0x0000000000004000 0x0000000000004000 0x002000 0x002000 R E %s\n' \
        "${C_ALIGN:-0x4000}" "${C_ALIGN:-0x4000}" > "$c/readelf.txt"
    printf '%s\n' "${C_DEX:-}" > "$c/dex.txt"
    [ -n "${C_NO_VERIFIER:-}" ] || echo 'Lorg/rustls/platformverifier/CertificateVerifier;' >> "$c/dex.txt"
    printf '%s\n' "${C_SO:-}" > "$c/so.txt"
    [ -n "${C_NO_INIT:-}" ] || echo 'Java_ch_holzcloud_holzwave_HolzwavePlugin_initTls' >> "$c/so.txt"
    if [ "$mode" = release ]; then
        echo 1 > "$c/signer.rc"
        printf 'DOES NOT VERIFY\nERROR: Missing META-INF/MANIFEST.MF\n' > "$c/signer.txt"
    else
        echo 0 > "$c/signer.rc"
        printf 'Verification succeeded\nV2 Signer: certificate DN: %s\n' "${C_DN:-C=US, O=Android, CN=Android Debug}" > "$c/signer.txt"
    fi
    if [ "${C_SIGNED:-}" = yes ]; then
        echo 0 > "$c/signer.rc"
        printf 'Verification succeeded\nV2 Signer: certificate DN: CN=Some Owner\n' > "$c/signer.txt"
        printf 'META-INF/CERT.SF\nMETA-INF/CERT.RSA\n' >> "$c/entries.txt"
    fi

    shim "$c/bin/java" "echo 'openjdk version \"21.0.5\"' >&2"
    shim "$sdk/build-tools/$BT/aapt2" 'case "$*" in
  "dump badging "*) cat "$HW_CASE/badging.txt" ;;
  "dump xmltree --file AndroidManifest.xml "*) cat "$HW_CASE/manifest.txt" ;;
  "dump resources "*) cat "$HW_CASE/resources.txt" ;;
  "dump xmltree --file res/xml/data_extraction_rules.xml "*) cat "$HW_CASE/rules.txt" ;;
  *) echo "unexpected aapt2 call" >&2; exit 9 ;;
esac'
    shim "$sdk/build-tools/$BT/zipalign" 'cat "$HW_CASE/zipalign.txt"; exit "$(cat "$HW_CASE/zipalign.rc")"'
    shim "$sdk/build-tools/$BT/apksigner" 'cat "$HW_CASE/signer.txt"; exit "$(cat "$HW_CASE/signer.rc")"'
    shim "$bin/llvm-readelf" 'cat "$HW_CASE/readelf.txt"'
    shim "$c/bin/unzip" 'case "$1" in
  -Z1) cat "$HW_CASE/entries.txt" ;;
  -p) case "$3" in lib/*) cat "$HW_CASE/so.txt" ;; *) cat "$HW_CASE/dex.txt" ;; esac ;;
  -q)
    # unzip -q -o <apk> <pattern> -d <dir>
    dir=$6
    for entry in $(grep "^lib/arm64-v8a/.*[.]so$" "$HW_CASE/entries.txt"); do
      mkdir -p "$dir/$(dirname "$entry")"
      : > "$dir/$entry"
    done
    ;;
  *) echo "unexpected unzip call" >&2; exit 9 ;;
esac'
    for forbidden in adb emulator avdmanager sdkmanager keytool jarsigner brew curl wget npm cargo; do
        shim "$c/bin/$forbidden" "exit 9"
    done
    case "${C_TOOL:-}" in
        aapt2) rm -f "$sdk/build-tools/$BT/aapt2" ;;
        zipalign) rm -f "$sdk/build-tools/$BT/zipalign" ;;
        apksigner) rm -f "$sdk/build-tools/$BT/apksigner" ;;
        readelf) rm -f "$bin/llvm-readelf" ;;
        unzip)
            # The machine has a real unzip: the case gets a PATH of its own tools without it.
            rm -f "$c/bin/unzip"
            mkdir -p "$c/tools"
            for t in awk sed grep sort mktemp wc tr head dirname cat rm ls env sh printf uname basename; do
                for d in /usr/bin /bin; do
                    [ -x "$d/$t" ] && { ln -sf "$d/$t" "$c/tools/$t"; break; }
                done
            done
            ;;
    esac
}
shim() { # <path> <body>: a program that logs its call, then runs the body
    printf '#!/bin/sh\necho "%s $*" >> "$HW_LOG"\n%s\n' "$(basename "$1")" "$2" > "$1"
    chmod +x "$1"
}
# check <apk-mode>: run the script on the fake machine; output in $out (stdout and stderr), status in $rc.
check() {
    out=$(cd "$c/repo" && env -i HOME="$c/home" HOMEBREW_PREFIX="$c/brew" HW_LOG="$LOG" HW_CASE="$c" TMPDIR="$c/tmpdir" \
        PATH="$c/bin:${C_PATH:-/usr/bin:/bin}" HOLZWAVE_ANDROID_HOME="$sdk" HOLZWAVE_JAVA_HOME="$c/jdk" \
        sh scripts/android-apk-check.sh "$c/app.apk" "${1:-$mode}" 2>&1)
    rc=$?
}
clean_after() { [ -z "$(ls -A "$c/tmpdir")" ]; }
reset_f() { unset F_PACKAGE F_BACKUP F_FULL_BACKUP F_RULES F_DEBUGGABLE F_CLEARTEXT F_PERMISSION F_COMPONENTS F_MAIN_EXPORTED F_INCLUDE; }
reset_c() { unset C_ENTRIES C_ZIPALIGN C_ALIGN C_CODE C_ABIS C_MIN C_SIGNED C_DN C_DEX C_SO C_TOOL C_RULES C_NO_VERIFIER C_NO_INIT; }

# rule <name> <mode>: with the F_ and C_ variables set by the caller, a run must fail with a FAIL line that matches <pattern>.
expect_fail() { # <what> <mode> <pattern>
    what=$1
    mkcase "$2"
    check
    if [ "$rc" -eq 1 ] && echo "$out" | grep -q "^FAIL .*$3" && ! echo "$out" | grep -q '^android-apk-check: ok$' && clean_after; then
        ok "fails: $what"
    else
        bad "$what: wanted exit 1 and a FAIL line matching '$3' (rc $rc, left: $(ls -A "$c/tmpdir")): $out"
    fi
    reset_f
    reset_c
}

echo "good APKs pass"
for mode in debug release; do
    mkcase "$mode"
    check
    if [ "$rc" -eq 0 ] && [ "$(echo "$out" | tail -n 1)" = "android-apk-check: ok" ] && ! echo "$out" | grep -q '^FAIL' && clean_after; then
        ok "a good $mode APK passes: exit 0, the last line android-apk-check: ok, no FAIL line, nothing left behind"
    else
        bad "good $mode APK (rc $rc): $out"
    fi
done
mkcase release
check
echo "$out" | grep -q 'info targetSdkVersion 37' && ok "the target SDK is printed" || bad "no targetSdkVersion line: $out"
echo "$out" | grep -q 'signature permission that AndroidX defines' && ok "the signature permission of AndroidX is accepted and said so" || bad "no info line for the own permission: $out"

echo "identity and platform"
F_PACKAGE=com.example.other; expect_fail "a wrong package name" release "package name: found 'com.example.other'"
F_PACKAGE=ch.holzcloud.holzwave.debug; expect_fail "a debug suffix on a release APK" release "package name: found 'ch.holzcloud.holzwave.debug', wanted 'ch.holzcloud.holzwave'"
F_PACKAGE=ch.holzcloud.holzwave; expect_fail "no debug suffix on a debug APK" debug "package name: found 'ch.holzcloud.holzwave', wanted 'ch.holzcloud.holzwave.debug'"
C_CODE=1; expect_fail "a wrong version code (the default of the Tauri CLI)" release "version code: found '1', wanted '1202'"
C_MIN=24; expect_fail "a minSdkVersion of 24" release "minSdkVersion: found '24', wanted '31'"
C_ABIS="arm64-v8a' 'x86_64"; expect_fail "a second ABI in native-code" release "native code"
C_ABIS="x86_64"; expect_fail "a wrong ABI in native-code" release "native code"
C_ENTRIES='AndroidManifest.xml
lib/arm64-v8a/libholzwave_app_lib.so
lib/x86_64/libholzwave_app_lib.so'; expect_fail "a second ABI folder" release "another ABI: found lib/x86_64/"

echo "permissions"
F_PERMISSION=android.permission.CAMERA; expect_fail "a permission that is not on the list" release "permission: found android.permission.CAMERA"
F_PERMISSION=android.permission.REQUEST_INSTALL_PACKAGES; expect_fail "the install-packages permission" release "permission: found android.permission.REQUEST_INSTALL_PACKAGES"
mkcase release
printf 'android.permission.INTERNET\nandroid.permission.POST_NOTIFICATIONS\n' > "$c/repo/app/src-tauri/android-permissions.txt"
check
if [ "$rc" -eq 0 ] && echo "$out" | grep -q 'info the list names android.permission.POST_NOTIFICATIONS, which the APK does not ask for'; then
  ok "a listed permission that the APK lacks is an info line, not a failure"
else
  bad "listed but not asked (rc $rc): $out"
fi
mkcase release
printf 'android.permission.INTERNET\n' > "$c/repo/app/src-tauri/android-permissions.txt"
sed 's/protectionLevel(0x01010009)=0x00000002/protectionLevel(0x01010009)=0x00000000/' "$c/manifest.txt" > "$c/manifest.new" && mv "$c/manifest.new" "$c/manifest.txt"
check
if [ "$rc" -eq 1 ] && echo "$out" | grep -q '^FAIL permission: found ch.holzcloud.holzwave.DYNAMIC_RECEIVER_NOT_EXPORTED_PERMISSION'; then
  ok "fails: the permission of AndroidX without signature protection is not accepted"
else
  bad "own permission without signature level (rc $rc): $out"
fi

echo "backup and extraction rules"
F_BACKUP="A: http://schemas.android.com/apk/res/android:allowBackup(0x01010280)=true"; expect_fail "allowBackup true" release "allowBackup: found 'true'"
F_BACKUP="A: http://schemas.android.com/apk/res/android:label(0x01010001)=@0x7f0f001c"; expect_fail "allowBackup not set" release "allowBackup: found 'not set'"
F_FULL_BACKUP="A: http://schemas.android.com/apk/res/android:fullBackupContent(0x01010327)=@0x7f120002"; expect_fail "a fullBackupContent file" release "fullBackupContent: found '@0x7f120002'"
F_RULES="A: http://schemas.android.com/apk/res/android:label(0x01010001)=@0x7f0f001c"; expect_fail "no data extraction rules" release "dataExtractionRules: found none"
C_RULES="root file database sharedpref"; expect_fail "a rules file that leaves out the external domain" release "does not exclude the whole domain external"
C_RULES="file database sharedpref external"; expect_fail "a rules file that leaves out the root domain" release "does not exclude the whole domain root"
F_INCLUDE=1; expect_fail "a rules file that includes a path" release "an <include> element"
mkcase release
: > "$c/resources.txt"
check
[ "$rc" -eq 1 ] && echo "$out" | grep -q '^FAIL data extraction rules: found no file behind' && ok "fails: a rules reference with no file behind it" || bad "no file behind the reference (rc $rc): $out"
mkcase release
sed 's/device-transfer/device-move/' "$c/rules.txt" > "$c/rules.new" && mv "$c/rules.new" "$c/rules.txt"
check
[ "$rc" -eq 1 ] && echo "$out" | grep -q 'no <device-transfer> section' && ok "fails: a rules file without the device-transfer section" || bad "no device-transfer section (rc $rc): $out"

echo "exported components"
F_COMPONENTS='          E: service (line=90)
            A: http://schemas.android.com/apk/res/android:name(0x01010003)="ch.holzcloud.holzwave.Sync" (Raw: "x")
            A: http://schemas.android.com/apk/res/android:exported(0x01010010)=true'
expect_fail "an exported service" release "exported component: found the service ch.holzcloud.holzwave.Sync exported"
F_COMPONENTS='          E: receiver (line=90)
            A: http://schemas.android.com/apk/res/android:name(0x01010003)="androidx.profileinstaller.ProfileInstallReceiver" (Raw: "x")
            A: http://schemas.android.com/apk/res/android:permission(0x01010006)="android.permission.DUMP" (Raw: "x")
            A: http://schemas.android.com/apk/res/android:exported(0x01010010)=true'
expect_fail "an exported receiver of a library, even one guarded by a permission" release "the receiver androidx.profileinstaller.ProfileInstallReceiver exported"
F_COMPONENTS='          E: service (line=90)
            A: http://schemas.android.com/apk/res/android:name(0x01010003)="ch.holzcloud.holzwave.Quiet" (Raw: "x")'
expect_fail "a component that does not say whether it is exported" release "component service ch.holzcloud.holzwave.Quiet: found android:exported 'unset'"
F_COMPONENTS='          E: activity (line=90)
            A: http://schemas.android.com/apk/res/android:name(0x01010003)="ch.holzcloud.holzwave.Other" (Raw: "x")
            A: http://schemas.android.com/apk/res/android:exported(0x01010010)=true
              E: intent-filter (line=91)
                  E: action (line=92)
                    A: http://schemas.android.com/apk/res/android:name(0x01010003)="android.intent.action.MAIN" (Raw: "x")
                  E: category (line=93)
                    A: http://schemas.android.com/apk/res/android:name(0x01010003)="android.intent.category.LAUNCHER" (Raw: "x")'
expect_fail "a second launcher activity" release "exported components: found 2"
F_MAIN_EXPORTED=false; expect_fail "the launcher activity not exported (nothing exported)" release "exported components: found 0"

echo "debuggable and cleartext"
F_DEBUGGABLE="A: http://schemas.android.com/apk/res/android:debuggable(0x0101000f)=true"; expect_fail "a debuggable release APK" release "debuggable: found 'true' in the release APK"
F_CLEARTEXT="A: http://schemas.android.com/apk/res/android:usesCleartextTraffic(0x010104ec)=true"; expect_fail "cleartext traffic in a release APK" release "usesCleartextTraffic: found 'true' in the release APK"
F_CLEARTEXT="A: http://schemas.android.com/apk/res/android:networkSecurityConfig(0x01010527)=@0x7f120001"; expect_fail "a network security config in a release APK" release "networkSecurityConfig: found '@0x7f120001' in the release APK"
F_DEBUGGABLE=""; expect_fail "a debug APK that is not debuggable" debug "debuggable: found 'not set' in the debug APK"
F_CLEARTEXT="A: http://schemas.android.com/apk/res/android:usesCleartextTraffic(0x010104ec)=true"; expect_fail "a blanket cleartext allow in the debug APK" debug "usesCleartextTraffic: found true in the debug APK"

echo "16 KB pages"
C_ZIPALIGN=1; expect_fail "zipalign -P 16 failing" release "zipalign: found '.*BAD"
C_ALIGN=0x1000; expect_fail "a library with 4 KB alignment" release "alignment of libholzwave_app_lib.so: found a LOAD segment aligned to 0x1000"
C_ENTRIES='AndroidManifest.xml
classes.dex'; expect_fail "no native library at all" release "native libraries: found none"
mkcase release
check
grep -q '^zipalign -c -P 16 -v 4 ' "$LOG" && ok "zipalign is called as zipalign -c -P 16 -v 4" || bad "zipalign call: $(grep '^zipalign' "$LOG" | tail -n 1)"
grep -q '^llvm-readelf -lW ' "$LOG" && ok "llvm-readelf is called with -lW on the library" || bad "llvm-readelf call: $(grep '^llvm-readelf' "$LOG" | tail -n 1)"

echo "services"
C_DEX='xxLcom/google/firebase/analytics/FirebaseAnalytics;xx'; expect_fail "a Firebase class in the dex" release "services: found 1"
C_DEX='Lcom/google/android/gms/common/api/Api;'; expect_fail "a Play services class in the dex" release "services: found 1"
C_DEX='Lcom/google/android/play/core/review/ReviewManager;'; expect_fail "a Play core class in the dex" release "services: found 1"

echo "https: the platform certificate verifier"
C_NO_VERIFIER=1; expect_fail "no verifier component in the dex of a release APK" release "https: found no class org.rustls.platformverifier.CertificateVerifier"
C_NO_VERIFIER=1; expect_fail "no verifier component in the dex of a debug APK" debug "https: found no class org.rustls.platformverifier.CertificateVerifier"
C_NO_INIT=1; expect_fail "no set-up function in the native library of a release APK" release "https: found no Java_ch_holzcloud_holzwave_HolzwavePlugin_initTls"
C_NO_INIT=1; expect_fail "no set-up function in the native library of a debug APK" debug "https: found no Java_ch_holzcloud_holzwave_HolzwavePlugin_initTls"
mkcase release
check
echo "$out" | grep -q '^ok   the dex files hold the Kotlin component of the platform certificate verifier' && echo "$out" | grep -q '^ok   the native library exports the function that sets the verifier up' \
    && ok "a good release APK is reported as holding the verifier component and its set-up function" || bad "no verifier lines: $out"

echo "the debug-only self-test"
C_DEX='xxholzwave_selftestxx'; expect_fail "the self-test trigger in the dex of a release APK" release "self-test: found 1"
C_SO='holzwave-selftest vault-plain-write: ok'; expect_fail "the self-test report tag in the native library of a release APK" release "self-test: found 1"
C_DEX='holzwave_selftest'; C_SO='holzwave-selftest'; mkcase debug
check debug
[ "$rc" -eq 0 ] && echo "$out" | grep -q 'the debug APK holds the self-test, as intended' && ok "a debug APK may hold the self-test and is told so" || bad "debug with the self-test (rc $rc): $out"
reset_c
mkcase release
check
echo "$out" | grep -q '^ok   the release APK holds no self-test' && ok "a clean release APK is reported as holding no self-test" || bad "no self-test line: $out"

echo "signature"
C_SIGNED=yes; expect_fail "a signed release APK" release "signature: found a release APK that verifies or holds signature files"
C_DN="CN=Some Owner, O=X"; expect_fail "a debug APK signed with another certificate" debug "signature: found 'CN=Some Owner, O=X', wanted the debug certificate"
mkcase release
check
echo "$out" | grep -q '^ok   the release APK is UNSIGNED' && ok "the release APK is reported as unsigned" || bad "no unsigned line: $out"

echo "missing tools and files are exit 2, never a skip"
for tool in aapt2 zipalign apksigner readelf unzip; do
    C_TOOL=$tool
    mkcase release
    [ "$tool" = unzip ] && C_PATH="$c/tools"
    check
    unset C_PATH
    if [ "$rc" -eq 2 ] && echo "$out" | grep -q 'a tool is missing' && ! echo "$out" | grep -q 'android-apk-check: ok' && clean_after; then
        ok "a missing $tool is exit 2"
    else
        bad "missing $tool (rc $rc): $out"
    fi
    reset_c
done
mkcase release
rm -f "$c/app.apk"
check
[ "$rc" -eq 2 ] && ok "no APK file is exit 2" || bad "no APK (rc $rc): $out"
mkcase release
check sideways
[ "$rc" -eq 2 ] && ok "an unknown mode is exit 2" || bad "unknown mode (rc $rc): $out"

echo "nothing was installed, signed or reached"
if grep -Eq '^(adb|emulator|avdmanager|sdkmanager|keytool|jarsigner|brew|curl|wget|npm|cargo) ' "$LOG"; then
    bad "a tool outside the shims was called: $(grep -E '^(adb|emulator|avdmanager|sdkmanager|keytool|jarsigner|brew|curl|wget|npm|cargo) ' "$LOG" | head -n 3)"
else
    ok "over every run above no adb, emulator, installer, key tool, download tool, npm or cargo was called ($(wc -l < "$LOG" | tr -d ' ') calls logged)"
fi
if grep '^apksigner ' "$LOG" | grep -vq '^apksigner verify '; then
    bad "apksigner was called with something other than verify"
else
    ok "apksigner was only ever asked to verify"
fi
if grep -v '^[[:space:]]*#' "$here/android-apk-check.sh" | grep -Eq '(^|[[:space:];&|(])(adb|emulator|sdkmanager|avdmanager|keytool|jarsigner|brew|curl|wget)([[:space:]]|$)' ; then
    bad "android-apk-check.sh names an install, device or download tool in its code"
else
    ok "android-apk-check.sh names no install, device, key or download tool in its code"
fi
if grep -v '^[[:space:]]*#' "$here/android-apk-check.sh" | grep -Eq 'apksigner" +(sign|rotate)|--ks|keystore'; then
    bad "android-apk-check.sh signs or reads a keystore"
else
    ok "android-apk-check.sh never signs and never reads a keystore"
fi

if [ "$failures" -ne 0 ]; then
    echo "android-apk-check-test: $failures of $checks checks FAILED" >&2
    exit 1
fi
echo "android-apk-check-test: $checks checks"
echo "android-apk-check-test: ok"
