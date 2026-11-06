<div align="center">

<img src="assets/brand/banner.png" alt="holzWave, a terminal emulator for every screen" width="100%">

<br>

[![Status](https://img.shields.io/badge/status-0.0.1--beta2%20in%20development-FF8FB8?style=for-the-badge)](docs/roadmap.md)
[![License](https://img.shields.io/github/license/holzcloud/holzwave?style=for-the-badge&color=4B5563)](LICENSE)
[![Rust](https://img.shields.io/badge/core-Rust-8D3F63?style=for-the-badge&logo=rust&logoColor=white)](docs/architecture.md)

</div>

**holzWave** is an open-source terminal emulator written from scratch in Rust, built on Tauri 3 and
[libghostty](https://github.com/ghostty-org/ghostty). It aims to run on macOS, Windows, Linux, Android and in a browser with one
look: tiled panes, tabs and workspaces, a WebGL2 renderer, SSH that reconnects, settings that sync through your own
Nextcloud, and an AI chat that sees your terminal only after you share that pane. On a Mac with Apple Intelligence the AI runs
**completely offline**, on the device, with no account and no key. Website: [holzcloud.ch/holzwave](https://holzcloud.ch/holzwave).

> [!WARNING]
> **Beta preview.** holzWave is at `0.0.1-beta2`: a preview build. The Windows installer and the macOS app are unsigned (no Authenticode, no
> Developer ID, no notarisation). The Linux packages can carry a minisign signature, but none of the releases made so far carries one
> ([what is signed and what is not](docs/signing.md)).
> The author has run it on macOS (Apple Silicon) and Windows. The Linux packages of `0.0.1-beta2` were built on a GitHub runner
> and were not started on a Linux desktop by the author for this release (earlier alphas and betas were run on Linux). The window in the images below is the
> design for the release; the table [What works today](#what-works-today) says what is built and what is tested.

## Install (macOS, Apple Silicon)

Two Homebrew channels from the casks in this repository:

- **Preview** (`holzwave@preview`): the newest alpha, beta or plain release.
- **Stable** (`holzwave`): the newest plain release, from `0.0.1` on. There is none yet.

```sh
brew tap holzcloud/holzwave https://github.com/holzcloud/holzWave
brew trust --cask holzcloud/holzwave/holzwave@preview
brew install --cask holzcloud/holzwave/holzwave@preview
```

`brew trust` is needed once per cask. Or take the DMG from the [releases](https://github.com/holzcloud/holzWave/releases)
(right-click, Open, once). The cask removes the quarantine attribute so the unsigned app opens.

## Install (Windows and Linux)

Every [release](https://github.com/holzcloud/holzWave/releases) carries the packages, each with a `.sha256` file:

- **Windows (x64):** `holzWave_<version>_x64-setup.exe`, per user, unsigned (SmartScreen: More info, Run anyway).
- **Linux (x86_64):** `.deb`, `.rpm` and `.AppImage`; they need glibc 2.39 or newer and WebKitGTK 4.1 (Ubuntu 24.04+,
  Debian 13+, Fedora 40+).
- **Check a Linux package:** a release that carries `.minisig` files can be checked with
  `minisign -Vm <file> -p holzwave.pub` and `sha256sum -c SHA256SUMS --ignore-missing`, or with `sh scripts/verify-release.sh <directory>`;
  the steps and the public key are in [docs/signing.md](docs/signing.md). No release has been signed with the real key yet, and nobody has
  checked a signature on a real distribution; the scripts were tested with stand-ins and once with a throwaway key in a container.

The Windows installer is built on a Mac as a GNU-ABI cross build (not the MSVC build) and was run by the author. The Linux packages of
`0.0.1-beta2` were built once on a GitHub runner (Ubuntu 24.04, amd64, in the repository's build container; a manual workflow) because Zig 0.17
crashes in the Mac's emulated container. They were not started on a Linux desktop for this release. Reports of what breaks are the point of the preview.

## Install (server, Docker or Kubernetes)

The web build runs holzWave in a browser from one container: people sign in, and each person's terminals run as a user of their
own on the server. There is no published image; you build it from this repository and run it with Docker Compose or with the
Helm chart in `packaging/helm/holzwave`. The steps, the three TLS modes, single sign-on, backup and the sentences the server
prints when it refuses to start are in the [Server guide](docs/server.md). Built and tested against a test identity provider, virtual authenticators, the container tests and a Pebble certificate authority on the author's machines; not yet run against a real cluster, a real identity provider, hardware authenticators or real networks. A restart ends every pane, and the rows that
still have to be run are in Part 10 of the [test plan](docs/test-plan.md).

> [!IMPORTANT]
> holzWave is inspired by [Wave Terminal](https://github.com/wavetermdev/waveterm), whose idea of the shell as one block
> among others is the UX reference. holzWave started as a fork and is now a separate project that **contains none of
> Wave's code**. It is independent and not affiliated with or endorsed by the Wave project. If you want a finished
> terminal of that kind today, use Wave and consider [sponsoring it](https://github.com/sponsors/wavetermdev).

## Design

| Workspaces, tabs and tiled panes | Phone: one pane per screen |
|---|---|
| ![holzWave window with workspaces, tabs, tiled panes and a sidebar](docs/design/6-desktop.png) | <img src="docs/design/7-phone.png" width="360" alt="holzWave on a phone: pane, workspace sheet and key row"> |

| Command palette | Shortcuts | Previews |
|---|---|---|
| ![Command palette](docs/design/palette.png) | ![Key binding editor](docs/design/shortcuts.png) | ![File browser with image, PDF and video panes](docs/design/previews.png) |

| macOS | Windows | Linux |
|---|---|---|
| ![holzWave on macOS](docs/design/platforms/macos.png) | ![holzWave on Windows](docs/design/platforms/windows.png) | ![holzWave on Linux](docs/design/platforms/linux.png) |

<table>
<tr><th>AI chat in a pane</th><th>Sharing a pane</th><th>Approving a tool call</th></tr>
<tr>
<td><img src="docs/design/ai-chat-pane.png" alt="AI chat in a pane beside the shell"></td>
<td><img src="docs/design/ai-consent-ask.png" alt="The card that asks before a pane is shared with the model"></td>
<td><img src="docs/design/ai-approval.png" alt="The approval card of a tool call with its fields and hash"></td>
</tr>
<tr><th><code>wsh ai</code> request</th><th>Chats in the sidebar</th><th>Changing a file</th></tr>
<tr>
<td><img src="docs/design/ai-wsh.png" alt="A wsh ai request waiting for an answer in the window"></td>
<td><img src="docs/design/ai-sidebar-list.png" alt="The list of stored chats in the sidebar column"></td>
<td><img src="docs/design/ai-write.png" alt="The diff card of a file change with its Undo button"></td>
</tr>
</table>

<table>
<tr><th>A Kubernetes pods block</th><th>Block states</th><th>The Context section</th></tr>
<tr>
<td><img src="docs/design/integration-block.png" alt="A Kubernetes pods block beside its shell, marked as production"></td>
<td><img src="docs/design/integration-states.png" alt="The block in four states: reading, the tool not found with a path field, an error with the last rows dimmed, and a table that was cut"></td>
<td><img src="docs/design/integration-context.png" alt="The Context section of the sidebar with one row for each of the four tools"></td>
</tr>
<tr><th>Confirming an action</th><th>Card states</th><th>Integration profiles</th></tr>
<tr>
<td><img src="docs/design/integration-confirm.png" alt="The card for deleting a pod in a production context, armed for the second press"></td>
<td><img src="docs/design/integration-confirm-states.png" alt="Four cards: a change of the tool's own configuration, an exec session, an allowed card running, and one that is no longer waiting"></td>
<td><img src="docs/design/integration-profiles.png" alt="The dialog of integration profiles with a production profile chosen"></td>
</tr>
</table>

| The browser build: sign in | First administrator | Invitation |
|---|---|---|
| ![The sign-in page of the server: the passkey button, a single sign-on button, and the password form](docs/design/web-signin.png) | ![The first-administrator page: the setup token, a name, a display name and the sign-in method](docs/design/web-first-admin.png) | ![The invitation page: the key of the authenticator app in groups of four and the recovery code step](docs/design/web-invite.png) |

| Unlock the saved passwords | Account | People |
|---|---|---|
| ![The unlock dialog with the passphrase and the recovery code, each with Not now and Unlock](docs/design/web-unlock.png) | ![The account dialog with the layer that asks to confirm it is you](docs/design/web-account.png) | ![The people dialog: a table of accounts with their state, a row menu and the invitation link](docs/design/web-people.png) |

| The window in a browser | The web as a platform |
|---|---|
| ![The window as a browser shows it: no window buttons, status line marks, the upload target and the Download row menu](docs/design/web-window.png) | ![The holzWave window inside a browser tab](docs/design/platforms/web.png) |

| Links at rest and under the pointer | The question before a link opens | Pictures in a pane |
|---|---|---|
| ![Terminal text with two dotted links at rest, one solid under the pointer with its address in the status line and a notice beside it that says why a click could not open it, one in rose under the open gesture, one underlined by the program, and an address with a scheme that is not allowed left as plain text](docs/design/links.png) | ![The dialog Open this link? that shows the text of a link and the address it goes to, with Cancel focused and Open](docs/design/link-ask.png) | ![A pane with pictures below the cell backgrounds, between the backgrounds and the text and above the text, pictures drawn by placeholder characters, a cursor and a selection over a picture, and pictures clipped at the pane edges](docs/design/images.png) |

The same window is drawn on every platform; only the OS window buttons and the keyboard shortcuts differ. All views and
the design rules are in [docs/design.md](docs/design.md).

## What works today

Built and tested by the author on macOS and Windows, and on Linux up to `0.0.1-beta1` (the `0.0.1-beta2` Linux packages were not started):

| Feature | Status |
|---|---|
| Local shell in a pane, resize, wide Unicode | built |
| Tiled panes with draggable dividers, tabs, workspaces | built |
| Scrollback, selection, copy, paste | built |
| Layout saved and restored | built |
| WebGL2 renderer with Canvas 2D fallback | built |
| SSH (reconnect, jump host, keychain), WSL and Docker panes, connection manager | built, tested against a test server, not yet against real hosts |
| File browser, previews (Markdown, text, image, video, audio, PDF) and code editor, local and over SFTP | built, tested against a test server, not yet against real hosts |
| Encrypted Nextcloud/WebDAV sync (XChaCha20-Poly1305, Argon2id) | built, tested against a test server, not yet against a real Nextcloud |
| `wsh` CLI (`view`, `edit`, `setmeta title`, `ai`, the history hooks) over a private local socket | built; the macOS, Windows and Linux packages made by the release scripts carry it beside the app (not the Flatpak) |
| `wsh` on a Linux host over SSH (`view`, `edit`, `setmeta title` and `ping`; never the AI or the history) | built, tested against a test server and a real OpenSSH on loopback, not yet on a real Linux host. It needs three things: tick `Use wsh on this host` for the saved connection, choose `Install wsh on this host` in its pane (it uploads the bundled build to `~/.holzwave/bin`), and a server that allows Unix socket forwarding |
| Sidebar with hosts, history search and widgets (described by a JSON Schema), a `^R` binding for the history | built, tested against test servers |
| System monitor of this computer and of Linux hosts over SSH | built, tested against a test server, not yet against real hosts |
| AI chat in a pane, the sidebar, the palette and `wsh ai`: Anthropic, OpenAI, Gemini, local servers and gateways; you share a pane before its text leaves, and every tool action asks first | built, tested against test servers, not yet against real providers |
| **Fully offline AI on a Mac:** the Apple on-device model (macOS 26 or later with Apple Intelligence) answers on the Mac itself, with no account, no key and nothing leaving the computer | built |
| Kubernetes, Talos, Azure CLI and GAM7 as blocks (your own `kubectl`, `talosctl`, `az` and GAM7 on this computer), a Context section with a status for each, a card with the exact command and a second press on a production context, a profiles dialog, palette entries | built, tested against fake tools and recorded fixtures, not yet against real clusters or accounts or on Windows and Linux hardware; the Flatpak build does not run them and the mobile apps do not have them |
| **The web build:** holzWave in a browser from one container, with sign-in by passkey, password and authenticator code or single sign-on, one user of its own for every person, and the Compose and Helm files | built, tested against a test identity provider, virtual authenticators, the container tests and a Pebble certificate authority, not yet against a real cluster, a real identity provider, hardware authenticators or real networks; Linux container only, one replica, a restart ends every pane ([Server guide](docs/server.md), Part 10 of the test plan) |
| Pictures (the Kitty graphics protocol, all three levels) and hyperlinks (OSC 8) in a pane: a link the page treats as one is dotted at rest and shows its address in the status line, a link it refuses has no dots, a click it refuses is answered in words, and a link whose text names another place asks first; only the schemes on your list open, and the page keeps its picture memory within your setting for each pane and four times it for all panes | built, tested against the terminal engine, a real server in WebKit and Chromium and a page with a mock core; not tried with a real program that sends pictures or links, not by an automated test of the desktop window, not with a real SSH pane or a real opener of Linux or Windows, and not on Windows |
| A recorded performance baseline and a guard (`sh scripts/perf-check.sh`) for the core, the server and the page in two browsers | built, recorded on the author's Mac and in a Linux container, judged by the recorded numbers and no fixed target; Windows is not measured and the desktop window's key to glass is not measured |
| Android | planned; iOS is not planned (no Apple developer account) |

Per-phase detail: [docs/status.md](docs/status.md).

## Built with AI help

holzWave is built with the help of an AI coding assistant: Claude, made by Anthropic, working inside a planning and review
workflow under the direction of the owner. The code (Rust, TypeScript and the shell and PowerShell scripts), the tests, the
documentation and the packaging files were written with that help. The numbers come from the repository's own history: on
2026-11-06 it held 1,053 commits since 2026-10-10, and 1,051 of them carry a `Co-Authored-By: Claude` trailer. The two that
do not are a merge commit and a work-in-progress commit. The commits are made under the owner's own identity.

The owner reads the results, runs the tests, tries the app on the machines named above and decides what ships. AI-written
work can hold mistakes that the tests do not find, which is why every page here says what was not run.

The Flathub manifest, its metainfo text and the submission are written and made by the owner by hand, as Flathub's policy
asks. The files under `packaging/flatpak/` are references for the owner to read, not a submission.

## Build from source

macOS on Apple Silicon, Windows and Linux are the verified platforms; the steps below are for macOS, the others are in
[docs/build-windows.md](docs/build-windows.md) and [docs/build-linux.md](docs/build-linux.md). You need Rust, Node and **Zig 0.17.0 exactly**, because
the pinned `libghostty-vt` (Ghostty's `build.zig.zon` says `minimum_zig_version = "0.17.0"`) is built with that Zig. On a Mac the Swift toolchain (Xcode or the Command Line Tools) is also needed
for the bridge of the on-device model, and the model itself needs the Xcode 26 SDK; with an older SDK the app builds without
it.

```sh
brew install rustup zig@0.17 node
export PATH="/opt/homebrew/opt/zig@0.17/bin:$PATH"
git clone https://github.com/holzcloud/holzwave && cd holzwave
npm --prefix app ci && npm --prefix app run build
cargo run -p holzwave-app
```

More in [docs/building.md](docs/building.md).

## Roadmap

Phase 1 (spike) is done, phase 2 (desktop MVP, including the Windows and Linux versions, which the author has run up to `0.0.1-beta1`) is
built, and phase 3 (SSH, WSL, Docker, connection manager), phase 4 (file browser, previews, editor), the encrypted sync
and the `wsh` CLI are built and tested against test servers. The system monitor (local and over SSH), the history search
and the sidebar with widgets are built too, and so are the AI chat and the on-device model, whose manual checks wait for
real accounts and Macs (Part 7 of the [test plan](docs/test-plan.md)). The native integrations (Kubernetes, Talos, Azure
CLI, GAM7) are built and tested against fake tools and recorded fixtures; their rows for real clusters and accounts and for
Windows and Linux hardware are open (Part 9 of the test plan). The web build (a server in one container, with the same window in a browser) is built and tested in containers; its rows for a real cluster, identity provider, authenticators, certificate authority, proxy and browsers are open (Part 10 of the test plan). Mobile comes next.
Pictures and hyperlinks in the terminal, a recorded performance baseline with a guard, and the signing of the Linux packages by minisign are built too: the first two are tested against the engine, a real server and two browsers and not against real programs, and no release has been signed yet (Windows and macOS stay unsigned); what nobody has tried is listed in Part 11 of the test plan.
Versions go `0.0.1-alpha3`, `0.0.1-beta1`, `0.0.1-beta2` (now), `0.0.1`. `0.0.1-beta2` added the native integrations, the web build and
libghostty on Ghostty 1.4 (Zig 0.17) with a fix for the scrollback limit.
Full plan: [docs/roadmap.md](docs/roadmap.md).

## Documentation

[Architecture](docs/architecture.md) · [Design](docs/design.md) · [Roadmap](docs/roadmap.md) ·
[Status and stack](docs/status.md) · [Building](docs/building.md) · [Sync format](docs/sync.md) ·
[Configuration files](docs/config.md) · [Server guide](docs/server.md) · [Test plan](docs/test-plan.md) ·
[Distribution channels](docs/distribution.md) · [Privacy statement](PRIVACY.md) ·
[Profile schema](docs/integrations.schema.json) · [View schema](docs/integration-view.schema.json)

Questions and bug reports go to [GitHub Issues](https://github.com/holzcloud/holzWave/issues).

## License and credits

GPL-3.0, see [LICENSE](LICENSE).

- [Wave Terminal](https://github.com/wavetermdev/waveterm) for the idea and the interaction model. Please consider
  [sponsoring Wave](https://github.com/sponsors/wavetermdev).
- [Ghostty](https://github.com/ghostty-org/ghostty) for `libghostty-vt`, and
  [libghostty-rs](https://github.com/Uzaaft/libghostty-rs) for the Rust bindings.
- [Plus Jakarta Sans](https://fonts.google.com/specimen/Plus+Jakarta+Sans), [Manrope](https://fonts.google.com/specimen/Manrope)
  and [JetBrains Mono](https://www.jetbrains.com/lp/mono/) (SIL Open Font License 1.1).

Part of the [holzcloud](https://holzcloud.ch) family: [holzBar](https://github.com/holzcloud/holzBar) ·
[holzkube-manager](https://github.com/holzcloud/holzkube-manager) ·
[holzcloud-cms](https://github.com/holzcloud/holzcloud-cms).
