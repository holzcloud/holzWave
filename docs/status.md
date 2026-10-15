# Where holzWave stands

## Why holzWave

- **One core, five platforms.** Terminal, layout, connections and sync live in one Rust crate. The platform shells stay
  thin, so a feature is written once.
- **One look everywhere.** The chrome is drawn by holzWave and uses the same tokens on every platform: flat, mono,
  hairlines, rose where it matters.
- **Measured performance.** A WebGL2 renderer draws a 160x50 grid in about 1 ms, against 4.5 to 7.7 ms for Canvas 2D.
  The engine parses about 290 MiB/s.
- **Tiled blocks.** Panes, tabs and workspaces with draggable dividers and a layout that survives a restart. Previews
  and widgets come as blocks later.
- **SSH that reconnects.** A pane can run a shell on another machine over SSH (`russh`), with a jump host, host key
  checks, and a reconnect that keeps the pane and what you type. Secrets are in the OS keychain only. WSL and Docker
  connections open as local panes. Tested against an in-process SSH server; not yet tried against real hosts.
- **Your own sync (planned).** Settings, themes, profiles and layouts over WebDAV/Nextcloud, end-to-end encrypted, no
  server of ours.
- **AI with consent (planned).** Terminal context leaves the device only when you allow that one request. Local models
  first.
- **Few dependencies.** No frontend framework and no bundler. Security-critical code (VT parsing, SSH) comes from
  established libraries, never hand-rolled.

## Behind today

- No release, no installer, no signing. macOS from source is the only verified platform.
- Tauri 3 and `libghostty-vt` are pre-release; their APIs move and the build pins exact versions.
- The keyboard uses a small table until the engine's key encoder lands (Phase 2).
- A single maintainer, and no CI by design: everything is built and tested locally.

## What it aims to do differently

- One Rust core for desktop and mobile, so the same layout and sync work on a phone.
- Sync without a server of ours: your Nextcloud, encrypted end to end.
- AI that only sees terminal output when you allow that request.
- Few dependencies, measured performance, and a documented plan that says what is not built.

## Stack

| Part | Choice |
|---|---|
| Core | One Rust crate shared by every platform |
| Terminal engine | `libghostty-vt` behind a `TerminalEngine` trait, `alacritty_terminal` as fallback |
| PTY / SSH | `portable-pty` / `russh` |
| App shell | Tauri 3 (alpha) for desktop, iOS and Android |
| UI | TypeScript, no framework, no bundler |
| Rendering | WebGL2 glyph atlas, one instanced draw per pane, Canvas 2D fallback |
| Persistence | JSON today, SQLite for settings and history |

## Feature status by phase

Built and tested on macOS: local shell, resize, wide Unicode; tiled panes, dividers, tabs, workspaces; scrollback,
selection, copy, paste; saved layout; WebGL2 with Canvas 2D fallback. SSH, WSL and Docker connections with a connection manager are built (core and page; see the note above). Not built:

| Phase | Planned |
|---|---|
| 2 | Key encoder, Kitty keyboard, engine replies, themes, dirty-row frames |
| 4 | Encrypted sync over WebDAV/Nextcloud |
| 5 | File browser, Markdown, images, video, PDF, code editor |
| 6 | `wsh` CLI |
| 7 | Widgets, history search, system monitor, AI chat, Apple on-device AI |
| 8 | Kubernetes, Talos, Azure CLI, GAM7 blocks |
| 9 | iOS (SSH) and Android (local shell and SSH) |
| 10 | Kitty graphics, hyperlinks, signed packages |

Deliberately not included: a web browser block. Phase numbers here follow the project plan summary; the detailed
breakdown is in [roadmap.md](roadmap.md).
