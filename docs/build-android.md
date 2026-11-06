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

This runs `npm --prefix app run tauri -- android build --ci --target aarch64 --apk --debug` after sourcing the environment,
and prints the path of the APK as its last line
(`app/src-tauri/gen/android/app/build/outputs/apk/universal/debug/app-universal-debug.apk`; the flavour is called `universal` but holds the one ABI, arm64-v8a). The debug build has the application id
`ch.holzcloud.holzwave.debug`, is debuggable and may use cleartext HTTP (so that an emulator can reach the host machine); the
release build is neither.

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
`INTERNET` permission only, and that only the debug build is debuggable and may use cleartext.

## What was not tried

- Any real phone or tablet: the soft keyboard and hardware keyboards, biometrics and the hardware keystore, background kills, the
  limit on background processes, real storage providers, mobile networks, touch feel, the WebView version of a phone, and the system
  shell of a phone maker.
- Any Android version other than the emulator image the check ran on (the default is Android 37, arm64).
- x86_64 and 32-bit images or builds, which are refused by design.
- A signed release APK, installing from F-Droid or Obtainium, and a 16 KB page device other than the emulator image with that page size.
