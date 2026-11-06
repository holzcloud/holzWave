# Building holzWave for Android

For the owner's own Mac (Apple silicon). Everything here is built and run locally: no step uses GitHub to build, and nothing
here costs money. The toolchain, the SDK, the NDK, the emulator and the system images are free downloads from their vendors.

> [!IMPORTANT]
> **Status.** The Android app is being built in stages. When this page was written **nothing of the Android app had been built
> or run**: this page covers only the toolchain (what to install and how to check it), and the stand-in test of the check runs on
> a fake SDK tree. The sections for building the project, running it on an emulator and checking the APK are added when the
> scripts that do those things exist.

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

6. **Zig stays 0.17.0 exactly**, as in [building.md](building.md): the terminal engine is built by Ghostty's own build script for
   Android too. Homebrew's `zig@0.17` formula has since moved on to 0.17.1, so the check fails on a machine that only has that
   one. Unpack 0.17.0 to `$HOME/.holzwave/zig-0.17.0` (the steps and the checksum source are in
   [build-linux.md](build-linux.md#zig-0170-exactly); on a Mac the archive is `zig-aarch64-macos-0.17.0.tar.xz`), which
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
present, the Rust target `aarch64-linux-android` and Zig exactly 0.17.0. The last line is `android-env: ok` only when nothing
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

## Not covered yet

Building the project, running it on an emulator and checking the APK are not described here, because the scripts for them do
not exist yet; they are added to this page together with those scripts. Everything that only a real phone shows (the soft and
hardware keyboard, biometrics, background kills, mobile networks, touch feel, real WebView versions) is not run: there is no
test device, only arm64 emulator images.
