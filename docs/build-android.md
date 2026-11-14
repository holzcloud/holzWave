# Building holzWave for Android

For the owner's own Mac (Apple silicon). Everything here is built and run locally: no step uses GitHub to build, and nothing
here costs money. The toolchain, the SDK, the NDK, the emulator and the system images are free downloads from their vendors.

> [!IMPORTANT]
> **Status.** The Android app is built, and it was run on **arm64 emulators only**: Android 17 (API 37), Android 12 (API 31) and an
> image with 16 KB memory pages, never on a phone and never on a tablet. What exists: the toolchain and its check, a compile gate for
> the Rust code, a script that builds a debug APK and an unsigned release APK, a check of a built APK, scripts that create and start
> arm64 emulators, and a set of end to end checks run with one command (see [the end to end check](#the-end-to-end-check) and
> [the whole set](#the-whole-set-on-three-images)). **Only one full run of that set was all green** (the last one, and
> earlier full runs failed in places), and several checks ended `not run`: [What ran and what did not](#what-ran-and-what-did-not) says which. Everything that only a real phone or tablet shows is
> **not run**: see [what was not tried](#what-was-not-tried) and Part 13 of [test-plan.md](test-plan.md). Two defects found on an
> emulator are [fixed, and were checked on emulators only](#what-the-three-images-showed).

Contents: [What to install](#what-to-install-all-free) · [arm64 only](#arm64-only-and-why) · [Check it](#check-it) ·
[Compile the Rust code](#compile-the-rust-code-for-android) · [Build the APK](#build-the-debug-apk) · [The emulator](#the-emulator) ·
[The end to end check](#the-end-to-end-check) · [The whole set](#the-whole-set-on-three-images) ·
[The committed project](#the-committed-project) · [The version code](#the-version-code) · [The manifest](#what-the-manifest-decides) ·
[Permissions](#one-list-of-permissions) · [The service](#the-service-that-keeps-sessions-alive) · [The secret store](#the-secret-store) ·
[HTTPS](#https-and-certificates) · [Hand-over](#handing-one-file-over) · [The clipboard](#the-clipboard) ·
[Check an APK](#check-a-built-apk) · [Unsigned release](#the-release-is-unsigned) · [Reproducibility](#reproducibility) ·
[What was not tried](#what-was-not-tried) · [What ran and what did not](#what-ran-and-what-did-not) ·
[Facts for whoever offers the app](#facts-for-whoever-offers-the-app)

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
sh scripts/android-emulator.sh create [profile]   # once per profile: a virtual device
sh scripts/android-emulator.sh start [profile]    # headless, waits for the boot
sh scripts/android-emulator.sh install [apk] [profile]  # adb install -r of the debug APK
sh scripts/android-emulator.sh shot out.png
sh scripts/android-emulator.sh stop [profile]
```

There are three profiles, each an arm64-v8a image and a virtual device of its own:

| Profile | Virtual device | System image | What it is for |
|---|---|---|---|
| `api37` (the default) | `holzwave-arm64` | `system-images;android-37.0;default;arm64-v8a` | the newest Android, 4 KB pages |
| `api31` | `holzwave-arm64-api31` | `system-images;android-31;default;arm64-v8a` | Android 12, the oldest version the app installs on (minimum API 31) |
| `16k` | `holzwave-arm64-16k` | `system-images;android-37.0;google_apis_ps16k;arm64-v8a` | the 16 KB memory page size that new devices ship with |

`create` **refuses** an image that is not arm64-v8a (also when `HOLZWAVE_ANDROID_IMAGE` names one) and a host that is not arm64, and
it never installs a system image: a missing one is exit 2 with the `sdkmanager` line for it printed as text (installing is the
owner's step). It sets `hw.keyboard=no` in the device's configuration, so the soft keyboard can show (with a hardware keyboard the
system keeps it hidden, and the touch checks need it). `start` boots without a window (`-no-window -no-audio -no-snapshot -gpu swiftshader_indirect`; set
`HOLZWAVE_EMULATOR_GPU=host` for `-gpu host` if the first does not draw). The scripts talk to one device only, the emulator on
console port 5554 (`emulator-5554`; `HOLZWAVE_EMULATOR_PORT` changes it), never to a phone on the cable.

**Why arm64 only.** The emulator on an Apple silicon Mac cannot run an x86_64 system image, because that would need x86 hardware
support that the Mac does not have, and an x86_64 build would be built and never run. The terminal engine does not build for 32-bit
ARM or 32-bit x86 at all.

## The end to end check

```sh
sh scripts/android-e2e.sh tracer
```

(The section `service` is described with the foreground service below, the section `vault` with the secret store, the section `tls` with HTTPS and certificates, and the sections `handover` and `clipboard` with the single-file hand-over and the clipboard.) It starts the emulator if none runs (and stops it at the end only if it started it), builds and installs the debug APK, starts
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

## The whole set on three images

```sh
sh scripts/android-e2e.sh all [profile]   # every section below on one image
sh scripts/android-e2e.sh matrix          # the three images, one emulator at a time
```

`all` runs the sections `tracer`, `service`, `vault`, `tls`, `handover`, `clipboard`, `touch`, `ssh` and `pagesize` in that order on the
emulator that is up (or on one it starts for the profile and stops again). It builds the debug APK once. `matrix` runs `all` on the
newest image (`api37`), then `tracer service vault pagesize` on Android 12 (`api31`), then `tracer service pagesize` on the 16 KB
page image (`16k`), and stops each emulator, and waits until it has left the list of devices, before the next starts. Each section is
a process of its own; the run ends with **one table per image**: the sections, how many checks were `ok`, `not run` and `FAIL`, every
check that was **not run** with its reason, and every failure with its reason. A check that could not be made is `not run`, counted on
its own and named: it is never `ok`, and a section that ends with a failure status and no result line is a `FAIL`. Exit 0 means no
failure (the last line also says how many checks were not run), 1 a failure, 2 a missing tool or a refused request.

**Only emulators.** Every `adb` call the scripts make names the emulator (`adb -s emulator-5554`; `scripts/android-adb.sh` is the one
door), and a script refuses (exit 2, before it sends anything) unless that serial is attached as a device **and** reports the
emulator property. A phone on the cable is never addressed, and a typed port that is not a number is refused.

**The `ssh` section.** `scripts/android-ssh-server.sh` starts a throwaway OpenSSH server as the current user (no root) in a private
folder: bound to `127.0.0.1` only, key login only, no forwarding, one throwaway Ed25519 key sealed with a random passphrase, which is
given to `ssh-keygen` through a private askpass program, never as an argument. The emulator reaches it through `adb reverse` (a
loopback address needs no permission; see the local-network note under HTTPS, and the end of this paragraph for the private address). In a debug build a self-test then saves the connection
with the key file and its passphrase **in the vault**, trusts exactly the server's host key, opens a pane and waits for `connected`,
and the script has a command typed into that pane leave a file on the server side. With a PIN set, the vault is then **locked**: the
live pane keeps working (a second command still lands), a new connect in the locked state is refused with the answer for a locked
secret store and the server logs **no** new login for it, and after the unlock a second pane opens on the same saved connection and
shares the open login. The passphrase is a random canary: it is checked to appear in no log. The server, its keys and the proof files
are removed at every way out.

**The `touch` section.** It drives the **real WebView** on the emulator, not a browser. `uiautomator` does not see a page's content
unless the WebView exposes it, so the script writes `--force-renderer-accessibility` into `/data/local/tmp/webview-command-line` (which
only a userdebug image reads) and removes the file and the screen size it changed at every way out. It checks: the phone layout at
1080x2400 (the key row and the phone's bars, none of the tablet's chrome), the tablet layout at 1800x2560 and density 240, a change
between the two, the key row above the real soft keyboard, the sticky Control key (armed, sent with `c` as an interrupt, and cleared),
scrolling the scrollback by touch, and a long press that selects text with Copy and Paste in the page's own menu (a number that was
copied reached the shell). Three checks are reported as **not run**, with their reasons: the hardware-keyboard rule (the emulator's
input command could not make the page see a hardware keyboard; the page's unit tests hold the rule), the pinch (the input command has no
two-finger gesture), and text composed by a soft keyboard (`input text` sends key events; Gboard is a real-device row).

**The `pagesize` section.** It reads the page size of the image (`getconf PAGE_SIZE`) and checks, from `/proc/<pid>/maps`, that the
app's native library is mapped from the APK with every mapping aligned to the page size and the `base.apk` mapped executable. On the 16 KB image
the page size is 16384, on the other two 4096.

### What the three images showed

The run of the whole set on the three images (one emulator at a time) ended with these tables. A check that could not be made is
`not run` and was not proved; the reasons are printed with every run.

**Only one full run of the set was all green, the last.** The first four full runs each had a failed or a flaky check. In the fourth, Android 17 had 117 checks `ok`, 3 `not run` and 1 `FAIL` (the page size section, a broken pipe from the activity manager when the app was started; run alone afterwards it had 5 `ok`), Android 12 had 53 `ok` and 3 `not run`, and the 16 KB image had 36 `ok`. The hand-over section on Android 17 failed in three earlier full runs (the system's document dialog was in front and the tap chose nothing) and passed run alone and in the fourth run; the driver now taps again when the dialog is still in front, and the retry for the broken pipe was added after the failing run. The fifth full run, made by `sh scripts/android-gate.sh --emulator` on the finished tree, was all green: Android 17 had 122 `ok`, 3 `not run` and 0 `FAIL`, Android 12 had 53 `ok` and 3 `not run`, and the 16 KB image had 36 `ok`. One green run does not show that the earlier failures are gone, and it cannot be said whether the retries are what made it pass. During the Android 12 part of that run the script printed five `Broken pipe` lines from its own `echo` (a noise in the output of the service section; those checks were still `ok`; that noise is fixed, see below).

A sixth full run was made by `sh scripts/android-gate.sh --emulator` after the fixes of the two Android 12 defects, the local-network permission and the release smoke test, and was all green again: Android 17 had 135 `ok`, 3 `not run` and 0 `FAIL` (the SSH section now holds the ten checks of the local-network permission, the service section the window the system relaunched), Android 12 had 56 `ok` and 3 `not run` (the bar below the system buttons is now laid out with three-button navigation and no workaround, and the relaunch check ran there), and the 16 KB image had 39 `ok` and none `not run`. That is two green full runs in a row, not a proof of stability, and the retries stay in place.

| Image | Android | WebView | Page size | Sections | Not run |
|---|---|---|---|---|---|
| `api37` | 17 (API 37) | 145.0.7632.218 | 4096 | tracer, service, vault, tls, handover, clipboard, touch, ssh, pagesize | the hardware-keyboard rule, the pinch, text composed by a soft keyboard |
| `api31` | 12 (API 31) | 91.0.4472.114 | 4096 | tracer, service, vault, pagesize | the foreground service type (it is read from Android 14), the notification permission and its dialog (they start with Android 13) |
| `16k` | 17 (API 37, Google APIs) | 145.0.7632.218 | **16384** | tracer, service, pagesize | none |

Two things the Android 12 image showed. Both are fixed, and both were checked on emulators only.

- **The page's bottom bar under the system buttons (fixed).** On that image the system starts with the three-button navigation bar,
  and its old WebView (91) does not tell the page the height of that bar as a safe area, so the page's bottom bar (the tab row with the
  "+") was drawn under the system buttons, and its top strip under the status bar. The Android shell now measures the system bars
  (`WindowInsets`, status bar, navigation bar and display cutout) and writes the four heights in CSS pixels into the page as
  `--hw-sys-top`, `-right`, `-bottom` and `-left` on the root element; the page uses the larger of those and its own
  `env(safe-area-inset-*)` (`--hw-inset-*`, [design.md](design.md) section 6.6). The values are written when the system reports the
  bars and again when a page has finished loading. Checked on the Android 12 image with three-button navigation and WebView 91: the
  screenshot shows both bars clear of the system's, and the `service` section runs there without the switch to gesture navigation that
  it used to need (the switch is removed). Not tried: a real Android 12 phone, a display cutout, a landscape phone, and the tablet
  layout, which does not use the values yet.
- **A relaunched activity and the service (fixed).** When the system relaunched the activity while the app ran (seen when the
  navigation mode changed on that image; a change of the font scale does the same on every image), a shell that was started after
  `Stop all` ran but the service was not started again and no notification showed, although the core told the plugin the new counts.
  The cause was the one suspected, and it was confirmed by a check that failed before the change and passes after it: the plugin is made
  once per process with the first activity and kept that one, so its check for a visible app read an activity that was destroyed and
  never answered yes. The plugin now keeps the application context for everything that needs no window and the activity in use (set
  when one comes to the front, cleared when that one is destroyed) for the rest: the visibility check, the app switcher picture
  and the system prompt of the secret store. The `service` section changes the font scale while the app runs, confirms from the system
  log that the window was relaunched, and requires `Stop all` and a new shell to bring the service back; it ran on the Android 12 image.
  Not tried: other reasons for a relaunch than the font scale and the navigation mode, a relaunch while the system prompt is open, and
  a phone.

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

`app/src-tauri/android-permissions.txt` is the one list of the permissions the app may ask for (today the network, the three of the foreground service described below, and the one for the system prompt of the secret store).
The `android_release` test requires the manifest to hold exactly these, and the APK check requires the same of a built APK, so a
permission that is added anywhere else fails one of them. A feature that needs a permission adds its line to the file in its own
commit, with the manifest line, and says in the message what it is for. A permission that installs packages, reads the screen, draws
over other apps or reads contacts, messages or location is never on the list without a decision of its own.

## The service that keeps sessions alive

Android freezes or ends an app a few minutes after its screen turns off. A terminal that loses its SSH connection whenever the screen
goes dark is not usable, so while a shell or a connection is alive the app runs a **foreground service**, which Android requires to
show a notification.

- **What the notification says.** The title is `holzWave is running` and the text is the counts and nothing else: `3 shells, 1
  connection`, `1 shell`, `2 connections`. It never holds a host name, a command or any output of a shell, and on a locked screen only
  the title shows. It is on a channel called `Running sessions` with low importance, no sound, no vibration and no badge, and it is
  ongoing (it cannot be swiped away while sessions run). Tapping it opens the app.
- **`Stop all`.** The notification has one action. It ends every shell and every connection of the app and removes the notification. It
  does not close the app's window and does not delete any setting, layout, history or secret: the panes stay where they were and show
  the state they show when a shell has ended on its own. A new pane starts a new shell and the service again.
- **When it runs.** It starts when the first shell or connection is alive and stops when the last one has ended. The app counts them
  every two seconds and tells the service only when the numbers change. A connection counts while it is up or is being made again after
  a loss; one that is still connecting, failed or ended does not.
- **Only while the app is in front.** Android 12 and later refuse a foreground service that is started from the background, so the app
  starts it only while its window is visible. If Android refuses anyway, the app notes it and tries again the next time the window is in
  front. Nothing starts it from a boot, a receiver or another app: the service is not exported, and there is no wake lock, no request
  to leave the battery optimisation and no start-at-boot permission. The service holds the process and does no work of its own.
- **In the manifest.** The service has the type `specialUse`, because no other type describes a terminal, and carries the public
  sentence `Keeps terminal shells and SSH connections running while the app is in the background`, which stores and installers show.
  Three permissions are in `android-permissions.txt` for it: `FOREGROUND_SERVICE` and `FOREGROUND_SERVICE_SPECIAL_USE` (the service and
  its type) and `POST_NOTIFICATIONS` (its notification).
- **The notification permission.** On Android 13 and later the system asks once, at the first start of a session. Whatever the answer
  is, the app does not ask again. If it is denied the service still runs, but its notification is not shown, and Android lists the app
  among the active apps; `Stop all` is then not reachable, and closing the panes ends the sessions as before.
- **How Rust and Kotlin talk.** One Kotlin class of the app (`HolzwavePlugin`) is registered with Tauri's own plugin builder; no
  `tauri-plugin-*` crate is used. It has no command on the Rust side, so the page cannot call it. The release build keeps the class by
  name (R8 is on) with a rule in `gen/android/app/proguard-rules.pro`.

**The known limit.** Android 12 and later may end the local shells of a backgrounded app under its limit on background processes
(the phantom-process limit), even while the service runs. SSH connections are not affected: they live inside the app's own process,
and a shell on a remote host keeps running there. This limit was **not observed** on the emulator, which does not apply it as phones
do, and nothing was run on a device that applies it. Whether local shells survive an hour in the background on a real phone is open.

```sh
sh scripts/android-e2e.sh service
```

proves on the emulator image that the service is foreground with the type `specialUse` (dumpsys prints `types=0x40000000`), that the
notification has the title, the counts, the action, the channel and no sound, that both shells are alive after 20 seconds in the
background and the pane answers when the app comes back, that tapping `Stop all` in the notification shade ends the shells and removes
the service and the notification while the app keeps its window, that a new shell starts the service again, and the three states of
the notification permission (granted, revoked, and the system's dialog at the first start). **Not proved:** the limit above, a swipe
of the app out of the recents list, doze and app standby over hours, the skins of phone makers, the permission dialog of another
Android version or locale, and any real phone.

## The secret store

Passwords, key passphrases, the sync password and the AI provider keys are kept in one encrypted file, `vault.bin`, in the app's data
folder (beside the app's own files, not inside the shell's `home` folder). The shell runs as the same user and can read that file,
but it holds only ciphertext.

- **What is encrypted with what.** One random data key encrypts the file with XChaCha20-Poly1305. That data key is wrapped by an
  AES-256-GCM key that lives in the Android Keystore and never leaves it: in the secure chip (StrongBox) when the device has one,
  otherwise in the trusted environment of the system. The file holds the wrapped data key and the ciphertext; the app holds the data
  key in memory only while the store is unlocked. No cryptography is written by hand and no library is used for the prompt: the keys
  are made through the platform's `KeyGenParameterSpec` and the prompt is the platform's `BiometricPrompt`.
- **Protected mode.** When the device has a screen lock, the store is protected by default: the wrapping key needs the screen lock or
  a strong biometric for every single use (no time window), through the system prompt, once per unlock. Locking again happens five
  minutes after the app's page was hidden, measured with the clock that also counts the time the phone slept (the elapsed-realtime
  clock; the ordinary monotonic clock stops in suspend), and a check every thirty seconds applies that rule even while the page stays
  hidden. Live shell and SSH sessions go on while the store is locked; only reading or saving a secret waits for the unlock.
- **A new fingerprint does not end the key (the owner's decision).** The wrapping key is made with
  `setInvalidatedByBiometricEnrollment(false)`, so enrolling a new fingerprint or face does not delete it and the saved secrets stay
  readable. This was decided by the owner on purpose and is not an oversight: the key also accepts the device credential, so whoever
  can enrol a finger already knows the PIN and could simply unlock, and losing every saved secret over a harmless act is the worse
  failure. Removing the screen lock still deletes the key, which the system does for every key that needs authentication. The
  decision is in the code beside the setting (`KeystoreBridge.kt`); a stricter choice (the key dies at an enrolment) would be a new
  decision of the owner, with a way to enter the secrets again.
- **Plain mode.** On a device with no screen lock the key needs nothing, the store opens by itself, and the page shows a notice that
  the secrets are protected by the device key only. When a screen lock is set later the store can be switched to protected mode, and
  back: the data key stays the same and only its wrapping changes.
- **A lost store.** Removing the screen lock makes the system delete every key that needs authentication. The store then reports
  that it is lost (its key is gone), never that it is empty, and only an explicit reset starts a new one.
- **The app switcher.** While the store is protected, the app switcher hides the picture of the app whenever the app is not in the
  foreground, whether the store is locked or unlocked, so a store that locked five minutes after the app went to the background
  already has no picture to show. This is a call of Android 13 and later that changes only that picture; **screenshots and screen
  recordings are not blocked**, on purpose, because a terminal's content is what people want to screenshot. Plain mode (protection
  off) does not hide the picture. **On Android 12 and 12L nothing hides it**: the call does not exist there, no cover view or other
  workaround is used, and the picture may show terminal content, protected store or not. That is a known limit, not a best effort.
- **What the emulator run proved.** `sh scripts/android-e2e.sh vault` runs a self-test that exists in debug builds only (it is
  compiled out of a release build and the APK check fails a release APK that holds its trigger). On the arm64 emulator it showed: a
  plain store made by the first save and reopened with no prompt after a restart; the switch to protected mode with the system
  prompt answered by a PIN typed through the input path; a locked store after a restart that answers the locked error, the prompt,
  the PIN, and the saved secret read back; the lock after the page was hidden and a device clock that advances; an AI key saved while
  unlocked and present after a restart and an unlock (no provider is called); the removed screen lock giving a lost store and a reset
  giving a new one; the app switcher call applied for the protected store and not applied for plain mode, with the system's own
  picture of the app's task a placeholder of about a kilobyte in the first case and a real image of tens of kilobytes in the second
  (Android 37, read as root on the emulator image), and the app switcher screenshots saved under `target/android-e2e/`. It also
  searches for a canary secret in every file of the app's data folder and of its folder on the shared storage, the whole log, the
  environment of the process and the argument lists of all processes, and finds it in none. The same run passes on an Android 12
  image, where the call reports that it is unsupported and the system keeps a real picture of the app in the protected case too.
- **A virtual fingerprint, once by hand.** The emulator image has a virtual fingerprint sensor. It was enrolled once through the
  Settings app and touched with `adb emu finger touch`: the prompt of a newly made protected store, and the unlock after a restart,
  were each answered by the virtual finger and the system counted them as accepted crypto operations. This was not scripted and
  is not part of `e2e vault`; it shows that the prompt works with a crypto object and a biometric on that image, not on a phone.
- **What it did not prove.** StrongBox (the emulator has none and reports it truthfully), a real fingerprint sensor or a face (the
  scripted run drives the prompt with a PIN), how the strong-biometric prompt behaves on any given phone, a key that a vendor's Keystore invalidates or
  loses in its own ways, a key made unusable by enrolling a new fingerprint (the key is made to survive an enrolment: whoever can
  enrol a finger already knows the PIN that also opens the prompt, and losing every secret over a harmless act would be the worse
  failure), the app switcher card of any real phone (vendors draw their own), and every Android version other than the two images
  named. Whether the prompt works on a particular phone is a row of the test plan without a result.

## HTTPS and certificates

Both network clients of the app, the WebDAV client of the sync and the client of the AI chat, judge a server's certificate with the
platform certificate verifier (`rustls-platform-verifier`), and with nothing else: no bundled list of roots, no custom verifier, no
setting that accepts any certificate (a text test searches the sources for each of those and finds none). On Android that means the
chain is judged by the system trust store through the platform's own trust manager.

- **What Android needs that other systems do not.** The verifier calls into the Java side, so two things must exist or every HTTPS
  request fails: a small Kotlin component in the app, and one call at start that hands the verifier the application context. The
  component is a prebuilt Maven artifact (`org.rustls:rustls-platform-verifier`) that the verifier's own project publishes on GitHub.
  It is outside Maven Central and Google's repository, so **the Gradle build needs network access to that repository**. The Gradle
  project asks that repository for its own group only; the version of the artifact is read from the entry of the Rust crate
  `rustls-platform-verifier-android` in `Cargo.lock`, so the Kotlin and the Rust half cannot drift apart; and the SHA-256 of the
  artifact and its metadata file are pinned in `app/src-tauri/gen/android/gradle/verification-metadata.xml`, so Gradle refuses a
  file that does not match (every other artifact is trusted as before). When the crate is updated the pinned sums are updated in
  the same commit, and a text test compares the version there with the lock file. The call that hands over the context is one
  native function (`init_tls` in `src/android/tls.rs`) that the app's Kotlin plugin class calls when it is made, before any network
  code can run.
- **A failed set-up refuses, it never accepts.** If the call fails, the app does not crash and no check is loosened: the sync run
  and the send of an AI chat message refuse with the fixed word `tls_not_ready` (the sync shows it as an error, the chat as a
  network failure), and a restart tries again. The two refusals exist in Android builds only.
- **Revocation.** The verifier asks Android to check revocation of the server's certificate as a soft failure only, and the
  component's own manifest would allow plain HTTP to the hosts of the certificate authorities' revocation lists. The app takes that
  allowance out of its manifest, because the release build allows no cleartext at all (the APK check enforces it). The result: the
  system trust store decides what is trusted, and online revocation lookups that need plain HTTP are not made.
- **Sync and the locked vault.** While the secret store is locked, the sync does not run at all: its state is `paused`, with no
  error text and no network connection, and the first unlock starts one run at once.
- **What the emulator run proved.** `sh scripts/android-e2e.sh tls` runs a debug-only self-test on the arm64 emulator against a
  throwaway TLS server that the script starts on the Mac (a self-signed certificate made for the run, the server bound to the Mac's
  loopback address and forwarded to the emulator's own loopback address with `adb reverse`, all removed at every way out). On the
  Android 37 image and on the Android 12 image it showed: a public site is accepted by both clients (the sync client reaches the
  server's own answer, `405 Method Not Allowed`, and the chat client gets an answer); the self-signed server is refused by both
  clients, as `invalid peer certificate: UnknownIssuer` for the sync and `the server certificate was not accepted` for the chat,
  and the server's own log shows the failed handshakes; a save of a sync account against it keeps nothing; after a restart with a
  locked vault the sync is `paused` with no error text and the server sees no connection, and the unlock starts one run that
  reaches the handshake and refuses the certificate. The canary secret is in no log, file, environment or argument list.
- **Local-network addresses on Android 17 (declared).** The emulator's name for the development machine, `10.0.2.2`, is a local-network
  address, and so is any private address (10/8, 172.16/12, 192.168/16, 100.64/10, link-local, fc00::/7, `.local` names). An app that
  targets Android 17 (this one does) can not reach such an address until it holds the runtime permission `ACCESS_LOCAL_NETWORK`; without
  it the connection simply times out (`timeout: global`). The app now declares the permission (the sixth line of
  `android-permissions.txt`) and asks for it **when the first connection to such an address is made**, never at the start: the SSH
  login, the sync's WebDAV request and the AI chat's request each ask the platform first (`reach.rs` in the core holds the rule, the
  Kotlin plugin shows the system's own dialog, which is worded `Allow holzWave to find, connect to, and determine the relative position
  of nearby devices?`). A refusal is a plain sentence that names the host and says what to do (`holzWave is not allowed to reach
  192.168.1.20, which is on your local network. Allow Nearby devices for holzWave in the Android settings (Apps, holzWave, Permissions),
  then try again`), never a timeout; an SSH pane shows it and waits for Enter, which asks again. A grant is remembered for the run. An
  address that is not local (loopback, public) is never asked about, and below Android 17 nothing is asked because the permission does
  not exist. If the app is not on screen when the question is due (a reconnect in the background), the sentence says the permission can
  only be asked while the app is on screen. The `ssh` section checks it on the Android 17 image without `adb reverse`: the app reaches
  the Mac as `10.0.2.2`; saving such a connection and starting the app ask nothing; `Don't allow` gives the sentence and no login at the
  server; `Allow` connects (the server logs a login); after the permission is revoked the next connect asks again and `Don't allow`
  fails the pane with no crash; and after the permission is allowed in the settings (`pm grant` does what the switch there does) the
  connection is made with no dialog. Not run: an address on a real network, a real phone, the dialog after two refusals (the system then
  stops asking and the person has to use the settings), a `.local` name resolved by a real mDNS responder, and the sync and the chat
  against a private address on the emulator (their checks are in the core's tests with a gate that refuses).
- **What it did not prove.** Any real phone, any Android version other than the images named, user-installed certificate
  authorities (Android applies them to an app only as its target SDK and network security config allow; this app has no such
  config, so none is expected to be trusted, and that was not tried), corporate proxies and TLS inspection, captive portals,
  certificate pinning (the app pins nothing), revocation behaviour, mobile networks, a real WebDAV server and a real AI provider
  from a phone.

## Handing one file over

The shell lives in a folder of the app's own storage, and Android does not let a program there open a folder in another app's
storage. A folder dialog could grant a whole tree, and that grant would have to be kept and managed; the app does without it. What
it offers instead is a copy of **one file at a time** through the system's own document dialog, in either direction.

- **Copy in.** The palette entry and the sheet row open the system's open-document dialog (`ACTION_OPEN_DOCUMENT`, one document, only
  documents that can be opened). The chosen document is copied to `home/handover/` of the shell's folder, under a name the app
  decides: the provider's display name is another app's text, so the host cleans it to one safe file name (no separator, no `..`, no
  control or direction character, at most 255 bytes) and numbers it when the name is taken (`report.pdf`, `report-2.pdf`). The file
  has mode 0600. A file larger than **256 MiB** is refused: a size the provider reports above the limit is refused before a byte is
  read, and the bytes are counted while they are copied, so a provider that reports no size or a wrong one is stopped at the limit
  and the partial file is removed.
- **Copy out.** The page asks for the path of a file in the shell's folder, the host checks that it is a regular file that stays
  inside the folder, and the system's create-document dialog opens with its name; the file is copied to the document the person
  names. Nothing outside the shell's folder can be copied out.
- **What the app asks Android for.** Nothing: no storage permission, no media permission, no `provider` element in the manifest, no
  folder dialog, and no kept grant to a document (a document's address is used once and dropped). A text test fails if any of
  these appears anywhere in the Android project. The permission list is the same as before the dialogs.
- **Where the work is done.** The Kotlin part of the app shows the dialog and copies the bytes on a thread of its own, never on the
  main thread, and the call into it waits on a blocking worker of the host, so a dialog that stays open for minutes blocks neither
  the page nor the window. A path that comes from the Rust side must be inside the shell's folder and must not be a link, and a
  result of the dialog is used only when it is a `content` address; both are checked again in Kotlin. One hand-over runs at a time;
  a second one answers `Could not copy the file` at once. A dismissed dialog changes nothing.
- **What the emulator run proved.** `sh scripts/android-e2e.sh handover` drives the real dialog of the image (the documents app of
  the system, window `com.android.documentsui/.picker.PickActivity`) with `uiautomator`. On the Android 37 image
  (`system-images;android-37.0;default;arm64-v8a`) and on the Android 12 image (`android-31`), every check was ok: a pushed file of
  1 MiB of random bytes was chosen and its copy has the SHA-256 of the source, mode `-rw-------` and no staging file beside it; a
  second copy of the same file is `hw-handover-test-2.bin` with the same checksum and the first copy is unchanged; a dialog
  dismissed with Back changes nothing; a file of 300 MiB (made on the emulator) is refused as too large with its name; a file named
  `hw späce.bin` (a space and a letter outside ASCII) is copied in under exactly that name; and a copy out of a fixed 4 KiB pattern
  to the Download folder has the pattern's SHA-256. The script removes every file it pushed, made or copied.
- **What it did not prove.** Real providers: a cloud drive, a USB stick, an SD card or another app's provider, which may report no
  or a wrong size, revoke a document or stream slowly; very large files on a phone with little free storage; the dialogs of other
  Android versions and phone makers' skins (the emulator has the plain system dialog of two images); a dialog result that arrives
  after the app's window was recreated (two attempts to force that on the emulator, a change of the font scale and the developer
  setting that destroys activities, did not recreate the window, so it was not tried); the touch feel; and any real phone.

## The clipboard

Paste on Android does not rely on the web view's own clipboard permission. The page asks the app for the clipboard first, after the
person's own paste (the paste chord or the menu's Paste), and the app answers through the Kotlin part of the app; a copy of the
terminal selection goes the same way.

- **Plain text only.** The clipboard is read as the plain text of the first item of the primary clip and nothing else: a link, a
  picture or a page of HTML gives an empty text, and nothing is converted into text. A copy writes a plain text clip.
- **Bounded, never cut.** A text of more than **512 KiB** (UTF-8) is refused in either direction, with the same message form as the
  desktop's much larger bound, and a refused write leaves the clipboard as it was. The system clipboard lives in another process and
  a clip crosses to it in one transaction whose buffer is about 1 MiB, so the bound is below the desktop's.
- **Marked sensitive.** On Android 13 and later a copy marks the clip as sensitive, so the system's confirmation does not show the
  text. Android 12 has no such mark; the text is written all the same.
- **Read only inside a person's own paste.** The Kotlin part has no listener for clipboard changes and no entry the page can call:
  only the Rust code of the app reaches it, and the page's `clipboard_text` is a read with the same limits as on the desktop.
  Android 10 and later refuse a read to an app that is not in front, and Android 12 and later show a short message when an app reads
  a clip that another app wrote; the app works around neither. A text is never logged and never put in an error message.
- **What the emulator run proved.** `sh scripts/android-e2e.sh clipboard` writes a fixed text through the app's own path, reads it
  back and gets the same text, tries a text of 600 000 bytes (refused, and the first text is still on the clipboard), and reports
  the sensitive mark: set on the Android 37 image, absent on the Android 12 image, as the script requires. `dumpsys clipboard` lists
  nothing about the clip on either image; that line is printed for information and is not a pass condition.
- **What it did not prove.** The paste through the page itself (the menu's Paste into a running pane) was not run on the emulator;
  the clipboard strips of third party keyboards, the clipboard managers of phone makers, multi-window and foldable focus rules, how
  a real phone's system shows or hides the preview of a sensitive clip, and any real phone.

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
services and Firebase classes, that a release APK holds no trace of the debug-only self-test of the secret store (its trigger is searched
in the dex files and the native library), and the signature (a release APK must be unsigned; a debug APK carries the debug
certificate). It
installs nothing and writes only to a temporary folder that it removes on every way out.
`sh scripts/test-android-apk-check.sh` proves every rule once passing and once failing with stand-in tools, without any Android
software.

The target SDK is printed and not judged: what minimum a store such as F-Droid asks for today was not verified.

## The release is unsigned

`sh scripts/android-build.sh release` builds `app-universal-release-unsigned.apk`. No script of the repository creates a key or
reads a keystore password, the Gradle project has no signing configuration, and no script signs a release for distribution. Signing
a release is the owner's own step, with the owner's own key kept outside the repository, when a release is published. An unsigned APK
cannot be installed. The one exception is the smoke test of the minified release (R8 is on): `sh scripts/android-e2e.sh release` signs
a **copy** of the release APK in a temporary folder with the Android SDK's public debug key (`~/.android/debug.keystore`, alias
`androiddebugkey`, password `android`; never a distribution key, and no other key is accepted), installs it on the Android 17 arm64
emulator and runs it, and then deletes the copy and uninstalls the app. `HOLZWAVE_E2E_RELEASE_APK` names another unsigned release APK
to test. The section is not part of `all` or `matrix`; it runs only on its own. The
original is never signed: its SHA-256 and its unsigned state were the same before and after, and `android-apk-check.sh <apk> release`
still passes it and still fails a release APK that holds `holzwave_selftest` (the stand-in test proves that failing case in the dex
files and in the native library).

What the minified release did on that emulator: the app started without a crash and the log shows the platform certificate verifier
set up; the page appeared and a first shell pane opened by itself (`sh -l`, a child of the app, in the app's `home` folder); the
system's notification dialog showed once, when the first shell started, and after Allow the foreground service ran with its notification;
a command typed with the input command (`sleep 777`) became a child process of that shell; and with a screen lock set the app started and
its bars were clear of the system bars. The dex files of the minified build still hold the plugin commands (`localNetworkEnsure`,
`insetsApply`), the Keystore bridge and the enrolment setting, and hold no self-test.

What could not run there: the `tracer`, `vault` and `tls` sections of `scripts/android-e2e.sh` are driven by the debug-only self-test, which
a release build does not contain (that is the point of the check above), so none of them ran against the minified build. The phone page
has no control that opens the secret store or a connection (the SSH connection manager and the save-a-password dialogs are not on the
phone layout), so the system's unlock prompt, a TLS request and the local-network question could not be reached by hand either. For these
the minified build is only shown to keep the code they call; their behaviour was proved on the debug build.

## Reproducibility

Every build sets `SOURCE_DATE_EPOCH` (the date of the last commit, or the value already in the environment), replaces `RUSTFLAGS` by
`--remap-path-prefix` flags for the repository folder, the Cargo home and the NDK folder, and passes `--locked` to Cargo; it prints
the NDK path and the Rust version it used. The Gradle project leaves the commit and Google's dependency block out of the APK.

`sh scripts/android-repro-check.sh` makes this measurement: it fixes `SOURCE_DATE_EPOCH` once, clears the build outputs, builds the
unsigned release twice and compares the two APKs byte for byte (it names the archive entries that differ when they do). It clears the
outputs, the debug APK included, at the end unless `HOLZWAVE_REPRO_KEEP=1` is set.

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
- The limit of Android 12 and later on background processes for local shells (the emulator does not apply it), a swipe of the app out
  of the recents list, doze and app standby over hours, and the system's notification dialog on another version or locale.
- The HTTPS check on any Android version other than 12 and 37, and the local-network permission on a real phone and on a real network.
- Any Android version other than the three emulator images (Android 17 with API 37, Android 12 with API 31, and the 16 KB page image,
  all arm64): no Android 13, 14, 15 or 16 image, no phone maker's skin, and for the Android 12 image only the sections named above.
- The hardware-keyboard rule of the key row, the two-finger pinch, and text composed by a real soft keyboard (Gboard): the emulator's
  input command cannot make them, so those checks are `not run`.
- A real SSH server on a network: the SSH check reaches a throwaway OpenSSH server on this computer's loopback address through
  `adb reverse` (and once as `10.0.2.2` for the local-network permission), with one key. Password login to a server, other key types, a
  server that is slow or drops, and the local-network permission for a real address on a real network were not tried.
- x86_64 and 32-bit images or builds, which are refused by design.
- A signed release APK for distribution, installing from F-Droid or Obtainium, Play Protect or installer warnings for a self-signed APK, and a 16 KB page device other than the emulator image with that page size.

## What ran and what did not

Everything below ran on arm64 emulators of the Android SDK on one Mac (Apple silicon), on the images of the table above, and on no
phone or tablet. `not run` means the check could not be made and was not proved.

**What ran.** On Android 17 (API 37, the default image, 4 KB pages, WebView 145): the tracer (the page loads in the real web view, a
pane starts the system shell in the app's own folder, keys from the Android input path reach it), the service, the secret store, HTTPS,
the hand-over through the real document dialog, the clipboard, the touch page driven in the real web view, a saved SSH connection
against a throwaway server on this computer's loopback address, and the page size check. On Android 12 (API 31, WebView 91): the tracer,
the service (three checks `not run`), the secret store and the page size check in the closing matrix, and the HTTPS, hand-over and
clipboard sections in their own runs. On the 16 KB image (API 37, Google APIs, page size 16384): the tracer, the service and the page
size check, with every mapping of the app's library aligned to 16384 bytes. The release APK was built, passes the APK check and is
unsigned. The whole workspace test run, the page tests and the stand-in tests of the scripts ran on the Mac without an emulator.

**What ended `not run`**, with the reason: the rule that hides the key row for a hardware keyboard (the emulator's input command
never made the page see a hardware keyboard; unit tests hold the rule), the two-finger pinch (the input command has no two-finger
gesture), text composed by a soft keyboard (the input command sends key events), the foreground service type `specialUse` on Android 12
(Android reads it from Android 14) and the notification permission that was revoked, and its system dialog, on Android 12 (they start
with Android 13). The app switcher picture was looked at on Android 17 and Android 12 only (below), never on a phone.

**The app switcher picture while the vault is protected.** On Android 13 and later the app hides it whenever the app is not in
front, locked or unlocked, and the emulator image of Android 17 showed a blank placeholder in place of the app's picture. Screenshots
are not blocked. On Android 12 and 12L nothing hides it and it may show terminal content: a known limit, observed on the Android 12 image.

**The things that only a real device or a person shows**, all untested; each is a row of Part 13 of [test-plan.md](test-plan.md) with
an empty result:

1. **Soft keyboards (Gboard and others).** An emulator showed the stock keyboard opening and the key row sitting above it, and the
   sticky `ctrl` key working with keys sent by `adb shell input`. Composition, auto-correct, key repeat, emoji, dictation and
   third-party keyboards are untested.
2. **Hardware keyboards.** The rule is held by unit tests; the emulator could not trigger it. Layouts, modifier combinations and the
   desktop chords on a tablet with a keyboard case are untested.
3. **Real biometrics.** A PIN prompt was driven by the keys of the real input path, and a virtual fingerprint was enrolled and touched
   once by hand. StrongBox, a real sensor or a face, a new enrolment, the removal of the lock and device-admin resets on a phone are
   untested.
4. **Background behaviour and vendor skins.** The service and its notification ran on all three images. The limit of Android on
   background processes (the emulator does not apply it), battery optimisation, task killers, doze over hours and the notification
   behaviour of a vendor are untested.
5. **Real storage providers.** The system's own document dialog of two images was driven for one file in and one file out. A cloud
   drive, an SD card, a USB stick, a provider that gives no size or a wrong one, and a revoked document are untested.
6. **Real mobile networks.** SSH ran against a loopback server through `adb reverse`; HTTPS and the sync's refusal of a bad certificate
   ran against a local throwaway server and a public site. Cellular data, Wi-Fi roaming, the sync over mobile data and a private
   certificate authority on a phone are untested.
7. **Touch feel.** Long-press selection with `Copy` and `Paste`, scrolling by a drag and the phone and tablet layouts were seen in the
   real web view. The timing of a long press, the precision of the handles, a pinch together with a scroll, a palm, a stylus, foldables
   and multi-window are untested, and the pinch itself never ran.
8. **Real web views, graphics and memory.** WebView 145 and WebView 91 were each seen on one image. Updates of the system web view,
   canvas speed, thermal throttling during output floods and memory kills are untested.
9. **A 16 KB-page device from a vendor.** The emulator image showed the app, its shell and its library mappings on 16 KB pages; a
   vendor's phone was not tried. x86_64 and 32-bit builds are refused by design.
10. **An APK on a phone.** The unsigned release APK passes the APK check and was never installed or signed for use. The prompts of the
    system installer, updates over an installed copy, the warnings of a phone's protection service and the developer verification
    that Google plans for 2027 are untested.
11. **The AI chat and the sync from a phone.** Not part of any emulator check: the sync's refusal of a bad certificate ran against a
    local server only, and no real provider or WebDAV server was used.
12. **Accessibility.** The page's nodes can be read by the emulator's automated test tool, which is not TalkBack. TalkBack, large font
    sizes and high contrast on a phone are untested.

Also untested: the tablet layout on a tablet, the paste through the page's own menu into a running pane, and the local shell on the
shells of phone makers.

**Two defects, fixed and checked on emulators only.** The page's bottom bar that lay under the system buttons on the Android 12 image,
and the service that did not come back for a shell started after `Stop all` once the system had recreated the activity (see [what the
three images showed](#what-the-three-images-showed)). A phone is still the place to look at both (rows in Part 13 of the test plan).

## Facts for whoever offers the app

These are the facts a listing, a store form or an update checker asks for, as the project stands. Nothing here says the app is offered
anywhere: no package has been signed for use and no channel exists.

- **Application id:** `ch.holzcloud.holzwave` (`ch.holzcloud.holzwave.debug` for a debug build). **One ABI:** `arm64-v8a`; no other
  ABI is built or listed. **Android versions:** minimum 31 (Android 12), target 37.
- **Version code:** explicit, from `sh scripts/android-version-code.sh <version>` (the scheme is in [the version code](#the-version-code));
  `0.0.1-beta2` is 1202. A build for a listing comes from `sh scripts/android-build.sh release`, which builds the APK **unsigned**.
- **Permissions** (the one list, `app/src-tauri/android-permissions.txt`): the network (`INTERNET`), the foreground service
  (`FOREGROUND_SERVICE`, `FOREGROUND_SERVICE_SPECIAL_USE`), the notification (`POST_NOTIFICATIONS`), the system prompt of the secret
  store (`USE_BIOMETRIC`) and, from Android 17, reaching a device on the local network (`ACCESS_LOCAL_NETWORK`, asked at the first such
  connection). No storage, media, contacts, location, install-packages or boot permission, no wake lock, no Google services,
  no tracker and no updater.
- **The foreground service type:** `specialUse`, with the public text `Keeps terminal shells and SSH connections running while the app
  is in the background`.
- **Network services the app talks to:** the SSH hosts, the WebDAV server of the sync and the AI providers that a person adds; the
  cloud AI providers are part of every Android build, and the app contacts one only after a person has added it and sends a message.
- **A component fetched from outside the usual archives.** The Kotlin component that carries the platform certificate verifier
  (`org.rustls:rustls-platform-verifier`) is a **prebuilt artifact** that Gradle downloads from the verifier project's own Maven
  archive on GitHub (`https://github.com/rustls/rustls-platform-verifier/raw/maven-archive/android-release-support/maven/`), which is
  neither Maven Central nor Google's repository. Its version is read from `Cargo.lock` and its SHA-256 is pinned in
  `app/src-tauri/gen/android/gradle/verification-metadata.xml`, which protects what is downloaded and does not change where it comes
  from. A build that must be made from source only may refuse it; the Rust workspace of the crate holds the Kotlin sources, so the
  component can be built from source at the pinned version.
- **The self-test:** the debug build holds a self-test that the emulator scripts drive; it is compiled out of the release build, and
  the APK check fails a release APK that holds its trigger.
- **Reproducibility:** two builds of the release APK in one folder on one machine were byte for byte equal; another folder, another
  machine and another build server were not tried.

