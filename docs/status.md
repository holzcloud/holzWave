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
- The own window frame of Windows and Linux (the page's bar is the window bar, macOS keeps its overlay bar, no Windows 11 snap layouts) has never been seen on a real Windows or Linux window. It is covered by unit tests, a browser test with a mock window in WebKit and Chromium and compile checks, so the drag, the resize edges, the double click, the buttons and the Wayland behaviour are unverified. Under Wayland the setting `Frame` on `Auto` is the own frame, which was not tried there: if the window cannot be moved or resized, set `Frame` to `System` in Settings (the dialog opens from the palette), or edit `window_frame.json` with the window closed ([design.md](design.md) section 7, [config.md](config.md)), and start again to get the system's title bar back. Also unverified or open: the 300 ms deadline of the `gsettings` read, `gsettings` inside a Flatpak, the first macOS window (now built in the start-up code), the keyboard cannot reach any button of the bar (a finding from before this work), and at 75 percent UI scale the window buttons are 4 px taller than the title row.
- Tauri 3 and `libghostty-vt` are pre-release; their APIs move and the build pins exact versions.
- A single maintainer. Everything is built and tested locally; the one exception is a manual GitHub workflow (`linux-packages.yml`) that builds the Linux packages on an amd64 runner, because Zig 0.17 crashes in the Mac's emulated container. It never runs on a push.

## Distribution

Every release is a beta, and no build checks for or announces a new version. The state of each channel, in the one set of
words that [distribution.md](distribution.md) defines (that page is the source; a test of the repository fails when this list
differs from it):

- **GitHub downloads:** `available`
- **Homebrew:** `available`
- **Flathub:** `not submitted`
- **Microsoft Store:** `on hold`
- **Obtainium:** `not built`
- **F-Droid:** `not built`

Builds that were never run: the Linux desktops (GNOME, KDE Plasma, Xfce, Cinnamon, MATE, Sway, Hyprland, i3), Linux aarch64
on real hardware, Windows ARM64, the Flatpak on any machine, the Microsoft Store package (it was never built into an installed
package), and Android on a phone or a tablet (the Android app is built and was run on arm64 emulators only). No release has been
signed with a real key; the rows that will hold the results are in Part 12 of [test-plan.md](test-plan.md), and for Android in Part 13.

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
them, and the New block picker is drawn in the design and not built. The own window frame is built: Windows and Linux draw their own window bar and window buttons in the page, macOS keeps its overlay bar, and the Windows 11 snap layouts are not built. It is tested against a mock window in WebKit and Chromium, by unit tests and by compile checks for Windows and Linux; it has never been seen on a real Windows or Linux window, so the drag, the double click, the resize edges, the buttons, their drawn states, the tear-off of a window without a frame, `gsettings` inside a Flatpak and the behaviour under Wayland are unverified. The first macOS window is now built in the start-up code and nobody has looked at it. The window buttons cannot be reached with the keyboard yet (the page's key handler sends `Tab` and `Enter` to the terminal while a bar button has the focus; the same holds for every button of the bar, and it is open), and at a UI scale of 75 percent the buttons are 4 px taller than the title row. The list of the deviations is in [design.md](design.md) section 7. The web build is built too: holzWave in a browser from one
container, with sign-in by passkey, by password and authenticator code or by single sign-on, a first administrator,
invitations and roles, an encrypted store of each person's saved passwords, one worker and one user id for every person, TLS
from files, ACME or a proxy, and the Compose file and the Helm chart ([server.md](server.md)). Built and tested against a test identity provider, virtual authenticators, the container tests and a Pebble certificate authority on the author's machines; not yet run against a real cluster, a real identity provider, hardware authenticators or real networks. The rows that are
still open (a cluster at the baseline level and a stricter one, an identity provider, passkeys and PRF keys, a certificate
authority, a reverse proxy, the four browsers, both architectures, two real people, restart, backup and capacity) are Part 10
of [test-plan.md](test-plan.md); the suites that need Linux ran in a container only. Android is built too, and tested on three arm64 emulator images (Android 12, the newest Android and an image with 16 KB memory pages), **never on a phone or a tablet**. It has a local shell, SSH, the sync, a secret store behind the Android Keystore with the system prompt, a touch page with a key row, selection and pinch, a tablet layout, one-file hand-over through the system's document dialog, a foreground service that keeps sessions running, and HTTPS through the system's trust store; the cloud AI providers are part of the build. The emulator runs and the checks that ended not run are in [build-android.md](build-android.md), and what only a phone or a person shows is open in Part 13 of [test-plan.md](test-plan.md): keyboards, biometrics, background behaviour, real storage providers and networks, touch feel, real web views, accessibility and the vendors' shells. Only the last full runs of the emulator set were all green, after several runs with a failure (the runs, the one failure that passed when run alone and the one that failed three times and passed alone are in Part 13); a few green runs do not show that the set is stable. Android may end the local shells of an app in the background; SSH connections are not affected. Two defects found on an emulator are fixed and were checked on emulators only: with three-button navigation on Android 12 the page's bottom bar was drawn under the system buttons (its old web view gives the page no safe-area inset; the app now measures the bars and tells the page), and after the system recreated the app's window a shell started after `Stop all` did not bring the foreground service back (the plugin asked the destroyed window; it now asks the one in use). Not built:

| Phase | Planned |
|---|---|
| 9 | Android is built (the paragraph above). Not built or not run: widgets on the phone layout (the sidebar that holds them is not drawn on a phone; the tablet layout keeps the desktop's sidebar, whose widgets were not tried on Android), anything on a phone or a tablet, and the rows of Part 13 of [test-plan.md](test-plan.md). The AI chat on Android sends once the secret store is unlocked; a server on a private address needs the local-network permission of Android 17, which the app declares and asks for at the first such connection (run on an Android 17 emulator only, never on a phone or a real network). iOS is out, no Apple developer account |
| Web build, left out | shared panes between people; an installable web app; a mobile layout of the web page; a long-lived token for scripts; an operator for Kubernetes; compression of the WebSocket (measured, not adopted). As the browser answers them: the two tear-off controls and the local network list say `Not available in the browser.`, the Apple on-device model is not offered, and updating is the operator's job (build and run the new image). Upload takes a file dropped on a shell pane only, and download covers images, video, audio and PDF files |

Deliberately not included: a web browser block. Kitty graphics, hyperlinks, the baseline and the guard, and the signing of the Linux packages are built and are described in the list at the top, together with what nobody has tried; the rows for those are Part 11 of [test-plan.md](test-plan.md). Phase numbers here follow the project plan summary; the detailed
breakdown is in [roadmap.md](roadmap.md).
