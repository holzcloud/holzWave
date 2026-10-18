<div align="center">

<img src="assets/brand/banner.png" alt="holzWave, a terminal emulator for every screen" width="100%">

<br>

[![Status](https://img.shields.io/badge/status-0.0.1--alpha2%20in%20development-FF8FB8?style=for-the-badge)](docs/roadmap.md)
[![License](https://img.shields.io/github/license/holzcloud/holzwave?style=for-the-badge&color=4B5563)](LICENSE)
[![Rust](https://img.shields.io/badge/core-Rust-8D3F63?style=for-the-badge&logo=rust&logoColor=white)](docs/architecture.md)

</div>

**holzWave** is an open-source terminal emulator written from scratch in Rust, built on Tauri 3 and
[libghostty](https://github.com/ghostty-org/ghostty). It aims to run on macOS, Windows, Linux, iOS and Android with one
look: tiled panes, tabs and workspaces, a WebGL2 renderer, SSH that reconnects, settings that sync through your own
Nextcloud, and an AI chat that sees your terminal only after you share that pane. Website: [holzcloud.ch/holzwave](https://holzcloud.ch/holzwave).

> [!WARNING]
> **Alpha.** holzWave is at `0.0.1-alpha3`: a preview build, unsigned (no Developer ID, no notarisation, no Authenticode).
> The macOS build (Apple Silicon) is the one the author tests; the Windows and Linux packages are cross-built on a Mac and
> have not run on real machines yet. The window in the images below is the
> design for the release; the table [What works today](#what-works-today) says what is built.

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

The same window is drawn on every platform; only the OS window buttons and the keyboard shortcuts differ. All views and
the design rules are in [docs/design.md](docs/design.md).

## What works today

Built and tested on macOS from source:

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
| `wsh` CLI (`view`, `edit`, `setmeta title`, `ai`, the history hooks) over a private local socket | built; the macOS, Windows and Linux packages made by the release scripts carry it beside the app (not the Flatpak); no installed build has been run by the owner yet |
| `wsh` on a Linux host over SSH (`view`, `edit`, `setmeta title` and `ping`; never the AI or the history) | built, tested against a test server and a real OpenSSH on loopback, not yet on a real Linux host. It needs three things: tick `Use wsh on this host` for the saved connection, choose `Install wsh on this host` in its pane (it uploads the bundled build to `~/.holzwave/bin`), and a server that allows Unix socket forwarding |
| Sidebar with hosts, history search and widgets (described by a JSON Schema), a `^R` binding for the history | built, tested against test servers |
| System monitor of this computer and of Linux hosts over SSH | built, tested against a test server, not yet against real hosts |
| AI chat in a pane, the sidebar, the palette and `wsh ai`: Anthropic, OpenAI, Gemini, local servers and gateways; you share a pane before its text leaves, and every tool action asks first | built, tested against test servers, not yet against real providers |
| Apple on-device model (macOS 26 or later with Apple Intelligence) | built, not yet run on a real Mac |
| iOS, Android, Windows, Linux | planned |

Per-phase detail: [docs/status.md](docs/status.md).

## Build from source

macOS on Apple Silicon is the only verified platform. You need Rust, Node and **Zig 0.16.0 exactly**, because
`libghostty-vt` does not build with Zig 0.17. On a Mac the Swift toolchain (Xcode or the Command Line Tools) is also needed
for the bridge of the on-device model, and the model itself needs the Xcode 26 SDK; with an older SDK the app builds without
it.

```sh
brew install rustup zig@0.16 node
export PATH="/opt/homebrew/opt/zig@0.16/bin:$PATH"
git clone https://github.com/holzcloud/holzwave && cd holzwave
npm --prefix app ci && npm --prefix app run build
cargo run -p holzwave-app
```

More in [docs/building.md](docs/building.md).

## Roadmap

Phase 1 (spike) is done, phase 2 (desktop MVP) is built and waits for its manual checks, and phase 3 (SSH, WSL, Docker,
connection manager), phase 4 (file browser, previews, editor), the encrypted sync and the `wsh` CLI are built and
tested against test servers. The system monitor (local and over SSH), the history search and the sidebar with widgets are
built too, and so are the AI chat and the on-device model, whose manual checks wait for real accounts and Macs (Part 7 of
the [test plan](docs/test-plan.md)); the native integrations and mobile follow.
Versions go `0.0.1-alpha3`, `0.0.1-beta1`, `0.0.1`.
Full plan: [docs/roadmap.md](docs/roadmap.md).

## Documentation

[Architecture](docs/architecture.md) · [Design](docs/design.md) · [Roadmap](docs/roadmap.md) ·
[Status and stack](docs/status.md) · [Building](docs/building.md) · [Sync format](docs/sync.md) ·
[Configuration files](docs/config.md) · [Test plan](docs/test-plan.md)

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
