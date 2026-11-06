# Where holzWave can be had, and what each channel does and does not do

> [!IMPORTANT]
> **Status, as recorded on 2026-11-06.** holzWave is a beta (`0.0.1-beta2`, and every `0.0.x` after it). Two channels are
> `available`: the downloads on the GitHub releases page and the author's own Homebrew tap. Flathub is `not submitted`
> (a local Flatpak build recipe exists and was never started on a Linux desktop; it is a recipe, not a listing). The
> Microsoft Store is `on hold`. Obtainium and F-Droid are `not built`, because no Android app exists yet. Nothing was
> handed to Flathub, the Microsoft Store, F-Droid or any other listing, and nothing was run on a Linux desktop. Nothing
> here costs money; the page says where that limits what can be done. The list of what has not been tried is at the end
> ([Untested](#untested)).

**The status words.** Each channel has exactly one, in the first cell after its name, and the README names a channel as a
way to install holzWave only when its word is `available`:

| Word | Meaning |
|---|---|
| `not built` | The package for the channel does not exist |
| `not submitted` | A package or recipe exists locally; nothing was handed to the channel |
| `in review` | It was handed over and the channel's reviewers have not decided |
| `available` | A person can install holzWave through the channel today |
| `on hold` | Work on the channel is paused until the owner decides or acts |
| `refused` | The channel's reviewers said no |

This is the one page that every other text links to for the channels. The detail of each lives on the page the row links
to. A claim that came from a web page carries its address and the day it was read (**2026-11-05** unless another day is
given); a figure or a rule that was not checked says "as far as the author knows". Those pages change, so read the
current one before relying on a number here.

## The channels

| Channel | Status today | Cost | What the free path cannot do | How a beta is offered | How a person updates | What the channel does to the signature | Architectures built | Untested |
|---|---|---|---|---|---|---|---|---|
| **GitHub downloads** (Windows installer, macOS DMG, Linux deb, rpm and AppImage) | `available`. The [releases page](https://github.com/holzcloud/holzWave/releases): `0.0.1-beta2` (2026-11-01) is the newest, and every release so far is a GitHub pre-release (read with `gh release list` on 2026-11-06) | Nothing | No Authenticode certificate and no Apple notarization, so SmartScreen warns on the installer and macOS asks for one allowance on the app; the Linux packages can carry a minisign signature, but no release has been signed | Every `0.0.x` release is marked as a pre-release | Download the new file from the releases page and install it over the old one. The app does not tell you that a new file exists | None. The Windows installer is not signed, the macOS app is ad hoc signed only, the Linux packages are unsigned so far ([signing.md](signing.md)) | Windows x64, macOS Apple silicon, Linux x86_64. The Linux aarch64 packages have a build script and were only read back as aarch64 in a container; no Windows ARM64 installer is part of any release | The `0.0.1-beta2` Linux packages were not started on a Linux desktop; Windows ARM64 and Linux aarch64 were never run |
| **Homebrew, own tap** | `available`. The casks in this repository, which the README names as the macOS route. Not in the official Homebrew cask repository, which cannot take the app ([homebrew.md](homebrew.md)) | Nothing | No listing on formulae.brew.sh and no `brew search` hit; the person runs `brew tap` and `brew trust` once; the app is not notarized, so the cask removes the quarantine attribute, which bypasses a macOS protection | The preview cask `holzwave@preview` follows every alpha and beta; the stable cask `holzwave` is written only for a plain version and does not exist yet | `brew upgrade --cask holzcloud/holzwave/holzwave@preview` | Ad hoc signature, no developer named, not notarized | macOS on Apple silicon (macOS 13 or newer) | A clean-machine install of the new caveats; Intel Macs are not served |
| **Flathub** (Flatpak) | `not submitted`. Only a local build recipe exists under `packaging/flatpak/` ([flatpak.md](flatpak.md)) | Nothing on any Flathub page read; no fee is stated, as far as the author knows there is none | Nothing paid is missing. Review is by volunteers and has no deadline; the manifest must be written by the owner by hand (see [Flathub](#flathub)) | A `beta` branch builds into a separate `flathub-beta` remote. Whether a brand-new app may live only there is not stated, so the listing says beta in its text | `flatpak update` | Flathub builds the package itself and signs its own build; none of this project's signatures apply | Flathub builds x86_64 and aarch64 itself. Only an arm64 container build of the engine was run here; x86_64 was not | Never started on any machine or desktop; whether Flathub accepts it at all; the host shell inside the sandbox |
| **Microsoft Store** (MSIX) | `on hold`. The owner has not yet tried the publisher name `holzcloud.ch` in the free Store sign-up and no publisher name is decided (recorded 2026-11-06). No package was made or uploaded; the staging scripts for the package exist and were never run on a Windows machine | No registration fee for an individual account under the new onboarding flow; a personal Microsoft account, a government ID and a selfie are needed | The Store re-signs an MSIX, so no certificate is needed, but that holds for an MSIX only. Whether the sign-up works from Switzerland and accepts the publisher name is not known until the sign-up is done | Before the first publish a private audience (people the owner names); after it package flights; a public listing has no beta label, so the description says beta | The Store updates the package | The Store signs the package with a Microsoft certificate; the project's own signatures play no part | x64 first. An ARM64 package is not built | Not built or installed: whether the app starts as a packaged app, whether the terminal, the web view and the settings folder work inside the package, how certification treats a terminal |
| **Obtainium** (Android APK) | `not built`. The Android app does not exist yet; this channel waits for it | Nothing | The package cannot be registered with Google (see [Android](#android)): from the 2027 global rollout an unregistered app installs only through Android's advanced flow or ADB | Every `0.0.x` release is a GitHub pre-release; Obtainium shows pre-releases only if the person turns that option on for the app, as far as the author knows | Refresh the app in Obtainium; it fetches the APK from the GitHub release | The author's own key signs the APK; Obtainium installs it unchanged | arm64-v8a only, Android 12 or newer | Everything: no APK exists |
| **F-Droid** (Android) | `not built`. Nothing was sent to F-Droid; the channel waits for the Android app | Nothing in the pages read; no fee is stated, as far as the author knows there is none | F-Droid signs with its own key unless the build is reproducible, and nobody has shown a bit-identical build for this stack. The listing carries the anti-feature `NonFreeNet`, because the cloud AI providers are in every Android build | F-Droid has no beta channel and no pre-release flag; the recipe's tag pattern decides what it builds, so the listing says beta in its text | The F-Droid client offers the new version | F-Droid's key, or the author's if the build is reproducible. With two different keys Obtainium and F-Droid installs cannot update each other: uninstall before switching, and settings on that phone are lost unless exported | arm64-v8a only, Android 12 or newer | Everything: no APK exists and nothing was sent to F-Droid |

holzWave has no update check, and no build of it shows a notice that a new version exists (see [Updates](#updates)).
Whether a channel accepts the app is up to the people who run it; this page claims none of it.

## Flathub

- **The application id** is `ch.holzcloud.holzwave`. For an id of that form Flathub asks the owner to prove control of
  `holzcloud.ch`, through a file at `/.well-known/org.flathub.VerifiedApps.txt` on that domain or a DNS record
  (<https://docs.flathub.org/docs/for-app-authors/verification>, read 2026-11-05). Both are free, and both are the
  owner's to place. Neither is in place today.
- **Flathub's policy on AI.** Flathub's requirements say its manifests must not contain AI-generated or AI-assisted content
  and that AI tools or agents must not open or automate submission pull requests
  (<https://docs.flathub.org/docs/for-app-authors/requirements> and
  <https://docs.flathub.org/docs/for-app-authors/submission>, read 2026-11-05). holzWave is built with AI help. So the
  owner writes the Flathub manifest, the metainfo text and the pull request by hand, and publishes a statement that the
  project is built with AI help and which parts. The files under `packaging/flatpak/` are a reference for the owner to read,
  not a submission.
- **The host shell.** The Flatpak asks for the permission to talk to the Flatpak service, and an app setting (off by
  default) decides whether panes run on the host. Terminals already on Flathub ask for the same permission (Konsole,
  WezTerm, BlackBox, Ptyxis and Contour, read 2026-11-05); whether the reviewers accept it for this app is open, and the
  setting cannot grant a permission the manifest does not carry ([flatpak.md](flatpak.md)).
- **Maturity.** Flathub looks for real-world use and an ongoing commitment to maintain the app and takes stable
  software. A single-maintainer beta may be refused or asked to wait. Whether a beta can be submitted is the reviewers'
  decision.

## Microsoft Store

- **The account.** The new onboarding at <https://storedeveloper.microsoft.com> has no registration fee for an individual
  account (<https://learn.microsoft.com/en-us/windows/apps/publish/partner-center/open-a-developer-account>, page dated
  2026-10-29, read 2026-11-05). An individual account is meant for distribution outside a business or trade; whether that
  fits the project is the owner's call. Nothing is paid, and a step that asks for a card is a stop.
- **The signature.** The Store re-signs an MSIX with a Microsoft certificate; the page on package requirements says no
  CA-trusted code-signing certificate is needed for an MSIX submission
  (<https://learn.microsoft.com/en-us/windows/apps/publish/publish-your-app/msix/app-package-requirements>, read
  2026-11-05). The Store does not re-sign a plain EXE or MSI, which would need a certificate that costs money.
- **The version.** The Store refuses a package whose first version section is 0, so a `0.0.x` version cannot be used as it
  is. The owner chose the mapping on 2026-11-06: the Store version is `(major + 1).minor.(patch * 100 + stage).0`, where the
  stage is the alpha number for an alpha, 20 plus the number for a beta, 60 plus the number for a release candidate and 99 for
  a plain release, so `0.0.1-beta2` becomes `1.0.122.0` and `0.0.1` becomes `1.0.199.0`. It cannot be changed after the first
  upload.
- **The privacy page.** The Store needs a web address for the privacy statement. It will be the rendered `PRIVACY.md` of
  the repository at a release tag on github.com. The file [PRIVACY.md](../PRIVACY.md) is in the repository; it was written
  with AI help and the owner has not yet signed it off, and whether the Store accepts a rendered Markdown file as the page is
  not verified.
- **The AI chat.** The Store's policies ask that content generated live by an AI provider is disclosed in the listing
  (policy 11.16, <https://learn.microsoft.com/en-us/windows/apps/publish/store-policies>, version 7.20, read 2026-11-05).
- **The publisher name** is `holzcloud.ch` in every channel. Whether the Store's free individual sign-up accepts it is
  `decided at the Store sign-up`, and the owner has not run the sign-up yet (recorded 2026-11-06), which is why the channel is
  `on hold`. The name is part of the package family name and is hard to change later. If the sign-up refuses it, the Store
  shows the author's own name and the app stays named `holzWave`. No Store address exists: nothing was submitted.

## Android

- **Registration.** Google's developer verification asks apps to be registered with a developer account. A full
  distribution account costs $25; a limited account is free, needs no ID and covers up to 20 devices
  (<https://developer.android.com/developer-verification/guides/faq>, updated 2026-07-15, read 2026-11-05). Enforcement began
  on 2026-09-30 only for participating stores in Brazil, Indonesia, Singapore and Thailand and is planned to be global in
  2027 (<https://developer.android.com/developer-verification>, read 2026-11-05). Sideloaded apps and apps from other
  stores are not affected yet.
- **What the free path leaves.** The $25 account is not paid, so the package is not registered. From the global rollout an
  unregistered app installs only through the advanced flow (a switch in the developer options, a one-time wait of 24
  hours, then "Install Anyway") or through ADB, and an update of an unregistered app fails while the advanced flow is off,
  according to that FAQ. Normal users may therefore be unable to install the Android app from 2027, in Switzerland too.
  The owner accepted this limit on 2026-11-06, and every Android page repeats it.
- **Two signers.** Obtainium installs the author's signed APK. F-Droid builds from source and signs with its own key
  unless its build matches the author's APK bit for bit. Android does not update an app across two different signing
  certificates, as far as the author knows, so a person moving from one source to the other reinstalls first.
- **The app that does not exist.** The Android app is not built. These two channels, and the rows of the table above, wait
  for it. The package name is `ch.holzcloud.holzwave`, the developer name `holzcloud.ch`.

## macOS

The own Homebrew tap is the only Homebrew channel; why the official repository cannot take the app, and what that
costs, is on [homebrew.md](homebrew.md). The app is not notarized, so the DMG asks for one allowance on first start
(right-click, Open) and the cask removes the quarantine attribute.

## The developer name

The developer or publisher name is `holzcloud.ch` in every channel: Flathub's metainfo (tied to the verified domain), the
cask text, the Android signing certificate and the F-Droid author, the listings and the privacy statement. Where a
channel's free sign-up does not accept that name, the sentence of that channel above says which name it shows instead.
The Store outcome is not known today: `decided at the Store sign-up`, and the sign-up has not been run.

## Updates

holzWave has no update check and no updater of its own, and no build shows a notice that a new version exists. The owner
decided that on 2026-11-06. A notice would need a request to GitHub, and the privacy statement in
[config.md](config.md) says there is no update check; a test of the repository fails if an updater appears. A control the
person presses that opens the releases page was considered and is not planned. So a person updates through the channel:

| Channel | How |
|---|---|
| GitHub downloads | Download the new file from the releases page and install it |
| Homebrew own tap | `brew upgrade --cask holzcloud/holzwave/holzwave@preview` |
| Flathub | `flatpak update` |
| Microsoft Store | The Store's own update |
| Obtainium | Refresh the app in Obtainium |
| F-Droid | The F-Droid client |

## What would cost money, and is not planned

Three things cost money. None is paid, and none is planned. Each leaves a limit.

| Item | What it would buy | What the free path leaves |
|---|---|---|
| A code-signing certificate for the Windows installer (Authenticode) | A signed installer that SmartScreen trusts sooner | The NSIS installer stays unsigned and SmartScreen warns on the first run ([signing.md](signing.md)). The Store route needs no certificate, but it is a separate channel |
| Apple notarization (the Apple Developer Program, about $99 a year as far as the author knows) | An app that passes Gatekeeper | The macOS app stays ad hoc signed. The official Homebrew cask repository does not take an app that bypasses or fails Gatekeeper, so the app stays in the own tap ([homebrew.md](homebrew.md)) |
| Google's Android developer registration (a full account, $25) | A registered Android package | An unregistered package installs only through the advanced flow or ADB from the 2027 rollout; the limited free account covers up to 20 devices |

The channels that were found free, and the pages read for them, are in each section above. Google Play and the App Store
are not channels of this project.

## Untested

The builds and desktops below were not run. Where a result will be recorded is named for each; until a row holds
`pass` or `fail`, the texts say `untried` or `not run`.

- **Linux desktops:** GNOME, KDE Plasma on Wayland and on X11, Xfce, Cinnamon, MATE, Sway, Hyprland and i3. None was
  started. The rows `DSK1` to `DSK10` of Part 12 of [test-plan.md](test-plan.md) hold the checks. The dialogs are GTK also
  on KDE, because the web view is WebKitGTK.
- **Linux aarch64 on real hardware:** the packages were built in a native arm64 container on a Mac and read back as
  aarch64; none was started. Row `ARM1` of Part 12.
- **Windows ARM64:** an installer can be built by a script and was never run on any machine. Row `WAR1` of Part 12.
- **The Flatpak on any machine:** it was never installed or started. Row L10 of Part 4 of [test-plan.md](test-plan.md).
- **The MSIX on any Windows machine:** no package exists. Rows `MSX1` and `MSX2` of Part 12.
- **The AppImage starting in a container:** a container cannot start it, so no AppImage of the aarch64 set was started
  anywhere. Rows L6 and L11 of Part 4, and row `ARM1`.
- **The Store sign-up and a Store package:** the owner has not run the sign-up or built the package, so nothing is known
  about whether the Store accepts the publisher name, the app inside a package, or a terminal in certification.
- **Android:** nothing exists to run. No row exists yet; one is added with the Android build.
- **Signatures:** no release has been signed with a real key and nobody has checked one on a real distribution
  ([signing.md](signing.md)).

The build steps behind the rows are in [build-linux.md](build-linux.md) and [build-windows.md](build-windows.md); what the
app does today is in [status.md](status.md).
