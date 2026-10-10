<div align="center">

<img src="assets/brand/banner.png" alt="holzWave — a terminal for every screen" width="100%">

<br>

[![Status](https://img.shields.io/badge/status-0.0.1--alpha1%20in%20development-FF8FB8?style=for-the-badge)](docs/roadmap.md)
[![Platforms](https://img.shields.io/badge/macOS%20·%20Windows%20·%20Linux%20·%20iOS%20·%20Android-planned-111827?style=for-the-badge)](docs/roadmap.md#platforms)
[![Rust](https://img.shields.io/badge/core-Rust-8D3F63?style=for-the-badge&logo=rust&logoColor=white)](docs/architecture.md)
[![Tauri](https://img.shields.io/badge/shell-Tauri%202-8D3F63?style=for-the-badge&logo=tauri&logoColor=white)](docs/architecture.md)
[![License](https://img.shields.io/github/license/holzcloud/holzWave?style=for-the-badge&color=4B5563)](LICENSE)
[![Website](https://img.shields.io/badge/website-holzcloud.ch-8D3F63?style=for-the-badge)](https://holzcloud.ch)
[![Sponsor](https://img.shields.io/badge/sponsor-%E2%99%A5-EA4AAA?style=for-the-badge&logo=githubsponsors&logoColor=white)](https://github.com/sponsors/holzcloud)

**holzWave** is a terminal emulator built from scratch in Rust, for macOS, Windows, Linux, iOS and Android.<br>
Tiling blocks, SSH that reconnects, previews next to the shell, and settings that sync through your own Nextcloud.

[Roadmap](docs/roadmap.md) · [Architecture](docs/architecture.md) · [Sync format](docs/sync.md) · [Credits](#-credits)

</div>

---

> [!WARNING]
> **There is no release and no download yet.** holzWave is at `0.0.1-alpha1`, in development. It builds and runs on
> macOS from source (`cargo run -p holzwave-app`): a shell in tiled panes with tabs and workspaces, scrollback,
> selection and a saved layout, drawn by a WebGL2 renderer. The window itself has been built and started but not yet
> checked by hand on every point. Everything in the feature list below beyond that is *planned*, and order and scope
> will change.

## What it is going to be

A modern, efficient terminal in the spirit of [Wave Terminal](https://github.com/wavetermdev/waveterm): the shell is
one block among others, and you arrange blocks the way you work. holzWave starts from an empty repository rather
than from a fork, so it can share one Rust core between desktop and mobile.

| | |
|---|---|
| 🧱 **Block tiling** | Terminals, previews and widgets in tiles you split and drag. Tabs, workspaces, themes and backgrounds. |
| 🔌 **SSH that reconnects** | An SSH manager, plus WSL and Docker as connection types on the desktop. One connection abstraction serves the terminal and the file previews. |
| 👀 **Previews** | A file browser, Markdown, images, video, PDF and a code editor that also edits remote files over SFTP. |
| ⌨️ **`wsh`** | A small CLI that controls the running app: `view`, `edit`, `setmeta`, `ai`. It works on remote hosts through an SSH tunnel. |
| 🔐 **Secrets in the OS keychain** | Passwords and keys never sit in a settings file. |
| ☁️ **Your own sync** | Settings, themes, widgets, connection profiles and layouts over WebDAV or Nextcloud, end-to-end encrypted, on every platform. |
| 🤖 **AI with consent** | A chat that can see terminal context only when you allow it. Local models, cloud providers, and Apple's on-device models on supported Macs. |
| ☸️ **Native integrations** | Kubernetes, Talos, Azure CLI and GAM7 as blocks, with the connection data in the secret store. Desktop only. |
| 📱 **Mobile** | iOS with SSH only (the sandbox has no local shell), Android with a local shell and SSH. One block per screen. |

Deliberately **not** included: a web browser block.

## How it is built

| Part | Choice |
|---|---|
| Core | One Rust crate, shared by every platform |
| Terminal engine | `libghostty-vt` behind a `TerminalEngine` trait, with `alacritty_terminal` as the fallback |
| PTY / SSH | `portable-pty` / `russh` |
| App shell | Tauri 3 (alpha) for desktop, iOS and Android |
| UI | TypeScript with React or Solid, decided in the spike |
| Rendering | ghostty compiled to WASM in the webview, or an own WebGL renderer, decided in the spike |
| Persistence | SQLite |

The details, and why the engine sits behind a trait, are in [docs/architecture.md](docs/architecture.md).

## 🗺️ Roadmap

| Phase | What |
|---|---|
| 1 | **Spike:** engine from Rust, PTY into a Tauri window, renderer decision, benchmarks |
| 2 | **Desktop MVP:** local shell, scrollback, tiling, tabs, workspaces, persisted layout |
| 3 / 3b | **Remote** and **Sync** |
| 4 – 5 | **Previews** and the **`wsh`** CLI |
| 6 / 6b | **Extras** and **native integrations** |
| 7 – 8 | **Mobile** and **polish** (Kitty graphics, packaging, signing) |

The full plan with the acceptance points of each phase is in [docs/roadmap.md](docs/roadmap.md).

## Relationship to Wave Terminal

holzWave was first started as a fork of Wave and is now a separate project that **contains none of Wave's code**.
Wave remains the UX reference. If code from Wave is taken over later, it is Apache-2.0 and the notices travel with
it. holzWave is independent and not affiliated with or endorsed by the Wave project. If you want a finished terminal
of this kind today, use [Wave](https://github.com/wavetermdev/waveterm) and consider
[sponsoring it](https://github.com/sponsors/wavetermdev).

## License

GPL-3.0, see [LICENSE](LICENSE).

## 🙏 Credits

The idea and much of the interaction model come from [Wave Terminal](https://github.com/wavetermdev/waveterm). The
terminal engine is planned to be [Ghostty's `libghostty-vt`](https://github.com/ghostty-org/ghostty).

Part of the [holzcloud](https://holzcloud.ch) family: [holzBar](https://github.com/holzcloud/holzBar) ·
[holzkube-manager](https://github.com/holzcloud/holzkube-manager) ·
[holzcloud-cms](https://github.com/holzcloud/holzcloud-cms).
