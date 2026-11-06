# Where holzWave stands

## Why holzWave

- **One core, five platforms.** Terminal, layout, connections and sync live in one Rust crate. The platform shells stay
  thin, so a feature is written once.
- **One look everywhere.** The chrome is drawn by holzWave and uses the same tokens on every platform: flat, mono,
  hairlines, rose where it matters.
- **Measured performance.** A WebGL2 renderer draws a 160x50 grid in about 1 ms, against 4.5 to 7.7 ms for Canvas 2D.
  The engine parses about 290 MiB/s.
- **A recorded baseline and a guard.** The numbers of the core (parse and pane throughput, update plus encode, key to
  frame, pane start, memory per pane with and without scrollback and with pictures), of the server (key to frame, reattach,
  memory per pane) and of the page in WebKit and Chromium (key to paint, reload to first paint) are recorded for the author's Mac
  and for a Linux container in `perf/`, next to the numbers from before the pictures and links were built. `sh scripts/perf-check.sh`
  measures again and judges by the recorded numbers; it sets no fixed target. Windows is not measured, the desktop window's
  key to glass is not measured, and the browsers ran headless on one Mac ([architecture.md](architecture.md) section 12).
- **Tiled blocks.** Panes, tabs and workspaces with draggable dividers and a layout that survives a restart. Previews
  and widgets come as blocks later.
- **SSH that reconnects.** A pane can run a shell on another machine over SSH (`russh`), with a jump host, host key
  checks, and a reconnect that keeps the pane and what you type. Secrets are in the OS keychain only. WSL and Docker
  connections open as local panes. Tested against an in-process SSH server; not yet tried against real hosts.
- **Your own sync.** Settings, themes, profiles and layouts over WebDAV/Nextcloud, end-to-end encrypted, no
  server of ours.
- **AI with consent, and fully offline on a Mac.** A chat in a pane, in the sidebar, in the palette actions and in
  `wsh ai`, with Anthropic, OpenAI, Gemini, local servers (Ollama, LM Studio, llama.cpp), AI gateways and, on a Mac with
  macOS 26 and Apple Intelligence, the on-device model: the whole AI feature then runs on the Mac itself, offline, with no
  account, no key and nothing leaving the computer. You can share the selection, the screen, the last command and its output (with a
  shell hook) and a file. Terminal context leaves the device only after you share that pane, every send shows what
  goes out, and every tool action asks first. Tested against test servers and scripted providers, not yet against real
  providers or a real Apple model.
- **Your own tools as blocks.** `kubectl`, `talosctl`, `az` and GAM7 on this computer: tables that follow the tool, a Context
  section with a status for each, a card with the exact command before anything changes, and a second press on a context you
  mark as production. Nothing is imported from a kubeconfig or a token file, and nothing is sent anywhere by holzWave. Tested
  against fake tools and recorded fixtures, not yet against real clusters or accounts.
- **Pictures and links in the terminal.** Kitty graphics at all three levels (still pictures, animations, pictures drawn with
  placeholder characters; a picture must carry its bytes, a request that names a file is refused) and OSC 8 hyperlinks, drawn by
  both renderers. A link the page treats as one is dotted at rest and shows its real address in the status line under the
  pointer; a link it refuses (a scheme that is not on the person's list, or a `file` link in a pane of a connection or in the
  browser build) has no dots. A click on a link whose text names another place asks first, a click that the program refuses is
  answered in words in the status line, and only the schemes on the person's list open; a `file` link only shows the file in the
  file manager. The page holds one pane's pictures within that pane's memory setting and all panes together within four times it,
  counts the decoded bytes only (a WebGL texture is a second copy that it does not count), and lets go of a picture that no
  placement asks for any more after five seconds. Tested against the engine, a real server in WebKit and Chromium and a page
  with a mock core. Not tried: a real program that sends pictures or links, the desktop window (no automated test drives it), a
  real SSH pane, a real opener of Linux or Windows, a custom scheme with a real handler, a flood of pictures from a real program
  or many panes at once with the memory of the app measured, a real screen reader or popup blocker, any GPU other than the
  headless one, and Windows at all.
- **Signing.** The Linux packages can be signed by the owner with minisign, together with a signed `SHA256SUMS`, and checked with
  `sh scripts/verify-release.sh`; no release has been signed yet. The Windows installer and the macOS app stay unsigned (the
  macOS app only ad hoc), as [signing.md](signing.md) says.
- **Few dependencies.** No frontend framework and no bundler. Security-critical code (VT parsing, SSH) comes from
  established libraries, never hand-rolled.

## Behind today

- Preview releases only (`0.0.1-beta2`). The Windows installer is not signed (no Authenticode), the macOS app is signed ad hoc only (no Developer ID, no notarisation), and the Linux packages can carry the owner's minisign signatures but none has been signed yet ([signing.md](signing.md)). The author has run it on macOS and Windows; the Linux packages of `0.0.1-beta2` were built on a GitHub runner and not started on a Linux desktop for this release.
- Tauri 3 and `libghostty-vt` are pre-release; their APIs move and the build pins exact versions.
- A single maintainer. Everything is built and tested locally; the one exception is a manual GitHub workflow (`linux-packages.yml`) that builds the Linux packages on an amd64 runner, because Zig 0.17 crashes in the Mac's emulated container. It never runs on a push.

## What it aims to do differently

- One Rust core for desktop and mobile, so the same layout and sync work on a phone.
- Sync without a server of ours: your Nextcloud, encrypted end to end.
- AI that only sees terminal output after you share that pane.
- Few dependencies, measured performance, and a documented plan that says what is not built.

## Stack

| Part | Choice |
|---|---|
| Core | One Rust crate shared by every platform |
| Terminal engine | `libghostty-vt` behind a `TerminalEngine` trait, `alacritty_terminal` as fallback |
| PTY / SSH | `portable-pty` / `russh` |
| App shell | Tauri 3 (alpha) for desktop and Android; the browser build is served by `holzwave-server` |
| UI | TypeScript, no framework, no bundler |
| Rendering | WebGL2 glyph atlas, one instanced draw per pane, Canvas 2D fallback |
| Persistence | JSON today, SQLite for settings and history |
| Server | Rust (`holzwave-server`, axum, rustls), one container, SQLite, a small privileged helper for per-person user ids; the same page and the same commands as the desktop ([server.md](server.md)) |

## Feature status by phase

Built and tested by the author on macOS, Windows and Linux: local shell, resize, wide Unicode; tiled panes, dividers, tabs, workspaces; scrollback,
selection, copy, paste; saved layout; WebGL2 with Canvas 2D fallback. SSH, WSL and Docker connections with a connection manager, and file browsers and previews (Markdown, text, images,
video, audio, PDF; local and over SFTP) are built (core and page; see the notes above). So are the AI chat with its providers and
surfaces, the on-device model, the widgets (described by a JSON Schema), the `^R` binding, a history that follows the host of the focused pane, and the system monitor of this computer and of Linux hosts over SSH. These were tested against test
servers and scripted providers, not yet against real providers or a real Apple model; the manual
rows are in Part 7 of [test-plan.md](test-plan.md). The native integrations are built too: Kubernetes, Talos, Azure CLI and
GAM7 as blocks, the Context section of the sidebar and the status segments, the confirmation card with the second press on
a production context, the profiles dialog and the palette entries. They are tested against fake tools and recorded fixtures
and have not been run against a real cluster, a real Talos, Azure or Google Workspace account, or on Windows and Linux
hardware; those rows are open (Part 9 of [test-plan.md](test-plan.md)). The Flatpak build does not run them, mobile drops
them, and the New block picker is drawn in the design and not built. The web build is built too: holzWave in a browser from one
container, with sign-in by passkey, by password and authenticator code or by single sign-on, a first administrator,
invitations and roles, an encrypted store of each person's saved passwords, one worker and one user id for every person, TLS
from files, ACME or a proxy, and the Compose file and the Helm chart ([server.md](server.md)). Built and tested against a test identity provider, virtual authenticators, the container tests and a Pebble certificate authority on the author's machines; not yet run against a real cluster, a real identity provider, hardware authenticators or real networks. The rows that are
still open (a cluster at the baseline level and a stricter one, an identity provider, passkeys and PRF keys, a certificate
authority, a reverse proxy, the four browsers, both architectures, two real people, restart, backup and capacity) are Part 10
of [test-plan.md](test-plan.md); the suites that need Linux ran in a container only. Not built:

| Phase | Planned |
|---|---|
| 9 | Android (local shell and SSH; iOS is out, no Apple developer account), widgets on mobile, AI provider keys on mobile (they need a mobile secret store) |
| Web build, left out | shared panes between people; an installable web app; a mobile layout of the web page; a long-lived token for scripts; an operator for Kubernetes; compression of the WebSocket (measured, not adopted). As the browser answers them: the two tear-off controls and the local network list say `Not available in the browser.`, the Apple on-device model is not offered, and updating is the operator's job (build and run the new image). Upload takes a file dropped on a shell pane only, and download covers images, video, audio and PDF files |

Deliberately not included: a web browser block. Kitty graphics, hyperlinks, the baseline and the guard, and the signing of the Linux packages are built and are described in the list at the top, together with what nobody has tried; the rows for those are Part 11 of [test-plan.md](test-plan.md). Phase numbers here follow the project plan summary; the detailed
breakdown is in [roadmap.md](roadmap.md).
