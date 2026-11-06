# Homebrew on macOS

> [!IMPORTANT]
> **Status.** The own tap exists and its preview cask is at `0.0.1-beta2`. It is not in the official Homebrew
> cask repository, and it cannot be today (see [Why the official repository cannot take the app](#why-the-official-repository-cannot-take-the-app)).
> The app is signed ad hoc only and is **not notarized** by Apple, and the cask removes the quarantine attribute, which
> bypasses a macOS protection. Every release so far is a pre-release. The list of what has not been tried is at the end
> ([Not covered](#not-covered)).

## What exists

The tap is this repository (`holzcloud/holzWave`). There is no separate tap repository.

| File | What it is |
|---|---|
| `Casks/holzwave@preview.rb` | The preview channel: the newest alpha, beta or plain release. It follows every release. |
| `Casks/holzwave.rb` | The stable channel. `scripts/release-macos.sh` writes it only for a plain version (no hyphen), so it does not exist yet. |

The script writes both files with the version and the SHA-256 of the DMG of that release; the files are committed by hand
after a release. The cask text, including the caveats, is the script's template, and the committed preview cask is the same
text.

```sh
brew tap holzcloud/holzwave https://github.com/holzcloud/holzWave
brew trust --cask holzcloud/holzwave/holzwave@preview
brew install --cask holzcloud/holzwave/holzwave@preview

brew upgrade --cask holzcloud/holzwave/holzwave@preview
brew uninstall --cask holzcloud/holzwave/holzwave@preview
```

Homebrew 6 asks for an explicit trust (`brew trust`) of a cask in a tap that is not official, because the cask is
Ruby that runs on your Mac. The tap address has to be given in full once because, as far as the author knows, Homebrew
only finds a tap by name when the repository is called `homebrew-<something>`.

The tap clones the whole application repository: 2,034 files and about 160 MB on 2026-11-05 on the author's machine. A
dedicated small tap repository (`holzcloud/homebrew-holzwave`) would avoid that and would let people run `brew tap
holzcloud/holzwave` without an address. It is possible later and is not planned.

## What the cask does to your Mac

- The app is signed ad hoc only. The signature is the one the build makes for itself; it names no developer, and Gatekeeper
  does not accept it as one. The app is **not notarized** by Apple. See [signing.md](signing.md).
- After the install the cask runs `xattr -dr com.apple.quarantine` on `holzWave.app`, so that macOS opens the app without the
  Gatekeeper warning. That is a bypass of a macOS protection. The cask says so in its `caveats` text, Homebrew prints it
  after the install, and the release notes and the README say it too.
- If you do not accept that, do not use the cask. Take the DMG from the
  [releases](https://github.com/holzcloud/holzWave/releases), drag the app to Applications, and right-click and Open once.
- Who publishes it: holzcloud.ch. The DMG comes from the release assets of this repository, and the cask pins its SHA-256
  for each release.

## Why the official repository cannot take the app

The official cask repository is `Homebrew/homebrew-cask`. Three blockers stand in the way; only the first costs money. The
author read the pages named here on 2026-11-05.

1. **The Gatekeeper rule.** Homebrew's page on acceptable casks says that a macOS app that Gatekeeper can assess "must pass
   Homebrew's Gatekeeper checks and must not require System Integrity Protection or Gatekeeper to be disabled or bypassed"
   (<https://docs.brew.sh/Acceptable-Casks>). Support for casks that fail Gatekeeper checks ended on 2026-09-01
   (<https://github.com/Homebrew/brew/issues/20755>), and a maintainer wrote in September 2026 that unsigned casks have not
   been accepted for a while (<https://github.com/orgs/Homebrew/discussions/7050>). Passing the check needs a Developer ID
   signature and Apple notarization, which need the paid Apple Developer Program (about $99 a year, as far as the author
   knows). This project takes no money for distribution, so that is out. The quarantine-removal step of this cask is exactly
   the bypass that the rule names.
2. **Notability.** A new package needs at least 30 forks, 30 watchers or 75 stars, and a submission by the owner of the
   repository needs at least 90 forks, 90 watchers or 225 stars; a repository younger than 30 days is normally not eligible
   (<https://docs.brew.sh/Package-Acceptance-Policy>). On 2026-11-05 this repository had 2 stars, 0 forks and 0 watchers,
   and it was created on 2026-10-10.
3. **A GitHub pre-release.** `brew audit` flags a version that is a GitHub pre-release, and every `0.0.x` release of this
   project is one, so the audit fails until a plain release exists (from `0.1.0` on).

Not a blocker, only a note: official casks must use the structured `*_steps` stanzas instead of legacy flight blocks
(<https://docs.brew.sh/Cask-Cookbook>), and this cask already does (`postflight_steps`).

No pull request to `Homebrew/homebrew-cask` is opened, and none is planned. As far as the author knows the process would be
a fork, `brew create --cask`, `brew audit --new --cask`, `brew style` and a pull request that the reviewers' bots audit again;
the author has not tried it.

## The audit the author ran

On 2026-11-05 with Homebrew 7.0.12, `brew audit --new --cask holzcloud/holzwave/holzwave@preview` reported "3 problems in 1
cask detected":

| Message | What it means |
|---|---|
| `v0.0.1-beta2 is a GitHub pre-release.` | The release is marked as a pre-release on GitHub, as every `0.0.x` release is. This is blocker 3. |
| `Signature verification failed for holzWave.app: ... the software has been altered.` | The scan could not verify a developer signature, because there is none: an ad hoc signature is all the app has. This is the Gatekeeper side of blocker 1. |
| `GitHub repository not notable enough (<30 forks, <30 watchers and <75 stars)` | The repository is too small and too young for the policy. This is blocker 2. |

The audit printed no complaint about the quarantine step itself. A maintainer's proposal in the issue linked above would make
an `xattr` quarantine check a warning for third-party taps and fatal for the official repository, so a warning may come for
this tap later; as far as the author knows that would not block it.

The audit of the caveats text added after that run has not been made: `brew audit` reads the tap as it is pushed, so the
first audit of the new text comes after the owner pushes it. That is a step for the owner and not a condition of the release.

## What the owner loses by staying on the own tap, and what stays

| Loses | Keeps |
|---|---|
| `brew install --cask holzwave` without a tap step | The same install for people who run three commands |
| A listing on formulae.brew.sh and in `brew search` | Control of the version and of the timing |
| Homebrew's autobump and its CI | The preview channel `holzwave@preview` |
| The trust of an official tap (no `brew trust` step) | No money and no Apple account |
| People whose organisation excludes third-party taps | |

What would unlock the official repository, not planned: Apple notarization (the only item that costs money), a plain
release from `0.1.0`, the star, fork or watcher counts above, and a repository older than 30 days.

## Not covered

- **Intel Macs** are not served. The cask depends on `arm64`, and a cross build for Intel has not been tried.
- **macOS below 13** (Ventura) is refused by the cask.
- **A clean install with the new caveats text** has not been made by anyone. The caveats are pinned by a text test and the
  cask is checked with `ruby -c`; nobody has watched Homebrew print them on a machine that never had the tap.
- **How Gatekeeper treats the app on a new macOS major release** is not known in advance.
- **The audit of the pushed change** (see above) has not been made.

Every release is a beta until the [README](../README.md) says otherwise.
