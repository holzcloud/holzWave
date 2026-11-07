# Building holzWave for Android

For the owner's own Mac (Apple silicon). Everything here is built and run locally: no step uses GitHub to build, and nothing
here costs money. The toolchain, the SDK, the NDK, the emulator and the system images are free downloads from their vendors.

> [!IMPORTANT]
> **Status.** The Android app is being built in stages, and this is an early one. What exists: the toolchain and its check, a
> compile gate for the Rust code, a script that builds a debug APK, scripts that create and start an arm64 emulator, and one
> end to end check on that emulator (see [the end to end check](#the-end-to-end-check)). It was run on **one** arm64 emulator image
> of **one** Android version, and on no phone. Everything that only a real phone shows is **not run**: see
> [what was not tried](#what-was-not-tried).

## What to install, all free

Install these yourself. Nothing in this repository downloads or installs any of it, and accepting the SDK licences is your own
act: an agent or a script must not accept them for you.

1. **A JDK.** Version 21 is what the check wants, because it is the current long-term-support release. Any of 17 to 26 runs the
   Gradle that the Tauri Android template pins, but the check names 21 so that every machine builds with the same one. Either
   Homebrew's formula (`brew install openjdk@21`, which is what the author's machine uses; the Temurin cask `temurin@21` asks
   for `sudo` to install, and its name should be confirmed with `brew search temurin`) or the JDK that ships with Android
   Studio. The environment script finds Homebrew's `openjdk@21`, then `/usr/libexec/java_home -v 21`; set
   `HOLZWAVE_JAVA_HOME` to use another one.
2. **The Android command-line tools**, from <https://developer.android.com/studio#command-line-tools-only> (or
   `brew install --cask android-commandlinetools`, which unpacks them to `/opt/homebrew/share/android-commandlinetools`).
   They must end up in `$ANDROID_HOME/cmdline-tools/latest`, so that `sdkmanager` is at
   `$ANDROID_HOME/cmdline-tools/latest/bin/sdkmanager`.
3. **Accept the licences yourself:**

   ```sh
   sdkmanager --licenses
   ```

4. **The SDK packages**, in one line (several GiB are downloaded; the system images are the large part):

   ```sh
   sdkmanager "platform-tools" "platforms;android-37.0" "build-tools;37.0.0" "ndk;30.0.16248370" "emulator" \
     "system-images;android-31;default;arm64-v8a" \
     "system-images;android-37.0;default;arm64-v8a" \
     "system-images;android-37.0;google_apis_ps16k;arm64-v8a"
   ```

   The package ids were read from the SDK manager's own list on 2026-11-05 and again on 2026-11-06. `sdkmanager --list` is the
   authority and the ids move, so read it if one is refused. The two Android 37 images are the current platform: one with 4 KiB
   pages and one with 16 KiB pages (`ps16k`), which Android 15 and later may use. The Android 31 image is the lowest version the
   app supports (Android 12).
5. **The Rust target:**

   ```sh
   rustup target add aarch64-linux-android
   ```

6. **Zig stays 0.17.1 exactly**, as in [building.md](building.md): the terminal engine is built by Ghostty's own build script for
   Android too. Homebrew's `zig@0.17` formula has since moved on to 0.17.1, so the check fails on a machine that only has that
   one. Unpack 0.17.1 to `$HOME/.holzwave/zig-0.17.1` (the steps and the checksum source are in
   [build-linux.md](build-linux.md#zig-0170-exactly); on a Mac the archive is `zig-aarch64-macos-0.17.1.tar.xz`), which
   `scripts/env.sh` puts first on `PATH`.

Only the vendors' own sites are the place to get these (developer.android.com, the SDK manager's own repository, Homebrew's
formula and cask index): `sdkmanager` checks each package it downloads against the repository manifest.

## arm64 only, and why

The app is built for **arm64-v8a only**, which is the Rust target `aarch64-linux-android`. Two reasons:

- The terminal engine cannot be built for 32-bit ARM or for 32-bit x86: its build script stops there.
- An x86_64 build cannot be run on an Apple silicon Mac, because the emulator refuses an x86_64 system image on an arm64 host.
  It would be built and never run, and the project does not ship what it cannot run.

So every build command passes the target explicitly, and the check script requires `aarch64-linux-android` and says it is
unused if you have installed another Android Rust target. System images of other ABIs are not used either.

## Check it

From the repository root:

```sh
. scripts/android-env.sh          # JAVA_HOME, ANDROID_HOME, ANDROID_NDK_HOME, NDK_HOME and PATH; sourcing prints nothing
sh scripts/android-env-check.sh   # one line per tool; exit status 1 when any line is FAIL
```

`scripts/android-env.sh` is meant to be sourced (with the dot). It never exits your shell, installs nothing and writes nothing.
It is the one place where the versions are named. The SDK is looked for in `$HOLZWAVE_ANDROID_HOME`, then in
`$HOME/Library/Android/sdk` (the Android Studio place), then in Homebrew's `share/android-commandlinetools`.

The check prints the paths it resolved, then one line per item:

| Line | Meaning |
|---|---|
| `ok <what> <found>` | the item is there and as wanted |
| `FAIL <what> found <found> wanted <wanted>` | the item is missing or wrong; the next line names the command that fixes it |
| `info <what> <found>` | worth knowing, not a failure (the `adb` version, the arm64 system images found, an unused Android Rust target, a system image of another ABI) |

What it requires: the JDK major version (21), the NDK revision exactly (`30.0.16248370`), the directories
`platforms/android-37.0` and `build-tools/37.0.0`, platform-tools and the emulator at least at the pinned minimum, `sdkmanager`
present, the Rust target `aarch64-linux-android` and Zig exactly 0.17.1. The last line is `android-env: ok` only when nothing
failed. **The check only reads** (`java -version`, `rustup target list --installed`, `zig version`, `adb version` and the
`source.properties` files of the SDK packages): it never runs `sdkmanager --install`, `rustup target add`, `brew` or a download.

**The NDK is pinned by its path**, not found. The engine's Zig build takes the newest NDK it finds when only `ANDROID_HOME` is
set, so a second NDK on the machine would silently change the build. `ANDROID_NDK_HOME` and `NDK_HOME` are set to the one
pinned directory and the check prints it.

The versions are a snapshot of 2026-11-05 and 2026-11-06 (NDK r30, platform 37.0, build-tools 37.0.0, platform-tools 37.0.1,
emulator 37.2.13). A newer stable release may exist by the time you read this; a version moves only by a change of
`scripts/android-env.sh`.

To prove the check script without any Android software, `sh scripts/test-android-env.sh` runs it against a fake SDK tree with
stand-in programs, and asserts that no install, add, update or licence command is ever called.

## Compile the Rust code for Android

```sh
sh scripts/android-check.sh
```

This runs `cargo check --locked --offline --target aarch64-linux-android` for the engine, the host, the local socket and the app
shell, with the pinned NDK. It compiles; it does not link an app or run anything. The last line is `android-check: ok`. Exit
status 1 means a check failed and 2 means a tool or the Rust target is missing (the line it prints for the target is text for
you to run, it does not run it). `scripts/android-env.sh` exports `CC_aarch64_linux_android`, `CXX_aarch64_linux_android`,
`AR_aarch64_linux_android`, `RANLIB_aarch64_linux_android` and the cargo linker and archiver of that target, pointing into the
pinned NDK at API level 31 (the minimum SDK), because the C build scripts of the engine's dependencies need a compiler for the
target. Without them a direct `cargo check --target aarch64-linux-android` stops with `ToolNotFound: aarch64-linux-android-clang`.

Every build passes `--target aarch64`, and the scripts never name another target.

## Build the debug APK

```sh
sh scripts/android-build.sh debug      # or: release
```

This runs `npm --prefix app run tauri -- android build --ci --target aarch64 --apk --debug -- --locked` after sourcing the
environment and setting the reproducibility variables (below), and prints the path of the APK as its last line
(`app/src-tauri/gen/android/app/build/outputs/apk/universal/debug/app-universal-debug.apk`; the flavour is called `universal` but holds the one ABI, arm64-v8a). The debug build has the application id
`ch.holzcloud.holzwave.debug`, is debuggable and may use cleartext HTTP to the emulator's alias of the host machine, `10.0.2.2`, and to
nothing else; the release build is neither.

`release` builds the APK **without a signature** and says so. Signing is your own step with your own key: the script never signs,
never creates a key and never calls `keytool` or `apksigner`, and no key belongs in the repository.

The first build downloads the Android Gradle plugin, Kotlin and the AndroidX libraries that the generated project names from
Google's and Maven Central's repositories. That is Gradle's own job. The script itself installs and downloads nothing, and the
build runs the page's own `npm run build` first, which clears and rewrites `app/dist`.

## The emulator

```sh
sh scripts/android-emulator.sh create   # once: the virtual device holzwave-arm64
sh scripts/android-emulator.sh start    # headless, waits for the boot
sh scripts/android-emulator.sh install  # adb install -r of the debug APK
sh scripts/android-emulator.sh shot out.png
sh scripts/android-emulator.sh stop
```

`create` uses the system image `system-images;android-37.0;default;arm64-v8a` from the install line above (set
`HOLZWAVE_ANDROID_IMAGE` for another arm64 image, for example the Android 31 one). It **refuses** an image that is not arm64-v8a and
a host that is not arm64, and it never installs a system image: a missing one is exit 2 with the `sdkmanager` line printed as
text. `start` boots without a window (`-no-window -no-audio -no-snapshot -gpu swiftshader_indirect`; set
`HOLZWAVE_EMULATOR_GPU=host` for `-gpu host` if the first does not draw). The scripts talk to one device only, the emulator on
console port 5554 (`emulator-5554`; `HOLZWAVE_EMULATOR_PORT` changes it), never to a phone on the cable.

**Why arm64 only.** The emulator on an Apple silicon Mac cannot run an x86_64 system image, because that would need x86 hardware
support that the Mac does not have, and an x86_64 build would be built and never run. The terminal engine does not build for 32-bit
ARM or 32-bit x86 at all.

## The end to end check

```sh
sh scripts/android-e2e.sh tracer
```

It starts the emulator if none runs (and stops it at the end only if it started it), builds and installs the debug APK, starts
the app, waits until the app has started a shell, taps the screen, types three lines with the real Android key path (`adb shell
input text` and the Enter key), and reads the results back with `run-as`:

| Typed | The file must hold |
|---|---|
| `pwd>e2e-pwd` | the folder `home` inside the app's private storage |
| `echo hello>e2e-echo` | `hello` |
| `readlink /proc/$$/exe>e2e-sh` | a path under `/system/bin/` |

It prints one line per check and `e2e tracer: ok` at the end (exit 1 when a check failed, 2 when a tool is missing); it removes the
three files and the temporary directory on every way out. `HOLZWAVE_E2E_SKIP_BUILD=1` reuses the APK that is already built.

**What it proves:** on one emulator image the page loads in the Android WebView, the page asks the core for its layout through the
bridge, a pane starts the system shell in the app's own folder, and key events from the Android input path reach it.

**What it does not prove:** anything on a real phone. The soft keyboard is never opened (`input text` sends key events), the WebView
is the one of the emulator image (the script prints it), and the shell is the one of that image, not the shell of a phone maker.

The shell's own folder is `home` inside the app's data folder: a folder of its own, kept apart from the app's settings and layout
files, with mode 0700. A program a person starts in a pane does not find those files next to its own, and a later hand-over of files
can be limited to it. The shell's `TMPDIR` is the app's cache folder.

## The committed project

`app/src-tauri/gen/android` is the Android project that `tauri android init` writes, committed so that a plain Gradle build and Android
Studio build the same thing. What it pins (read from the files, not from memory):

| What | Value |
|---|---|
| Application id | `ch.holzcloud.holzwave` (the debug build adds `.debug`) |
| Gradle | 9.6.1 (wrapper) |
| Android Gradle plugin | 9.3.1 |
| Kotlin Gradle plugin | 2.2.10 |
| Compile SDK and target SDK | 37 |
| Minimum SDK | 31 (Android 12), also set in `tauri.android.conf.json` |
| ABI | arm64-v8a only, fixed in `app/build.gradle.kts` (`defaultConfig.ndk.abiFilters`) and in the Rust plugin under `buildSrc` (one ABI, one flavour, one target) |

Run `sh scripts/android-build.sh debug` once before opening the project in Android Studio or running Gradle by hand: the Tauri command line writes `tauri.settings.gradle` (ignored by git, because it holds this machine's paths) and the native library that Gradle packages.

The generated template lists four ABIs; the committed project lists one, so no command line, Android Studio run or plain Gradle
build can add another. `local.properties`, the build folders, the generated Tauri settings and every key file are ignored by the
project's own `.gitignore`, and `cargo test -p holzwave-app --test android_project` reads the committed files to check the id, the
minimum SDK, the one ABI, that no machine path, key, Google service or installer permission is in them, that the manifest asks for the
permissions of the list below only, and that only the debug build is debuggable and may use cleartext.

## The version code

Android refuses an update whose `versionCode` is not higher than the installed one, and stores and update checkers order builds by it.
The code the Tauri command line derives when none is written down is `major * 1000000 + minor * 1000 + patch`, which ignores the
pre-release part: `0.0.1-beta2` and `0.0.1` would both be 1. So the code is written down in `app/src-tauri/tauri.android.conf.json`
(`bundle.android.versionCode`), computed by `scripts/android-version-code.sh`, and a test fails when it no longer follows the version
in `tauri.conf.json`.

```text
versionCode = major * 10000000 + minor * 100000 + patch * 1000 + stage * 100 + number
```

| Part | Range | Meaning |
|---|---|---|
| major | 0 to 209 | the code must stay below Android's limit of 2100000000, so 209.99.99 is the last version |
| minor, patch | 0 to 99 | |
| stage | 1, 2, 3, 9 | alpha, beta, release candidate (`rc`), final |
| number | 1 to 99, or 0 for a final | the number after the stage word: `alpha1`, `beta2`, `rc1` |

| Version | Code |
|---|---|
| `0.0.1-alpha1` | 1101 |
| `0.0.1-beta1` | 1201 |
| `0.0.1-beta2` | 1202 |
| `0.0.1-rc1` | 1301 |
| `0.0.1` | 1900 |
| `0.0.2-alpha1` | 2101 |
| `0.1.0` | 100900 |
| `1.0.0` | 10000900 |

Inside one patch alpha is below beta, beta below release candidate and release candidate below final, and every build of one patch is
below every build of the next. A number out of range, a leading zero, an unknown stage word and build metadata are refused with exit
status 2 (nothing is clamped). When the version of the app changes, run `sh scripts/android-version-code.sh <version>` and write the
number into `tauri.android.conf.json` in the same commit; `sh scripts/test-android-version-code.sh` and the `android_release` test
check the scheme.

## What the manifest decides

- **No backup.** `allowBackup` is false and `data_extraction_rules.xml` excludes every domain, for the cloud backup and for the
  transfer to another phone. The app keeps its vault file and its history in its private folder, and those must not be copied to a
  backup service or to another device. There is no `fullBackupContent` file.
- **Cleartext HTTP only in the debug build, and only to `10.0.2.2`.** The debug source set (`app/src/debug`) adds a network security
  config that permits cleartext to the emulator's alias of the host machine and refuses it everywhere else. The release build has
  neither the attribute nor the file.
- **One exported component:** the launcher activity. Every other activity, service, receiver and provider says `exported="false"`.
  A receiver that a library adds (the profile installer's, guarded by the shell-only DUMP permission) is removed from the merged
  manifest.
- **No TV launcher, no file provider.** The template's Android TV lines and its file provider (which shared the whole external
  storage and the cache, and which nothing used) are removed.
- **No Google services, no tracker, no updater.** No build file names a Google service, Firebase, Crashlytics, an analytics library or
  the Play core library, and the APK check looks for their classes in the dex files. The release is not debuggable, keeps the R8
  setting of the template, and leaves the commit out of the APK (`vcsInfo`) and the dependency block that only Google Play can read
  (`dependenciesInfo`).

## One list of permissions

`app/src-tauri/android-permissions.txt` is the one list of the permissions the app may ask for (today `android.permission.INTERNET`).
The `android_release` test requires the manifest to hold exactly these, and the APK check requires the same of a built APK, so a
permission that is added anywhere else fails one of them. A feature that needs a permission adds its line to the file in its own
commit, with the manifest line, and says in the message what it is for. A permission that installs packages, reads the screen, draws
over other apps or reads contacts, messages or location is never on the list without a decision of its own.

## Check a built APK

```sh
sh scripts/android-apk-check.sh <apk> [debug|release]
```

It reads the real file with the pinned `aapt2`, `zipalign`, `apksigner` and the NDK's `llvm-readelf` and prints one line per check,
`FAIL <what>: found <x>, wanted <y>` for each problem, and `android-apk-check: ok` when none failed (exit 1 when a check failed, 2
when a tool or the file is missing). It checks the package name (with the `.debug` suffix for debug only), the version code against
the scheme, `minSdkVersion` 31, the native code (arm64-v8a and nothing else, and no file under `lib/` of another ABI), the permissions
against the list, `allowBackup`, the extraction rules inside the APK, the exported components of the **merged** manifest (including
those of libraries), `debuggable` and cleartext in debug only, `zipalign -c -P 16 -v 4`, the alignment of every `LOAD` segment of
every native library (at least `0x4000`, the 16 KB page size that Android 15 and later devices may use), the absence of Google Play
services and Firebase classes, and the signature (a release APK must be unsigned; a debug APK carries the debug certificate). It
installs nothing and writes only to a temporary folder that it removes on every way out.
`sh scripts/test-android-apk-check.sh` proves every rule once passing and once failing with stand-in tools, without any Android
software.

The target SDK is printed and not judged: what minimum a store such as F-Droid asks for today was not verified.

## The release is unsigned

`sh scripts/android-build.sh release` builds `app-universal-release-unsigned.apk`. No script of the repository signs, creates a key
or reads a keystore password, and the Gradle project has no signing configuration. Signing is the owner's own step, with the owner's
own key kept outside the repository, when a release is published. An unsigned APK cannot be installed. To see that the minified release (R8 is on) still starts, a copy of the release APK was signed
once, by hand and outside the repository, with the Android SDK's public debug key, installed on the arm64 emulator (Android 37), and
started: the native library loaded, the page appeared and a pane started the system shell. That copy was removed afterwards and is not
a build output of any script.

## Reproducibility

Every build sets `SOURCE_DATE_EPOCH` (the date of the last commit, or the value already in the environment), replaces `RUSTFLAGS` by
`--remap-path-prefix` flags for the repository folder, the Cargo home and the NDK folder, and passes `--locked` to Cargo; it prints
the NDK path and the Rust version it used. The Gradle project leaves the commit and Google's dependency block out of the APK.

**Measured:** two builds of the unsigned release, one after the other, in the same folder, on the same machine, with the same
`SOURCE_DATE_EPOCH`, with `app/src-tauri/gen/android/app/build`, `app/dist` and the whole `target/aarch64-linux-android/release`
folder removed in between (so the Rust code, the terminal engine and the page were built again, not reused). The two APK files were
byte for byte the same (the same SHA-256), and so were the native library, the dex file, `resources.arsc` and the manifest. The
release native library holds no machine path: a search for the home folder, the repository folder and the NDK folder found none
(the debug library, which keeps its debug information, still names paths of the C code of some dependencies and of the Zig cache;
the compiler flags above only cover Rust code).

**Not proved:** a rebuild in another folder or under another user name, a rebuild on another machine or operating system, a rebuild
with another Rust, Zig, NDK or Gradle version, and a rebuild by F-Droid's build server. The release library carries remapped
registry paths of the form `/cargo-home/registry/...`; whether those stay the same in another place was not tried.

## What was not tried

- Any real phone or tablet: the soft keyboard and hardware keyboards, biometrics and the hardware keystore, background kills, the
  limit on background processes, real storage providers, mobile networks, touch feel, the WebView version of a phone, and the system
  shell of a phone maker.
- Any Android version other than the emulator image the check ran on (the default is Android 37, arm64).
- x86_64 and 32-bit images or builds, which are refused by design.
- A signed release APK for distribution, installing from F-Droid or Obtainium, Play Protect or installer warnings for a self-signed APK, and a 16 KB page device other than the emulator image with that page size.
