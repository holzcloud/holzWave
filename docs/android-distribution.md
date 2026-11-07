# holzWave on Android: the two channels, who signs, and the limits

> [!IMPORTANT]
> **Status.** **The Android app does not exist yet.** No APK has been built, nothing was run on an Android device or an
> emulator, and nothing was sent to F-Droid or published for Obtainium. This page says what will be true when the app exists,
> so that nobody is surprised by it. It also says up front what no money can fix: Google's developer verification. The
> package `ch.holzcloud.holzwave` is **not registered** with it and will not be (a full account costs $25, which this project
> does not pay). From the global rollout in 2027 an unregistered app installs only through Android's advanced flow or through
> ADB ([the limit](#the-limit-google-developer-verification)). The channels overview is in [distribution.md](distribution.md).

Facts that came from a web page carry its address and the day it was read, **2026-11-05** unless another day is given. What
was not checked says "as far as the author knows". Those pages change, so read the current one before relying on a rule here.

## The two channels

| Channel | What it is | Who signs the APK | Cost |
|---|---|---|---|
| **Obtainium** | An app on your phone that watches the GitHub releases page of this project and installs the APK from there. The release carries an APK signed with the author's own key | The author's key (`CN=holzcloud.ch, OU=holzWave`). Obtainium installs the file unchanged | Nothing |
| **F-Droid** | A free app store that builds apps from their source on its own servers | F-Droid's own key. If the build is reproducible and F-Droid publishes the author's signature instead, the author's key (as far as the author knows, from <https://f-droid.org/docs/Reproducible_Builds/>) | Nothing in the pages read; no fee is stated, as far as the author knows there is none |

A person gets the certificate of whoever signs. Both channels are planned together; neither can be used until the app exists.
Google Play is not used and not planned. iOS is out of scope: there is no Apple developer account.

## Moving from one source to the other

Android does not update an app with an APK signed by a different certificate, as far as the author knows. So:

- With the two different keys (the normal case), a person who moves from Obtainium to F-Droid or back **uninstalls the app
  first and installs it again**. The app's data on the phone goes with it; export what you want to keep before you switch.
- If F-Droid builds the app reproducibly and publishes the author's signature, an update in place across the two sources would
  work. Nobody has shown a bit-identical build of this stack (Rust, Zig, Tauri, Gradle). It is a goal, not a promise.

Pick one source and stay with it.

## The limit: Google developer verification

Google's developer verification binds a package name and a signing key to a developer account.

- A full distribution account costs $25 once. That is not paid. A limited account is free, needs no ID and covers up to 20
  devices (<https://developer.android.com/developer-verification/guides/faq>, updated 2026-07-15, read 2026-11-05).
- Enforcement began on 2026-09-30 only for participating stores in Brazil, Indonesia, Singapore and Thailand. It is planned
  to be global in 2027 (<https://developer.android.com/developer-verification>, read 2026-11-05).
- Without registration, from the global rollout the app installs only through the **advanced flow** (a switch in the developer
  options, a one-time wait of 24 hours, then "Install Anyway") or through **ADB**. An update of an unregistered app can fail
  where the advanced flow is off. Sideloaded apps and apps from other stores are not affected yet.

This applies to Obtainium and F-Droid alike, in Switzerland too. The project goes ahead with this limit and says it on every
Android page. Nobody spends money to remove it.

## Using Obtainium

When the app exists: add the repository address `https://github.com/holzcloud/holzWave` in Obtainium (or open
`obtainium://add/https://github.com/holzcloud/holzWave`).

- Every `0.0.x` release is a GitHub pre-release, so turn on Obtainium's option to include pre-releases for this app (the name
  and the default of the option are as far as the author knows; the wiki page for GitHub sources could not be read).
- The desktop packages sit on the same releases page. Set Obtainium's APK file-name filter so that it matches only the Android
  file (a regular expression on `.apk`), otherwise it may offer a file that is not for your phone.

## F-Droid has no beta track

F-Droid has no beta channel and no pre-release flag. A tag that its recipe matches is what people get. So the recipe will
match only tags that the author means to offer, and the listing text says it is a beta.

## Check the key

The SHA-256 fingerprint of the release key's certificate will be published here **when the key exists**. It does not exist
yet, so there is no fingerprint on this page. To compare it with an APK, run:

```
apksigner verify --print-certs holzWave.apk
```

and read the line `Signer #1 certificate SHA-256 digest`. It must equal the published value. (`apksigner` comes with the
Android SDK build tools.) A matching fingerprint says the file was signed with that key; it does not say the code is safe.

## What the Android build contains

- **Cloud AI providers are in the build.** Anthropic, OpenAI and Gemini are part of every Android build, F-Droid's included;
  there is no build flavour without them. F-Droid labels an app that "promotes or fully depends on a proprietary network
  service" with the anti-feature **NonFreeNet**, and the F-Droid listing will carry it. The label is about those services
  being available, not about the app depending on them: the AI chat also works with local servers and gateways, and a
  terminal and SSH need none of it. Nothing leaves the phone to an AI provider unless the person shares a pane and sends it.
- **One processor family: arm64-v8a.** Only arm64-v8a is built. The APK installs on 64-bit ARM phones and tablets with
  Android 12 or newer. It does not install on devices that are 32-bit ARM only, and not on x86 devices; the terminal engine
  cannot be built for those targets either.
- **Permissions: exactly six.** No other permission, no Google services and no tracker.
  - `INTERNET`: SSH connections, sync and the requests of the AI chat.
  - `FOREGROUND_SERVICE` and `FOREGROUND_SERVICE_SPECIAL_USE`: a foreground service of type `specialUse` keeps shells and SSH
    connections running while the app is in the background, with a persistent notification. Its subtype text is: `Keeps
    terminal shells and SSH connections running while the app is in the background`.
  - `POST_NOTIFICATIONS`: that notification, on Android 13 and later.
  - `USE_BIOMETRIC`: unlocking the secret store with the screen lock or a biometric.
  - `ACCESS_LOCAL_NETWORK`: reaching a device on the local network (a NAS, a home server, another computer on the same Wi-Fi) on Android 17
    and later, where an app that targets it needs this permission and a connection without it times out. It is a runtime permission:
    the system's own dialog (worded as access to nearby devices) shows when the first connection to a private address is made, never
    at the start, and the app works without it for everything that is not on the local network. It does not exist below Android 17.
- **Version code.** Android needs a number that goes up with every build. It is
  `major * 10000000 + minor * 100000 + patch * 1000 + stage * 100 + number`, with the stage digit alpha 1, beta 2, rc 3 and
  final 9. For example `0.0.1-beta2` gives 1202. The script `scripts/android-version-code.sh` computes it, and every later
  build has a higher code.
- **A prebuilt component.** Certificate checks on Android use the system's trust store through the platform verifier library.
  Its Kotlin part is a prebuilt component that the build downloads from the verifier project's Maven archive on GitHub (not
  Maven Central and not Google's repository), and it is not built from this repository's source. Whether F-Droid accepts that
  is not known; the author has asked or will ask its maintainers. If F-Droid refuses, the fallback is to rebuild the
  component from source.
- **Developer name.** `holzcloud.ch`, the same string as the certificate subject and the F-Droid author.

## The listing text

The folder `packaging/android/fastlane/metadata/android/en-US/` holds a draft of the F-Droid texts (`title.txt`,
`short_description.txt`, `full_description.txt`). It is a draft: the files cannot carry a header, because F-Droid shows their
text. They move to `fastlane/` at the repository root together with the build recipe when the app exists. There are no
screenshots, no icon and no changelog yet; they come with the app, and they show only what is built.

## What is not covered

- No Android device or emulator run of anything. The app does not exist.
- No APK, no release script for it and no F-Droid recipe.
- Google Play is not used and not planned. iOS is out.
- The release key is not made yet, so no APK can be signed.
